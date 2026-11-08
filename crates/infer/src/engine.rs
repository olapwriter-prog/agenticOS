//! Generation: prompt in, text out, with the grammar enforced, and a long shared prefix (agentd's
//! system prompt and tool list) processed once and reused from a snapshot.
use crate::grammar::Grammar;
use crate::model::{Arch, Model, Snapshot, State};
use crate::tokenizer::Tokenizer;
use std::time::Instant;

pub struct Engine {
    pub model: Model,
    pub tok: Tokenizer,
    state: State,
    /// The tokens `state` currently holds.
    held: Vec<u32>,
    /// A snapshot after a shared prefix, and that prefix.
    snap: Option<(Vec<u32>, Snapshot)>,
    seed: u64,
}

#[derive(Debug, Clone)]
pub struct Request {
    pub prompt: String,
    pub max_tokens: usize,
    pub temperature: f32,
    pub top_k: usize,
    pub grammar: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Timings {
    pub prompt_n: usize,
    /// prompt tokens reused from the cache or a snapshot
    pub prompt_cached: usize,
    pub prompt_ms: f64,
    pub predicted_n: usize,
    pub predicted_ms: f64,
}

/// One chat message.
pub struct Msg {
    pub role: String,
    pub content: String,
}

impl Engine {
    pub fn new(model: Model, tok: Tokenizer) -> Engine {
        let state = model.new_state();
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1)
            | 1;
        Engine {
            model,
            tok,
            state,
            held: vec![],
            snap: None,
            seed,
        }
    }

    /// ChatML, as Qwen models expect. With thinking off, Qwen3/3.5 get an empty think block so
    /// they answer at once.
    pub fn chat_prompt(&self, msgs: &[Msg], thinking: bool) -> String {
        let mut p = String::new();
        for m in msgs {
            p.push_str(&format!(
                "<|im_start|>{}\n{}<|im_end|>\n",
                m.role, m.content
            ));
        }
        p.push_str("<|im_start|>assistant\n");
        if !thinking && matches!(self.model.cfg.arch, Arch::Qwen3 | Arch::Qwen35) {
            p.push_str("<think>\n\n</think>\n\n");
        }
        p
    }

    fn rand(&mut self) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        (self.seed >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Bring the state to hold exactly `prompt`'s tokens, reusing what it can. Returns how many
    /// prompt tokens were reused, and the logits after the last one.
    fn prefill(&mut self, prompt: &[u32]) -> Result<(usize, Vec<f32>), String> {
        // where the last user turn starts: everything before it is the shared part to snapshot
        let im_start = self.tok.id("<|im_start|>");
        let boundary = im_start
            .and_then(|s| {
                (0..prompt.len()).rev().find(|&i| {
                    prompt[i] == s
                        && self.tok.decode(&prompt[i + 1..(i + 2).min(prompt.len())]) == "user"
                })
            })
            .unwrap_or(0);
        let starts_with = |a: &[u32], b: &[u32]| a.len() >= b.len() && a[..b.len()] == *b;
        let mut reused = 0;
        if !self.held.is_empty()
            && starts_with(prompt, &self.held)
            && self.held.len() < prompt.len()
        {
            reused = self.held.len();
        } else if let Some((pre, snap)) = &self.snap {
            if starts_with(prompt, pre) && pre.len() < prompt.len() {
                self.state.restore(snap);
                reused = pre.len();
            } else {
                self.state.reset();
            }
        } else {
            self.state.reset();
        }
        if reused == 0 {
            self.state.reset();
        }
        self.held.truncate(reused);
        const BATCH: usize = 64;
        let mut logits = Vec::new();
        let mut at = reused;
        while at < prompt.len() {
            // stop at the boundary once, to take the snapshot there
            let mut end = (at + BATCH).min(prompt.len());
            if boundary > at && boundary < end {
                end = boundary;
            }
            logits = self.model.forward(&mut self.state, &prompt[at..end])?;
            self.held.extend_from_slice(&prompt[at..end]);
            at = end;
            if at == boundary
                && boundary > 0
                && self
                    .snap
                    .as_ref()
                    .is_none_or(|(p, _)| p[..] != prompt[..boundary])
            {
                self.snap = Some((prompt[..boundary].to_vec(), self.state.snapshot()));
            }
        }
        if logits.is_empty() {
            return Err("empty prompt".into());
        }
        Ok((reused, logits))
    }

    pub fn generate(&mut self, req: &Request) -> Result<(String, Timings), String> {
        let grammar = match &req.grammar {
            Some(g) if !g.trim().is_empty() => Some(Grammar::parse(g)?),
            _ => None,
        };
        let mut matcher = grammar.as_ref().map(|g| g.matcher());
        let prompt = self.tok.encode(&req.prompt);
        let t0 = Instant::now();
        let (reused, mut logits) = self.prefill(&prompt)?;
        let mut timings = Timings {
            prompt_n: prompt.len(),
            prompt_cached: reused,
            prompt_ms: t0.elapsed().as_secs_f64() * 1000.0,
            ..Default::default()
        };
        let t1 = Instant::now();
        let mut out: Vec<u32> = Vec::new();
        for _ in 0..req.max_tokens {
            let Some(next) = self.sample(&logits, req, matcher.as_ref()) else {
                break;
            };
            if self.tok.eot.contains(&next) {
                break;
            }
            if let Some(m) = matcher.as_mut() {
                m.accept_bytes(&self.tok.token_bytes(next));
            }
            out.push(next);
            if matcher
                .as_ref()
                .is_some_and(|m| m.complete() && !m.can_continue())
            {
                break;
            }
            if self.state.pos + 1 >= self.model.cfg.ctx {
                break;
            }
            logits = self.model.forward(&mut self.state, &[next])?;
            self.held.push(next);
        }
        timings.predicted_n = out.len();
        timings.predicted_ms = t1.elapsed().as_secs_f64() * 1000.0;
        Ok((self.tok.decode(&out), timings))
    }

    /// Pick the next token: top-k by logit among what the grammar allows, then temperature.
    fn sample(
        &mut self,
        logits: &[f32],
        req: &Request,
        m: Option<&crate::grammar::Matcher>,
    ) -> Option<u32> {
        let mut order: Vec<u32> = (0..logits.len() as u32).collect();
        let k = req.top_k.max(1);
        let mut cands: Vec<(u32, f32)> = Vec::with_capacity(k);
        match m {
            None => {
                let nth = k.min(order.len() - 1);
                order.select_nth_unstable_by(nth, |a, b| {
                    logits[*b as usize].total_cmp(&logits[*a as usize])
                });
                order.truncate(k);
                cands = order.iter().map(|&t| (t, logits[t as usize])).collect();
            }
            Some(m) => {
                order.sort_unstable_by(|a, b| logits[*b as usize].total_cmp(&logits[*a as usize]));
                for &t in &order {
                    if self.tok.eot.contains(&t) {
                        if m.complete() {
                            cands.push((t, logits[t as usize]));
                        }
                    } else if !self.tok.is_special(t) && m.allows(&self.tok.token_bytes(t)) {
                        cands.push((t, logits[t as usize]));
                    }
                    if cands.len() >= k {
                        break;
                    }
                }
            }
        }
        if cands.is_empty() {
            return None;
        }
        cands.sort_by(|a, b| b.1.total_cmp(&a.1));
        if req.temperature <= 0.0 {
            return Some(cands[0].0);
        }
        let mx = cands[0].1;
        let w: Vec<f32> = cands
            .iter()
            .map(|(_, l)| ((l - mx) / req.temperature).exp())
            .collect();
        let total: f32 = w.iter().sum();
        let mut r = self.rand() * total;
        for (i, wi) in w.iter().enumerate() {
            r -= wi;
            if r <= 0.0 {
                return Some(cands[i].0);
            }
        }
        Some(cands[0].0)
    }
}
