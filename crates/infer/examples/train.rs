//! LoRA trainer checks and runs.
//!   cargo run --release -p agentos-infer --example train -- gradcheck model.gguf
//!   cargo run --release -p agentos-infer --example train -- fit model.gguf [steps]
use agentos_infer::{gguf::Gguf, model::Model, tokenizer::Tokenizer, train::*};
use std::time::Instant;

fn chat(prompt: &str) -> String {
    format!("<|im_start|>system\nAnswer with the tool call only.<|im_end|>\n<|im_start|>user\n{prompt}<|im_end|>\n<|im_start|>assistant\n")
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let g = Gguf::open(std::path::Path::new(&a[2])).unwrap();
    let tok = Tokenizer::from_gguf(&g).unwrap();
    let mut m = Model::load(g, 4096).unwrap();
    match a[1].as_str() {
        "gradcheck" => gradcheck(&m, &tok),
        "fit" => {
            let held = ["ring up dad", "can you call ravi", "phone mum please"];
            let before: Vec<String> = held.iter().map(|p| gen(&m, &tok, p)).collect();
            let ad = fit(&m, &tok, a.get(3).and_then(|s| s.parse().ok()).unwrap_or(30));
            m.adapter = Some(ad);
            for (p, b) in held.iter().zip(before) {
                println!("held-out {p:?}: base {b:?} -> adapter {:?}", gen(&m, &tok, p));
            }
        }
        "bench" => {
            // one long example (the phone's system prompt) through forward and backward
            let sys = std::fs::read_to_string(&a[3]).unwrap();
            let prompt = format!("<|im_start|>system\n{sys}<|im_end|>\n<|im_start|>user\nring ravi<|im_end|>\n<|im_start|>assistant\n");
            let (tokens, train) = example(&tok, &prompt, r#"{"tool":"call","args":{"to":"ravi"}}"#);
            let ad = Adapter::new(&m, 8, 16.0, 1).unwrap();
            let tr = Trainer::new(&m);
            for _ in 0..2 {
                let t0 = Instant::now();
                let (l, _) = tr.loss(&ad, &tokens, &train, false);
                let t1 = Instant::now();
                let (_, g) = tr.loss(&ad, &tokens, &train, true);
                assert!(g.is_some());
                println!("{} tokens: forward {:?}, forward+backward {:?} (loss {l:.3})", tokens.len(), t1 - t0, t1.elapsed());
                let p = &agentos_infer::model::PROFILE;
                println!("  of which quantized matmul {:.2}s", p[0].swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / 1e9);
            }
        }
        x => panic!("unknown {x}"),
    }
}

fn gradcheck(m: &Model, tok: &Tokenizer) {
    // LONG=1: a prompt past the DeltaNet checkpoint interval (64 tokens), so the backward pass
    // replays the recurrence across chunks
    let prompt = if std::env::var("LONG").is_ok() {
        chat(&"please call mum on her mobile when you get the chance, thank you. ".repeat(8))
    } else {
        chat("call mum")
    };
    let (tokens, train) = example(tok, &prompt, "dial Mum");
    println!("{} tokens", tokens.len());
    let mut tr = Trainer::new(m);
    tr.exact = true;
    // forward agrees with the inference engine (adapter zero)
    let ad0 = Adapter::new(m, 4, 8.0, 7).unwrap();
    let (l0, _) = tr.loss(&ad0, &tokens, &train, false);
    let mut st = m.new_state();
    let mut ce = 0.0;
    let mut n = 0;
    for t in 0..tokens.len() - 1 {
        let logits = m.forward(&mut st, &tokens[t..t + 1]).unwrap();
        if train[t + 1] {
            let mx = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let s: f32 = logits.iter().map(|v| (v - mx).exp()).sum();
            ce += -(logits[tokens[t + 1] as usize] - mx - s.ln());
            n += 1;
        }
    }
    println!("loss trainer {l0:.4}  engine {:.4}", ce / n as f32);
    // random B so every gradient is live
    let mut ad = ad0.clone();
    let mut s = 99u64;
    for p in ad.pairs.values_mut() {
        for b in p.b.iter_mut() {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *b = ((s >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.02;
        }
    }
    let t0 = Instant::now();
    let (_, gr) = tr.loss(&ad, &tokens, &train, true);
    println!("loss+grads in {:?}", t0.elapsed());
    let gr = gr.unwrap();
    let mut worst = 0f32;
    // the first and last attention layers, and (Qwen3.5) the first DeltaNet layer's output
    let attn: Vec<usize> = ad.pairs.keys().filter(|k| k.1 == Target::Q).map(|k| k.0).collect();
    let (first, last) = (attn[0], *attn.last().unwrap());
    let mut keys = vec![(first, Target::Q), (first, Target::V), (last, Target::K), (m.cfg.n_layer - 1, Target::O)];
    if let Some(r) = (0..m.cfg.n_layer).find(|&i| m.cfg.is_recurrent(i)) {
        keys.push((r, Target::O));
    }
    for key in keys {
        for (which, idx) in [(0usize, 3usize), (0, 101), (1, 5), (1, 77)] {
            let h: f32 = std::env::var("H").ok().and_then(|s| s.parse().ok()).unwrap_or(1e-2);
            let mut plus = ad.clone();
            let mut minus = ad.clone();
            let get = |a: &mut Adapter| -> *mut f32 {
                let p = a.pairs.get_mut(&key).unwrap();
                if which == 0 { &mut p.a[idx] } else { &mut p.b[idx] }
            };
            unsafe {
                *get(&mut plus) += h;
                *get(&mut minus) -= h;
            }
            let num = (tr.loss(&plus, &tokens, &train, false).0 - tr.loss(&minus, &tokens, &train, false).0) / (2.0 * h);
            let (ga, gb) = &gr.pairs[&key];
            let ana = if which == 0 { ga[idx] } else { gb[idx] };
            let rel = (num - ana).abs() / (num.abs() + ana.abs()).max(1e-4);
            worst = worst.max(rel);
            println!("{key:?} {} [{idx}]  analytic {ana:+.6}  numeric {num:+.6}  rel {rel:.3}", if which == 0 { "A" } else { "B" });
        }
    }
    println!("worst relative error {worst:.4}");
}

fn gen(m: &Model, tok: &Tokenizer, prompt: &str) -> String {
    let mut st = m.new_state();
    let mut logits = m.forward(&mut st, &tok.encode(&chat(prompt))).unwrap();
    let mut out = Vec::new();
    for _ in 0..12 {
        let next = logits.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0 as u32;
        if tok.eot.contains(&next) {
            break;
        }
        out.push(next);
        logits = m.forward(&mut st, &[next]).unwrap();
    }
    tok.decode(&out)
}

fn fit(m: &Model, tok: &Tokenizer, steps: usize) -> Adapter {
    let data = [
        ("ring ravi", "dial Ravi"),
        ("give mum a call", "dial Mum"),
        ("phone the office", "dial Office"),
        ("call dad", "dial Dad"),
    ];
    let ex: Vec<_> = data.iter().map(|(p, a)| example(tok, &chat(p), a)).collect();
    let tr = Trainer::new(m);
    let mut ad = Adapter::new(m, 8, 16.0, 1).unwrap();
    println!("adapter params {}", ad.params());
    let mut opt = AdamW::new(1e-3);
    for step in 0..steps {
        let t0 = Instant::now();
        let mut total = 0.0;
        let mut acc: Option<Grads> = None;
        for (tokens, train) in &ex {
            let (l, g) = tr.loss(&ad, tokens, train, true);
            total += l;
            match acc.as_mut() {
                Some(a) => a.add(&g.unwrap()),
                None => acc = g,
            }
        }
        opt.step(&mut ad, &acc.unwrap());
        println!("step {step:3}  loss {:.4}  ({:?})", total / ex.len() as f32, t0.elapsed());
    }
    let path = std::env::temp_dir().join("agentos-fit.lora");
    ad.save(&path).unwrap();
    let back = Adapter::load(&path).unwrap();
    back.fits(m).unwrap();
    println!("saved and reloaded {} ({} params)", path.display(), back.params());
    back
}
