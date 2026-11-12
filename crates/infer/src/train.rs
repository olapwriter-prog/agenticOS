//! LoRA training: teach the local model from the requests the human confirmed, on the phone.
//!
//! The base weights stay frozen and compressed (memory-mapped, as for inference). Small adapter
//! matrices on the attention projections learn the change: y = W·x + (α/r)·B·(A·x). B starts at
//! zero, so a new adapter changes nothing until it is trained.
//!
//! This module holds the forward pass with saved activations and the exact backward pass for
//! every architecture the engine runs: dense Qwen2/2.5 and Qwen3, and the Qwen3.5 hybrid.
//! Cross-entropy on the answer tokens only, the output layer, RMSNorm, SwiGLU, residuals, causal
//! grouped-query attention with RoPE, Qwen3's per-head q/k norms, Qwen3.5's output gate, and its
//! Gated DeltaNet layers (short convolution, L2-normed q/k, the gated delta rule recurrence and
//! the gated output norm). The frozen matrices pass the gradient to their input only, through
//! `Model::matmul_t`. Adapters sit on the attention layers; on Qwen3.5 that is every fourth layer,
//! and the gradient flows back through the DeltaNet layers between them.
//!
//! Memory: long examples keep only each layer's input and run the layer again on the way back
//! (`Trainer::budget`); DeltaNet states are recomputed a chunk at a time. AdamW updates the
//! adapters. Gradients are checked against finite differences in the tests.
use crate::model::{rms_norm, rope, sigmoid, silu, softplus, Arch, Model};
use rayon::prelude::*;
use std::collections::BTreeMap;

/// Which projection an adapter sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    Q,
    K,
    V,
    O,
}

impl Target {
    pub const ALL: [Target; 4] = [Target::Q, Target::K, Target::V, Target::O];
    fn tag(self) -> char {
        match self {
            Target::Q => 'q',
            Target::K => 'k',
            Target::V => 'v',
            Target::O => 'o',
        }
    }
}

/// One adapter: A is rank x in, B is out x rank (row-major).
#[derive(Debug, Clone)]
pub struct Pair {
    pub input: usize,
    pub output: usize,
    pub a: Vec<f32>,
    pub b: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct Adapter {
    pub rank: usize,
    pub alpha: f32,
    pub pairs: BTreeMap<(usize, Target), Pair>,
}

fn rng(seed: &mut u64) -> f32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    ((*seed >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
}

impl Adapter {
    /// Adapters on every attention projection of every attention layer. A is small random, B zero.
    pub fn new(m: &Model, rank: usize, alpha: f32, seed: u64) -> Result<Adapter, String> {
        let mut s = seed | 1;
        let mut pairs = BTreeMap::new();
        for (li, l) in m.layers.iter().enumerate() {
            if m.cfg.is_recurrent(li) {
                continue; // DeltaNet layers have no attention projections
            }
            for t in Target::ALL {
                let w = match t {
                    Target::Q => l.q.as_ref(),
                    Target::K => l.k.as_ref(),
                    Target::V => l.v.as_ref(),
                    Target::O => l.o.as_ref(),
                }
                .ok_or(format!("layer {li} has no attention {t:?}"))?;
                let (input, output) = (w.dims[0] as usize, w.dims[1] as usize);
                let scale = (1.0 / input as f32).sqrt();
                let a = (0..rank * input).map(|_| rng(&mut s) * scale).collect();
                pairs.insert((li, t), Pair { input, output, a, b: vec![0.0; output * rank] });
            }
        }
        Ok(Adapter { rank, alpha, pairs })
    }
    /// Does this adapter fit the model (same layers and shapes)?
    pub fn fits(&self, m: &Model) -> Result<(), String> {
        for ((li, t), p) in &self.pairs {
            let l = m.layers.get(*li).ok_or(format!("adapter has layer {li}, the model does not"))?;
            let w = match t {
                Target::Q => l.q.as_ref(),
                Target::K => l.k.as_ref(),
                Target::V => l.v.as_ref(),
                Target::O => l.o.as_ref(),
            }
            .ok_or(format!("layer {li} has no {t:?}"))?;
            if (w.dims[0] as usize, w.dims[1] as usize) != (p.input, p.output) {
                return Err(format!("adapter {li}{t:?} is {}x{}, the model's is {:?}", p.input, p.output, w.dims));
            }
        }
        Ok(())
    }
    pub fn scale(&self) -> f32 {
        self.alpha / self.rank as f32
    }
    pub fn params(&self) -> usize {
        self.pairs.values().map(|p| p.a.len() + p.b.len()).sum()
    }

    /// y (n x out) += scale · B·(A·x) for n inputs x (n x in). Returns A·x (n x rank) for backward.
    /// A projection without an adapter is left as it is.
    pub fn apply(&self, key: (usize, Target), x: &[f32], n: usize, y: &mut [f32]) -> Vec<f32> {
        let Some(p) = self.pairs.get(&key) else { return vec![] };
        let (r, s) = (self.rank, self.scale());
        let mut ax = vec![0f32; n * r];
        for k in 0..n {
            let xk = &x[k * p.input..(k + 1) * p.input];
            for i in 0..r {
                ax[k * r + i] = crate::quant::dot(&p.a[i * p.input..(i + 1) * p.input], xk);
            }
            let yk = &mut y[k * p.output..(k + 1) * p.output];
            for (o, yo) in yk.iter_mut().enumerate() {
                let brow = &p.b[o * r..(o + 1) * r];
                *yo += s * crate::quant::dot(brow, &ax[k * r..(k + 1) * r]);
            }
        }
        ax
    }

    /// Save as a small binary file: magic, rank, alpha, then each pair.
    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        let mut out = Vec::new();
        out.extend(b"AGLORA1\0");
        out.extend((self.rank as u32).to_le_bytes());
        out.extend(self.alpha.to_le_bytes());
        out.extend((self.pairs.len() as u32).to_le_bytes());
        for ((layer, t), p) in &self.pairs {
            out.extend((*layer as u32).to_le_bytes());
            out.push(t.tag() as u8);
            out.extend((p.input as u32).to_le_bytes());
            out.extend((p.output as u32).to_le_bytes());
            for v in p.a.iter().chain(&p.b) {
                out.extend(v.to_le_bytes());
            }
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, out).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    pub fn load(path: &std::path::Path) -> Result<Adapter, String> {
        let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if b.len() < 20 || &b[..8] != b"AGLORA1\0" {
            return Err("not an agentos LoRA adapter".into());
        }
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as usize;
        let rank = u32_at(8);
        let alpha = f32::from_le_bytes(b[12..16].try_into().unwrap());
        let n = u32_at(16);
        let mut at = 20;
        let mut pairs = BTreeMap::new();
        for _ in 0..n {
            let layer = u32_at(at);
            let t = match b[at + 4] {
                b'q' => Target::Q,
                b'k' => Target::K,
                b'v' => Target::V,
                b'o' => Target::O,
                x => return Err(format!("bad target {x}")),
            };
            let (input, output) = (u32_at(at + 5), u32_at(at + 9));
            at += 13;
            let count = rank * input + output * rank;
            if at + count * 4 > b.len() {
                return Err("adapter file is cut short".into());
            }
            let vals: Vec<f32> = b[at..at + count * 4]
                .chunks(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect();
            at += count * 4;
            let (a, bb) = vals.split_at(rank * input);
            pairs.insert((layer, t), Pair { input, output, a: a.to_vec(), b: bb.to_vec() });
        }
        Ok(Adapter { rank, alpha, pairs })
    }
}

/// Gradients for every adapter, same shapes.
pub struct Grads {
    pub pairs: BTreeMap<(usize, Target), (Vec<f32>, Vec<f32>)>,
}

impl Grads {
    fn zeros(a: &Adapter) -> Grads {
        Grads {
            pairs: a.pairs.iter().map(|(k, p)| (*k, (vec![0.0; p.a.len()], vec![0.0; p.b.len()]))).collect(),
        }
    }
    pub fn add(&mut self, o: &Grads) {
        for (k, (ga, gb)) in self.pairs.iter_mut() {
            let (oa, ob) = &o.pairs[k];
            ga.iter_mut().zip(oa).for_each(|(x, y)| *x += y);
            gb.iter_mut().zip(ob).for_each(|(x, y)| *x += y);
        }
    }
}

/// Backward through one adapter: given dy (n x out), the saved A·x (n x rank) and x (n x in),
/// accumulate dA, dB and return the adapter's contribution to dx (n x in).
fn adapter_back(ad: &Adapter, key: (usize, Target), x: &[f32], ax: &[f32], dy: &[f32], n: usize, g: &mut Grads) -> Vec<f32> {
    let Some(p) = ad.pairs.get(&key) else { return vec![0.0; x.len()] };
    let (r, s) = (ad.rank, ad.scale());
    let (ga, gb) = g.pairs.get_mut(&key).unwrap();
    let mut dx = vec![0f32; n * p.input];
    for k in 0..n {
        let dyk = &dy[k * p.output..(k + 1) * p.output];
        let axk = &ax[k * r..(k + 1) * r];
        // dB[o][i] += s · dy[o] · ax[i];  d(ax)[i] = s · Σ_o B[o][i] dy[o]
        let mut dax = vec![0f32; r];
        for (o, &d) in dyk.iter().enumerate() {
            if d == 0.0 {
                continue;
            }
            for i in 0..r {
                gb[o * r + i] += s * d * axk[i];
                dax[i] += s * p.b[o * r + i] * d;
            }
        }
        let xk = &x[k * p.input..(k + 1) * p.input];
        for i in 0..r {
            if dax[i] == 0.0 {
                continue;
            }
            let arow = &p.a[i * p.input..(i + 1) * p.input];
            let garow = &mut ga[i * p.input..(i + 1) * p.input];
            let dxk = &mut dx[k * p.input..(k + 1) * p.input];
            for j in 0..p.input {
                garow[j] += dax[i] * xk[j];
                dxk[j] += dax[i] * arow[j];
            }
        }
    }
    dx
}

/// RMSNorm backward for one row: y = w · x · r, r = 1/sqrt(mean(x²)+eps).
fn rms_back(x: &[f32], w: &[f32], dy: &[f32], eps: f32, dx: &mut [f32]) {
    let d = x.len() as f32;
    let ms = x.iter().map(|v| v * v).sum::<f32>() / d;
    let r = 1.0 / (ms + eps).sqrt();
    let dot: f32 = x.iter().zip(w).zip(dy).map(|((x, w), g)| x * w * g).sum();
    for i in 0..x.len() {
        dx[i] += r * w[i] * dy[i] - r * r * r / d * x[i] * dot;
    }
}

/// The inverse rotation, for RoPE's backward pass.
fn rope_back(v: &mut [f32], pos: usize, n_rot: usize, base: f32) {
    let half = n_rot / 2;
    for i in 0..half {
        let theta = pos as f32 * base.powf(-2.0 * i as f32 / n_rot as f32);
        let (s, c) = theta.sin_cos();
        let (a, b) = (v[i], v[i + half]);
        v[i] = a * c + b * s;
        v[i + half] = -a * s + b * c;
    }
}

/// What an attention layer keeps from its forward pass for the backward pass.
struct AttnSave {
    ax: BTreeMap<Target, Vec<f32>>,
    /// q and k before their RMS norms (Qwen3, Qwen3.5); empty without norms
    q_pre: Vec<f32>,
    k_pre: Vec<f32>,
    /// q and k as attention sees them (normed, rotated), and v
    q: Vec<f32>,
    k: Vec<f32>,
    v: Vec<f32>,
    /// attention output before the output gate and the gate itself (Qwen3.5); empty otherwise
    att: Vec<f32>,
    gate: Vec<f32>,
    /// the input to W_o
    o: Vec<f32>,
}

/// What a Gated DeltaNet layer keeps. The recurrent states are not kept (one d x d matrix per
/// head per token): the backward pass recomputes them a chunk at a time.
struct DeltaSave {
    /// short convolution output before SiLU (T x channels)
    pre: Vec<f32>,
    /// after SiLU, with the q and k heads L2-normalized
    mixed: Vec<f32>,
    z: Vec<f32>,
    beta: Vec<f32>,
    alpha: Vec<f32>,
    /// the recurrence's output before the gated norm (T x value dim)
    o_pre: Vec<f32>,
}

enum Mix {
    Attn(AttnSave),
    Delta(DeltaSave),
}

struct LayerSave {
    x0: Vec<f32>,
    h: Vec<f32>,
    mix: Mix,
    x1: Vec<f32>,
    g: Vec<f32>,
    u: Vec<f32>,
}

/// Steps between kept DeltaNet states in the backward pass: memory per head is
/// (T/CHUNK + CHUNK) states instead of T.
const CHUNK: usize = 32;

/// Per-head inputs of the DeltaNet recurrence at one step.
struct Step<'a> {
    q: &'a [f32],
    k: &'a [f32],
    v: &'a [f32],
    b: f32,
    decay: f32,
}

/// One step of the gated delta rule on one head's state S (rows: value index, columns: key
/// index): S = decay·S; S += k ⊗ b·(v − S·k). Writes S·q·qs into `o` if given.
fn delta_step(s: &mut [f32], st: &Step, qs: f32, o: Option<&mut [f32]>) {
    let sd = st.k.len();
    for j in 0..sd {
        let row = &mut s[j * sd..(j + 1) * sd];
        row.iter_mut().for_each(|x| *x *= st.decay);
        let sk = crate::quant::dot(row, st.k);
        let delta = (st.v[j] - sk) * st.b;
        row.iter_mut().zip(st.k).for_each(|(x, k)| *x += k * delta);
    }
    if let Some(o) = o {
        for j in 0..sd {
            o[j] = crate::quant::dot(&s[j * sd..(j + 1) * sd], st.q) * qs;
        }
    }
}

fn silu_back(x: f32) -> f32 {
    let s = 1.0 / (1.0 + (-x).exp());
    s * (1.0 + x * (1.0 - s))
}

/// How a batch is laid out in one pass: a prefix every example shares (the system prompt), once,
/// then each example's own tokens. A token sees the prefix and the earlier tokens of its own
/// example, at the positions it would have alone, so the result is exactly that of running each
/// example by itself; the shared part is computed once instead of once per example.
pub struct Layout {
    pub tokens: Vec<u32>,
    /// length of the shared prefix (0 when nothing is shared)
    pub shared: usize,
    /// each example's own tokens, as packed index ranges
    pub spans: Vec<(usize, usize)>,
    /// each token's position in its own sequence (for RoPE)
    pub pos: Vec<usize>,
    /// the trained positions: what comes next there, how much it counts, for which example
    pub targets: Vec<Goal>,
}

/// One trained position: at `pos`, predict `token`, weighted so each example's loss is its mean.
pub struct Goal {
    pub pos: usize,
    pub token: u32,
    pub weight: f32,
    pub example: usize,
}

impl Layout {
    /// One example, nothing shared.
    pub fn one(tokens: &[u32], train: &[bool]) -> Layout {
        Layout::pack(&[(tokens.to_vec(), train.to_vec())])
    }

    /// Pack examples: the longest prefix they all share, up to (not including) the token before
    /// the first trained one, goes first and once.
    pub fn pack(items: &[(Vec<u32>, Vec<bool>)]) -> Layout {
        let mut shared = 0;
        if items.len() > 1 {
            let first = &items[0].0;
            shared = first.len();
            for (t, train) in items {
                let common = first.iter().zip(t).take_while(|(a, b)| a == b).count();
                let first_trained = train.iter().position(|&x| x).unwrap_or(t.len());
                // the prefix's last token must not be a trained prediction (each example's next
                // token there may differ), and every example keeps a token of its own
                shared = shared.min(common).min(first_trained.saturating_sub(1)).min(t.len().saturating_sub(1));
            }
        }
        let mut tokens = items.first().map(|i| i.0[..shared].to_vec()).unwrap_or_default();
        let mut pos: Vec<usize> = (0..shared).collect();
        let mut spans = Vec::with_capacity(items.len());
        let mut targets = Vec::new();
        for (e, (t, train)) in items.iter().enumerate() {
            let start = tokens.len();
            tokens.extend_from_slice(&t[shared..]);
            pos.extend(shared..t.len());
            spans.push((start, tokens.len()));
            let trained: Vec<usize> = (0..t.len().saturating_sub(1)).filter(|&i| train[i + 1]).collect();
            let weight = 1.0 / trained.len().max(1) as f32;
            for i in trained {
                // i >= shared: the prefix ends before the token that predicts the first answer
                targets.push(Goal { pos: start + i - shared, token: t[i + 1], weight, example: e });
            }
        }
        Layout { tokens, shared, spans, pos, targets }
    }

    /// The span a token belongs to (`None` for the shared prefix).
    fn span_of(&self, t: usize) -> Option<(usize, usize)> {
        if t < self.shared {
            return None;
        }
        self.spans.iter().copied().find(|&(a, b)| t >= a && t < b)
    }

    /// The tokens `t` attends to: the shared prefix, then its own example up to itself.
    pub fn visible(&self, t: usize) -> impl Iterator<Item = usize> {
        let (prefix, own) = match self.span_of(t) {
            None => (0, 0),
            Some((a, _)) => (self.shared, a),
        };
        (0..prefix).chain(own..=t)
    }

    /// The packed index of the token `back` steps before `t` in t's own sequence.
    pub fn before(&self, t: usize, back: usize) -> Option<usize> {
        let p = self.pos[t].checked_sub(back)?;
        match self.span_of(t) {
            None => Some(p),
            Some(_) if p < self.shared => Some(p),
            Some((a, _)) => Some(a + p - self.shared),
        }
    }
}

pub struct Trainer<'m> {
    pub model: &'m Model,
    /// Exact f32 matmuls (for gradient checks) instead of the 8-bit fast path.
    pub exact: bool,
    /// Bytes the saved activations may take. Above this only each layer's input is kept and
    /// the layer is run again during the backward pass (activation checkpointing: about a
    /// third more compute, memory of one layer instead of all).
    pub budget: usize,
}

impl<'m> Trainer<'m> {
    pub fn new(model: &'m Model) -> Self {
        Trainer { model, exact: false, budget: 256 << 20 }
    }

    fn mm(&self, w: &crate::gguf::TensorInfo, x: &[f32], n: usize) -> Vec<f32> {
        if self.exact {
            self.model.matmul_exact(w, x, n)
        } else {
            self.model.matmul(w, x, n)
        }
    }

    /// Bytes the activations of a `t_len`-token example take if every layer's are kept.
    pub fn saved_bytes(&self, t_len: usize) -> usize {
        let c = &self.model.cfg;
        let (hd, nh, kvd) = (c.head_dim, c.n_head, c.n_head_kv * c.head_dim);
        let gated = c.arch == Arch::Qwen35;
        let normed = c.arch != Arch::Qwen2;
        let qw = if gated { 2 * nh * hd } else { nh * hd };
        let ch = c.ssm_head_dim * (2 * c.ssm_k_heads + c.ssm_v_heads);
        let val = c.ssm_v_heads * c.ssm_head_dim;
        let per_token: usize = (0..c.n_layer)
            .map(|li| {
                let mix = if c.is_recurrent(li) {
                    2 * ch + 2 * val + 2 * c.ssm_v_heads
                } else {
                    let r = self.model.adapter.as_ref().map_or(8, |a| a.rank) * 4;
                    let norms = if normed { nh * hd + kvd } else { 0 };
                    let gate = if gated { 2 * nh * hd } else { 0 };
                    r + norms + nh * hd + 2 * kvd + gate + nh * hd + (qw - nh * hd)
                };
                3 * c.n_embd + 2 * c.n_ff + mix
            })
            .sum();
        per_token * t_len * 4
    }

    /// Will an example of this length be trained with activation checkpointing?
    pub fn checkpoints(&self, t_len: usize) -> bool {
        self.saved_bytes(t_len) > self.budget
    }

    /// One layer's forward pass on `x` (T x d, replaced by the layer's output), keeping what
    /// the backward pass needs.
    fn layer_fwd(&self, ad: &Adapter, li: usize, x: &mut [f32], lay: &Layout) -> LayerSave {
        let m = self.model;
        let c = &m.cfg;
        let l = &m.layers[li];
        let d = c.n_embd;
        let t_len = x.len() / d;
        let x0 = x.to_vec();
        let mut h = x0.clone();
        for row in h.chunks_mut(d) {
            rms_norm(row, &l.norms.0, c.eps);
        }
        let (out, mix) = if c.is_recurrent(li) {
            let (o, s) = self.delta_fwd(li, &h, lay);
            (o, Mix::Delta(s))
        } else {
            let (o, s) = self.attn_fwd(ad, li, &h, lay);
            (o, Mix::Attn(s))
        };
        x.iter_mut().zip(&out).for_each(|(a, b)| *a += b);
        let x1 = x.to_vec();
        let mut h2 = x1.clone();
        for row in h2.chunks_mut(d) {
            rms_norm(row, &l.norms.1, c.eps);
        }
        let g = self.mm(&l.ffn_gate, &h2, t_len);
        let u = self.mm(&l.ffn_up, &h2, t_len);
        let a: Vec<f32> = g.iter().zip(&u).map(|(g, u)| silu(*g) * u).collect();
        let f = self.mm(&l.ffn_down, &a, t_len);
        x.iter_mut().zip(&f).for_each(|(a, b)| *a += b);
        LayerSave { x0, h, mix, x1, g, u }
    }

    /// Causal (gated) attention with the adapters; each token sees what the layout lets it.
    fn attn_fwd(&self, ad: &Adapter, li: usize, h: &[f32], lay: &Layout) -> (Vec<f32>, AttnSave) {
        let t_len = lay.tokens.len();
        let m = self.model;
        let c = &m.cfg;
        let l = &m.layers[li];
        let (hd, nh, nkv) = (c.head_dim, c.n_head, c.n_head_kv);
        let kvd = nkv * hd;
        let group = nh / nkv;
        let scale = 1.0 / (hd as f32).sqrt();
        let gated = c.arch == Arch::Qwen35;
        let qw = if gated { 2 * nh * hd } else { nh * hd };
        let mut ax = BTreeMap::new();
        let mut q_full = self.mm(l.q.as_ref().unwrap(), h, t_len);
        ax.insert(Target::Q, ad.apply((li, Target::Q), h, t_len, &mut q_full));
        let mut k = self.mm(l.k.as_ref().unwrap(), h, t_len);
        ax.insert(Target::K, ad.apply((li, Target::K), h, t_len, &mut k));
        let mut v = self.mm(l.v.as_ref().unwrap(), h, t_len);
        ax.insert(Target::V, ad.apply((li, Target::V), h, t_len, &mut v));
        // per head: [query | gate] when gated
        let (mut q, gate) = if gated {
            let mut q = vec![0f32; t_len * nh * hd];
            let mut gate = vec![0f32; t_len * nh * hd];
            for t in 0..t_len {
                for hh in 0..nh {
                    let src = &q_full[t * qw + hh * 2 * hd..t * qw + (hh + 1) * 2 * hd];
                    q[(t * nh + hh) * hd..(t * nh + hh + 1) * hd].copy_from_slice(&src[..hd]);
                    gate[(t * nh + hh) * hd..(t * nh + hh + 1) * hd].copy_from_slice(&src[hd..]);
                }
            }
            (q, gate)
        } else {
            (q_full, vec![])
        };
        for (buf, bias) in [(&mut q, &l.q_bias), (&mut k, &l.k_bias), (&mut v, &l.v_bias)] {
            if let Some(b) = bias {
                for row in buf.chunks_mut(b.len()) {
                    row.iter_mut().zip(b).for_each(|(a, b)| *a += b);
                }
            }
        }
        let q_pre = if l.q_norm.is_some() { q.clone() } else { vec![] };
        let k_pre = if l.k_norm.is_some() { k.clone() } else { vec![] };
        for t in 0..t_len {
            for hh in 0..nh {
                let hv = &mut q[(t * nh + hh) * hd..(t * nh + hh + 1) * hd];
                if let Some(w) = &l.q_norm {
                    rms_norm(hv, w, c.eps);
                }
                rope(hv, lay.pos[t], c.n_rot, c.rope_base);
            }
            for hh in 0..nkv {
                let hv = &mut k[t * kvd + hh * hd..t * kvd + (hh + 1) * hd];
                if let Some(w) = &l.k_norm {
                    rms_norm(hv, w, c.eps);
                }
                rope(hv, lay.pos[t], c.n_rot, c.rope_base);
            }
        }
        // causal attention, one head per thread, one row of weights at a time
        let heads: Vec<Vec<f32>> = (0..nh)
            .into_par_iter()
            .map(|hh| {
                let kvh = hh / group;
                let mut oh = vec![0f32; t_len * hd];
                let mut row = vec![0f32; t_len];
                for t in 0..t_len {
                    let qv = &q[(t * nh + hh) * hd..(t * nh + hh + 1) * hd];
                    let mut mx = f32::NEG_INFINITY;
                    for j in lay.visible(t) {
                        row[j] = crate::quant::dot(qv, &k[j * kvd + kvh * hd..j * kvd + (kvh + 1) * hd]) * scale;
                        mx = mx.max(row[j]);
                    }
                    let mut sum = 0.0;
                    for j in lay.visible(t) {
                        row[j] = (row[j] - mx).exp();
                        sum += row[j];
                    }
                    let ov = &mut oh[t * hd..(t + 1) * hd];
                    for j in lay.visible(t) {
                        let w = row[j] / sum;
                        let vr = &v[j * kvd + kvh * hd..j * kvd + (kvh + 1) * hd];
                        ov.iter_mut().zip(vr).for_each(|(a, b)| *a += w * b);
                    }
                }
                oh
            })
            .collect();
        let mut att = vec![0f32; t_len * nh * hd];
        for (hh, oh) in heads.iter().enumerate() {
            for t in 0..t_len {
                att[(t * nh + hh) * hd..(t * nh + hh + 1) * hd].copy_from_slice(&oh[t * hd..(t + 1) * hd]);
            }
        }
        let (att, o) = if gated {
            let o = att.iter().zip(&gate).map(|(a, g)| a * sigmoid(*g)).collect();
            (att, o)
        } else {
            (vec![], att)
        };
        let mut y = self.mm(l.o.as_ref().unwrap(), &o, t_len);
        ax.insert(Target::O, ad.apply((li, Target::O), &o, t_len, &mut y));
        (y, AttnSave { ax, q_pre, k_pre, q, k, v, att, gate, o })
    }

    /// Gated DeltaNet from a zero state, as `Model::delta_net` computes it. The shared prefix runs
    /// once; every example continues from the state (and convolution window) it ends with.
    fn delta_fwd(&self, li: usize, h: &[f32], lay: &Layout) -> (Vec<f32>, DeltaSave) {
        let t_len = lay.tokens.len();
        let m = self.model;
        let c = &m.cfg;
        let l = &m.layers[li];
        let sd = c.ssm_head_dim;
        let (hk, hv) = (c.ssm_k_heads, c.ssm_v_heads);
        let (key_dim, val_dim) = (hk * sd, hv * sd);
        let ch = 2 * key_dim + val_dim;
        let kern = c.conv_kernel;
        let qkv = self.mm(l.qkv.as_ref().unwrap(), h, t_len);
        let z = self.mm(l.gate.as_ref().unwrap(), h, t_len);
        let beta = self.mm(l.ssm_beta.as_ref().unwrap(), h, t_len);
        let alpha = self.mm(l.ssm_alpha.as_ref().unwrap(), h, t_len);
        // short causal convolution over each token's own sequence (zeros before its first
        // token), then SiLU
        let mut pre = vec![0f32; t_len * ch];
        for t in 0..t_len {
            for kk in 0..kern {
                let Some(src) = lay.before(t, kern - 1 - kk) else { continue };
                for cc in 0..ch {
                    pre[t * ch + cc] += qkv[src * ch + cc] * l.conv1d[cc * kern + kk];
                }
            }
        }
        let mut mixed: Vec<f32> = pre.iter().map(|v| silu(*v)).collect();
        for row in mixed.chunks_mut(ch) {
            for head in row[..2 * key_dim].chunks_mut(sd) {
                let norm = head.iter().map(|v| v * v).sum::<f32>().sqrt().max(c.eps);
                head.iter_mut().for_each(|v| *v /= norm);
            }
        }
        let (bs, decays) = self.gates(li, &beta, &alpha);
        let qs = 1.0 / (sd as f32).sqrt();
        let per_head: Vec<Vec<f32>> = (0..hv)
            .into_par_iter()
            .map(|vh| {
                let mut s = vec![0f32; sd * sd];
                let mut o = vec![0f32; t_len * sd];
                for t in 0..lay.shared {
                    let st = self.step(&mixed, &bs, &decays, t, vh);
                    delta_step(&mut s, &st, qs, Some(&mut o[t * sd..(t + 1) * sd]));
                }
                for &(a, b) in &lay.spans {
                    let mut s = s.clone(); // each example starts where the prefix ended
                    for t in a..b {
                        let st = self.step(&mixed, &bs, &decays, t, vh);
                        delta_step(&mut s, &st, qs, Some(&mut o[t * sd..(t + 1) * sd]));
                    }
                }
                o
            })
            .collect();
        let mut o_pre = vec![0f32; t_len * val_dim];
        let mut outs = vec![0f32; t_len * val_dim];
        for (vh, o) in per_head.iter().enumerate() {
            for t in 0..t_len {
                let at = t * val_dim + vh * sd;
                o_pre[at..at + sd].copy_from_slice(&o[t * sd..(t + 1) * sd]);
                let y = &mut outs[at..at + sd];
                y.copy_from_slice(&o[t * sd..(t + 1) * sd]);
                rms_norm(y, &l.ssm_norm, c.eps);
                y.iter_mut().zip(&z[at..at + sd]).for_each(|(a, z)| *a *= silu(*z));
            }
        }
        let out = self.mm(l.ssm_out.as_ref().unwrap(), &outs, t_len);
        (out, DeltaSave { pre, mixed, z, beta, alpha, o_pre })
    }

    /// Write strength b = σ(β) and decay exp(softplus(α + dt)·A) for every token and head.
    fn gates(&self, li: usize, beta: &[f32], alpha: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let l = &self.model.layers[li];
        let hv = self.model.cfg.ssm_v_heads;
        let bs = beta.iter().map(|b| sigmoid(*b)).collect();
        let decays = alpha
            .iter()
            .enumerate()
            .map(|(i, a)| (softplus(a + l.dt_bias[i % hv]) * l.ssm_a[i % hv]).exp())
            .collect();
        (bs, decays)
    }

    fn step<'a>(&self, mixed: &'a [f32], bs: &[f32], decays: &[f32], t: usize, vh: usize) -> Step<'a> {
        let c = &self.model.cfg;
        let sd = c.ssm_head_dim;
        let (hk, hv) = (c.ssm_k_heads, c.ssm_v_heads);
        let key_dim = hk * sd;
        let ch = 2 * key_dim + hv * sd;
        let kh = vh % hk; // llama.cpp tiles the k/q heads to the v heads
        let row = &mixed[t * ch..(t + 1) * ch];
        Step {
            q: &row[kh * sd..(kh + 1) * sd],
            k: &row[key_dim + kh * sd..key_dim + (kh + 1) * sd],
            v: &row[2 * key_dim + vh * sd..2 * key_dim + (vh + 1) * sd],
            b: bs[t * hv + vh],
            decay: decays[t * hv + vh],
        }
    }

    /// Mean cross-entropy of `tokens[t+1]` at every position t with `train[t+1]` true, and,
    /// if asked, the gradients for the adapter.
    pub fn loss(&self, ad: &Adapter, tokens: &[u32], train: &[bool], want_grads: bool) -> (f32, Option<Grads>) {
        let lay = Layout::one(tokens, train);
        let (losses, g) = self.loss_layout(ad, &lay, want_grads);
        (losses[0], g)
    }

    /// Several examples in one pass: each example's loss as `loss` would give it, and the sum of
    /// their gradients. The prompt they share (the system prompt) is computed once.
    pub fn loss_batch(&self, ad: &Adapter, items: &[(Vec<u32>, Vec<bool>)], want_grads: bool) -> (Vec<f32>, Option<Grads>) {
        self.loss_layout(ad, &Layout::pack(items), want_grads)
    }

    fn loss_layout(&self, ad: &Adapter, lay: &Layout, want_grads: bool) -> (Vec<f32>, Option<Grads>) {
        let m = self.model;
        let c = &m.cfg;
        let (t_len, d) = (lay.tokens.len(), c.n_embd);
        let recompute = want_grads && self.checkpoints(t_len);
        let mut x = m.embed(&lay.tokens);
        let mut saves: Vec<Option<LayerSave>> = Vec::with_capacity(c.n_layer);
        let mut inputs: Vec<Vec<f32>> = Vec::new();
        for li in 0..c.n_layer {
            if recompute {
                inputs.push(x.clone());
            }
            let s = self.layer_fwd(ad, li, &mut x, lay);
            if want_grads && !recompute {
                saves.push(Some(s));
            }
        }
        // loss on the answer positions
        let n_ex = lay.spans.len();
        let pos: Vec<usize> = lay.targets.iter().map(|t| t.pos).collect();
        if pos.is_empty() {
            return (vec![0.0; n_ex], want_grads.then(|| Grads::zeros(ad)));
        }
        let mut hf = Vec::with_capacity(pos.len() * d);
        for &t in &pos {
            let mut row = x[t * d..(t + 1) * d].to_vec();
            rms_norm(&mut row, &m.output_norm, c.eps);
            hf.extend(row);
        }
        let logits = self.mm(&m.output, &hf, pos.len());
        let nv = c.n_vocab;
        let mut losses = vec![0.0f32; n_ex];
        let mut dlogits = vec![0f32; logits.len()];
        for (i, tg) in lay.targets.iter().enumerate() {
            let row = &logits[i * nv..(i + 1) * nv];
            let mx = row.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let sum: f32 = row.iter().map(|v| (v - mx).exp()).sum();
            let target = tg.token as usize;
            losses[tg.example] += -(row[target] - mx - sum.ln()) * tg.weight;
            if want_grads {
                let dr = &mut dlogits[i * nv..(i + 1) * nv];
                for (j, v) in row.iter().enumerate() {
                    dr[j] = (v - mx).exp() / sum * tg.weight;
                }
                dr[target] -= tg.weight;
            }
        }
        if !want_grads {
            return (losses, None);
        }

        // ---- backward ----
        let mut grads = Grads::zeros(ad);
        let dhf = m.matmul_t(&m.output, &dlogits, pos.len());
        let mut dx = vec![0f32; t_len * d];
        for (i, &t) in pos.iter().enumerate() {
            rms_back(&x[t * d..(t + 1) * d], &m.output_norm, &dhf[i * d..(i + 1) * d], c.eps, &mut dx[t * d..(t + 1) * d]);
        }
        for li in (0..c.n_layer).rev() {
            let l = &m.layers[li];
            let s = if recompute {
                let mut xx = std::mem::take(&mut inputs[li]);
                self.layer_fwd(ad, li, &mut xx, lay)
            } else {
                saves[li].take().unwrap()
            };
            // FFN: x2 = x1 + down(silu(gate(h2)) * up(h2))
            let da = m.matmul_t(&l.ffn_down, &dx, t_len);
            let mut dg = vec![0f32; da.len()];
            let mut du = vec![0f32; da.len()];
            for i in 0..da.len() {
                let (g, u) = (s.g[i], s.u[i]);
                let sg = 1.0 / (1.0 + (-g).exp());
                dg[i] = da[i] * u * sg * (1.0 + g * (1.0 - sg));
                du[i] = da[i] * g * sg;
            }
            let dh2a = m.matmul_t(&l.ffn_gate, &dg, t_len);
            let dh2b = m.matmul_t(&l.ffn_up, &du, t_len);
            let mut dx1 = dx;
            for t in 0..t_len {
                let dh2: Vec<f32> = dh2a[t * d..(t + 1) * d].iter().zip(&dh2b[t * d..(t + 1) * d]).map(|(a, b)| a + b).collect();
                rms_back(&s.x1[t * d..(t + 1) * d], &l.norms.1, &dh2, c.eps, &mut dx1[t * d..(t + 1) * d]);
            }
            // mixer: x1 = x0 + mix(h)
            let dh = match &s.mix {
                Mix::Attn(a) => self.attn_back(ad, li, &s.h, a, &dx1, lay, &mut grads),
                Mix::Delta(dn) => self.delta_back(li, dn, &dx1, lay),
            };
            let mut dx0 = dx1;
            for t in 0..t_len {
                rms_back(&s.x0[t * d..(t + 1) * d], &l.norms.0, &dh[t * d..(t + 1) * d], c.eps, &mut dx0[t * d..(t + 1) * d]);
            }
            dx = dx0;
        }
        (losses, Some(grads))
    }

    /// Backward through attention: dy is the gradient of the layer's attention output. Adds the
    /// adapter gradients, returns the gradient of the normed input h.
    #[allow(clippy::too_many_arguments)]
    fn attn_back(&self, ad: &Adapter, li: usize, h: &[f32], s: &AttnSave, dy: &[f32], lay: &Layout, grads: &mut Grads) -> Vec<f32> {
        let m = self.model;
        let c = &m.cfg;
        let l = &m.layers[li];
        let (hd, nh, nkv) = (c.head_dim, c.n_head, c.n_head_kv);
        let t_len = dy.len() / c.n_embd;
        let kvd = nkv * hd;
        let group = nh / nkv;
        let scale = 1.0 / (hd as f32).sqrt();
        let gated = c.arch == Arch::Qwen35;
        let mut d_o = m.matmul_t(l.o.as_ref().unwrap(), dy, t_len);
        let extra = adapter_back(ad, (li, Target::O), &s.o, &s.ax[&Target::O], dy, t_len, grads);
        d_o.iter_mut().zip(&extra).for_each(|(a, b)| *a += b);
        // output gate: o = att · σ(gate)
        let mut dgate = vec![];
        if gated {
            dgate = vec![0f32; d_o.len()];
            for i in 0..d_o.len() {
                let sg = sigmoid(s.gate[i]);
                dgate[i] = d_o[i] * s.att[i] * sg * (1.0 - sg);
                d_o[i] *= sg;
            }
        }
        let mut dq = vec![0f32; s.q.len()];
        let mut dk = vec![0f32; s.k.len()];
        let mut dv = vec![0f32; s.v.len()];
        // per head, into per-head buffers (heads of a group share k and v)
        let per_head: Vec<(usize, Vec<f32>, Vec<f32>, Vec<f32>)> = (0..nh)
            .into_par_iter()
            .map(|hh| {
                let kvh = hh / group;
                let mut dqh = vec![0f32; t_len * hd];
                let mut dkh = vec![0f32; t_len * hd];
                let mut dvh = vec![0f32; t_len * hd];
                // the attention weights are recomputed, not kept: T x T per head per layer
                // would not fit a phone for long prompts
                let mut pr = vec![0f32; t_len];
                let mut dp = vec![0f32; t_len];
                for t in 0..t_len {
                    let qv = &s.q[(t * nh + hh) * hd..(t * nh + hh + 1) * hd];
                    let mut mx = f32::NEG_INFINITY;
                    for j in lay.visible(t) {
                        pr[j] = crate::quant::dot(qv, &s.k[j * kvd + kvh * hd..j * kvd + (kvh + 1) * hd]) * scale;
                        mx = mx.max(pr[j]);
                    }
                    let mut sum = 0.0;
                    for j in lay.visible(t) {
                        pr[j] = (pr[j] - mx).exp();
                        sum += pr[j];
                    }
                    lay.visible(t).for_each(|j| pr[j] /= sum);
                    let dov = &d_o[(t * nh + hh) * hd..(t * nh + hh + 1) * hd];
                    for j in lay.visible(t) {
                        let vr = &s.v[j * kvd + kvh * hd..j * kvd + (kvh + 1) * hd];
                        dp[j] = crate::quant::dot(dov, vr);
                        let w = pr[j];
                        dvh[j * hd..(j + 1) * hd].iter_mut().zip(dov).for_each(|(a, b)| *a += w * b);
                    }
                    let sum: f32 = lay.visible(t).map(|j| pr[j] * dp[j]).sum();
                    for j in lay.visible(t) {
                        let ds = pr[j] * (dp[j] - sum) * scale;
                        if ds == 0.0 {
                            continue;
                        }
                        let kr = &s.k[j * kvd + kvh * hd..j * kvd + (kvh + 1) * hd];
                        dqh[t * hd..(t + 1) * hd].iter_mut().zip(kr).for_each(|(a, b)| *a += ds * b);
                        dkh[j * hd..(j + 1) * hd].iter_mut().zip(qv).for_each(|(a, b)| *a += ds * b);
                    }
                }
                (kvh, dqh, dkh, dvh)
            })
            .collect();
        for (hh, (kvh, dqh, dkh, dvh)) in per_head.into_iter().enumerate() {
            for t in 0..t_len {
                dq[(t * nh + hh) * hd..(t * nh + hh + 1) * hd].copy_from_slice(&dqh[t * hd..(t + 1) * hd]);
                let kslot = &mut dk[t * kvd + kvh * hd..t * kvd + (kvh + 1) * hd];
                kslot.iter_mut().zip(&dkh[t * hd..(t + 1) * hd]).for_each(|(a, b)| *a += b);
                let vslot = &mut dv[t * kvd + kvh * hd..t * kvd + (kvh + 1) * hd];
                vslot.iter_mut().zip(&dvh[t * hd..(t + 1) * hd]).for_each(|(a, b)| *a += b);
            }
        }
        // RoPE, then the per-head q/k norms (Qwen3, Qwen3.5)
        for t in 0..t_len {
            for hh in 0..nh {
                rope_back(&mut dq[(t * nh + hh) * hd..(t * nh + hh + 1) * hd], lay.pos[t], c.n_rot, c.rope_base);
            }
            for hh in 0..nkv {
                rope_back(&mut dk[t * kvd + hh * hd..t * kvd + (hh + 1) * hd], lay.pos[t], c.n_rot, c.rope_base);
            }
        }
        for (dbuf, pre, w) in [(&mut dq, &s.q_pre, &l.q_norm), (&mut dk, &s.k_pre, &l.k_norm)] {
            if let Some(w) = w {
                let mut out = vec![0f32; dbuf.len()];
                for ((o, g), x) in out.chunks_mut(hd).zip(dbuf.chunks(hd)).zip(pre.chunks(hd)) {
                    rms_back(x, w, g, c.eps, o);
                }
                *dbuf = out;
            }
        }
        // back into the projection's layout: per head [query | gate] when gated
        let dq_full = if gated {
            let qw = 2 * nh * hd;
            let mut f = vec![0f32; t_len * qw];
            for t in 0..t_len {
                for hh in 0..nh {
                    let dst = &mut f[t * qw + hh * 2 * hd..t * qw + (hh + 1) * 2 * hd];
                    dst[..hd].copy_from_slice(&dq[(t * nh + hh) * hd..(t * nh + hh + 1) * hd]);
                    dst[hd..].copy_from_slice(&dgate[(t * nh + hh) * hd..(t * nh + hh + 1) * hd]);
                }
            }
            f
        } else {
            dq
        };
        let mut dh = m.matmul_t(l.q.as_ref().unwrap(), &dq_full, t_len);
        let extra = adapter_back(ad, (li, Target::Q), h, &s.ax[&Target::Q], &dq_full, t_len, grads);
        dh.iter_mut().zip(&extra).for_each(|(a, b)| *a += b);
        for (w, dyy, tgt) in [(l.k.as_ref().unwrap(), &dk, Target::K), (l.v.as_ref().unwrap(), &dv, Target::V)] {
            let part = m.matmul_t(w, dyy, t_len);
            dh.iter_mut().zip(&part).for_each(|(a, b)| *a += b);
            let extra = adapter_back(ad, (li, tgt), h, &s.ax[&tgt], dyy, t_len, grads);
            dh.iter_mut().zip(&extra).for_each(|(a, b)| *a += b);
        }
        dh
    }

    /// Backward through a Gated DeltaNet layer (frozen, no adapters): the gradient of its input
    /// h from the gradient dy of its output.
    fn delta_back(&self, li: usize, s: &DeltaSave, dy: &[f32], lay: &Layout) -> Vec<f32> {
        let m = self.model;
        let c = &m.cfg;
        let l = &m.layers[li];
        let sd = c.ssm_head_dim;
        let (hk, hv) = (c.ssm_k_heads, c.ssm_v_heads);
        let (key_dim, val_dim) = (hk * sd, hv * sd);
        let ch = 2 * key_dim + val_dim;
        let kern = c.conv_kernel;
        let t_len = dy.len() / c.n_embd;
        let qs = 1.0 / (sd as f32).sqrt();
        // output projection, then the gated norm y = norm(o) · silu(z)
        let dout = m.matmul_t(l.ssm_out.as_ref().unwrap(), dy, t_len);
        let mut dz = vec![0f32; t_len * val_dim];
        let mut d_o = vec![0f32; t_len * val_dim];
        for i in (0..t_len * val_dim).step_by(sd) {
            let mut n = s.o_pre[i..i + sd].to_vec();
            rms_norm(&mut n, &l.ssm_norm, c.eps);
            let mut dn = vec![0f32; sd];
            for j in 0..sd {
                let z = s.z[i + j];
                dz[i + j] = dout[i + j] * n[j] * silu_back(z);
                dn[j] = dout[i + j] * silu(z);
            }
            rms_back(&s.o_pre[i..i + sd], &l.ssm_norm, &dn, c.eps, &mut d_o[i..i + sd]);
        }
        // the recurrence, backwards, one head per thread. States are kept every CHUNK steps on
        // the way forward; each chunk's states are recomputed from its first.
        let (bs, decays) = self.gates(li, &s.beta, &s.alpha);
        let mixed = &s.mixed;
        type HeadGrads = (Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>);
        let per_head: Vec<HeadGrads> = (0..hv)
            .into_par_iter()
            .map(|vh| {
                let mut dq = vec![0f32; t_len * sd];
                let mut dk = vec![0f32; t_len * sd];
                let mut dv = vec![0f32; t_len * sd];
                let mut db = vec![0f32; t_len];
                let mut ddecay = vec![0f32; t_len];
                let mut states: Vec<Vec<f32>> = Vec::with_capacity(CHUNK);
                let mut sp = vec![0f32; sd * sd];
                let mut cur = vec![0f32; sd * sd];
                // Backward over the steps a..b, which start from state `s0`; `ds` comes in as the
                // gradient of the state after step b-1 and leaves as the gradient of `s0`.
                let mut back = |a: usize, b: usize, s0: &[f32], ds: &mut Vec<f32>| {
                    let mut marks = Vec::with_capacity((b - a).div_ceil(CHUNK));
                    let mut st = s0.to_vec();
                    for t in a..b {
                        if (t - a).is_multiple_of(CHUNK) {
                            marks.push(st.clone());
                        }
                        delta_step(&mut st, &self.step(mixed, &bs, &decays, t, vh), qs, None);
                    }
                    for (ci, mark) in marks.iter().enumerate().rev() {
                        let t0 = a + ci * CHUNK;
                        let t1 = (t0 + CHUNK).min(b);
                        states.clear();
                        let mut st = mark.clone();
                        for t in t0..t1 {
                            states.push(st.clone()); // the state before step t
                            delta_step(&mut st, &self.step(mixed, &bs, &decays, t, vh), qs, None);
                        }
                        for t in (t0..t1).rev() {
                            let stp = self.step(mixed, &bs, &decays, t, vh);
                            let prev = &states[t - t0];
                            sp.iter_mut().zip(prev).for_each(|(a, b)| *a = b * stp.decay);
                            cur.copy_from_slice(prev);
                            delta_step(&mut cur, &stp, qs, None);
                            // o_j = S_j · q · qs
                            let doo = &d_o[t * val_dim + vh * sd..t * val_dim + (vh + 1) * sd];
                            let dqt = &mut dq[t * sd..(t + 1) * sd];
                            for j in 0..sd {
                                let row = &cur[j * sd..(j + 1) * sd];
                                let g = doo[j] * qs;
                                dqt.iter_mut().zip(row).for_each(|(a, b)| *a += g * b);
                                ds[j * sd..(j + 1) * sd].iter_mut().zip(stp.q).for_each(|(a, b)| *a += g * b);
                            }
                            // S_j = S'_j + k·δ_j, δ_j = b·(v_j − S'_j·k), S' = decay·S_prev
                            let dkt = &mut dk[t * sd..(t + 1) * sd];
                            let dvt = &mut dv[t * sd..(t + 1) * sd];
                            let mut dbt = 0.0;
                            let mut ddec = 0.0;
                            for j in 0..sd {
                                let spj = &sp[j * sd..(j + 1) * sd];
                                let dsj = &mut ds[j * sd..(j + 1) * sd];
                                let sk = crate::quant::dot(spj, stp.k);
                                let delta = (stp.v[j] - sk) * stp.b;
                                let dd = crate::quant::dot(dsj, stp.k);
                                dvt[j] = dd * stp.b;
                                dbt += dd * (stp.v[j] - sk);
                                let dsk = -dd * stp.b;
                                for i in 0..sd {
                                    dkt[i] += delta * dsj[i] + dsk * spj[i];
                                    dsj[i] += dsk * stp.k[i]; // now dS'_j
                                }
                                ddec += crate::quant::dot(dsj, &prev[j * sd..(j + 1) * sd]);
                                dsj.iter_mut().for_each(|v| *v *= stp.decay); // now dS_prev_j
                            }
                            db[t] = dbt;
                            ddecay[t] = ddec;
                        }
                    }
                };
                // the state where the shared prefix ends: every example starts from it
                let mut s_p = vec![0f32; sd * sd];
                for t in 0..lay.shared {
                    delta_step(&mut s_p, &self.step(mixed, &bs, &decays, t, vh), qs, None);
                }
                // each example back to that state; their gradients of it add up
                let mut ds_p = vec![0f32; sd * sd];
                for &(a, b) in &lay.spans {
                    let mut ds = vec![0f32; sd * sd];
                    back(a, b, &s_p, &mut ds);
                    ds_p.iter_mut().zip(&ds).for_each(|(x, y)| *x += y);
                }
                // then the prefix, once, from a zero state
                if lay.shared > 0 {
                    back(0, lay.shared, &vec![0f32; sd * sd], &mut ds_p);
                }
                (dq, dk, dv, db, ddecay)
            })
            .collect();
        // gather into the convolution's channels and the gate projections' outputs
        let mut dmixed = vec![0f32; t_len * ch];
        let mut dbeta = vec![0f32; t_len * hv];
        let mut dalpha = vec![0f32; t_len * hv];
        for (vh, (dq, dk, dv, db, ddec)) in per_head.iter().enumerate() {
            let kh = vh % hk;
            for t in 0..t_len {
                let row = &mut dmixed[t * ch..(t + 1) * ch];
                let src = t * sd..(t + 1) * sd;
                row[kh * sd..(kh + 1) * sd].iter_mut().zip(&dq[src.clone()]).for_each(|(a, b)| *a += b);
                row[key_dim + kh * sd..key_dim + (kh + 1) * sd].iter_mut().zip(&dk[src.clone()]).for_each(|(a, b)| *a += b);
                row[2 * key_dim + vh * sd..2 * key_dim + (vh + 1) * sd].copy_from_slice(&dv[src]);
                let i = t * hv + vh;
                dbeta[i] = db[t] * bs[i] * (1.0 - bs[i]);
                let u = s.alpha[i] + l.dt_bias[vh];
                let dsp = if u > 20.0 { 1.0 } else { sigmoid(u) };
                dalpha[i] = ddec[t] * decays[i] * l.ssm_a[vh] * dsp;
            }
        }
        // L2 norms of the q and k heads, then SiLU
        let mut dpre = vec![0f32; t_len * ch];
        for t in 0..t_len {
            let r = t * ch..(t + 1) * ch;
            let (pre, u, du, dp) = (&s.pre[r.clone()], &mixed[r.clone()], &dmixed[r.clone()], &mut dpre[r]);
            for hs in (0..2 * key_dim).step_by(sd) {
                let mv: Vec<f32> = pre[hs..hs + sd].iter().map(|v| silu(*v)).collect();
                let norm = mv.iter().map(|v| v * v).sum::<f32>().sqrt();
                let ud: f32 = (hs..hs + sd).map(|i| u[i] * du[i]).sum();
                for i in hs..hs + sd {
                    let dm = if norm > c.eps { (du[i] - u[i] * ud) / norm } else { du[i] / c.eps };
                    dp[i] = dm * silu_back(pre[i]);
                }
            }
            for i in 2 * key_dim..ch {
                dp[i] = du[i] * silu_back(pre[i]);
            }
        }
        // the causal convolution: pre[t] = Σ_kk qkv[the token kern−1−kk before t] · w[kk]
        // (a prefix token feeds every example's first tokens: those gradients add up)
        let mut dqkv = vec![0f32; t_len * ch];
        for t in 0..t_len {
            for kk in 0..kern {
                let Some(src) = lay.before(t, kern - 1 - kk) else { continue };
                for cc in 0..ch {
                    dqkv[src * ch + cc] += dpre[t * ch + cc] * l.conv1d[cc * kern + kk];
                }
            }
        }
        let mut dh = m.matmul_t(l.qkv.as_ref().unwrap(), &dqkv, t_len);
        for (w, g) in [(&l.gate, &dz), (&l.ssm_beta, &dbeta), (&l.ssm_alpha, &dalpha)] {
            let part = m.matmul_t(w.as_ref().unwrap(), g, t_len);
            dh.iter_mut().zip(&part).for_each(|(a, b)| *a += b);
        }
        dh
    }
}

/// AdamW over the adapters.
pub struct AdamW {
    pub lr: f32,
    pub beta1: f32,
    pub beta2: f32,
    pub eps: f32,
    pub weight_decay: f32,
    step: u32,
    m: BTreeMap<(usize, Target), (Vec<f32>, Vec<f32>)>,
    v: BTreeMap<(usize, Target), (Vec<f32>, Vec<f32>)>,
}

impl AdamW {
    pub fn new(lr: f32) -> Self {
        AdamW { lr, beta1: 0.9, beta2: 0.999, eps: 1e-8, weight_decay: 0.0, step: 0, m: BTreeMap::new(), v: BTreeMap::new() }
    }
    /// Save the optimizer's moments and step count, so a job that stops mid-way continues
    /// exactly rather than restarting them from zero (the first steps after a cold restart are
    /// too large: AdamW's bias correction assumes fresh moments).
    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        let mut out = Vec::new();
        out.extend(b"AGADAM1\0");
        out.extend(self.step.to_le_bytes());
        out.extend((self.m.len() as u32).to_le_bytes());
        for ((layer, t), (ma, mb)) in &self.m {
            let (va, vb) = &self.v[&(*layer, *t)];
            out.extend((*layer as u32).to_le_bytes());
            out.push(t.tag() as u8);
            out.extend((ma.len() as u32).to_le_bytes());
            out.extend((mb.len() as u32).to_le_bytes());
            for x in ma.iter().chain(mb).chain(va).chain(vb) {
                out.extend(x.to_le_bytes());
            }
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, out).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    /// The saved moments, into an optimizer with the given learning rate. Fails if the file does
    /// not match the adapter (shapes), so a changed adapter starts fresh.
    pub fn load(path: &std::path::Path, lr: f32, ad: &Adapter) -> Result<AdamW, String> {
        let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if b.len() < 16 || &b[..8] != b"AGADAM1\0" {
            return Err("not an agentos optimizer state".into());
        }
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let mut o = AdamW::new(lr);
        o.step = u32_at(8);
        let n = u32_at(12) as usize;
        let mut at = 16;
        for _ in 0..n {
            let layer = u32_at(at) as usize;
            let t = match b[at + 4] {
                b'q' => Target::Q,
                b'k' => Target::K,
                b'v' => Target::V,
                b'o' => Target::O,
                x => return Err(format!("bad target {x}")),
            };
            let (la, lb) = (u32_at(at + 5) as usize, u32_at(at + 9) as usize);
            at += 13;
            let p = ad.pairs.get(&(layer, t)).ok_or("optimizer state is for another adapter")?;
            if (la, lb) != (p.a.len(), p.b.len()) {
                return Err("optimizer state is for another adapter".into());
            }
            let count = 2 * (la + lb);
            if at + count * 4 > b.len() {
                return Err("optimizer state is cut short".into());
            }
            let vals: Vec<f32> = b[at..at + count * 4].chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
            at += count * 4;
            o.m.insert((layer, t), (vals[..la].to_vec(), vals[la..la + lb].to_vec()));
            o.v.insert((layer, t), (vals[la + lb..2 * la + lb].to_vec(), vals[2 * la + lb..].to_vec()));
        }
        if o.m.len() != ad.pairs.len() {
            return Err("optimizer state is for another adapter".into());
        }
        Ok(o)
    }

    pub fn step(&mut self, ad: &mut Adapter, g: &Grads) {
        self.step += 1;
        let (b1, b2) = (self.beta1, self.beta2);
        let c1 = 1.0 - b1.powi(self.step as i32);
        let c2 = 1.0 - b2.powi(self.step as i32);
        for (key, p) in ad.pairs.iter_mut() {
            let (ga, gb) = &g.pairs[key];
            let (ma, mb) = self.m.entry(*key).or_insert_with(|| (vec![0.0; ga.len()], vec![0.0; gb.len()]));
            let (va, vb) = self.v.entry(*key).or_insert_with(|| (vec![0.0; ga.len()], vec![0.0; gb.len()]));
            for (param, grad, m, v) in [(&mut p.a, ga, ma, va), (&mut p.b, gb, mb, vb)] {
                for i in 0..param.len() {
                    m[i] = b1 * m[i] + (1.0 - b1) * grad[i];
                    v[i] = b2 * v[i] + (1.0 - b2) * grad[i] * grad[i];
                    let upd = (m[i] / c1) / ((v[i] / c2).sqrt() + self.eps);
                    param[i] -= self.lr * (upd + self.weight_decay * param[i]);
                }
            }
        }
    }
}

/// One training example as tokens, with the answer tokens marked for the loss.
pub fn example(tok: &crate::tokenizer::Tokenizer, prompt: &str, answer: &str) -> (Vec<u32>, Vec<bool>) {
    let p = tok.encode(prompt);
    let mut a = tok.encode(answer);
    if let Some(end) = tok.id("<|im_end|>") {
        a.push(end);
    }
    let mut train = vec![false; p.len()];
    train.extend(std::iter::repeat_n(true, a.len()));
    let mut tokens = p;
    tokens.extend(a);
    (tokens, train)
}

#[cfg(test)]
mod tests {
    //! Gradient checks on tiny random models of each architecture, written as GGUF files here,
    //! so they run without downloading a model.
    use super::*;
    use crate::gguf::Gguf;

    struct W(Vec<u8>);
    impl W {
        fn s(&mut self, v: &str) {
            self.0.extend((v.len() as u64).to_le_bytes());
            self.0.extend(v.as_bytes());
        }
    }

    /// arch, layers; dims are fixed small. Qwen3.5 uses attention every 2nd layer.
    fn tiny(arch: &str, n_layer: usize) -> Model {
        let (d, nh, nkv, hd, ff, vocab) = (16usize, 4usize, 2usize, 8usize, 24usize, 24usize);
        let (hk, hv, sd, kern) = (2usize, 4usize, 4usize, 4usize);
        let gated = arch == "qwen35";
        let mut kv: Vec<(String, u32, Vec<u8>)> = vec![];
        let u = |k: &str, v: usize| (format!("{arch}.{k}"), 4u32, (v as u32).to_le_bytes().to_vec());
        kv.push(u("embedding_length", d));
        kv.push(u("attention.head_count", nh));
        kv.push(u("attention.head_count_kv", nkv));
        kv.push(u("attention.key_length", hd));
        kv.push(u("block_count", n_layer));
        kv.push(u("feed_forward_length", ff));
        kv.push(u("rope.dimension_count", if gated { 4 } else { hd }));
        kv.push(u("ssm.conv_kernel", kern));
        kv.push(u("ssm.group_count", hk));
        kv.push(u("ssm.time_step_rank", hv));
        kv.push(u("ssm.state_size", sd));
        kv.push(u("full_attention_interval", 2));
        kv.push((format!("{arch}.rope.freq_base"), 6, 10000f32.to_le_bytes().to_vec()));
        let mut seed = 0x1234_5678u64 ^ arch.len() as u64;
        let mut tensors: Vec<(String, Vec<u64>, Vec<f32>)> = vec![];
        let mut add = |name: String, dims: Vec<u64>, f: &mut dyn FnMut(&mut u64) -> f32| {
            let n: u64 = dims.iter().product();
            let v = (0..n).map(|_| f(&mut seed)).collect();
            tensors.push((name, dims, v));
        };
        let mat = |cols: usize| move |s: &mut u64| rng(s) / (cols as f32).sqrt() * 1.5;
        let ones = |s: &mut u64| 1.0 + 0.2 * rng(s);
        add("token_embd.weight".into(), vec![d as u64, vocab as u64], &mut |s| rng(s));
        add("output_norm.weight".into(), vec![d as u64], &mut ones.clone());
        for i in 0..n_layer {
            let p = |s: &str| format!("blk.{i}.{s}");
            let rec = gated && (i + 1) % 2 != 0;
            add(p("attn_norm.weight"), vec![d as u64], &mut ones.clone());
            add(p("post_attention_norm.weight"), vec![d as u64], &mut ones.clone());
            add(p("ffn_gate.weight"), vec![d as u64, ff as u64], &mut mat(d));
            add(p("ffn_up.weight"), vec![d as u64, ff as u64], &mut mat(d));
            add(p("ffn_down.weight"), vec![ff as u64, d as u64], &mut mat(ff));
            if rec {
                let ch = sd * (2 * hk + hv);
                add(p("attn_qkv.weight"), vec![d as u64, ch as u64], &mut mat(d));
                add(p("attn_gate.weight"), vec![d as u64, (hv * sd) as u64], &mut mat(d));
                add(p("ssm_alpha.weight"), vec![d as u64, hv as u64], &mut mat(d));
                add(p("ssm_beta.weight"), vec![d as u64, hv as u64], &mut mat(d));
                add(p("ssm_out.weight"), vec![(hv * sd) as u64, d as u64], &mut mat(hv * sd));
                add(p("ssm_conv1d.weight"), vec![kern as u64, ch as u64], &mut |s| rng(s) * 0.8);
                add(p("ssm_dt.bias"), vec![hv as u64], &mut |s| rng(s));
                add(p("ssm_a"), vec![hv as u64], &mut |s| -(1.0 + 0.5 * rng(s)));
                add(p("ssm_norm.weight"), vec![sd as u64], &mut ones.clone());
            } else {
                let qw = if gated { 2 * nh * hd } else { nh * hd };
                add(p("attn_q.weight"), vec![d as u64, qw as u64], &mut mat(d));
                add(p("attn_k.weight"), vec![d as u64, (nkv * hd) as u64], &mut mat(d));
                add(p("attn_v.weight"), vec![d as u64, (nkv * hd) as u64], &mut mat(d));
                add(p("attn_output.weight"), vec![(nh * hd) as u64, d as u64], &mut mat(nh * hd));
                if arch == "qwen2" {
                    add(p("attn_q.bias"), vec![(nh * hd) as u64], &mut |s| 0.1 * rng(s));
                    add(p("attn_k.bias"), vec![(nkv * hd) as u64], &mut |s| 0.1 * rng(s));
                    add(p("attn_v.bias"), vec![(nkv * hd) as u64], &mut |s| 0.1 * rng(s));
                } else {
                    add(p("attn_q_norm.weight"), vec![hd as u64], &mut ones.clone());
                    add(p("attn_k_norm.weight"), vec![hd as u64], &mut ones.clone());
                }
            }
        }
        let mut w = W(b"GGUF".to_vec());
        w.0.extend(3u32.to_le_bytes());
        w.0.extend((tensors.len() as u64).to_le_bytes());
        w.0.extend((kv.len() as u64 + 1).to_le_bytes());
        w.s("general.architecture");
        w.0.extend(8u32.to_le_bytes());
        w.s(arch);
        for (k, ty, v) in &kv {
            w.s(k);
            w.0.extend(ty.to_le_bytes());
            w.0.extend(v);
        }
        let mut off = 0u64;
        for (name, dims, v) in &tensors {
            w.s(name);
            w.0.extend((dims.len() as u32).to_le_bytes());
            dims.iter().for_each(|x| w.0.extend(x.to_le_bytes()));
            w.0.extend(0u32.to_le_bytes()); // f32
            w.0.extend(off.to_le_bytes());
            off += (v.len() as u64 * 4).div_ceil(32) * 32;
        }
        while !w.0.len().is_multiple_of(32) {
            w.0.push(0);
        }
        for (_, _, v) in &tensors {
            v.iter().for_each(|x| w.0.extend(x.to_le_bytes()));
            while !w.0.len().is_multiple_of(32) {
                w.0.push(0);
            }
        }
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("agentos-tiny-{arch}-{}-{n}.gguf", std::process::id()));
        std::fs::write(&path, &w.0).unwrap();
        let m = Model::load(Gguf::open(&path).unwrap(), 128).unwrap();
        let _ = std::fs::remove_file(&path);
        m
    }

    fn data(n: usize) -> (Vec<u32>, Vec<bool>) {
        let tokens: Vec<u32> = (0..n as u32).map(|i| (i * 7 + 3) % 24).collect();
        let train = (0..n).map(|i| i >= n / 2).collect();
        (tokens, train)
    }

    fn live_adapter(m: &Model) -> Adapter {
        let mut ad = Adapter::new(m, 3, 6.0, 7).unwrap();
        let mut s = 99u64;
        for p in ad.pairs.values_mut() {
            p.b.iter_mut().for_each(|b| *b = rng(&mut s) * 0.3);
        }
        ad
    }

    /// The trainer's loss equals the inference engine's, token by token (adapter on).
    fn forward_matches_engine(arch: &str) {
        let mut m = tiny(arch, 4);
        let (tokens, train) = data(40);
        let ad = live_adapter(&m);
        let mut tr = Trainer::new(&m);
        tr.exact = true;
        let (l, _) = tr.loss(&ad, &tokens, &train, false);
        m.adapter = Some(ad);
        let mut st = m.new_state();
        let (mut ce, mut n) = (0.0, 0);
        for t in 0..tokens.len() - 1 {
            let logits = m.forward(&mut st, &tokens[t..t + 1]).unwrap();
            if train[t + 1] {
                let mx = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                let s: f32 = logits.iter().map(|v| (v - mx).exp()).sum();
                ce += -(logits[tokens[t + 1] as usize] - mx - s.ln());
                n += 1;
            }
        }
        let e = ce / n as f32;
        assert!((l - e).abs() < 2e-3 * l.abs().max(1.0), "{arch}: trainer {l} engine {e}");
    }

    /// Every adapter gradient entry against central differences; checkpointed equals kept.
    fn gradients(arch: &str, n_tok: usize) {
        let m = tiny(arch, 4);
        let (tokens, train) = data(n_tok);
        let ad = live_adapter(&m);
        let mut tr = Trainer::new(&m);
        tr.exact = true;
        tr.budget = usize::MAX;
        let (_, g) = tr.loss(&ad, &tokens, &train, true);
        let g = g.unwrap();
        tr.budget = 0;
        assert!(tr.checkpoints(n_tok));
        let g2 = tr.loss(&ad, &tokens, &train, true).1.unwrap();
        for (k, (a, b)) in &g.pairs {
            let (a2, b2) = &g2.pairs[k];
            assert!(a.iter().chain(b).zip(a2.iter().chain(b2)).all(|(x, y)| (x - y).abs() <= 1e-6 * (1.0 + x.abs())), "{arch}: checkpointing changed {k:?}");
        }
        let h = 3e-3f32;
        let (mut worst, mut checked) = (0f32, 0);
        for (key, (ga, gb)) in &g.pairs {
            for (which, grad) in [(0usize, ga), (1, gb)] {
                // a spread of entries, including the largest
                let big = (0..grad.len()).max_by(|&i, &j| grad[i].abs().total_cmp(&grad[j].abs())).unwrap();
                for idx in [0, grad.len() / 3, grad.len() - 1, big] {
                    let mut plus = ad.clone();
                    let mut minus = ad.clone();
                    for (a, sign) in [(&mut plus, 1.0), (&mut minus, -1.0)] {
                        let p = a.pairs.get_mut(key).unwrap();
                        let v = if which == 0 { &mut p.a[idx] } else { &mut p.b[idx] };
                        *v += sign * h;
                    }
                    let num = (tr.loss(&plus, &tokens, &train, false).0 - tr.loss(&minus, &tokens, &train, false).0) / (2.0 * h);
                    let ana = grad[idx];
                    let err = (num - ana).abs();
                    assert!(err <= 2e-3 + 0.03 * num.abs().max(ana.abs()), "{arch} {key:?} {which} [{idx}]: analytic {ana} numeric {num}");
                    if num.abs().max(ana.abs()) > 1e-2 {
                        worst = worst.max(err / num.abs().max(ana.abs()));
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 10, "{arch}: too few live gradients ({checked})");
        eprintln!("{arch}: worst relative error {worst:.4} over {checked} entries");
    }

    #[test]
    fn qwen2_forward_and_gradients() {
        forward_matches_engine("qwen2");
        gradients("qwen2", 20);
    }

    #[test]
    fn qwen3_forward_and_gradients() {
        forward_matches_engine("qwen3");
        gradients("qwen3", 20);
    }

    #[test]
    fn qwen35_forward_and_gradients() {
        forward_matches_engine("qwen35");
        // longer than a DeltaNet chunk, so the chunked recomputation is crossed
        gradients("qwen35", CHUNK + 13);
    }

    /// Examples packed with a shared prefix give each example's own loss, and the sum of their
    /// gradients, exactly as running them one by one.
    fn packed_equals_one_by_one(arch: &str) {
        let m = tiny(arch, 4);
        let ad = live_adapter(&m);
        let mut tr = Trainer::new(&m);
        tr.exact = true;
        let prefix: Vec<u32> = (0..40u32).map(|i| (i * 5 + 1) % 24).collect();
        let mk = |own: &[u32], answer: usize| {
            let mut t = prefix.clone();
            t.extend_from_slice(own);
            let mut train = vec![false; t.len()];
            let n = t.len();
            train[n - answer..].iter_mut().for_each(|x| *x = true);
            (t, train)
        };
        let own1: Vec<u32> = (0..10u32).map(|i| (i * 7 + 2) % 24).collect();
        let own2: Vec<u32> = (0..15u32).map(|i| (i * 11 + 3) % 24).collect();
        let own3: Vec<u32> = (0..37u32).map(|i| (i * 13 + 5) % 24).collect(); // crosses a chunk
        let items = vec![mk(&own1, 4), mk(&own2, 6), mk(&own3, 9)];
        let lay = Layout::pack(&items);
        assert_eq!(lay.shared, 40, "{arch}: the whole common prefix is shared");
        assert_eq!(lay.tokens.len(), 40 + 10 + 15 + 37);
        for budget in [usize::MAX, 0] {
            tr.budget = budget; // with and without recomputing layers
            let (losses, g) = tr.loss_batch(&ad, &items, true);
            let g = g.unwrap();
            let mut sum: Option<Grads> = None;
            for (i, (t, train)) in items.iter().enumerate() {
                let (l, gi) = tr.loss(&ad, t, train, true);
                assert!((l - losses[i]).abs() <= 1e-4 * (1.0 + l.abs()), "{arch}: example {i}: alone {l}, packed {}", losses[i]);
                match sum.as_mut() {
                    Some(s) => s.add(&gi.unwrap()),
                    None => sum = gi,
                }
            }
            let sum = sum.unwrap();
            let mut worst = 0f32;
            for (k, (ga, gb)) in &g.pairs {
                let (sa, sb) = &sum.pairs[k];
                for (x, y) in ga.iter().chain(gb).zip(sa.iter().chain(sb)) {
                    let e = (x - y).abs() / (1e-3 + x.abs().max(y.abs()));
                    worst = worst.max(e);
                }
            }
            assert!(worst < 1e-3, "{arch}: packed gradients differ from the sum by {worst}");
        }
    }

    #[test]
    fn packing_with_a_shared_prefix_is_exact() {
        for arch in ["qwen2", "qwen3", "qwen35"] {
            packed_equals_one_by_one(arch);
        }
    }

    #[test]
    fn packing_shares_only_what_is_safe() {
        // the prefix stops before the token that predicts the first answer, and one example
        // alone shares nothing
        let a = (vec![1, 2, 3, 4, 5, 6], vec![false, false, false, false, true, true]);
        let b = (vec![1, 2, 3, 4, 9, 9, 9], vec![false, false, false, false, false, true, true]);
        let lay = Layout::pack(&[a.clone(), b]);
        assert_eq!(lay.shared, 3);
        assert_eq!(lay.pos, vec![0, 1, 2, 3, 4, 5, 3, 4, 5, 6]);
        assert_eq!(lay.spans, vec![(3, 6), (6, 10)]);
        assert_eq!(lay.visible(7).collect::<Vec<_>>(), vec![0, 1, 2, 6, 7]);
        assert_eq!(lay.before(6, 1), Some(2), "an example's first token follows the prefix");
        assert_eq!(lay.before(6, 4), None);
        assert_eq!(Layout::one(&a.0, &a.1).shared, 0);
    }

    #[test]
    fn optimizer_state_round_trips_and_resumes_exactly() {
        let m = tiny("qwen2", 2);
        let (tokens, train) = data(12);
        let tr = Trainer::new(&m);
        // two steps straight through
        let mut ad1 = Adapter::new(&m, 2, 4.0, 3).unwrap();
        let mut o1 = AdamW::new(1e-2);
        for _ in 0..2 {
            let g = tr.loss(&ad1, &tokens, &train, true).1.unwrap();
            o1.step(&mut ad1, &g);
        }
        // one step, save both, load, one more step
        let mut ad2 = Adapter::new(&m, 2, 4.0, 3).unwrap();
        let mut o2 = AdamW::new(1e-2);
        let g = tr.loss(&ad2, &tokens, &train, true).1.unwrap();
        o2.step(&mut ad2, &g);
        let dir = std::env::temp_dir().join(format!("agentos-opt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        o2.save(&dir.join("o.opt")).unwrap();
        ad2.save(&dir.join("a.lora")).unwrap();
        let mut ad3 = Adapter::load(&dir.join("a.lora")).unwrap();
        let mut o3 = AdamW::load(&dir.join("o.opt"), 1e-2, &ad3).unwrap();
        let g = tr.loss(&ad3, &tokens, &train, true).1.unwrap();
        o3.step(&mut ad3, &g);
        for (k, p1) in &ad1.pairs {
            let p3 = &ad3.pairs[k];
            assert!(p1.a.iter().chain(&p1.b).zip(p3.a.iter().chain(&p3.b)).all(|(x, y)| (x - y).abs() <= 1e-6 * (1.0 + x.abs())), "{k:?} differs after resume");
        }
        // a fresh optimizer would have taken a different (larger) step
        let mut ad4 = Adapter::load(&dir.join("a.lora")).unwrap();
        let mut o4 = AdamW::new(1e-2);
        let g = tr.loss(&ad4, &tokens, &train, true).1.unwrap();
        o4.step(&mut ad4, &g);
        let (p1, p4) = (&ad1.pairs[&(0, Target::Q)], &ad4.pairs[&(0, Target::Q)]);
        assert!(p1.b.iter().zip(&p4.b).any(|(x, y)| (x - y).abs() > 1e-6), "a cold restart should differ");
        // the state of another adapter is refused
        let other = Adapter::new(&m, 3, 4.0, 3).unwrap();
        assert!(AdamW::load(&dir.join("o.opt"), 1e-2, &other).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn adapters_sit_on_attention_layers_only() {
        let m = tiny("qwen35", 4);
        let ad = Adapter::new(&m, 2, 4.0, 1).unwrap();
        let layers: std::collections::BTreeSet<usize> = ad.pairs.keys().map(|k| k.0).collect();
        assert_eq!(layers.into_iter().collect::<Vec<_>>(), vec![1, 3]);
        ad.fits(&m).unwrap();
        assert_eq!(ad.pairs[&(1, Target::Q)].output, 2 * 4 * 8);
    }

    /// A state restored from the prompt cache continues exactly like reading everything again,
    /// for plain attention and for the recurrent (DeltaNet) layers of Qwen3.5.
    #[test]
    fn a_cached_prefix_state_continues_exactly() {
        use crate::cache::PrefixCache;
        for arch in ["qwen3", "qwen35"] {
            let m = tiny(arch, 2);
            let prefix: Vec<u32> = (0..9).map(|i| (i * 5 % 23) as u32).collect();
            let rest: Vec<u32> = vec![3, 7, 11];
            let mut full = m.new_state();
            let mut all = prefix.clone();
            all.extend(&rest);
            let want = m.forward(&mut full, &all).unwrap();

            let mut st = m.new_state();
            m.forward(&mut st, &prefix).unwrap();
            let mut cache = PrefixCache::new(64 << 20);
            let id = cache
                .insert("", prefix.clone(), "system", st.snapshot(), 10.0, true)
                .unwrap();
            // something else runs in the live state meanwhile
            let mut live = m.new_state();
            m.forward(&mut live, &[1, 2, 3, 4]).unwrap();
            let (hit, n) = cache.best("", &all).unwrap();
            assert_eq!((hit, n), (id, prefix.len()));
            live.restore(cache.hit(id).unwrap());
            let got = m.forward(&mut live, &rest).unwrap();
            let worst = want
                .iter()
                .zip(&got)
                .map(|(a, b)| (a - b).abs())
                .fold(0f32, f32::max);
            assert!(worst < 1e-3, "{arch}: restored state differs by {worst}");
            assert_eq!(cache.stats.warm_used, 1);
        }
    }
}
