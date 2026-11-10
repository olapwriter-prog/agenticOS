//! Weight formats: turning ggml's quantized blocks back into numbers, and the dot products the
//! model is made of. Block layouts follow ggml-common.h exactly.
//!
//! The matrix-vector product never dequantizes a whole matrix: each row is decoded one block at
//! a time into a small buffer on the stack and used at once, so the weights stay compressed in
//! the page cache (a 4B model is 2.6 GB as Q4_K_M, 16 GB as f32).
use crate::gguf::DType;

pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1f) as u32;
    let mant = (h & 0x3ff) as u32;
    let bits = match exp {
        0 if mant == 0 => sign,
        0 => {
            // subnormal: normalise it
            let mut e = 127 - 15 + 1;
            let mut m = mant;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            sign | ((e as u32) << 23) | ((m & 0x3ff) << 13)
        }
        0x1f => sign | 0x7f80_0000 | (mant << 13),
        _ => sign | ((exp + 127 - 15) << 23) | (mant << 13),
    };
    f32::from_bits(bits)
}

pub fn f32_to_f16(f: f32) -> u16 {
    let x = f.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let exp = ((x >> 23) & 0xff) as i32;
    let mant = x & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if mant != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = (mant | 0x80_0000) >> (1 - e);
        // round to nearest even
        let half = m >> 13;
        let rest = m & 0x1fff;
        let r = if rest > 0x1000 || (rest == 0x1000 && half & 1 == 1) {
            half + 1
        } else {
            half
        };
        return sign | r as u16;
    }
    let half = ((e as u32) << 10) | (mant >> 13);
    let rest = mant & 0x1fff;
    let r = if rest > 0x1000 || (rest == 0x1000 && half & 1 == 1) {
        half + 1
    } else {
        half
    };
    sign | r as u16
}

fn f16(b: &[u8], at: usize) -> f32 {
    f16_to_f32(u16::from_le_bytes([b[at], b[at + 1]]))
}

/// Q4_K and Q5_K pack eight 6-bit scales and mins into 12 bytes.
fn scale_min_k4(j: usize, q: &[u8]) -> (u8, u8) {
    if j < 4 {
        (q[j] & 63, q[j + 4] & 63)
    } else {
        (
            (q[j + 4] & 0xf) | ((q[j - 4] >> 6) << 4),
            (q[j + 4] >> 4) | ((q[j] >> 6) << 4),
        )
    }
}

/// Decode one block of `dtype` from `b` into `out` (32 or 256 values; 1 for f32/f16).
pub fn decode_block(dtype: DType, b: &[u8], out: &mut [f32]) {
    match dtype {
        DType::F32 => out[0] = f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        DType::F16 => out[0] = f16(b, 0),
        DType::BF16 => out[0] = f32::from_bits((u16::from_le_bytes([b[0], b[1]]) as u32) << 16),
        DType::Q8_0 => {
            let d = f16(b, 0);
            for j in 0..32 {
                out[j] = d * (b[2 + j] as i8) as f32;
            }
        }
        DType::Q4_0 => {
            let d = f16(b, 0);
            for j in 0..16 {
                let q = b[2 + j];
                out[j] = d * ((q & 0xf) as i32 - 8) as f32;
                out[j + 16] = d * ((q >> 4) as i32 - 8) as f32;
            }
        }
        DType::Q5_0 => {
            let d = f16(b, 0);
            let qh = u32::from_le_bytes([b[2], b[3], b[4], b[5]]);
            for j in 0..16 {
                let q = b[6 + j];
                let h0 = ((qh >> j) << 4) & 0x10;
                let h1 = (qh >> (j + 12)) & 0x10;
                out[j] = d * (((q & 0xf) as u32 | h0) as i32 - 16) as f32;
                out[j + 16] = d * (((q >> 4) as u32 | h1) as i32 - 16) as f32;
            }
        }
        DType::Q4K => {
            let d = f16(b, 0);
            let dmin = f16(b, 2);
            let scales = &b[4..16];
            let qs = &b[16..144];
            let mut is = 0;
            for c in 0..4 {
                let q = &qs[c * 32..c * 32 + 32];
                let (sc1, m1) = scale_min_k4(is, scales);
                let (sc2, m2) = scale_min_k4(is + 1, scales);
                let (d1, m1) = (d * sc1 as f32, dmin * m1 as f32);
                let (d2, m2) = (d * sc2 as f32, dmin * m2 as f32);
                let o = &mut out[c * 64..c * 64 + 64];
                for l in 0..32 {
                    o[l] = d1 * (q[l] & 0xf) as f32 - m1;
                    o[l + 32] = d2 * (q[l] >> 4) as f32 - m2;
                }
                is += 2;
            }
        }
        DType::Q5K => {
            let d = f16(b, 0);
            let dmin = f16(b, 2);
            let scales = &b[4..16];
            let qh = &b[16..48];
            let qs = &b[48..176];
            let (mut u1, mut u2) = (1u8, 2u8);
            let mut is = 0;
            for c in 0..4 {
                let ql = &qs[c * 32..c * 32 + 32];
                let (sc1, m1) = scale_min_k4(is, scales);
                let (sc2, m2) = scale_min_k4(is + 1, scales);
                let (d1, m1) = (d * sc1 as f32, dmin * m1 as f32);
                let (d2, m2) = (d * sc2 as f32, dmin * m2 as f32);
                let o = &mut out[c * 64..c * 64 + 64];
                for l in 0..32 {
                    let hi1 = if qh[l] & u1 != 0 { 16 } else { 0 };
                    let hi2 = if qh[l] & u2 != 0 { 16 } else { 0 };
                    o[l] = d1 * ((ql[l] & 0xf) + hi1) as f32 - m1;
                    o[l + 32] = d2 * ((ql[l] >> 4) + hi2) as f32 - m2;
                }
                is += 2;
                u1 <<= 2;
                u2 <<= 2;
            }
        }
        DType::Q6K => {
            let ql_all = &b[0..128];
            let qh_all = &b[128..192];
            let sc_all = &b[192..208];
            let d = f16(b, 208);
            for n in 0..2 {
                let ql = &ql_all[n * 64..n * 64 + 64];
                let qh = &qh_all[n * 32..n * 32 + 32];
                let sc = &sc_all[n * 8..n * 8 + 8];
                let o = &mut out[n * 128..n * 128 + 128];
                for l in 0..32 {
                    let is = l / 16;
                    let q1 = ((ql[l] & 0xf) | ((qh[l] & 3) << 4)) as i32 - 32;
                    let q2 = ((ql[l + 32] & 0xf) | (((qh[l] >> 2) & 3) << 4)) as i32 - 32;
                    let q3 = ((ql[l] >> 4) | (((qh[l] >> 4) & 3) << 4)) as i32 - 32;
                    let q4 = ((ql[l + 32] >> 4) | (((qh[l] >> 6) & 3) << 4)) as i32 - 32;
                    o[l] = d * (sc[is] as i8) as f32 * q1 as f32;
                    o[l + 32] = d * (sc[is + 2] as i8) as f32 * q2 as f32;
                    o[l + 64] = d * (sc[is + 4] as i8) as f32 * q3 as f32;
                    o[l + 96] = d * (sc[is + 6] as i8) as f32 * q4 as f32;
                }
            }
        }
        DType::Other(t) => panic!("ggml type {t} is not supported"),
    }
}

/// Decode a whole row of `n` values.
pub fn decode_row(dtype: DType, row: &[u8], out: &mut [f32]) {
    let (per, bytes) = dtype.block().expect("supported type");
    if per == 1 {
        for (i, o) in out.iter_mut().enumerate() {
            decode_block(dtype, &row[i * bytes..], std::slice::from_mut(o));
        }
        return;
    }
    for (bi, chunk) in out.chunks_mut(per).enumerate() {
        decode_block(dtype, &row[bi * bytes..(bi + 1) * bytes], chunk);
    }
}

/// Dot product written so LLVM vectorises it (NEON on phones): eight independent sums.
#[inline]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (a, b) = (&a[..n], &b[..n]);
    let mut acc = [0f32; 8];
    let chunks = n / 8;
    for c in 0..chunks {
        let (x, y) = (&a[c * 8..c * 8 + 8], &b[c * 8..c * 8 + 8]);
        for k in 0..8 {
            acc[k] += x[k] * y[k];
        }
    }
    let mut s: f32 = acc.iter().sum();
    for i in chunks * 8..n {
        s += a[i] * b[i];
    }
    s
}

/// One weight row times several input vectors, decoding the row a block at a time.
/// `xs` holds `n_x` vectors of length `cols` back to back; results go to `out[k]`.
pub fn row_dot_many(
    dtype: DType,
    row: &[u8],
    cols: usize,
    xs: &[f32],
    n_x: usize,
    out: &mut [f32],
) {
    let (per, bytes) = dtype.block().expect("supported type");
    for o in out.iter_mut().take(n_x) {
        *o = 0.0;
    }
    if per == 1 {
        let mut w = vec![0f32; cols];
        decode_row(dtype, row, &mut w);
        for k in 0..n_x {
            out[k] = dot(&w, &xs[k * cols..(k + 1) * cols]);
        }
        return;
    }
    let mut buf = [0f32; 256];
    for bi in 0..cols / per {
        let blk = &mut buf[..per];
        decode_block(dtype, &row[bi * bytes..(bi + 1) * bytes], blk);
        let at = bi * per;
        for k in 0..n_x {
            out[k] += dot(blk, &xs[k * cols + at..k * cols + at + per]);
        }
    }
}

// ---- integer kernels ------------------------------------------------------------------------
//
// The input vector is quantized once to 8-bit integers in blocks of 32 (a scale per block), and
// every weight row is dotted with it in integer arithmetic. These plain loops vectorise to NEON
// (and SDOT/SMLAL where the CPU has them) without any unsafe code: measured on an Apple core at
// 80 billion i8 multiply-adds a second, against ~10 for decoding weights to f32 first.

/// An input vector as 8-bit integers: per 32 values a scale and the sum of the integers (for
/// the "min" terms of K-quants).
#[derive(Clone)]
pub struct Q8 {
    pub q: Vec<i8>,
    pub d: Vec<f32>,
    pub sum: Vec<i32>,
}

pub fn quantize_q8(x: &[f32]) -> Q8 {
    let nb = x.len().div_ceil(32);
    let mut q = vec![0i8; nb * 32];
    let mut d = vec![0f32; nb];
    let mut sum = vec![0i32; nb];
    for b in 0..nb {
        let blk = &x[b * 32..((b + 1) * 32).min(x.len())];
        let amax = blk.iter().fold(0f32, |m, v| m.max(v.abs()));
        let scale = amax / 127.0;
        let inv = if scale > 0.0 { 1.0 / scale } else { 0.0 };
        let mut s = 0i32;
        for (i, v) in blk.iter().enumerate() {
            let qi = (v * inv).round().clamp(-127.0, 127.0) as i8;
            q[b * 32 + i] = qi;
            s += qi as i32;
        }
        d[b] = scale;
        sum[b] = s;
    }
    Q8 { q, d, sum }
}

#[inline(always)]
fn idot(a: &[i8], b: &[i8]) -> i32 {
    a.iter().zip(b).map(|(x, y)| *x as i32 * *y as i32).sum()
}

/// One K-quant superblock's 256 weights as integers (Q4_K, Q5_K: 0..31 before the per-32 scale
/// and min; Q6_K: -32..31 before the per-16 scale), in value order.
pub fn unpack_k(dtype: DType, blk: &[u8], w: &mut [i8; 256]) {
    match dtype {
        DType::Q4K => {
            let qs = &blk[16..144];
            for c in 0..4 {
                for l in 0..32 {
                    let q = qs[c * 32 + l];
                    w[c * 64 + l] = (q & 0xf) as i8;
                    w[c * 64 + 32 + l] = (q >> 4) as i8;
                }
            }
        }
        DType::Q5K => {
            let qh = &blk[16..48];
            let qs = &blk[48..176];
            for c in 0..4 {
                let (u1, u2) = (1u8 << (2 * c), 2u8 << (2 * c));
                for l in 0..32 {
                    let q = qs[c * 32 + l];
                    w[c * 64 + l] = ((q & 0xf) + if qh[l] & u1 != 0 { 16 } else { 0 }) as i8;
                    w[c * 64 + 32 + l] = ((q >> 4) + if qh[l] & u2 != 0 { 16 } else { 0 }) as i8;
                }
            }
        }
        DType::Q6K => {
            for n in 0..2 {
                let ql = &blk[n * 64..n * 64 + 64];
                let qh = &blk[128 + n * 32..128 + n * 32 + 32];
                for l in 0..32 {
                    w[n * 128 + l] = ((ql[l] & 0xf) | ((qh[l] & 3) << 4)) as i8 - 32;
                    w[n * 128 + l + 32] = ((ql[l + 32] & 0xf) | (((qh[l] >> 2) & 3) << 4)) as i8 - 32;
                    w[n * 128 + l + 64] = ((ql[l] >> 4) | (((qh[l] >> 4) & 3) << 4)) as i8 - 32;
                    w[n * 128 + l + 96] = ((ql[l + 32] >> 4) | (((qh[l] >> 6) & 3) << 4)) as i8 - 32;
                }
            }
        }
        _ => unreachable!("unpack_k is for K-quants"),
    }
}

/// A K-quant superblock's scales: Q4_K/Q5_K give 8 (scale, min) pairs per 32 values; Q6_K 16
/// scales per 16 values (in `sc`, `mn` unused).
fn k_scales(dtype: DType, blk: &[u8], sc: &mut [f32; 16], mn: &mut [f32; 8]) {
    if dtype == DType::Q6K {
        let d = f16(blk, 208);
        for (i, v) in sc.iter_mut().enumerate() {
            *v = d * (blk[192 + i] as i8) as f32;
        }
    } else {
        let (d, dmin) = (f16(blk, 0), f16(blk, 2));
        for j in 0..8 {
            let (s, m) = scale_min_k4(j, &blk[4..16]);
            sc[j] = d * s as f32;
            mn[j] = dmin * m as f32;
        }
    }
}

/// One weight row against several quantized inputs. Each 256-value superblock of the row is
/// unpacked to integers once and used for every input (that is what makes prompt batches fast).
pub fn row_dot_q8(dtype: DType, row: &[u8], cols: usize, xs: &[Q8], out: &mut [f32]) {
    if row_dot_q8_neon(dtype, row, cols, xs, out) {
        return;
    }
    row_dot_q8_portable(dtype, row, cols, xs, out)
}

/// The same, in plain Rust: any CPU (and the reference the NEON kernels are tested against).
pub fn row_dot_q8_portable(dtype: DType, row: &[u8], cols: usize, xs: &[Q8], out: &mut [f32]) {
    for o in out.iter_mut().take(xs.len()) {
        *o = 0.0;
    }
    let mut w = [0i8; 256];
    match dtype {
        DType::Q8_0 | DType::Q5_0 | DType::Q4_0 => {
            let bytes = dtype.block().unwrap().1;
            for b in 0..cols / 32 {
                let blk = &row[b * bytes..(b + 1) * bytes];
                let d = f16(blk, 0);
                match dtype {
                    DType::Q8_0 => {
                        for j in 0..32 {
                            w[j] = blk[2 + j] as i8;
                        }
                    }
                    DType::Q4_0 => {
                        for j in 0..16 {
                            w[j] = (blk[2 + j] & 0xf) as i8 - 8;
                            w[j + 16] = (blk[2 + j] >> 4) as i8 - 8;
                        }
                    }
                    _ => {
                        let qh = u32::from_le_bytes([blk[2], blk[3], blk[4], blk[5]]);
                        for j in 0..16 {
                            let q = blk[6 + j];
                            let h0 = (((qh >> j) << 4) & 0x10) as u8;
                            let h1 = ((qh >> (j + 12)) & 0x10) as u8;
                            w[j] = ((q & 0xf) | h0) as i8 - 16;
                            w[j + 16] = ((q >> 4) | h1) as i8 - 16;
                        }
                    }
                }
                for (k, x) in xs.iter().enumerate() {
                    out[k] += d * x.d[b] * idot(&w[..32], &x.q[b * 32..b * 32 + 32]) as f32;
                }
            }
        }
        DType::Q4K | DType::Q5K | DType::Q6K => {
            let bytes = dtype.block().unwrap().1;
            let (mut sc, mut mn) = ([0f32; 16], [0f32; 8]);
            for sb in 0..cols / 256 {
                let blk = &row[sb * bytes..(sb + 1) * bytes];
                unpack_k(dtype, blk, &mut w);
                k_scales(dtype, blk, &mut sc, &mut mn);
                for (k, x) in xs.iter().enumerate() {
                    let mut acc = 0f32;
                    for b in 0..8 {
                        let ab = sb * 8 + b;
                        let xq = &x.q[ab * 32..ab * 32 + 32];
                        if dtype == DType::Q6K {
                            let s0 = idot(&w[b * 32..b * 32 + 16], &xq[..16]);
                            let s1 = idot(&w[b * 32 + 16..b * 32 + 32], &xq[16..]);
                            acc += x.d[ab] * (sc[2 * b] * s0 as f32 + sc[2 * b + 1] * s1 as f32);
                        } else {
                            let s = idot(&w[b * 32..b * 32 + 32], xq);
                            acc += x.d[ab] * (sc[b] * s as f32 - mn[b] * x.sum[ab] as f32);
                        }
                    }
                    out[k] += acc;
                }
            }
        }
        _ => unreachable!("row_dot_q8 handles quantized types only"),
    }
}

/// ARM NEON kernels for the K-quants, over weights already unpacked to integers (once per
/// superblock, for every input): SDOT (ARMv8.2 dot product, on phones since ~2018), 16
/// multiply-adds per instruction; and SMMLA (ARMv8.6 i8mm), a 2x8 by 8x2 integer matrix product,
/// 32 per instruction, for two rows against two inputs at once. Both are emitted with inline
/// assembly because their intrinsics are not yet stable in Rust.
#[cfg(target_arch = "aarch64")]
mod neon {
    use std::arch::aarch64::*;

    #[inline(always)]
    unsafe fn sdot(acc: int32x4_t, a: int8x16_t, b: int8x16_t) -> int32x4_t {
        let mut r = acc;
        core::arch::asm!("sdot {0:v}.4s, {1:v}.16b, {2:v}.16b",
            inout(vreg) r, in(vreg) a, in(vreg) b, options(pure, nomem, nostack));
        r
    }

    /// Sum of 32 products: weights `w0|w1`, inputs at `y`.
    #[inline(always)]
    unsafe fn dot32(w0: int8x16_t, w1: int8x16_t, y: *const i8) -> i32 {
        let z = vdupq_n_s32(0);
        let a = sdot(z, w0, vld1q_s8(y));
        let b = sdot(z, w1, vld1q_s8(y.add(16)));
        vaddvq_s32(vaddq_s32(a, b))
    }

    /// Eight sums of 32 products: unpacked weights `w` (256) against inputs `y` (256).
    #[target_feature(enable = "neon,dotprod")]
    pub unsafe fn sums32(w: *const i8, y: *const i8, out: &mut [i32; 8]) {
        for (j, o) in out.iter_mut().enumerate() {
            *o = dot32(vld1q_s8(w.add(j * 32)), vld1q_s8(w.add(j * 32 + 16)), y.add(j * 32));
        }
    }

    /// Sixteen sums of 16 products (Q6_K's scale groups).
    #[target_feature(enable = "neon,dotprod")]
    pub unsafe fn sums16(w: *const i8, y: *const i8, out: &mut [i32; 16]) {
        let z = vdupq_n_s32(0);
        for (g, o) in out.iter_mut().enumerate() {
            *o = vaddvq_s32(sdot(z, vld1q_s8(w.add(g * 16)), vld1q_s8(y.add(g * 16))));
        }
    }

    #[inline(always)]
    unsafe fn smmla(acc: int32x4_t, a: int8x16_t, b: int8x16_t) -> int32x4_t {
        let mut r = acc;
        core::arch::asm!("smmla {0:v}.4s, {1:v}.16b, {2:v}.16b",
            inout(vreg) r, in(vreg) a, in(vreg) b, options(pure, nomem, nostack));
        r
    }

    /// Two weight rows against two inputs over one superblock, with SMMLA (ARMv8.6 i8mm): `w` and
    /// `y` hold their pair interleaved per 8 values ([row0 8 | row1 8] x 32, the same for the
    /// inputs); each output is the 2x2 block [r0·x0, r0·x1, r1·x0, r1·x1] over `per` values.
    #[target_feature(enable = "neon,i8mm")]
    pub unsafe fn mm2x2(w: *const i8, y: *const i8, per: usize, out: &mut [[i32; 4]]) {
        let groups = per / 8;
        for (s, o) in out.iter_mut().enumerate() {
            let mut acc = vdupq_n_s32(0);
            for g in 0..groups {
                let at = (s * groups + g) * 16;
                acc = smmla(acc, vld1q_s8(w.add(at)), vld1q_s8(y.add(at)));
            }
            vst1q_s32(o.as_mut_ptr(), acc);
        }
    }

}

fn has_i8mm() -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        static D: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        // AGENTOS_NO_I8MM=1 turns the kernel off, to measure what it gains on a device
        *D.get_or_init(|| std::arch::is_aarch64_feature_detected!("i8mm") && std::env::var_os("AGENTOS_NO_I8MM").is_none())
    }
    #[cfg(not(target_arch = "aarch64"))]
    false
}

fn has_dotprod() -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        static D: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        // AGENTOS_NO_DOTPROD=1 turns the kernel off, to measure what it gains on a device
        *D.get_or_init(|| std::arch::is_aarch64_feature_detected!("dotprod") && std::env::var_os("AGENTOS_NO_DOTPROD").is_none())
    }
    #[cfg(not(target_arch = "aarch64"))]
    false
}

/// The K-quant rows through the NEON kernels (SDOT). Each superblock is unpacked once and used
/// for every input. Returns false if this CPU or type has none.
fn row_dot_q8_neon(dtype: DType, row: &[u8], cols: usize, xs: &[Q8], out: &mut [f32]) -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        if !has_dotprod() || !matches!(dtype, DType::Q4K | DType::Q5K | DType::Q6K) {
            return false;
        }
        let bytes = dtype.block().unwrap().1;
        for o in out.iter_mut().take(xs.len()) {
            *o = 0.0;
        }
        let mut w = [0i8; 256];
        let (mut sc, mut mn) = ([0f32; 16], [0f32; 8]);
        for sb in 0..cols / 256 {
            let blk = &row[sb * bytes..(sb + 1) * bytes];
            unpack_k(dtype, blk, &mut w);
            k_scales(dtype, blk, &mut sc, &mut mn);
            for (k, x) in xs.iter().enumerate() {
                let y = unsafe { x.q.as_ptr().add(sb * 256) };
                let mut acc = 0f32;
                if dtype == DType::Q6K {
                    let mut s = [0i32; 16];
                    unsafe { neon::sums16(w.as_ptr(), y, &mut s) };
                    for b in 0..8 {
                        acc += x.d[sb * 8 + b] * (sc[2 * b] * s[2 * b] as f32 + sc[2 * b + 1] * s[2 * b + 1] as f32);
                    }
                } else {
                    let mut s = [0i32; 8];
                    unsafe { neon::sums32(w.as_ptr(), y, &mut s) };
                    for j in 0..8 {
                        let b = sb * 8 + j;
                        acc += x.d[b] * (sc[j] * s[j] as f32 - mn[j] * x.sum[b] as f32);
                    }
                }
                out[k] += acc;
            }
        }
        true
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = (dtype, row, cols, xs, out);
        false
    }
}

/// Can two rows at a time go through the SMMLA kernel (ARMv8.6 i8mm, K-quants)?
pub fn has_pair_kernel(dtype: DType) -> bool {
    has_i8mm() && matches!(dtype, DType::Q4K | DType::Q5K | DType::Q6K)
}

/// The inputs interleaved in pairs for the SMMLA kernel: for inputs 2p and 2p+1, per 8 values,
/// 8 of the first then 8 of the second (an odd last input is paired with zeros). Done once per
/// matrix product, used by every row.
pub fn pair_inputs(xs: &[Q8], cols: usize) -> Vec<Vec<i8>> {
    xs.chunks(2)
        .map(|p| {
            let mut v = vec![0i8; cols * 2];
            for g in 0..cols / 8 {
                v[g * 16..g * 16 + 8].copy_from_slice(&p[0].q[g * 8..g * 8 + 8]);
                if let Some(x1) = p.get(1) {
                    v[g * 16 + 8..g * 16 + 16].copy_from_slice(&x1.q[g * 8..g * 8 + 8]);
                }
            }
            v
        })
        .collect()
}

/// Two weight rows against every input with SMMLA: each superblock of the two rows is unpacked
/// and interleaved once, then multiplied with each interleaved pair of inputs, 32 products per
/// instruction (SDOT does 16). Returns false if this CPU or type has no such kernel.
#[allow(clippy::too_many_arguments)]
pub fn rows2_dot_q8(dtype: DType, r0: &[u8], r1: &[u8], cols: usize, xs: &[Q8], pairs: &[Vec<i8>], out0: &mut [f32], out1: &mut [f32]) -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        if !has_pair_kernel(dtype) {
            return false;
        }
        let bytes = dtype.block().unwrap().1;
        out0.iter_mut().chain(out1.iter_mut()).for_each(|o| *o = 0.0);
        let (mut w0, mut w1) = ([0i8; 256], [0i8; 256]);
        let mut wi = [0i8; 512];
        let (mut sc0, mut mn0, mut sc1, mut mn1) = ([0f32; 16], [0f32; 8], [0f32; 16], [0f32; 8]);
        let q6 = dtype == DType::Q6K;
        for sb in 0..cols / 256 {
            let (b0, b1) = (&r0[sb * bytes..(sb + 1) * bytes], &r1[sb * bytes..(sb + 1) * bytes]);
            unpack_k(dtype, b0, &mut w0);
            unpack_k(dtype, b1, &mut w1);
            k_scales(dtype, b0, &mut sc0, &mut mn0);
            k_scales(dtype, b1, &mut sc1, &mut mn1);
            for g in 0..32 {
                wi[g * 16..g * 16 + 8].copy_from_slice(&w0[g * 8..g * 8 + 8]);
                wi[g * 16 + 8..g * 16 + 16].copy_from_slice(&w1[g * 8..g * 8 + 8]);
            }
            for (p, pq) in pairs.iter().enumerate() {
                let y = unsafe { pq.as_ptr().add(sb * 512) };
                let mut s = [[0i32; 4]; 16];
                let groups = if q6 { 16 } else { 8 };
                unsafe { neon::mm2x2(wi.as_ptr(), y, 256 / groups, &mut s[..groups]) };
                // lanes: 0 = row0·x0, 1 = row0·x1, 2 = row1·x0, 3 = row1·x1
                for (half, k) in [2 * p, 2 * p + 1].into_iter().enumerate() {
                    let Some(x) = xs.get(k) else { continue };
                    for (r, (sc, mn, out)) in [(&sc0, &mn0, &mut *out0), (&sc1, &mn1, &mut *out1)].into_iter().enumerate() {
                        let lane = 2 * r + half;
                        let mut acc = 0f32;
                        for b in 0..8 {
                            let ab = sb * 8 + b;
                            acc += if q6 {
                                x.d[ab] * (sc[2 * b] * s[2 * b][lane] as f32 + sc[2 * b + 1] * s[2 * b + 1][lane] as f32)
                            } else {
                                x.d[ab] * (sc[b] * s[b][lane] as f32 - mn[b] * x.sum[ab] as f32)
                            };
                        }
                        out[k] += acc;
                    }
                }
            }
        }
        true
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = (dtype, r0, r1, cols, xs, pairs, out0, out1);
        false
    }
}

/// Can this weight type use the integer kernels?
pub fn has_q8_kernel(dtype: DType) -> bool {
    matches!(
        dtype,
        DType::Q8_0 | DType::Q5_0 | DType::Q4_0 | DType::Q4K | DType::Q5K | DType::Q6K
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn half_floats_round_trip() {
        for v in [0.0f32, 1.0, -2.5, 0.000061035156, 65504.0, 1e-7, 3.14159] {
            let back = f16_to_f32(f32_to_f16(v));
            assert!((back - v).abs() <= v.abs() * 1e-3 + 1e-7, "{v} -> {back}");
        }
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
    }
    #[test]
    fn q8_0_decodes() {
        let mut b = vec![0u8; 34];
        b[0..2].copy_from_slice(&f32_to_f16(0.5).to_le_bytes());
        for j in 0..32 {
            b[2 + j] = (j as i8 - 16) as u8;
        }
        let mut out = [0f32; 32];
        decode_block(DType::Q8_0, &b, &mut out);
        assert_eq!(out[0], -8.0);
        assert_eq!(out[31], 7.5);
    }
    #[test]
    fn q4_k_decodes_scale_and_min() {
        // d = 1, dmin = 1, every scale 2 and min 1, every nibble 3: value = 2*3 - 1 = 5
        let mut b = vec![0u8; 144];
        b[0..2].copy_from_slice(&f32_to_f16(1.0).to_le_bytes());
        b[2..4].copy_from_slice(&f32_to_f16(1.0).to_le_bytes());
        for j in 0..4 {
            b[4 + j] = 2; // scales 0..3
            b[8 + j] = 1; // mins 0..3
            b[12 + j] = 2 | (1 << 4); // scales/mins 4..7, low bits
        }
        for j in 16..144 {
            b[j] = 0x33;
        }
        let mut out = [0f32; 256];
        decode_block(DType::Q4K, &b, &mut out);
        assert!(out.iter().all(|v| *v == 5.0), "{:?}", &out[..8]);
    }
    /// Random bytes are valid blocks for every format (scales just come out arbitrary), so the
    /// integer kernels can be checked against decode-then-dot on random data.
    #[test]
    fn kernel_paths() {
        // which kernels this CPU runs (shown with --nocapture; the cross-CPU runs check it)
        eprintln!("kernels: dotprod {} i8mm {}", has_dotprod(), has_i8mm());
    }

    /// Several rows against several inputs (odd counts too): the SDOT and SMMLA kernels give
    /// what the portable one does, whichever this CPU has.
    #[test]
    fn batched_kernels_agree_with_portable() {
        let mut seed = 777u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let cols = 512;
        for dtype in [DType::Q4K, DType::Q5K, DType::Q6K] {
            let (per, bytes) = dtype.block().unwrap();
            let rows: Vec<Vec<u8>> = (0..3)
                .map(|_| {
                    let mut r: Vec<u8> = (0..cols / per * bytes).map(|_| rnd() as u8).collect();
                    for b in 0..cols / per {
                        let at = if dtype == DType::Q6K { b * bytes + 208 } else { b * bytes };
                        r[at..at + 2].copy_from_slice(&f32_to_f16(0.01 + (rnd() % 100) as f32 * 1e-4).to_le_bytes());
                        if dtype != DType::Q6K {
                            r[at + 2..at + 4].copy_from_slice(&f32_to_f16(0.005).to_le_bytes());
                        }
                    }
                    r
                })
                .collect();
            for n in 1..=5 {
                let xs: Vec<Q8> = (0..n)
                    .map(|_| quantize_q8(&(0..cols).map(|_| (rnd() % 2000) as f32 / 1000.0 - 1.0).collect::<Vec<_>>()))
                    .collect();
                let want: Vec<Vec<f32>> = rows
                    .iter()
                    .map(|r| {
                        let mut o = vec![0f32; n];
                        row_dot_q8_portable(dtype, r, cols, &xs, &mut o);
                        o
                    })
                    .collect();
                let close = |a: f32, b: f32| (a - b).abs() <= b.abs() * 1e-5 + 1e-4;
                for (r, w) in rows.iter().zip(&want) {
                    let mut o = vec![0f32; n];
                    row_dot_q8(dtype, r, cols, &xs, &mut o);
                    assert!(o.iter().zip(w).all(|(a, b)| close(*a, *b)), "{dtype:?} n={n}: {o:?} vs {w:?}");
                }
                let pairs = pair_inputs(&xs, cols);
                let (mut o0, mut o1) = (vec![0f32; n], vec![0f32; n]);
                if rows2_dot_q8(dtype, &rows[1], &rows[2], cols, &xs, &pairs, &mut o0, &mut o1) {
                    assert!(o0.iter().zip(&want[1]).all(|(a, b)| close(*a, *b)), "{dtype:?} n={n} pair row0: {o0:?} vs {:?}", want[1]);
                    assert!(o1.iter().zip(&want[2]).all(|(a, b)| close(*a, *b)), "{dtype:?} n={n} pair row1: {o1:?} vs {:?}", want[2]);
                } else {
                    assert!(!has_pair_kernel(dtype));
                }
            }
        }
    }

    #[test]
    fn integer_kernels_agree_with_decoding() {
        let mut seed = 12345u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for dtype in [
            DType::Q8_0,
            DType::Q5_0,
            DType::Q4_0,
            DType::Q4K,
            DType::Q5K,
            DType::Q6K,
        ] {
            let (per, bytes) = dtype.block().unwrap();
            let cols = 512;
            let mut row: Vec<u8> = (0..cols / per * bytes).map(|_| rnd() as u8).collect();
            // keep the f16 scales small and finite
            for b in 0..cols / per {
                let at = match dtype {
                    DType::Q6K => b * bytes + 208,
                    _ => b * bytes,
                };
                row[at..at + 2]
                    .copy_from_slice(&f32_to_f16(0.01 + (rnd() % 100) as f32 * 1e-4).to_le_bytes());
                if matches!(dtype, DType::Q4K | DType::Q5K) {
                    row[at + 2..at + 4].copy_from_slice(&f32_to_f16(0.005).to_le_bytes());
                }
            }
            let x: Vec<f32> = (0..cols)
                .map(|_| (rnd() % 2000) as f32 / 1000.0 - 1.0)
                .collect();
            let mut w = vec![0f32; cols];
            decode_row(dtype, &row, &mut w);
            let want = dot(&w, &x);
            let q = [quantize_q8(&x)];
            let mut got = [0f32];
            row_dot_q8(dtype, &row, cols, &q, &mut got);
            let tol = want.abs() * 0.02 + 0.05;
            assert!(
                (got[0] - want).abs() < tol,
                "{dtype:?}: {} vs {want}",
                got[0]
            );
            // the NEON kernels must agree with the portable ones almost exactly
            let mut portable = [0f32];
            row_dot_q8_portable(dtype, &row, cols, &q, &mut portable);
            assert!(
                (got[0] - portable[0]).abs() <= portable[0].abs() * 1e-5 + 1e-4,
                "{dtype:?} neon {} vs portable {}",
                got[0],
                portable[0]
            );
        }
    }
    #[test]
    fn row_dot_matches_decoded_dot() {
        let mut row = vec![0u8; 2 * 34];
        for blk in 0..2 {
            row[blk * 34..blk * 34 + 2].copy_from_slice(&f32_to_f16(0.25).to_le_bytes());
            for j in 0..32 {
                row[blk * 34 + 2 + j] = (j as i8 - 10) as u8;
            }
        }
        let xs: Vec<f32> = (0..128).map(|i| (i % 7) as f32 - 3.0).collect();
        let mut w = vec![0f32; 64];
        decode_row(DType::Q8_0, &row, &mut w);
        let mut out = [0f32; 2];
        row_dot_many(DType::Q8_0, &row, 64, &xs, 2, &mut out);
        assert!((out[0] - dot(&w, &xs[..64])).abs() < 1e-4);
        assert!((out[1] - dot(&w, &xs[64..])).abs() < 1e-4);
    }
}
