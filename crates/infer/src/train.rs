//! LoRA training: teach the local model from the requests the human confirmed, on the phone.
//!
//! The base weights stay frozen and compressed (memory-mapped, as for inference). Small adapter
//! matrices on the attention projections learn the change: y = W·x + (α/r)·B·(A·x). B starts at
//! zero, so a new adapter changes nothing until it is trained.
//!
//! This module holds the forward pass with saved activations and the exact backward pass for the
//! dense models (Qwen2/2.5): cross-entropy on the answer tokens only, the output layer, RMSNorm,
//! SwiGLU, residuals, causal grouped-query attention, RoPE, and the frozen matrices (gradient to
//! the input only, through `Model::matmul_t`). AdamW updates the adapters. Gradients are checked
//! against finite differences in the tests.
use crate::model::{rms_norm, rope, silu, Arch, Model};
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
    /// Adapters on every attention projection of every layer. A is small random, B zero.
    pub fn new(m: &Model, rank: usize, alpha: f32, seed: u64) -> Result<Adapter, String> {
        if !matches!(m.cfg.arch, Arch::Qwen2) {
            return Err(format!("training {:?} is not supported yet (dense Qwen2/2.5 so far)", m.cfg.arch));
        }
        let mut s = seed | 1;
        let mut pairs = BTreeMap::new();
        for (li, l) in m.layers.iter().enumerate() {
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

struct LayerSave {
    x0: Vec<f32>,
    h: Vec<f32>,
    ax: BTreeMap<Target, Vec<f32>>,
    q: Vec<f32>,
    k: Vec<f32>,
    v: Vec<f32>,
    o: Vec<f32>, // attention output before W_o (T x n_head*hd)
    x1: Vec<f32>,
    h2: Vec<f32>,
    g: Vec<f32>,
    u: Vec<f32>,
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

    /// Mean cross-entropy of `tokens[t+1]` at every position t with `train[t+1]` true, and,
    /// if asked, the gradients for the adapter.
    pub fn loss(&self, ad: &Adapter, tokens: &[u32], train: &[bool], want_grads: bool) -> (f32, Option<Grads>) {
        let m = self.model;
        let c = &m.cfg;
        let (t_len, d, hd) = (tokens.len(), c.n_embd, c.head_dim);
        let (nh, nkv) = (c.n_head, c.n_head_kv);
        let kvd = nkv * hd;
        let group = nh / nkv;
        let scale = 1.0 / (hd as f32).sqrt();
        let mut x = m.embed(tokens);
        let mut saves = Vec::with_capacity(c.n_layer);
        for (li, l) in m.layers.iter().enumerate() {
            let x0 = x.clone();
            let mut h = x.clone();
            for row in h.chunks_mut(d) {
                rms_norm(row, &l.norms.0, c.eps);
            }
            let mut ax = BTreeMap::new();
            let mut q = self.mm(l.q.as_ref().unwrap(), &h, t_len);
            ax.insert(Target::Q, ad.apply((li, Target::Q), &h, t_len, &mut q));
            let mut k = self.mm(l.k.as_ref().unwrap(), &h, t_len);
            ax.insert(Target::K, ad.apply((li, Target::K), &h, t_len, &mut k));
            let mut v = self.mm(l.v.as_ref().unwrap(), &h, t_len);
            ax.insert(Target::V, ad.apply((li, Target::V), &h, t_len, &mut v));
            for (buf, bias) in [(&mut q, &l.q_bias), (&mut k, &l.k_bias), (&mut v, &l.v_bias)] {
                if let Some(b) = bias {
                    for row in buf.chunks_mut(b.len()) {
                        row.iter_mut().zip(b).for_each(|(a, b)| *a += b);
                    }
                }
            }
            for t in 0..t_len {
                for hh in 0..nh {
                    rope(&mut q[(t * nh + hh) * hd..(t * nh + hh + 1) * hd], t, c.n_rot, c.rope_base);
                }
                for hh in 0..nkv {
                    rope(&mut k[t * kvd + hh * hd..t * kvd + (hh + 1) * hd], t, c.n_rot, c.rope_base);
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
            let mut attn = self.mm(l.o.as_ref().unwrap(), &o, t_len);
            ax.insert(Target::O, ad.apply((li, Target::O), &o, t_len, &mut attn));
            x.iter_mut().zip(&attn).for_each(|(a, b)| *a += b);
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
            saves.push(LayerSave { x0, h, ax, q, k, v, o, x1, h2, g, u });
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
            let s = &saves[li];
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
            let mut dx1 = dx.clone();
            for t in 0..t_len {
                let dh2: Vec<f32> = dh2a[t * d..(t + 1) * d].iter().zip(&dh2b[t * d..(t + 1) * d]).map(|(a, b)| a + b).collect();
                rms_back(&s.x1[t * d..(t + 1) * d], &l.norms.1, &dh2, c.eps, &mut dx1[t * d..(t + 1) * d]);
            }
            let _ = &s.h2;
            // attention: x1 = x0 + o_proj(attn(q, k, v))
            let mut d_o = m.matmul_t(l.o.as_ref().unwrap(), &dx1, t_len);
            let extra = adapter_back(ad, (li, Target::O), &s.o, &s.ax[&Target::O], &dx1, t_len, &mut grads);
            d_o.iter_mut().zip(&extra).for_each(|(a, b)| *a += b);
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
                    for t in 0..t_len {
                        let qv = &s.q[(t * nh + hh) * hd..(t * nh + hh + 1) * hd];
                        let mut mx = f32::NEG_INFINITY;
                        for j in 0..=t {
                            pr[j] = crate::quant::dot(qv, &s.k[j * kvd + kvh * hd..j * kvd + (kvh + 1) * hd]) * scale;
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
                            let vr = &s.v[j * kvd + kvh * hd..j * kvd + (kvh + 1) * hd];
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
            for t in 0..t_len {
                for hh in 0..nh {
                    rope_back(&mut dq[(t * nh + hh) * hd..(t * nh + hh + 1) * hd], t, c.n_rot, c.rope_base);
                }
                for hh in 0..nkv {
                    rope_back(&mut dk[t * kvd + hh * hd..t * kvd + (hh + 1) * hd], t, c.n_rot, c.rope_base);
                }
            }
            let mut dh = m.matmul_t(l.q.as_ref().unwrap(), &dq, t_len);
            for (w, dyy, tgt) in [(l.k.as_ref().unwrap(), &dk, Target::K), (l.v.as_ref().unwrap(), &dv, Target::V)] {
                let part = m.matmul_t(w, dyy, t_len);
                dh.iter_mut().zip(&part).for_each(|(a, b)| *a += b);
                let extra = adapter_back(ad, (li, tgt), &s.h, &s.ax[&tgt], dyy, t_len, &mut grads);
                dh.iter_mut().zip(&extra).for_each(|(a, b)| *a += b);
            }
            let extra = adapter_back(ad, (li, Target::Q), &s.h, &s.ax[&Target::Q], &dq, t_len, &mut grads);
            dh.iter_mut().zip(&extra).for_each(|(a, b)| *a += b);
            let mut dx0 = dx1;
            for t in 0..t_len {
                rms_back(&s.x0[t * d..(t + 1) * d], &l.norms.0, &dh[t * d..(t + 1) * d], c.eps, &mut dx0[t * d..(t + 1) * d]);
            }
            dx = dx0;
        }
        (loss, Some(grads))
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
