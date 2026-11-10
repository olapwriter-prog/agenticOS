//! A training job: the human-confirmed requests agentd exported (sft.jsonl) in, a LoRA adapter
//! out, kept only if it answers held-out requests better than the model without it.
//!
//! The job checkpoints after every step and picks up from the checkpoint when run again, so
//! agentd can stop it whenever the phone is unplugged, warm or needed, and continue the next night.
use crate::engine::{Engine, Msg, Request};
use crate::train::{example, Adapter, AdamW, Trainer};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Instant;

pub struct Job {
    pub data: PathBuf,
    pub out: PathBuf,
    pub grammar: Option<String>,
    pub steps: usize,
    pub batch: usize,
    pub rank: usize,
    pub alpha: f32,
    pub lr: f32,
    /// every nth example is held out for judging
    pub holdout: usize,
    /// stop (with a checkpoint) after this long; run again to continue
    pub max_secs: Option<u64>,
    /// bytes of saved activations per example before layers are recomputed instead
    pub memory: usize,
}

impl Default for Job {
    fn default() -> Self {
        Job {
            data: PathBuf::new(),
            out: PathBuf::new(),
            grammar: None,
            steps: 60,
            batch: 4,
            rank: 8,
            alpha: 16.0,
            lr: 1e-3,
            holdout: 5,
            max_secs: None,
            memory: 256 << 20,
        }
    }
}

/// One chat: (system, user, assistant).
type Chat = (String, String, String);

pub fn read_chats(text: &str) -> Vec<Chat> {
    let mut seen = std::collections::BTreeSet::new();
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| {
            let m = v["messages"].as_array()?;
            let get = |role: &str| {
                m.iter()
                    .find(|x| x["role"] == role)
                    .and_then(|x| x["content"].as_str())
                    .map(str::to_string)
            };
            Some((get("system").unwrap_or_default(), get("user")?, get("assistant")?))
        })
        // the same request confirmed twice counts once
        .filter(|c| seen.insert(c.1.trim().to_lowercase()))
        .collect()
}

/// 1 for the exact action, 0.5 for the right tool with other arguments (as agentos-learn scores).
pub fn score(got: &str, want: &str) -> f32 {
    let parse = |s: &str| serde_json::from_str::<Value>(s.trim()).ok();
    match (parse(got), parse(want)) {
        (Some(g), Some(w)) if g == w => 1.0,
        (Some(g), Some(w)) if g["tool"] == w["tool"] => 0.5,
        _ if got.trim() == want.trim() => 1.0,
        _ => 0.0,
    }
}

/// The answer as the grammar would let the model write it: the same action, keys in the order
/// the grammar accepts (the export's JSON has its keys sorted, so "args" comes before "tool").
pub fn canonical(answer: &str, grammar: &Option<String>) -> String {
    let Some(g) = grammar.as_ref().and_then(|g| crate::grammar::Grammar::parse(g).ok()) else {
        return answer.to_string();
    };
    let Ok(v) = serde_json::from_str::<Value>(answer.trim()) else {
        return answer.to_string();
    };
    let (Some(tool), Some(args)) = (v["tool"].as_str(), v["args"].as_object()) else {
        return answer.to_string();
    };
    let keys: Vec<&String> = args.keys().collect();
    let render = |order: &[&String]| {
        let parts: Vec<String> = order.iter().map(|k| format!("{}:{}", json!(k), args[k.as_str()])).collect();
        format!("{{\"tool\":{},\"args\":{{{}}}}}", json!(tool), parts.join(","))
    };
    let accepts = |s: &str| {
        let mut m = g.matcher();
        m.accept_bytes(s.as_bytes()) && m.complete()
    };
    // a handful of arguments at most: try the orders
    let mut order = keys.clone();
    let mut c = vec![0usize; order.len()];
    let first = render(&order);
    if accepts(&first) {
        return first;
    }
    let mut i = 0;
    while i < order.len() {
        if c[i] < i {
            order.swap(if i % 2 == 0 { 0 } else { c[i] }, i);
            let s = render(&order);
            if accepts(&s) {
                return s;
            }
            c[i] += 1;
            i = 0;
        } else {
            c[i] = 0;
            i += 1;
        }
    }
    first
}

fn prompt_of(e: &Engine, c: &Chat) -> String {
    let mut msgs = Vec::new();
    if !c.0.is_empty() {
        msgs.push(Msg { role: "system".into(), content: c.0.clone() });
    }
    msgs.push(Msg { role: "user".into(), content: c.1.clone() });
    e.chat_prompt(&msgs, false)
}

pub fn judge(e: &mut Engine, cases: &[Chat], grammar: &Option<String>) -> Result<f32, String> {
    if cases.is_empty() {
        return Ok(0.0);
    }
    let mut total = 0.0;
    for c in cases {
        let req = Request {
            prompt: prompt_of(e, c),
            max_tokens: 128,
            temperature: 0.0,
            top_k: 1,
            grammar: grammar.clone(),
        };
        total += score(&e.generate(&req)?.0, &c.2);
    }
    Ok(total / cases.len() as f32)
}

fn ckpt(out: &std::path::Path) -> (PathBuf, PathBuf) {
    (out.with_extension("ckpt"), out.with_extension("ckpt.json"))
}

/// Run (or continue) the job. Returns a JSON summary; `log` gets progress lines.
pub fn run(e: &mut Engine, job: &Job, log: &mut dyn FnMut(&str)) -> Result<Value, String> {
    let t0 = Instant::now();
    let text = std::fs::read_to_string(&job.data).map_err(|err| format!("{}: {err}", job.data.display()))?;
    let chats = read_chats(&text);
    let n = job.holdout.max(2);
    let (mut train, mut held) = (Vec::new(), Vec::new());
    for (i, c) in chats.into_iter().enumerate() {
        if i % n == n - 1 { held.push(c) } else { train.push(c) }
    }
    if train.is_empty() || held.is_empty() {
        return Err(format!(
            "need more confirmed requests to train and judge ({} to train, {} held out)",
            train.len(),
            held.len()
        ));
    }
    for c in train.iter_mut().chain(held.iter_mut()) {
        c.2 = canonical(&c.2, &job.grammar);
    }
    let examples: Vec<_> = train.iter().map(|c| example(&e.tok, &prompt_of(e, c), &c.2)).collect();

    // continue from a checkpoint, or start: score the model without an adapter first
    let (ck, ck_meta) = ckpt(&job.out);
    let meta: Option<Value> = std::fs::read_to_string(&ck_meta).ok().and_then(|s| serde_json::from_str(&s).ok());
    let resumed = meta
        .as_ref()
        .and_then(|m| Some((Adapter::load(&ck).ok()?, m["step"].as_u64()? as usize, m["base"].as_f64()? as f32)))
        .filter(|(ad, _, _)| ad.fits(&e.model).is_ok());
    let (mut ad, start, base) = match resumed {
        Some((ad, step, base)) => {
            log(&format!("continuing from step {step} (base score {base:.2})"));
            (ad, step, base)
        }
        None => {
            e.set_adapter(None);
            let base = judge(e, &held, &job.grammar)?;
            log(&format!("without an adapter: {:.0}% on {} held-out requests", base * 100.0, held.len()));
            (Adapter::new(&e.model, job.rank, job.alpha, 0x5eed)?, 0, base)
        }
    };
    e.set_adapter(None);
    let mut opt = AdamW::new(job.lr);
    let mut last_loss = f32::NAN;
    let mut step = start;
    let save = |ad: &Adapter, step: usize| -> Result<(), String> {
        ad.save(&ck)?;
        std::fs::write(&ck_meta, json!({"step": step, "base": base}).to_string()).map_err(|e| e.to_string())
    };
    while step < job.steps {
        if job.max_secs.is_some_and(|s| t0.elapsed().as_secs() >= s) {
            save(&ad, step)?;
            return Ok(json!({"done": false, "step": step, "steps": job.steps, "loss": last_loss}));
        }
        let mut tr = Trainer::new(&e.model);
        tr.budget = job.memory;
        let b = job.batch.max(1).min(examples.len());
        // the batch in one pass: the system prompt the examples share is computed once
        let batch: Vec<(Vec<u32>, Vec<bool>)> = (0..b).map(|k| examples[(step * b + k) % examples.len()].clone()).collect();
        let (losses, g) = tr.loss_batch(&ad, &batch, true);
        opt.step(&mut ad, &g.unwrap());
        last_loss = losses.iter().sum::<f32>() / b as f32;
        step += 1;
        log(&format!("step {step}/{} loss {last_loss:.4}", job.steps));
        // every step: a step takes minutes on a phone, the adapter a few hundred KB to write
        save(&ad, step)?;
    }
    e.set_adapter(Some(ad.clone()));
    let tuned = judge(e, &held, &job.grammar)?;
    e.set_adapter(None);
    let kept = tuned > base;
    if kept {
        ad.save(&job.out)?;
    }
    let _ = std::fs::remove_file(&ck);
    let _ = std::fs::remove_file(&ck_meta);
    log(&format!(
        "with the adapter: {:.0}% (was {:.0}%): {}",
        tuned * 100.0,
        base * 100.0,
        if kept { "kept" } else { "not better, discarded" }
    ));
    Ok(json!({
        "done": true, "base": base, "tuned": tuned, "kept": kept, "step": step,
        "loss": last_loss, "train": train.len(), "held_out": held.len(),
        "adapter": kept.then(|| job.out.display().to_string()),
        "secs": t0.elapsed().as_secs()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn answers_take_the_grammars_key_order() {
        let g = Some(r#"root ::= "{\"tool\":\"send_sms\",\"args\":{\"to\":" s ",\"text\":" s "}}"
s ::= "\"" [a-zA-Z ]* "\"""#.to_string());
        let a = r#"{"args":{"text":"hi there","to":"Ravi"},"tool":"send_sms"}"#;
        assert_eq!(canonical(a, &g), r#"{"tool":"send_sms","args":{"to":"Ravi","text":"hi there"}}"#);
        assert_eq!(score(&canonical(a, &g), a), 1.0);
        assert_eq!(score(r#"{"tool":"send_sms","args":{}}"#, a), 0.5);
        let chats = read_chats(&format!("{}\n{}\n", json!({"messages":[{"role":"user","content":"Hi"},{"role":"assistant","content":"x"}]}), json!({"messages":[{"role":"user","content":"hi "},{"role":"assistant","content":"y"}]})));
        assert_eq!(chats.len(), 1);
    }
}
