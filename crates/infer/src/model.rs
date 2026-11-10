//! The forward pass: dense Qwen2/Qwen3-style transformers and the Qwen3.5 hybrid, where three of
//! every four layers are Gated DeltaNet (a recurrent "linear attention" with a fixed-size state)
//! and every fourth is gated full attention. Ported from llama.cpp's qwen2.cpp / qwen35.cpp /
//! delta-net-base.cpp and checked against llama.cpp's output.
//!
//! Choices made for phones:
//!   - weights stay memory-mapped and compressed; rows are decoded a block at a time;
//!   - a prompt is processed in batches, so each weight row is decoded once per batch, not once
//!     per token (prompt processing dominates an agent's latency: the system prompt is long);
//!   - the attention cache is stored as f16, halving its memory;
//!   - Qwen3.5's recurrent layers keep a constant-size state (no cache growth with context).
use crate::gguf::{Gguf, TensorInfo};
use crate::quant::{dot, f16_to_f32, f32_to_f16, row_dot_many};
use rayon::prelude::*;
use std::sync::atomic::{AtomicU64, Ordering};

/// Time spent in each part, in nanoseconds: matmul, recurrent, attention, output (for tuning).
pub static PROFILE: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
fn timed<T>(slot: usize, f: impl FnOnce() -> T) -> T {
    let t = std::time::Instant::now();
    let r = f();
    PROFILE[slot].fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    r
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    /// Dense, biased q/k/v (Qwen2, Qwen2.5).
    Qwen2,
    /// Dense, q/k norms, no biases (Qwen3).
    Qwen3,
    /// Hybrid Gated DeltaNet + gated attention (Qwen3.5).
    Qwen35,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub arch: Arch,
    pub n_layer: usize,
    pub n_embd: usize,
    pub n_head: usize,
    pub n_head_kv: usize,
    pub head_dim: usize,
    pub n_ff: usize,
    pub n_vocab: usize,
    pub eps: f32,
    pub rope_base: f32,
    pub n_rot: usize,
    pub ctx: usize,
    // Gated DeltaNet (Qwen3.5)
    pub conv_kernel: usize,
    pub ssm_k_heads: usize,
    pub ssm_v_heads: usize,
    pub ssm_head_dim: usize,
    pub full_attn_interval: usize,
}

impl Config {
    pub fn is_recurrent(&self, layer: usize) -> bool {
        self.arch == Arch::Qwen35 && !(layer + 1).is_multiple_of(self.full_attn_interval)
    }
}

pub(crate) struct Layer {
    pub(crate) attn_norm: TensorInfo,
    pub(crate) ffn_norm: TensorInfo,
    pub(crate) ffn_gate: TensorInfo,
    pub(crate) ffn_up: TensorInfo,
    pub(crate) ffn_down: TensorInfo,
    // attention
    pub(crate) q: Option<TensorInfo>,
    pub(crate) k: Option<TensorInfo>,
    pub(crate) v: Option<TensorInfo>,
    pub(crate) o: Option<TensorInfo>,
    pub(crate) q_bias: Option<Vec<f32>>,
    pub(crate) k_bias: Option<Vec<f32>>,
    pub(crate) v_bias: Option<Vec<f32>>,
    pub(crate) q_norm: Option<Vec<f32>>,
    pub(crate) k_norm: Option<Vec<f32>>,
    // Gated DeltaNet
    pub(crate) qkv: Option<TensorInfo>,
    pub(crate) gate: Option<TensorInfo>,
    pub(crate) ssm_alpha: Option<TensorInfo>,
    pub(crate) ssm_beta: Option<TensorInfo>,
    pub(crate) ssm_out: Option<TensorInfo>,
    pub(crate) conv1d: Vec<f32>,
    pub(crate) dt_bias: Vec<f32>,
    pub(crate) ssm_a: Vec<f32>,
    pub(crate) ssm_norm: Vec<f32>,
    pub(crate) norms: (Vec<f32>, Vec<f32>),
}

pub struct Model {
    pub cfg: Config,
    pub(crate) g: Gguf,
    pub(crate) embd: TensorInfo,
    pub(crate) output: TensorInfo,
    pub(crate) output_norm: Vec<f32>,
    pub(crate) layers: Vec<Layer>,
    /// A trained LoRA adapter on the attention projections, if one is loaded.
    pub adapter: Option<crate::train::Adapter>,
}

/// Per-sequence memory: the attention cache (f16) and the recurrent states.
pub struct State {
    pub pos: usize,
    kvd: usize,
    k_cache: Vec<Vec<u16>>,
    v_cache: Vec<Vec<u16>>,
    conv: Vec<Vec<f32>>,
    ssm: Vec<Vec<f32>>,
}

/// A copy of a sequence's memory at some position, holding only the part in use: how a long
/// system prompt is processed once and reused. (Recurrent layers cannot be rewound, so for
/// Qwen3.5 a snapshot is the only way to reuse a prefix.)
pub struct Snapshot {
    pos: usize,
    k: Vec<Vec<u16>>,
    v: Vec<Vec<u16>>,
    conv: Vec<Vec<f32>>,
    ssm: Vec<Vec<f32>>,
}

/// Where a sequence was before a batch of tokens (see `State::mark`).
pub struct Mark {
    pub pos: usize,
    conv: Vec<Vec<f32>>,
    ssm: Vec<Vec<f32>>,
}

impl Snapshot {
    pub fn bytes(&self) -> usize {
        self.k
            .iter()
            .chain(&self.v)
            .map(|x| x.len() * 2)
            .sum::<usize>()
            + self
                .conv
                .iter()
                .chain(&self.ssm)
                .map(|x| x.len() * 4)
                .sum::<usize>()
    }
}

impl State {
    pub fn snapshot(&self) -> Snapshot {
        let n = self.pos * self.kvd;
        let cut = |c: &Vec<Vec<u16>>| c.iter().map(|l| l[..n.min(l.len())].to_vec()).collect();
        Snapshot {
            pos: self.pos,
            k: cut(&self.k_cache),
            v: cut(&self.v_cache),
            conv: self.conv.clone(),
            ssm: self.ssm.clone(),
        }
    }
    pub fn restore(&mut self, s: &Snapshot) {
        self.pos = s.pos;
        for (dst, src) in self
            .k_cache
            .iter_mut()
            .zip(&s.k)
            .chain(self.v_cache.iter_mut().zip(&s.v))
        {
            dst[..src.len()].copy_from_slice(src);
        }
        for (dst, src) in self
            .conv
            .iter_mut()
            .zip(&s.conv)
            .chain(self.ssm.iter_mut().zip(&s.ssm))
        {
            dst.copy_from_slice(src);
        }
    }
    /// The part of the state that cannot be rewound by moving `pos` back: the recurrent layers'
    /// states (constant size, nothing for a dense model). Taken before a speculative batch so
    /// the state can go back to before the tokens that turned out wrong.
    pub fn mark(&self) -> Mark {
        Mark { pos: self.pos, conv: self.conv.clone(), ssm: self.ssm.clone() }
    }
    /// Back to a mark: the attention cache entries after it are simply written over later.
    pub fn rewind(&mut self, m: &Mark) {
        self.pos = m.pos;
        for (dst, src) in self.conv.iter_mut().zip(&m.conv).chain(self.ssm.iter_mut().zip(&m.ssm)) {
            dst.copy_from_slice(src);
        }
    }
    /// Start over (recurrent states back to zero).
    pub fn reset(&mut self) {
        self.pos = 0;
        for v in self.conv.iter_mut().chain(self.ssm.iter_mut()) {
            v.iter_mut().for_each(|x| *x = 0.0);
        }
    }
}

pub(crate) fn floats(g: &Gguf, t: &TensorInfo) -> Vec<f32> {
    let mut v = vec![0f32; t.elements() as usize];
    crate::quant::decode_row(t.dtype, g.bytes(t), &mut v);
    v
}

pub(crate) fn rms_norm(x: &mut [f32], w: &[f32], eps: f32) {
    let ms = x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32;
    let s = 1.0 / (ms + eps).sqrt();
    for (v, w) in x.iter_mut().zip(w) {
        *v = *v * s * w;
    }
}

pub(crate) fn silu(x: f32) -> f32 {
    x / (1.0 + (-x).exp())
}
pub(crate) fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}
pub(crate) fn softplus(x: f32) -> f32 {
    if x > 20.0 {
        x
    } else {
        (1.0 + x.exp()).ln()
    }
}

/// NeoX-style rotary embedding on the first `n_rot` values of one head.
pub(crate) fn rope(v: &mut [f32], pos: usize, n_rot: usize, base: f32) {
    let half = n_rot / 2;
    for i in 0..half {
        let theta = pos as f32 * base.powf(-2.0 * i as f32 / n_rot as f32);
        let (s, c) = theta.sin_cos();
        let (a, b) = (v[i], v[i + half]);
        v[i] = a * c - b * s;
        v[i + half] = a * s + b * c;
    }
}

impl Model {
    pub fn load(g: Gguf, ctx: usize) -> Result<Model, String> {
        let arch = match g.arch() {
            "qwen2" => Arch::Qwen2,
            "qwen3" => Arch::Qwen3,
            "qwen35" => Arch::Qwen35,
            a => {
                return Err(format!(
                    "architecture {a:?} is not supported yet (qwen2, qwen3, qwen35)"
                ))
            }
        };
        let u = |k: &str| {
            g.arch_u64(k)
                .map(|v| v as usize)
                .ok_or(format!("missing {}.{k}", g.arch()))
        };
        let n_embd = u("embedding_length")?;
        let n_head = u("attention.head_count")?;
        let head_dim = g
            .arch_u64("attention.key_length")
            .map(|v| v as usize)
            .unwrap_or(n_embd / n_head);
        let embd = g
            .tensor("token_embd.weight")
            .ok_or("no token_embd")?
            .clone();
        let cfg = Config {
            arch,
            n_layer: u("block_count")?,
            n_embd,
            n_head,
            n_head_kv: u("attention.head_count_kv")?,
            head_dim,
            n_ff: u("feed_forward_length")?,
            n_vocab: embd.dims[1] as usize,
            eps: g
                .arch_f32("attention.layer_norm_rms_epsilon")
                .unwrap_or(1e-6),
            rope_base: g.arch_f32("rope.freq_base").unwrap_or(10000.0),
            n_rot: g
                .arch_u64("rope.dimension_count")
                .map(|v| v as usize)
                .unwrap_or(head_dim),
            ctx,
            conv_kernel: g.arch_u64("ssm.conv_kernel").unwrap_or(4) as usize,
            ssm_k_heads: g.arch_u64("ssm.group_count").unwrap_or(0) as usize,
            ssm_v_heads: g.arch_u64("ssm.time_step_rank").unwrap_or(0) as usize,
            ssm_head_dim: g.arch_u64("ssm.state_size").unwrap_or(0) as usize,
            full_attn_interval: g.arch_u64("full_attention_interval").unwrap_or(4) as usize,
        };
        let t = |n: String| g.tensor(&n).cloned();
        let need = |n: String| g.tensor(&n).cloned().ok_or(format!("missing tensor {n}"));
        let vec_of = |n: String| g.tensor(&n).map(|t| floats(&g, t));
        let mut layers = Vec::new();
        for i in 0..cfg.n_layer {
            let p = |s: &str| format!("blk.{i}.{s}");
            let ffn_norm = t(p("post_attention_norm.weight"))
                .or_else(|| t(p("ffn_norm.weight")))
                .ok_or(format!("layer {i}: no ffn norm"))?;
            let attn_norm = need(p("attn_norm.weight"))?;
            let norms = (floats(&g, &attn_norm), floats(&g, &ffn_norm));
            layers.push(Layer {
                attn_norm,
                ffn_norm,
                ffn_gate: need(p("ffn_gate.weight"))?,
                ffn_up: need(p("ffn_up.weight"))?,
                ffn_down: need(p("ffn_down.weight"))?,
                q: t(p("attn_q.weight")),
                k: t(p("attn_k.weight")),
                v: t(p("attn_v.weight")),
                o: t(p("attn_output.weight")),
                q_bias: vec_of(p("attn_q.bias")),
                k_bias: vec_of(p("attn_k.bias")),
                v_bias: vec_of(p("attn_v.bias")),
                q_norm: vec_of(p("attn_q_norm.weight")),
                k_norm: vec_of(p("attn_k_norm.weight")),
                qkv: t(p("attn_qkv.weight")),
                gate: t(p("attn_gate.weight")),
                ssm_alpha: t(p("ssm_alpha.weight")),
                ssm_beta: t(p("ssm_beta.weight")),
                ssm_out: t(p("ssm_out.weight")),
                conv1d: vec_of(p("ssm_conv1d.weight")).unwrap_or_default(),
                dt_bias: vec_of(p("ssm_dt.bias")).unwrap_or_default(),
                ssm_a: vec_of(p("ssm_a")).unwrap_or_default(),
                ssm_norm: vec_of(p("ssm_norm.weight")).unwrap_or_default(),
                norms,
            });
            if cfg.is_recurrent(i) && layers[i].qkv.is_none() {
                return Err(format!(
                    "layer {i} should be a DeltaNet layer but has no attn_qkv"
                ));
            }
        }
        let output = t("output.weight".into()).unwrap_or_else(|| embd.clone());
        let output_norm = floats(&g, g.tensor("output_norm.weight").ok_or("no output_norm")?);
        Ok(Model {
            cfg,
            g,
            embd,
            output,
            output_norm,
            layers,
            adapter: None,
        })
    }

    pub fn new_state(&self) -> State {
        let c = &self.cfg;
        let kv = c.n_head_kv * c.head_dim;
        let conv_ch = c.ssm_head_dim * (2 * c.ssm_k_heads + c.ssm_v_heads);
        let mut s = State {
            pos: 0,
            kvd: kv,
            k_cache: vec![],
            v_cache: vec![],
            conv: vec![],
            ssm: vec![],
        };
        for i in 0..c.n_layer {
            if c.is_recurrent(i) {
                s.k_cache.push(vec![]);
                s.v_cache.push(vec![]);
                s.conv.push(vec![0.0; (c.conv_kernel - 1) * conv_ch]);
                s.ssm
                    .push(vec![0.0; c.ssm_v_heads * c.ssm_head_dim * c.ssm_head_dim]);
            } else {
                s.k_cache.push(vec![0; c.ctx * kv]);
                s.v_cache.push(vec![0; c.ctx * kv]);
                s.conv.push(vec![]);
                s.ssm.push(vec![]);
            }
        }
        s
    }

    /// Bytes of memory a sequence's state takes (for the daemon's memory budget).
    pub fn state_bytes(&self) -> usize {
        let s = self.new_state();
        s.k_cache.iter().map(|v| v.len() * 2).sum::<usize>() * 2
            + s.conv
                .iter()
                .chain(&s.ssm)
                .map(|v| v.len() * 4)
                .sum::<usize>()
    }

    /// `w` (rows x cols) times `n` vectors in `x` (n x cols) -> n x rows, rows spread over threads.
    /// Quantized weights use the integer kernels: the inputs are quantized to 8 bits once here.
    pub fn matmul(&self, w: &TensorInfo, x: &[f32], n: usize) -> Vec<f32> {
        timed(0, || self.matmul_inner(w, x, n))
    }

    fn matmul_inner(&self, w: &TensorInfo, x: &[f32], n: usize) -> Vec<f32> {
        let cols = w.dims[0] as usize;
        let rows = w.dims[1] as usize;
        let (per, bytes) = w.dtype.block().expect("supported weight type");
        let row_bytes = cols / per * bytes;
        let data = self.g.bytes(w);
        let q8: Vec<crate::quant::Q8> = if crate::quant::has_q8_kernel(w.dtype) {
            x.chunks(cols).map(crate::quant::quantize_q8).collect()
        } else {
            vec![]
        };
        // ARMv8.6 i8mm: two rows at a time against inputs interleaved in pairs (once, here)
        let pairs = if n >= 2 && !q8.is_empty() && crate::quant::has_pair_kernel(w.dtype) {
            crate::quant::pair_inputs(&q8, cols)
        } else {
            vec![]
        };
        let mut t = vec![0f32; rows * n]; // rows x n
                                          // big enough tasks that scheduling is cheap, small enough that every core stays busy
        let chunk = (rows / (rayon::current_num_threads() * 8)).clamp(8, 256);
        t.par_chunks_mut(chunk * n)
            .enumerate()
            .for_each(|(ci, out)| {
                let r0 = ci * chunk;
                let here = out.len() / n;
                let mut k = 0;
                while k < here {
                    let r = r0 + k;
                    let row = &data[r * row_bytes..(r + 1) * row_bytes];
                    if !pairs.is_empty() && k + 1 < here {
                        let next = &data[(r + 1) * row_bytes..(r + 2) * row_bytes];
                        let (o0, o1) = out[k * n..(k + 2) * n].split_at_mut(n);
                        if crate::quant::rows2_dot_q8(w.dtype, row, next, cols, &q8, &pairs, o0, o1) {
                            k += 2;
                            continue;
                        }
                    }
                    let o = &mut out[k * n..(k + 1) * n];
                    if q8.is_empty() {
                        row_dot_many(w.dtype, row, cols, x, n, o);
                    } else {
                        crate::quant::row_dot_q8(w.dtype, row, cols, &q8, o);
                    }
                    k += 1;
                }
            });
        if n == 1 {
            return t;
        }
        let mut y = vec![0f32; n * rows];
        for r in 0..rows {
            for k in 0..n {
                y[k * rows + r] = t[r * n + k];
            }
        }
        y
    }

    /// `w` times `n` vectors without quantizing the inputs (weights decoded to f32): slower,
    /// but smooth, which training's gradient checks need.
    pub fn matmul_exact(&self, w: &TensorInfo, x: &[f32], n: usize) -> Vec<f32> {
        let cols = w.dims[0] as usize;
        let rows = w.dims[1] as usize;
        let (per, bytes) = w.dtype.block().expect("supported weight type");
        let row_bytes = cols / per * bytes;
        let data = self.g.bytes(w);
        let mut t = vec![0f32; rows * n];
        let chunk = (rows / (rayon::current_num_threads() * 8)).clamp(8, 256);
        t.par_chunks_mut(chunk * n).enumerate().for_each(|(ci, out)| {
            for (k, o) in out.chunks_mut(n).enumerate() {
                let r = ci * chunk + k;
                row_dot_many(w.dtype, &data[r * row_bytes..(r + 1) * row_bytes], cols, x, n, o);
            }
        });
        let mut y = vec![0f32; n * rows];
        for r in 0..rows {
            for k in 0..n {
                y[k * rows + r] = t[r * n + k];
            }
        }
        y
    }

    /// The transpose: `dx = wᵀ · dy` for `n` vectors `dy` (n x rows) -> n x cols. This is how a
    /// gradient flows back through a frozen (still compressed) weight matrix.
    pub fn matmul_t(&self, w: &TensorInfo, dy: &[f32], n: usize) -> Vec<f32> {
        let cols = w.dims[0] as usize;
        let rows = w.dims[1] as usize;
        let (per, bytes) = w.dtype.block().expect("supported weight type");
        let row_bytes = cols / per * bytes;
        let data = self.g.bytes(w);
        let mut dx = vec![0f32; n * cols];
        // A block of rows is decoded once (in parallel), then each thread adds it into its own
        // tile of tokens, small enough to stay in cache. No partial sums to add up afterwards.
        const ROWS: usize = 128;
        let tile = (n.div_ceil(rayon::current_num_threads().max(1))).clamp(1, 16);
        let mut block = vec![0f32; ROWS * cols];
        for r0 in (0..rows).step_by(ROWS) {
            let nr = ROWS.min(rows - r0);
            block[..nr * cols]
                .par_chunks_mut(cols)
                .enumerate()
                .for_each(|(i, out)| {
                    let r = r0 + i;
                    crate::quant::decode_row(w.dtype, &data[r * row_bytes..(r + 1) * row_bytes], out);
                });
            let block = &block;
            dx.par_chunks_mut(tile * cols)
                .enumerate()
                .for_each(|(ti, acc)| {
                    for (kk, a) in acc.chunks_mut(cols).enumerate() {
                        let k = ti * tile + kk;
                        let g = &dy[k * rows + r0..k * rows + r0 + nr];
                        for (i, &gi) in g.iter().enumerate() {
                            if gi != 0.0 {
                                let wr = &block[i * cols..(i + 1) * cols];
                                a.iter_mut().zip(wr).for_each(|(a, w)| *a += gi * w);
                            }
                        }
                    }
                });
        }
        dx
    }

    pub(crate) fn embed(&self, tokens: &[u32]) -> Vec<f32> {
        let d = self.cfg.n_embd;
        let (per, bytes) = self.embd.dtype.block().unwrap();
        let row_bytes = d / per * bytes;
        let data = self.g.bytes(&self.embd);
        let mut x = vec![0f32; tokens.len() * d];
        for (i, &t) in tokens.iter().enumerate() {
            let r = t as usize;
            crate::quant::decode_row(
                self.embd.dtype,
                &data[r * row_bytes..(r + 1) * row_bytes],
                &mut x[i * d..(i + 1) * d],
            );
        }
        x
    }

    /// Run `tokens` (positions st.pos..) through the model. Returns the logits of the last token.
    pub fn forward(&self, st: &mut State, tokens: &[u32]) -> Result<Vec<f32>, String> {
        self.forward_logits(st, tokens, false)
    }

    /// The same, returning the logits after every token (n x vocab): what checking a batch of
    /// drafted tokens needs.
    pub fn forward_all(&self, st: &mut State, tokens: &[u32]) -> Result<Vec<f32>, String> {
        self.forward_logits(st, tokens, true)
    }

    fn forward_logits(&self, st: &mut State, tokens: &[u32], all: bool) -> Result<Vec<f32>, String> {
        let c = &self.cfg;
        let n = tokens.len();
        if st.pos + n > c.ctx {
            return Err(format!("context full: {} + {n} tokens > {}", st.pos, c.ctx));
        }
        let d = c.n_embd;
        let mut x = self.embed(tokens);
        for (li, l) in self.layers.iter().enumerate() {
            let mut h = x.clone();
            for row in h.chunks_mut(d) {
                rms_norm(row, &l.norms.0, c.eps);
            }
            let out = if c.is_recurrent(li) {
                self.delta_net(li, l, st, &h, n)
            } else {
                self.attention(li, l, st, &h, n)
            };
            for (a, b) in x.iter_mut().zip(&out) {
                *a += b;
            }
            let mut h2 = x.clone();
            for row in h2.chunks_mut(d) {
                rms_norm(row, &l.norms.1, c.eps);
            }
            let gate = self.matmul(&l.ffn_gate, &h2, n);
            let up = self.matmul(&l.ffn_up, &h2, n);
            let act: Vec<f32> = gate.iter().zip(&up).map(|(g, u)| silu(*g) * u).collect();
            let down = self.matmul(&l.ffn_down, &act, n);
            for (a, b) in x.iter_mut().zip(&down) {
                *a += b;
            }
            let _ = (&l.attn_norm, &l.ffn_norm);
        }
        st.pos += n;
        if all {
            for row in x.chunks_mut(d) {
                rms_norm(row, &self.output_norm, c.eps);
            }
            return Ok(self.matmul(&self.output, &x, n));
        }
        let mut last = x[(n - 1) * d..].to_vec();
        rms_norm(&mut last, &self.output_norm, c.eps);
        let logits = self.matmul(&self.output, &last, 1);
        Ok(logits)
    }

    fn attention(&self, li: usize, l: &Layer, st: &mut State, h: &[f32], n: usize) -> Vec<f32> {
        let c = &self.cfg;
        let hd = c.head_dim;
        let kvd = c.n_head_kv * hd;
        let gated = c.arch == Arch::Qwen35;
        let mut q_full = self.matmul(l.q.as_ref().unwrap(), h, n);
        let mut k = self.matmul(l.k.as_ref().unwrap(), h, n);
        let mut v = self.matmul(l.v.as_ref().unwrap(), h, n);
        if let Some(ad) = &self.adapter {
            use crate::train::Target;
            ad.apply((li, Target::Q), h, n, &mut q_full);
            ad.apply((li, Target::K), h, n, &mut k);
            ad.apply((li, Target::V), h, n, &mut v);
        }
        let q_w = if gated {
            2 * c.n_head * hd
        } else {
            c.n_head * hd
        };
        let mut q = vec![0f32; n * c.n_head * hd];
        let mut gate = vec![0f32; if gated { n * c.n_head * hd } else { 0 }];
        for t in 0..n {
            let src = &q_full[t * q_w..(t + 1) * q_w];
            for hh in 0..c.n_head {
                let dst = &mut q[(t * c.n_head + hh) * hd..(t * c.n_head + hh + 1) * hd];
                if gated {
                    // per head: [query (hd) | gate (hd)]
                    dst.copy_from_slice(&src[hh * 2 * hd..hh * 2 * hd + hd]);
                    gate[(t * c.n_head + hh) * hd..(t * c.n_head + hh + 1) * hd]
                        .copy_from_slice(&src[hh * 2 * hd + hd..(hh + 1) * 2 * hd]);
                } else {
                    dst.copy_from_slice(&src[hh * hd..(hh + 1) * hd]);
                }
            }
        }
        if let Some(b) = &l.q_bias {
            for row in q.chunks_mut(b.len()) {
                row.iter_mut().zip(b).for_each(|(a, b)| *a += b);
            }
        }
        if let Some(b) = &l.k_bias {
            for row in k.chunks_mut(b.len()) {
                row.iter_mut().zip(b).for_each(|(a, b)| *a += b);
            }
        }
        if let Some(b) = &l.v_bias {
            for row in v.chunks_mut(b.len()) {
                row.iter_mut().zip(b).for_each(|(a, b)| *a += b);
            }
        }
        for t in 0..n {
            let pos = st.pos + t;
            for hh in 0..c.n_head {
                let hv = &mut q[(t * c.n_head + hh) * hd..(t * c.n_head + hh + 1) * hd];
                if let Some(w) = &l.q_norm {
                    rms_norm(hv, w, c.eps);
                }
                rope(hv, pos, c.n_rot, c.rope_base);
            }
            for hh in 0..c.n_head_kv {
                let hv = &mut k[t * kvd + hh * hd..t * kvd + (hh + 1) * hd];
                if let Some(w) = &l.k_norm {
                    rms_norm(hv, w, c.eps);
                }
                rope(hv, pos, c.n_rot, c.rope_base);
            }
            let kc = &mut st.k_cache[li][pos * kvd..(pos + 1) * kvd];
            for (dst, s) in kc.iter_mut().zip(&k[t * kvd..(t + 1) * kvd]) {
                *dst = f32_to_f16(*s);
            }
            let vc = &mut st.v_cache[li][pos * kvd..(pos + 1) * kvd];
            for (dst, s) in vc.iter_mut().zip(&v[t * kvd..(t + 1) * kvd]) {
                *dst = f32_to_f16(*s);
            }
        }
        let scale = 1.0 / (hd as f32).sqrt();
        let group = c.n_head / c.n_head_kv;
        let p0 = st.pos;
        let (kc, vc) = (&st.k_cache[li], &st.v_cache[li]);
        let mut out = vec![0f32; n * c.n_head * hd];
        out.par_chunks_mut(hd).enumerate().for_each(|(idx, o)| {
            let (t, hh) = (idx / c.n_head, idx % c.n_head);
            let kvh = hh / group;
            let qv = &q[idx * hd..(idx + 1) * hd];
            let upto = p0 + t;
            let mut scores = Vec::with_capacity(upto + 1);
            let mut kf = vec![0f32; hd];
            for j in 0..=upto {
                let kr = &kc[j * kvd + kvh * hd..j * kvd + (kvh + 1) * hd];
                kf.iter_mut().zip(kr).for_each(|(a, b)| *a = f16_to_f32(*b));
                scores.push(dot(qv, &kf) * scale);
            }
            let m = scores.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let mut sum = 0.0;
            for s in scores.iter_mut() {
                *s = (*s - m).exp();
                sum += *s;
            }
            for (j, s) in scores.iter().enumerate() {
                let w = s / sum;
                let vr = &vc[j * kvd + kvh * hd..j * kvd + (kvh + 1) * hd];
                o.iter_mut()
                    .zip(vr)
                    .for_each(|(a, b)| *a += w * f16_to_f32(*b));
            }
        });
        if gated {
            out.iter_mut()
                .zip(&gate)
                .for_each(|(a, g)| *a *= sigmoid(*g));
        }
        let mut y = self.matmul(l.o.as_ref().unwrap(), &out, n);
        if let Some(ad) = &self.adapter {
            ad.apply((li, crate::train::Target::O), &out, n, &mut y);
        }
        y
    }

    fn delta_net(&self, li: usize, l: &Layer, st: &mut State, h: &[f32], n: usize) -> Vec<f32> {
        let c = &self.cfg;
        let sd = c.ssm_head_dim;
        let (hk, hv) = (c.ssm_k_heads, c.ssm_v_heads);
        let key_dim = hk * sd;
        let val_dim = hv * sd;
        let ch = 2 * key_dim + val_dim;
        let kern = c.conv_kernel;
        let qkv = self.matmul(l.qkv.as_ref().unwrap(), h, n);
        let z = self.matmul(l.gate.as_ref().unwrap(), h, n);
        let beta = self.matmul(l.ssm_beta.as_ref().unwrap(), h, n);
        let alpha = self.matmul(l.ssm_alpha.as_ref().unwrap(), h, n);
        let mut outs = vec![0f32; n * val_dim];
        let q_scale = 1.0 / (sd as f32).sqrt();
        let eps = c.eps;
        for t in 0..n {
            // short causal convolution over [state (kern-1 steps), this step], then SiLU
            let cur = &qkv[t * ch..(t + 1) * ch];
            let conv_state = &mut st.conv[li];
            let mut mixed = vec![0f32; ch];
            for cc in 0..ch {
                let w = &l.conv1d[cc * kern..(cc + 1) * kern];
                let mut s = 0.0;
                for kk in 0..kern - 1 {
                    s += conv_state[kk * ch + cc] * w[kk];
                }
                s += cur[cc] * w[kern - 1];
                mixed[cc] = silu(s);
            }
            // shift the window
            conv_state.copy_within(ch.., 0);
            let last = (kern - 2) * ch;
            conv_state[last..last + ch].copy_from_slice(cur);

            let (qs, rest) = mixed.split_at_mut(key_dim);
            let (ks, vs) = rest.split_at_mut(key_dim);
            for head in qs.chunks_mut(sd).chain(ks.chunks_mut(sd)) {
                let norm = head.iter().map(|v| v * v).sum::<f32>().sqrt().max(eps);
                head.iter_mut().for_each(|v| *v /= norm);
            }
            // the heads are independent: one per task, across the cores
            let ssm = &mut st.ssm[li];
            let o_t = &mut outs[t * val_dim..(t + 1) * val_dim];
            let (qs, ks, vs) = (&*qs, &*ks, &*vs);
            ssm.par_chunks_mut(sd * sd)
                .zip(o_t.par_chunks_mut(sd))
                .enumerate()
                .for_each(|(vh, (s, o))| {
                    let kh = vh % hk; // llama.cpp tiles the k/q heads to the v heads
                    let qh = &qs[kh * sd..(kh + 1) * sd];
                    let kv = &ks[kh * sd..(kh + 1) * sd];
                    let vv = &vs[vh * sd..(vh + 1) * sd];
                    let b = sigmoid(beta[t * hv + vh]);
                    let g = softplus(alpha[t * hv + vh] + l.dt_bias[vh]) * l.ssm_a[vh];
                    let decay = g.exp();
                    // S[j][i]: j = value index, i = key index
                    for j in 0..sd {
                        let row = &mut s[j * sd..(j + 1) * sd];
                        row.iter_mut().for_each(|x| *x *= decay);
                        let sk = dot(row, kv);
                        let delta = (vv[j] - sk) * b;
                        row.iter_mut().zip(kv).for_each(|(x, k)| *x += k * delta);
                        o[j] = dot(row, qh) * q_scale;
                    }
                    // gated RMS norm: norm(o) * silu(z)
                    rms_norm(o, &l.ssm_norm, eps);
                    let zz = &z[t * val_dim + vh * sd..t * val_dim + (vh + 1) * sd];
                    o.iter_mut().zip(zz).for_each(|(a, z)| *a *= silu(*z));
                });
        }
        self.matmul(l.ssm_out.as_ref().unwrap(), &outs, n)
    }
}
