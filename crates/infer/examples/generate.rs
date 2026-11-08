//! Greedy generation: cargo run --release -p agentos-infer --example generate -- model.gguf "prompt" [n]
use agentos_infer::{gguf::Gguf, model::Model, tokenizer::Tokenizer};
use std::time::Instant;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let n: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(24);
    let t0 = Instant::now();
    let g = Gguf::open(std::path::Path::new(&a[1])).unwrap();
    let tok = Tokenizer::from_gguf(&g).unwrap();
    let m = Model::load(g, 4096).unwrap();
    eprintln!("loaded {:?} in {:?}", m.cfg.arch, t0.elapsed());
    let ids = tok.encode(&a[2]);
    let mut st = m.new_state();
    let t1 = Instant::now();
    let mut logits = m.forward(&mut st, &ids).unwrap();
    let pre = t1.elapsed();
    let t2 = Instant::now();
    let mut out = Vec::new();
    for _ in 0..n {
        let next = logits
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0 as u32;
        if tok.eot.contains(&next) {
            break;
        }
        out.push(next);
        logits = m.forward(&mut st, &[next]).unwrap();
    }
    let gen = t2.elapsed();
    let p = &agentos_infer::model::PROFILE;
    eprintln!(
        "time in matmul (all, incl. output): {:.2}s",
        p[0].load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e9
    );
    println!("{}", tok.decode(&out));
    eprintln!(
        "prompt {} tokens in {:?} ({:.1} tok/s); generated {} in {:?} ({:.1} tok/s)",
        ids.len(),
        pre,
        ids.len() as f64 / pre.as_secs_f64(),
        out.len(),
        gen,
        out.len() as f64 / gen.as_secs_f64()
    );
}
