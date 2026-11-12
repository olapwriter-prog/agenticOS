//! LoRA training: teach the local model from the requests the human confirmed, on the phone.
//!
//! The base weights stay frozen and compressed (memory-mapped, as for inference). Small adapter
//! matrices on the attention projections learn the change: y = W·x + (α/r)·B·(A·x). B starts at
//! zero, so a new adapter changes nothing until it is trained.
//!
//! This module holds the forward pass with saved activations and the exact backward pass for the
//! dense models (Qwen2/2.5, Qwen3) and the hybrid Qwen3.5: cross-entropy on the answer tokens only, the output layer, RMSNorm,
//! SwiGLU, residuals, causal grouped-query attention, per-head q/k norms, RoPE, Qwen3.5's output
//! gate and Gated DeltaNet recurrence (replayed from checkpoints), and the frozen matrices (gradient to
//! the input only, through `Model::matmul_t`). AdamW updates the adapters. Gradients are checked
//! against finite differences in the tests.
use crate::model::{rms_norm, rope, silu, Arch, Layer, Model};
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

/// The frozen matrix an adapter sits beside. Attention layers: q, k, v, o. Qwen3.5's DeltaNet
/// layers: only the output projection (as O), so every layer of the hybrid can learn.
fn target_weight(l: &Layer, t: Target) -> Option<&crate::gguf::TensorInfo> {
    match t {
        Target::Q => l.q.as_ref(),
        Target::K => l.k.as_ref(),
        Target::V => l.v.as_ref(),
        Target::O => l.o.as_ref().or(l.ssm_out.as_ref()),
    }
}

impl Adapter {
    /// Adapters on every attention projection of every layer. A is small random, B zero.
    pub fn new(m: &Model, rank: usize, alpha: f32, seed: u64) -> Result<Adapter, String> {
        let mut s = seed | 1;
        let mut pairs = BTreeMap::new();
        for (li, l) in m.layers.iter().enumerate() {
            for t in Target::ALL {
                let Some(w) = target_weight(l, t) else {
                    continue;
                };
                let (input, output) = (w.dims[0] as usize, w.dims[1] as usize);
                let scale = (1.0 / input as f32).sqrt();
                let a = (0..rank * input).map(|_| rng(&mut s) * scale).collect();
                pairs.insert((li, t), Pair { input, output, a, b: vec![0.0; output * rank] });
            }
        }
        if pairs.is_empty() {
            return Err(format!("{:?}: no layer to put an adapter on", m.cfg.arch));
        }
        Ok(Adapter { rank, alpha, pairs })
    }
    /// Does this adapter fit the model (same layers and shapes)?
    pub fn fits(&self, m: &Model) -> Result<(), String> {
        for ((li, t), p) in &self.pairs {
            let l = m.layers.get(*li).ok_or(format!("adapter has layer {li}, the model does not"))?;
            let w = target_weight(l, *t).ok_or(format!("layer {li} has no {t:?}"))?;
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
    pub fn apply(&self, key: (usize, Target), x: &[f32], n: usize, y: &mut [f32]) -> Vec<f32> {
        let p = &self.pairs[&key];
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
    let p = &ad.pairs[&key];
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

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}
fn softplus(x: f32) -> f32 {
    if x > 20.0 {
        x
    } else {
        (1.0 + x.exp()).ln()
    }
}
/// d silu(x) / dx
fn silu_grad(x: f32) -> f32 {
    let s = sigmoid(x);
    s * (1.0 + x * (1.0 - s))
}

/// The DeltaNet state is kept every this many tokens; the steps between are recomputed in the
/// backward pass (a state per token would be ~2 MB per layer per token on a 4B model).
const DELTA_CHUNK: usize = 64;

/// What the attention half of a layer keeps for its backward pass.
enum MixSave {
    Attn {
        ax: BTreeMap<Target, Vec<f32>>,
        /// q and k after the bias, before the per-head norm (Qwen3, Qwen3.5); empty without one
        q_pre: Vec<f32>,
        k_pre: Vec<f32>,
        /// q and k after norm and RoPE, v as projected
        q: Vec<f32>,
        k: Vec<f32>,
        v: Vec<f32>,
        /// Qwen3.5: the output gate (T x n_head*hd) and the attention output before it
        gate: Vec<f32>,
        o_pre: Vec<f32>,
        /// the input of W_o (T x n_head*hd)
        o: Vec<f32>,
    },
    Delta {
        ax_o: Vec<f32>,
        /// the convolution's input (qkv projection) and output before SiLU (T x channels)
        sconv: Vec<f32>,
        z: Vec<f32>,
        beta: Vec<f32>,
        alpha: Vec<f32>,
        /// per value head, the state before every DELTA_CHUNK-th step
        ckpts: Vec<Vec<Vec<f32>>>,
        /// the input of ssm_out (T x value dim)
        outs: Vec<f32>,
    },
}

struct LayerSave {
    x0: Vec<f32>,
    h: Vec<f32>,
    mix: MixSave,
    x1: Vec<f32>,
    g: Vec<f32>,
    u: Vec<f32>,
}

/// Normalized q and k heads of one DeltaNet step: (q heads, k heads, their norms before).
fn delta_qk(mixed: &[f32], key_dim: usize, sd: usize, eps: f32) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let mut qk = mixed[..2 * key_dim].to_vec();
    let mut norms = Vec::with_capacity(2 * key_dim / sd);
    for head in qk.chunks_mut(sd) {
        let n = head.iter().map(|v| v * v).sum::<f32>().sqrt().max(eps);
        head.iter_mut().for_each(|v| *v /= n);
        norms.push(n);
    }
    let k = qk.split_off(key_dim);
    (qk, k, norms)
}

pub struct Trainer<'m> {
    pub model: &'m Model,
    /// Exact f32 matmuls (for gradient checks) instead of the 8-bit fast path.
    pub exact: bool,
}

impl<'m> Trainer<'m> {
    pub fn new(model: &'m Model) -> Self {
        Trainer { model, exact: false }
    }

    fn mm(&self, w: &crate::gguf::TensorInfo, x: &[f32], n: usize) -> Vec<f32> {
        if self.exact {
            self.model.matmul_exact(w, x, n)
        } else {
            self.model.matmul(w, x, n)
        }
    }

    /// Attention (dense or Qwen3.5's gated) over the whole sequence from position 0.
    fn attn_fwd(&self, ad: &Adapter, li: usize, l: &Layer, h: &[f32], t_len: usize) -> (Vec<f32>, MixSave) {
        let c = &self.model.cfg;
        let hd = c.head_dim;
        let (nh, nkv) = (c.n_head, c.n_head_kv);
        let kvd = nkv * hd;
        let group = nh / nkv;
        let scale = 1.0 / (hd as f32).sqrt();
        let gated = c.arch == Arch::Qwen35;
        let mut ax = BTreeMap::new();
        let mut qf = self.mm(l.q.as_ref().unwrap(), h, t_len);
        ax.insert(Target::Q, ad.apply((li, Target::Q), h, t_len, &mut qf));
        let mut k = self.mm(l.k.as_ref().unwrap(), h, t_len);
        ax.insert(Target::K, ad.apply((li, Target::K), h, t_len, &mut k));
        let mut v = self.mm(l.v.as_ref().unwrap(), h, t_len);
        ax.insert(Target::V, ad.apply((li, Target::V), h, t_len, &mut v));
        let qw = if gated { 2 * nh * hd } else { nh * hd };
        let mut q = vec![0f32; t_len * nh * hd];
        let mut gate = vec![0f32; if gated { t_len * nh * hd } else { 0 }];
        for t in 0..t_len {
            let src = &qf[t * qw..(t + 1) * qw];
            for hh in 0..nh {
                let dst = (t * nh + hh) * hd;
                if gated {
                    q[dst..dst + hd].copy_from_slice(&src[hh * 2 * hd..hh * 2 * hd + hd]);
                    gate[dst..dst + hd].copy_from_slice(&src[hh * 2 * hd + hd..(hh + 1) * 2 * hd]);
                } else {
                    q[dst..dst + hd].copy_from_slice(&src[hh * hd..(hh + 1) * hd]);
                }
            }
        }
        drop(qf);
        for (buf, bias) in [(&mut q, &l.q_bias), (&mut k, &l.k_bias), (&mut v, &l.v_bias)] {
            if let Some(b) = bias {
                for row in buf.chunks_mut(b.len()) {
                    row.iter_mut().zip(b).for_each(|(a, b)| *a += b);
                }
            }
        }
        let q_pre = if l.q_norm.is_some() { q.clone() } else { Vec::new() };
        let k_pre = if l.k_norm.is_some() { k.clone() } else { Vec::new() };
        for t in 0..t_len {
            for hh in 0..nh {
                let hv = &mut q[(t * nh + hh) * hd..(t * nh + hh + 1) * hd];
                if let Some(w) = &l.q_norm {
                    rms_norm(hv, w, c.eps);
                }
                rope(hv, t, c.n_rot, c.rope_base);
            }
            for hh in 0..nkv {
                let hv = &mut k[t * kvd + hh * hd..t * kvd + (hh + 1) * hd];
                if let Some(w) = &l.k_norm {
                    rms_norm(hv, w, c.eps);
                }
                rope(hv, t, c.n_rot, c.rope_base);
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
                    for j in 0..=t {
                        row[j] = crate::quant::dot(qv, &k[j * kvd + kvh * hd..j * kvd + (kvh + 1) * hd]) * scale;
                        mx = mx.max(row[j]);
                    }
                    let mut sum = 0.0;
                    for v in row.iter_mut().take(t + 1) {
                        *v = (*v - mx).exp();
                        sum += *v;
                    }
                    let ov = &mut oh[t * hd..(t + 1) * hd];
                    for j in 0..=t {
                        let w = row[j] / sum;
                        let vr = &v[j * kvd + kvh * hd..j * kvd + (kvh + 1) * hd];
                        ov.iter_mut().zip(vr).for_each(|(a, b)| *a += w * b);
                    }
                }
                oh
            })
            .collect();
        let mut o = vec![0f32; t_len * nh * hd];
        for (hh, oh) in heads.iter().enumerate() {
            for t in 0..t_len {
                o[(t * nh + hh) * hd..(t * nh + hh + 1) * hd].copy_from_slice(&oh[t * hd..(t + 1) * hd]);
            }
        }
        let o_pre = if gated {
            let pre = o.clone();
            o.iter_mut().zip(&gate).for_each(|(a, g)| *a *= sigmoid(*g));
            pre
        } else {
            Vec::new()
        };
        let mut y = self.mm(l.o.as_ref().unwrap(), &o, t_len);
        ax.insert(Target::O, ad.apply((li, Target::O), &o, t_len, &mut y));
        (y, MixSave::Attn { ax, q_pre, k_pre, q, k, v, gate, o_pre, o })
    }

    /// Gated DeltaNet (Qwen3.5) over the whole sequence from position 0, as Model::delta_net.
    fn delta_fwd(&self, ad: &Adapter, li: usize, l: &Layer, h: &[f32], t_len: usize) -> (Vec<f32>, MixSave) {
        let c = &self.model.cfg;
        let sd = c.ssm_head_dim;
        let (hk, hv) = (c.ssm_k_heads, c.ssm_v_heads);
        let (key_dim, val_dim) = (hk * sd, hv * sd);
        let ch = 2 * key_dim + val_dim;
        let kern = c.conv_kernel;
        let eps = c.eps;
        let qkv = self.mm(l.qkv.as_ref().unwrap(), h, t_len);
        let z = self.mm(l.gate.as_ref().unwrap(), h, t_len);
        let beta = self.mm(l.ssm_beta.as_ref().unwrap(), h, t_len);
        let alpha = self.mm(l.ssm_alpha.as_ref().unwrap(), h, t_len);
        // causal convolution, zero history before position 0
        let mut sconv = vec![0f32; t_len * ch];
        sconv.par_chunks_mut(ch).enumerate().for_each(|(t, s)| {
            for (cc, sv) in s.iter_mut().enumerate() {
                let w = &l.conv1d[cc * kern..(cc + 1) * kern];
                let mut acc = 0.0;
                for (m, wm) in w.iter().enumerate() {
                    if let Some(tt) = (t + m).checked_sub(kern - 1) {
                        acc += qkv[tt * ch + cc] * wm;
                    }
                }
                *sv = acc;
            }
        });
        drop(qkv);
        // q, k (normalized) and v per step
        let steps: Vec<(Vec<f32>, Vec<f32>, Vec<f32>)> = sconv
            .par_chunks(ch)
            .map(|s| {
                let mixed: Vec<f32> = s.iter().map(|x| silu(*x)).collect();
                let (q, k, _) = delta_qk(&mixed, key_dim, sd, eps);
                (q, k, mixed[2 * key_dim..].to_vec())
            })
            .collect();
        let q_scale = 1.0 / (sd as f32).sqrt();
        let per_head: Vec<(Vec<f32>, Vec<Vec<f32>>)> = (0..hv)
            .into_par_iter()
            .map(|vh| {
                let kh = vh % hk;
                let mut s = vec![0f32; sd * sd];
                let mut ck = Vec::new();
                let mut oh = vec![0f32; t_len * sd];
                for t in 0..t_len {
                    if t % DELTA_CHUNK == 0 {
                        ck.push(s.clone());
                    }
                    let (qs, ks, vs) = &steps[t];
                    let qh = &qs[kh * sd..(kh + 1) * sd];
                    let kv = &ks[kh * sd..(kh + 1) * sd];
                    let vv = &vs[vh * sd..(vh + 1) * sd];
                    let b = sigmoid(beta[t * hv + vh]);
                    let decay = (softplus(alpha[t * hv + vh] + l.dt_bias[vh]) * l.ssm_a[vh]).exp();
                    let o = &mut oh[t * sd..(t + 1) * sd];
                    for j in 0..sd {
                        let row = &mut s[j * sd..(j + 1) * sd];
                        row.iter_mut().for_each(|x| *x *= decay);
                        let sk = crate::quant::dot(row, kv);
                        let delta = (vv[j] - sk) * b;
                        row.iter_mut().zip(kv).for_each(|(x, k)| *x += k * delta);
                        o[j] = crate::quant::dot(row, qh) * q_scale;
                    }
                    rms_norm(o, &l.ssm_norm, eps);
                    let zz = &z[t * val_dim + vh * sd..t * val_dim + (vh + 1) * sd];
                    o.iter_mut().zip(zz).for_each(|(a, z)| *a *= silu(*z));
                }
                (oh, ck)
            })
            .collect();
        let mut outs = vec![0f32; t_len * val_dim];
        let mut ckpts = Vec::with_capacity(hv);
        for (vh, (oh, ck)) in per_head.into_iter().enumerate() {
            for t in 0..t_len {
                outs[t * val_dim + vh * sd..t * val_dim + (vh + 1) * sd].copy_from_slice(&oh[t * sd..(t + 1) * sd]);
            }
            ckpts.push(ck);
        }
        let mut y = self.mm(l.ssm_out.as_ref().unwrap(), &outs, t_len);
        let ax_o = ad.apply((li, Target::O), &outs, t_len, &mut y);
        (y, MixSave::Delta { ax_o, sconv, z, beta, alpha, ckpts, outs })
    }

    /// Mean cross-entropy of `tokens[t+1]` at every position t with `train[t+1]` true, and,
    /// if asked, the gradients for the adapter.
    pub fn loss(&self, ad: &Adapter, tokens: &[u32], train: &[bool], want_grads: bool) -> (f32, Option<Grads>) {
        let m = self.model;
        let c = &m.cfg;
        let (t_len, d) = (tokens.len(), c.n_embd);
        let mut x = m.embed(tokens);
        let mut saves = Vec::with_capacity(c.n_layer);
        for (li, l) in m.layers.iter().enumerate() {
            let x0 = x.clone();
            let mut h = x.clone();
            for row in h.chunks_mut(d) {
                rms_norm(row, &l.norms.0, c.eps);
            }
            let (y, mix) = if c.is_recurrent(li) {
                self.delta_fwd(ad, li, l, &h, t_len)
            } else {
                self.attn_fwd(ad, li, l, &h, t_len)
            };
            x.iter_mut().zip(&y).for_each(|(a, b)| *a += b);
            let x1 = x.clone();
            let mut h2 = x.clone();
            for row in h2.chunks_mut(d) {
                rms_norm(row, &l.norms.1, c.eps);
            }
            let g = self.mm(&l.ffn_gate, &h2, t_len);
            let u = self.mm(&l.ffn_up, &h2, t_len);
            let a: Vec<f32> = g.iter().zip(&u).map(|(g, u)| silu(*g) * u).collect();
            let f = self.mm(&l.ffn_down, &a, t_len);
            x.iter_mut().zip(&f).for_each(|(a, b)| *a += b);
            if want_grads {
                saves.push(LayerSave { x0, h, mix, x1, g, u });
            }
        }
        // loss on the answer positions
        let pos: Vec<usize> = (0..t_len - 1).filter(|&t| train[t + 1]).collect();
        if pos.is_empty() {
            return (0.0, want_grads.then(|| Grads::zeros(ad)));
        }
        let mut hf = Vec::with_capacity(pos.len() * d);
        for &t in &pos {
            let mut row = x[t * d..(t + 1) * d].to_vec();
            rms_norm(&mut row, &m.output_norm, c.eps);
            hf.extend(row);
        }
        let logits = self.mm(&m.output, &hf, pos.len());
        let nv = c.n_vocab;
        let mut loss = 0.0f32;
        let mut dlogits = vec![0f32; logits.len()];
        for (i, &t) in pos.iter().enumerate() {
            let row = &logits[i * nv..(i + 1) * nv];
            let mx = row.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let sum: f32 = row.iter().map(|v| (v - mx).exp()).sum();
            let target = tokens[t + 1] as usize;
            loss += -(row[target] - mx - sum.ln());
            if want_grads {
                let dr = &mut dlogits[i * nv..(i + 1) * nv];
                for (j, v) in row.iter().enumerate() {
                    dr[j] = (v - mx).exp() / sum / pos.len() as f32;
                }
                dr[target] -= 1.0 / pos.len() as f32;
            }
        }
        loss /= pos.len() as f32;
        if !want_grads {
            return (loss, None);
        }

        // ---- backward ----
        let mut grads = Grads::zeros(ad);
        let dhf = m.matmul_t(&m.output, &dlogits, pos.len());
        let mut dx = vec![0f32; t_len * d];
        for (i, &t) in pos.iter().enumerate() {
            rms_back(&x[t * d..(t + 1) * d], &m.output_norm, &dhf[i * d..(i + 1) * d], c.eps, &mut dx[t * d..(t + 1) * d]);
        }
        for (li, l) in m.layers.iter().enumerate().rev() {
            let s = saves.pop().unwrap();
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
            // x1 = x0 + mix(h)
            let dh = match &s.mix {
                MixSave::Attn { .. } => self.attn_back(ad, li, l, &s.h, &s.mix, &dx1, t_len, &mut grads),
                MixSave::Delta { .. } => self.delta_back(ad, li, l, &s.mix, &dx1, t_len, &mut grads),
            };
            let mut dx0 = dx1;
            for t in 0..t_len {
                rms_back(&s.x0[t * d..(t + 1) * d], &l.norms.0, &dh[t * d..(t + 1) * d], c.eps, &mut dx0[t * d..(t + 1) * d]);
            }
            dx = dx0;
        }
        (loss, Some(grads))
    }

    /// Backward through attention: dy (T x d) in, dh (T x d) out, adapter gradients added.
    #[allow(clippy::too_many_arguments)]
    fn attn_back(&self, ad: &Adapter, li: usize, l: &Layer, h: &[f32], save: &MixSave, dy: &[f32], t_len: usize, grads: &mut Grads) -> Vec<f32> {
        let MixSave::Attn { ax, q_pre, k_pre, q, k, v, gate, o_pre, o } = save else {
            unreachable!()
        };
        let m = self.model;
        let c = &m.cfg;
        let hd = c.head_dim;
        let (nh, nkv) = (c.n_head, c.n_head_kv);
        let kvd = nkv * hd;
        let group = nh / nkv;
        let scale = 1.0 / (hd as f32).sqrt();
        let gated = c.arch == Arch::Qwen35;
        let mut d_o = m.matmul_t(l.o.as_ref().unwrap(), dy, t_len);
        let extra = adapter_back(ad, (li, Target::O), o, &ax[&Target::O], dy, t_len, grads);
        d_o.iter_mut().zip(&extra).for_each(|(a, b)| *a += b);
        // Qwen3.5's output gate: o = o_pre * sigmoid(gate)
        let mut dgate = Vec::new();
        if gated {
            dgate = vec![0f32; d_o.len()];
            for i in 0..d_o.len() {
                let sg = sigmoid(gate[i]);
                dgate[i] = d_o[i] * o_pre[i] * sg * (1.0 - sg);
                d_o[i] *= sg;
            }
        }
        let mut dq = vec![0f32; q.len()];
        let mut dk = vec![0f32; k.len()];
        let mut dv = vec![0f32; v.len()];
        // per head, into per-head buffers (heads of a group share k and v)
        type HeadGrad = (usize, Vec<f32>, Vec<f32>, Vec<f32>);
        let per_head: Vec<HeadGrad> = (0..nh)
            .into_par_iter()
            .map(|hh| {
                let kvh = hh / group;
                let mut dqh = vec![0f32; t_len * hd];
                let mut dkh = vec![0f32; t_len * hd];
                let mut dvh = vec![0f32; t_len * hd];
                // the attention weights are recomputed, not kept: T x T per head per layer
                // would not fit a phone for long prompts
                let mut pr = vec![0f32; t_len];
                for t in 0..t_len {
                    let qv = &q[(t * nh + hh) * hd..(t * nh + hh + 1) * hd];
                    let mut mx = f32::NEG_INFINITY;
                    for j in 0..=t {
                        pr[j] = crate::quant::dot(qv, &k[j * kvd + kvh * hd..j * kvd + (kvh + 1) * hd]) * scale;
                        mx = mx.max(pr[j]);
                    }
                    let mut sum = 0.0;
                    for v in pr.iter_mut().take(t + 1) {
                        *v = (*v - mx).exp();
                        sum += *v;
                    }
                    pr.iter_mut().take(t + 1).for_each(|v| *v /= sum);
                    let dov = &d_o[(t * nh + hh) * hd..(t * nh + hh + 1) * hd];
                    let mut dp = vec![0f32; t + 1];
                    for j in 0..=t {
                        let vr = &v[j * kvd + kvh * hd..j * kvd + (kvh + 1) * hd];
                        dp[j] = crate::quant::dot(dov, vr);
                        let w = pr[j];
                        dvh[j * hd..(j + 1) * hd].iter_mut().zip(dov).for_each(|(a, b)| *a += w * b);
                    }
                    let sum: f32 = (0..=t).map(|j| pr[j] * dp[j]).sum();
                    for j in 0..=t {
                        let ds = pr[j] * (dp[j] - sum) * scale;
                        if ds == 0.0 {
                            continue;
                        }
                        let kr = &k[j * kvd + kvh * hd..j * kvd + (kvh + 1) * hd];
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
        let mut tmp = vec![0f32; hd];
        for t in 0..t_len {
            for hh in 0..nh {
                let r = (t * nh + hh) * hd..(t * nh + hh + 1) * hd;
                rope_back(&mut dq[r.clone()], t, c.n_rot, c.rope_base);
                if let Some(w) = &l.q_norm {
                    tmp.iter_mut().for_each(|v| *v = 0.0);
                    rms_back(&q_pre[r.clone()], w, &dq[r.clone()], c.eps, &mut tmp);
                    dq[r].copy_from_slice(&tmp);
                }
            }
            for hh in 0..nkv {
                let r = t * kvd + hh * hd..t * kvd + (hh + 1) * hd;
                rope_back(&mut dk[r.clone()], t, c.n_rot, c.rope_base);
                if let Some(w) = &l.k_norm {
                    tmp.iter_mut().for_each(|v| *v = 0.0);
                    rms_back(&k_pre[r.clone()], w, &dk[r.clone()], c.eps, &mut tmp);
                    dk[r].copy_from_slice(&tmp);
                }
            }
        }
        // back to the projection's layout: Qwen3.5 interleaves [query | gate] per head
        let dqf = if gated {
            let mut f = vec![0f32; t_len * 2 * nh * hd];
            for t in 0..t_len {
                for hh in 0..nh {
                    let src = (t * nh + hh) * hd;
                    let dst = t * 2 * nh * hd + hh * 2 * hd;
                    f[dst..dst + hd].copy_from_slice(&dq[src..src + hd]);
                    f[dst + hd..dst + 2 * hd].copy_from_slice(&dgate[src..src + hd]);
                }
            }
            f
        } else {
            dq
        };
        let mut dh = m.matmul_t(l.q.as_ref().unwrap(), &dqf, t_len);
        let extra = adapter_back(ad, (li, Target::Q), h, &ax[&Target::Q], &dqf, t_len, grads);
        dh.iter_mut().zip(&extra).for_each(|(a, b)| *a += b);
        for (w, dyy, tgt) in [(l.k.as_ref().unwrap(), &dk, Target::K), (l.v.as_ref().unwrap(), &dv, Target::V)] {
            let part = m.matmul_t(w, dyy, t_len);
            dh.iter_mut().zip(&part).for_each(|(a, b)| *a += b);
            let extra = adapter_back(ad, (li, tgt), h, &ax[&tgt], dyy, t_len, grads);
            dh.iter_mut().zip(&extra).for_each(|(a, b)| *a += b);
        }
        dh
    }

    /// Backward through a Gated DeltaNet layer. The recurrence is replayed chunk by chunk from
    /// the saved states, last chunk first.
    #[allow(clippy::too_many_arguments)]
    fn delta_back(&self, ad: &Adapter, li: usize, l: &Layer, save: &MixSave, dy: &[f32], t_len: usize, grads: &mut Grads) -> Vec<f32> {
        let MixSave::Delta { ax_o, sconv, z, beta, alpha, ckpts, outs } = save else {
            unreachable!()
        };
        let m = self.model;
        let c = &m.cfg;
        let sd = c.ssm_head_dim;
        let (hk, hv) = (c.ssm_k_heads, c.ssm_v_heads);
        let (key_dim, val_dim) = (hk * sd, hv * sd);
        let ch = 2 * key_dim + val_dim;
        let kern = c.conv_kernel;
        let eps = c.eps;
        let q_scale = 1.0 / (sd as f32).sqrt();
        let mut douts = m.matmul_t(l.ssm_out.as_ref().unwrap(), dy, t_len);
        let extra = adapter_back(ad, (li, Target::O), outs, ax_o, dy, t_len, grads);
        douts.iter_mut().zip(&extra).for_each(|(a, b)| *a += b);
        type Step = (Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>);
        let steps: Vec<Step> = sconv
            .par_chunks(ch)
            .map(|s| {
                let mixed: Vec<f32> = s.iter().map(|x| silu(*x)).collect();
                let (q, k, norms) = delta_qk(&mixed, key_dim, sd, eps);
                (q, k, mixed[2 * key_dim..].to_vec(), norms)
            })
            .collect();
        // per value head: d(q head), d(k head), dv, dz (T x sd each), dbeta, dalpha (T)
        type HeadGrads = (Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>);
        let per_head: Vec<HeadGrads> = (0..hv)
            .into_par_iter()
            .map(|vh| {
                let kh = vh % hk;
                let w = &l.ssm_norm;
                let (mut dqh, mut dkh, mut dvh, mut dzh) =
                    (vec![0f32; t_len * sd], vec![0f32; t_len * sd], vec![0f32; t_len * sd], vec![0f32; t_len * sd]);
                let (mut dbh, mut dah) = (vec![0f32; t_len], vec![0f32; t_len]);
                let mut ds = vec![0f32; sd * sd]; // dL/dS_t, flowing backwards
                let n_chunks = t_len.div_ceil(DELTA_CHUNK);
                for ci in (0..n_chunks).rev() {
                    let (t0, t1) = (ci * DELTA_CHUNK, ((ci + 1) * DELTA_CHUNK).min(t_len));
                    // the state before each step of this chunk
                    let mut before = Vec::with_capacity(t1 - t0);
                    let mut s = ckpts[vh][ci].clone();
                    for t in t0..t1 {
                        before.push(s.clone());
                        let (_, ks, vs, _) = &steps[t];
                        let kv = &ks[kh * sd..(kh + 1) * sd];
                        let vv = &vs[vh * sd..(vh + 1) * sd];
                        let b = sigmoid(beta[t * hv + vh]);
                        let decay = (softplus(alpha[t * hv + vh] + l.dt_bias[vh]) * l.ssm_a[vh]).exp();
                        for j in 0..sd {
                            let row = &mut s[j * sd..(j + 1) * sd];
                            row.iter_mut().for_each(|x| *x *= decay);
                            let delta = (vv[j] - crate::quant::dot(row, kv)) * b;
                            row.iter_mut().zip(kv).for_each(|(x, k)| *x += k * delta);
                        }
                    }
                    for t in (t0..t1).rev() {
                        let s_prev = &before[t - t0];
                        let (qs, ks, vs, _) = &steps[t];
                        let qh = &qs[kh * sd..(kh + 1) * sd];
                        let kv = &ks[kh * sd..(kh + 1) * sd];
                        let vv = &vs[vh * sd..(vh + 1) * sd];
                        let braw = beta[t * hv + vh];
                        let b = sigmoid(braw);
                        let apre = alpha[t * hv + vh] + l.dt_bias[vh];
                        let decay = (softplus(apre) * l.ssm_a[vh]).exp();
                        // forward again for this step: S' = decay S_prev, sk, delta, S_t, o
                        let sp: Vec<f32> = s_prev.iter().map(|x| x * decay).collect();
                        let mut sk = vec![0f32; sd];
                        let mut delta = vec![0f32; sd];
                        let mut st = sp.clone();
                        let mut o = vec![0f32; sd];
                        for j in 0..sd {
                            sk[j] = crate::quant::dot(&sp[j * sd..(j + 1) * sd], kv);
                            delta[j] = (vv[j] - sk[j]) * b;
                            let row = &mut st[j * sd..(j + 1) * sd];
                            row.iter_mut().zip(kv).for_each(|(x, k)| *x += k * delta[j]);
                            o[j] = crate::quant::dot(row, qh) * q_scale;
                        }
                        // y = rmsnorm(o) * silu(z)
                        let zz = &z[t * val_dim + vh * sd..t * val_dim + (vh + 1) * sd];
                        let dyh = &douts[t * val_dim + vh * sd..t * val_dim + (vh + 1) * sd];
                        let mut on = o.clone();
                        rms_norm(&mut on, w, eps);
                        let mut don = vec![0f32; sd];
                        for i in 0..sd {
                            dzh[t * sd + i] = dyh[i] * on[i] * silu_grad(zz[i]);
                            don[i] = dyh[i] * silu(zz[i]);
                        }
                        let mut d_o = vec![0f32; sd];
                        rms_back(&o, w, &don, eps, &mut d_o);
                        // o = q_scale · S_t q
                        let dq = &mut dqh[t * sd..(t + 1) * sd];
                        for j in 0..sd {
                            let g = d_o[j] * q_scale;
                            if g == 0.0 {
                                continue;
                            }
                            let srow = &st[j * sd..(j + 1) * sd];
                            let dsrow = &mut ds[j * sd..(j + 1) * sd];
                            for i in 0..sd {
                                dsrow[i] += g * qh[i];
                                dq[i] += g * srow[i];
                            }
                        }
                        // S_t = S' + delta k^T;  delta = (v - S' k) b
                        let dk = &mut dkh[t * sd..(t + 1) * sd];
                        let mut db = 0.0;
                        for j in 0..sd {
                            let dsrow = &mut ds[j * sd..(j + 1) * sd];
                            let ddelta = crate::quant::dot(dsrow, kv);
                            for i in 0..sd {
                                dk[i] += dsrow[i] * delta[j];
                            }
                            dvh[t * sd + j] += ddelta * b;
                            db += ddelta * (vv[j] - sk[j]);
                            let dsk = -ddelta * b;
                            let sprow = &sp[j * sd..(j + 1) * sd];
                            for i in 0..sd {
                                dk[i] += sprow[i] * dsk;
                                dsrow[i] += dsk * kv[i];
                            }
                        }
                        // S' = decay S_prev
                        let ddecay: f32 = ds.iter().zip(s_prev).map(|(a, b)| a * b).sum();
                        ds.iter_mut().for_each(|v| *v *= decay);
                        dah[t] = ddecay * decay * l.ssm_a[vh] * sigmoid(apre);
                        dbh[t] = db * b * (1.0 - b);
                    }
                }
                (dqh, dkh, dvh, dzh, dbh, dah)
            })
            .collect();
        // gather: q/k heads are shared by the value heads that tile onto them
        let mut dmixed = vec![0f32; t_len * ch];
        let mut dz = vec![0f32; t_len * val_dim];
        let mut dbeta = vec![0f32; t_len * hv];
        let mut dalpha = vec![0f32; t_len * hv];
        for (vh, (dqh, dkh, dvh, dzh, dbh, dah)) in per_head.into_iter().enumerate() {
            let kh = vh % hk;
            for t in 0..t_len {
                let row = &mut dmixed[t * ch..(t + 1) * ch];
                for i in 0..sd {
                    row[kh * sd + i] += dqh[t * sd + i];
                    row[key_dim + kh * sd + i] += dkh[t * sd + i];
                    row[2 * key_dim + vh * sd + i] += dvh[t * sd + i];
                }
                dz[t * val_dim + vh * sd..t * val_dim + (vh + 1) * sd].copy_from_slice(&dzh[t * sd..(t + 1) * sd]);
                dbeta[t * hv + vh] = dbh[t];
                dalpha[t * hv + vh] = dah[t];
            }
        }
        // L2 norms of the q and k heads, then SiLU, then the causal convolution
        let mut dqkv = vec![0f32; t_len * ch];
        for t in 0..t_len {
            let (qs, ks, _, norms) = &steps[t];
            let row = &mut dmixed[t * ch..(t + 1) * ch];
            for (hi, n) in norms.iter().enumerate() {
                let (yv, off) = if hi < hk { (&qs[hi * sd..(hi + 1) * sd], hi * sd) } else { (&ks[(hi - hk) * sd..(hi - hk + 1) * sd], hi * sd) };
                let g = &mut row[off..off + sd];
                if *n > eps {
                    let yd: f32 = yv.iter().zip(g.iter()).map(|(a, b)| a * b).sum();
                    for i in 0..sd {
                        g[i] = (g[i] - yv[i] * yd) / n;
                    }
                } else {
                    g.iter_mut().for_each(|v| *v /= n);
                }
            }
            let sc = &sconv[t * ch..(t + 1) * ch];
            for cc in 0..ch {
                let dsv = row[cc] * silu_grad(sc[cc]);
                if dsv == 0.0 {
                    continue;
                }
                let w = &l.conv1d[cc * kern..(cc + 1) * kern];
                for (mm, wm) in w.iter().enumerate() {
                    if let Some(tt) = (t + mm).checked_sub(kern - 1) {
                        dqkv[tt * ch + cc] += dsv * wm;
                    }
                }
            }
        }
        let mut dh = m.matmul_t(l.qkv.as_ref().unwrap(), &dqkv, t_len);
        for (wt, g) in [(l.gate.as_ref().unwrap(), &dz), (l.ssm_beta.as_ref().unwrap(), &dbeta), (l.ssm_alpha.as_ref().unwrap(), &dalpha)] {
            let part = m.matmul_t(wt, g, t_len);
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
