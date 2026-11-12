//! Generation: prompt in, text out, with the grammar enforced, and a long shared prefix (agentd's
//! system prompt and tool list) processed once and reused from a snapshot. Snapshots live in a
//! budgeted cache (`cache::PrefixCache`), one per prefix and adapter, so switching adapters
//! (experts) does not throw the others away, and prefixes can be warmed before they are asked for.
//!
//! With a draft model (`Engine::set_draft`): speculative decoding. The small model proposes a few
//! tokens, the big one checks them all in one batch (a batch costs it little more than one token:
//! each weight is decoded once per batch). Accepted tokens are kept, the first wrong one is
//! replaced by the big model's own choice, and the text is exactly what the big model alone would
//! have produced (greedy: identical; sampling: the same distribution).
use crate::cache::PrefixCache;
use crate::grammar::{Grammar, Matcher};
use crate::model::{Arch, Mark, Model, Snapshot, State};
use crate::tokenizer::Tokenizer;
use std::collections::HashMap;
use std::time::Instant;

pub struct Engine {
    pub model: Model,
    pub tok: Tokenizer,
    state: State,
    /// The tokens `state` currently holds.
    held: Vec<u32>,
    /// States after shared prefixes, per adapter, within a memory budget.
    pub cache: PrefixCache<Snapshot>,
    /// The adapter in use ("" = none) and the others kept ready to swap in.
    adapter_tag: String,
    adapters: HashMap<String, crate::train::Adapter>,
    seed: u64,
    draft: Option<Draft>,
    /// drafted tokens per round
    pub draft_n: usize,
}

/// The draft model and where it is: its state holds exactly `held`, `logits` are for the next.
struct Draft {
    model: Model,
    state: State,
    held: Vec<u32>,
    logits: Vec<f32>,
}

impl Draft {
    /// Bring the draft to hold exactly `tokens`, from a `mark` taken when it held `mark_len`
    /// tokens (the rewind point: at most one round back).
    fn sync(&mut self, tokens: &[u32], mark: Option<(&Mark, usize)>) -> Result<(), String> {
        let common = self
            .held
            .iter()
            .zip(tokens)
            .take_while(|(a, b)| a == b)
            .count();
        let start = if common < self.held.len() || self.logits.is_empty() {
            // rewind: to the mark if there is one this side of the difference, else start over
            match mark {
                Some((m, len)) if len <= common => {
                    self.state.rewind(m);
                    len
                }
                _ => {
                    self.state.reset();
                    0
                }
            }
        } else {
            common
        };
        self.held.truncate(start);
        if start == tokens.len() {
            if self.logits.is_empty() {
                return Err("draft has no logits".into());
            }
            return Ok(());
        }
        const BATCH: usize = 64;
        let mut at = start;
        while at < tokens.len() {
            let end = (at + BATCH).min(tokens.len());
            if self.state.pos + (end - at) > self.model.cfg.ctx {
                return Err("draft context full".into());
            }
            self.logits = self.model.forward(&mut self.state, &tokens[at..end])?;
            self.held.extend_from_slice(&tokens[at..end]);
            at = end;
        }
        Ok(())
    }
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
    /// speculative decoding: tokens the draft proposed, and how many of them the model kept
    pub drafted: usize,
    pub accepted: usize,
}

/// Default memory for cached prompt states: enough for a system prompt on a 4B model.
pub const DEFAULT_CACHE_BYTES: usize = 512 << 20;

/// What warming a prefix did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Warmed {
    pub tokens: usize,
    pub bytes: usize,
    pub ms: f64,
    /// tokens that were already there (held or cached)
    pub reused: usize,
    /// it was cached already: nothing done
    pub already: bool,
    /// false when it did not fit the budget
    pub kept: bool,
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
            cache: PrefixCache::new(DEFAULT_CACHE_BYTES),
            adapter_tag: String::new(),
            adapters: HashMap::new(),
            seed,
            draft: None,
            draft_n: 5,
        }
    }

    /// Use a smaller model of the same family to draft tokens. It must share the vocabulary:
    /// checked on the token tables, and on encoding a sentence.
    pub fn set_draft(&mut self, draft: Model, draft_tok: &Tokenizer) -> Result<(), String> {
        if draft.cfg.n_vocab != self.model.cfg.n_vocab {
            return Err(format!(
                "draft vocabulary has {} tokens, the model's {}",
                draft.cfg.n_vocab, self.model.cfg.n_vocab
            ));
        }
        let probe = "Call Mum at 9pm; text Ravi \"I'm late\" {\"tool\":\"call\"} 12345 émoji 😀";
        if draft_tok.encode(probe) != self.tok.encode(probe) {
            return Err("the draft model tokenizes differently from the model".into());
        }
        let state = draft.new_state();
        self.draft = Some(Draft {
            model: draft,
            state,
            held: vec![],
            logits: vec![],
        });
        Ok(())
    }

    /// Is a draft model loaded?
    pub fn has_draft(&self) -> bool {
        self.draft.is_some()
    }

    /// Load or remove a LoRA adapter (unnamed: replaces whatever is in use). States cached
    /// with a previous unnamed adapter are dropped; those of the base model are kept.
    pub fn set_adapter(&mut self, ad: Option<crate::train::Adapter>) {
        let tag = if ad.is_some() { "adapter" } else { "" };
        if self.adapter_tag == "adapter" || tag == "adapter" {
            self.cache.drop_adapter("adapter");
        }
        self.model.adapter = ad;
        self.adapter_tag = tag.to_string();
        self.held.clear();
        self.state.reset();
    }

    /// Keep a named adapter (an expert) ready to swap in by `use_adapter`. Replacing one drops
    /// the states cached with the old version.
    pub fn add_adapter(&mut self, name: &str, ad: crate::train::Adapter) {
        self.cache.drop_adapter(name);
        if self.adapter_tag == name {
            self.model.adapter = Some(ad);
            self.held.clear();
            self.state.reset();
        } else {
            self.adapters.insert(name.to_string(), ad);
        }
    }

    /// The names of the adapters this engine can switch to ("" is always the base model).
    pub fn adapter_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.adapters.keys().cloned().collect();
        if !self.adapter_tag.is_empty() {
            v.push(self.adapter_tag.clone());
        }
        v.sort();
        v
    }

    pub fn adapter(&self) -> &str {
        &self.adapter_tag
    }

    /// Switch to a named adapter ("" = the base model) for the next requests. Cached states of
    /// every adapter stay; only the live state starts over. Unknown names are an error.
    pub fn use_adapter(&mut self, name: &str) -> Result<(), String> {
        if name == self.adapter_tag {
            return Ok(());
        }
        if !name.is_empty() && !self.adapters.contains_key(name) {
            return Err(format!("no adapter named {name}"));
        }
        if let Some(cur) = self.model.adapter.take() {
            self.adapters.insert(self.adapter_tag.clone(), cur);
        }
        self.model.adapter = if name.is_empty() {
            None
        } else {
            self.adapters.remove(name)
        };
        self.adapter_tag = name.to_string();
        self.held.clear();
        self.state.reset();
        Ok(())
    }

    /// Read a prompt's shared part ahead of time and keep the state, so the request that
    /// follows starts after it. The shared part is everything before the last user turn (the
    /// whole prompt when there is none).
    pub fn warm(&mut self, prompt: &str, label: &str) -> Result<Warmed, String> {
        let toks = self.tok.encode(prompt);
        let b = self.boundary(&toks);
        let pre: Vec<u32> = if b > 0 { toks[..b].to_vec() } else { toks };
        if pre.is_empty() {
            return Err("empty prompt".into());
        }
        if self.cache.contains(&self.adapter_tag, &pre) {
            return Ok(Warmed {
                tokens: pre.len(),
                already: true,
                ..Default::default()
            });
        }
        let t0 = Instant::now();
        let mut reused = 0;
        if !self.held.is_empty() && pre.starts_with(&self.held) {
            reused = self.held.len();
        } else if let Some((id, n)) = self.cache.best(&self.adapter_tag, &pre) {
            if let Some(s) = self.cache.hit(id) {
                self.state.restore(s);
                reused = n;
            }
        }
        if reused == 0 {
            self.state.reset();
        }
        self.held.truncate(reused);
        const BATCH: usize = 64;
        let mut at = reused;
        while at < pre.len() {
            let end = (at + BATCH).min(pre.len());
            if self.state.pos + (end - at) > self.model.cfg.ctx {
                return Err("context full".into());
            }
            self.model.forward(&mut self.state, &pre[at..end])?;
            self.held.extend_from_slice(&pre[at..end]);
            at = end;
        }
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        let snap = self.state.snapshot();
        let bytes = snap.bytes();
        let tag = self.adapter_tag.clone();
        let kept = self
            .cache
            .insert(&tag, pre.clone(), label, snap, ms, true)
            .is_some();
        Ok(Warmed {
            tokens: pre.len(),
            bytes,
            ms,
            reused,
            already: false,
            kept,
        })
    }

    /// Where the last user turn starts: everything before it is the shared part.
    fn boundary(&self, prompt: &[u32]) -> usize {
        let im_start = self.tok.id("<|im_start|>");
        im_start
            .and_then(|s| {
                (0..prompt.len()).rev().find(|&i| {
                    prompt[i] == s
                        && self.tok.decode(&prompt[i + 1..(i + 2).min(prompt.len())]) == "user"
                })
            })
            .unwrap_or(0)
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
        let t0 = Instant::now();
        self.cache.tick();
        // where the last user turn starts: everything before it is the shared part to keep
        let boundary = self.boundary(prompt);
        let starts_with = |a: &[u32], b: &[u32]| a.len() >= b.len() && a[..b.len()] == *b;
        let mut reused = 0;
        if !self.held.is_empty()
            && starts_with(prompt, &self.held)
            && self.held.len() < prompt.len()
        {
            reused = self.held.len();
        }
        // a cached state that covers more than what is held wins
        if let Some((id, n)) = self.cache.best(&self.adapter_tag, prompt) {
            if n > reused {
                if let Some(s) = self.cache.hit(id) {
                    self.state.restore(s);
                    reused = n;
                }
            } else {
                // the live state already holds it (it was just warmed or used): still a use of
                // that entry, so it stays valued and its warm-up counts as used
                let _ = self.cache.hit(id);
            }
        } else if boundary > 0 && reused < boundary {
            self.cache.miss();
        }
        if reused == 0 {
            self.state.reset();
        }
        self.held.truncate(reused);
        const BATCH: usize = 64;
        let mut logits = Vec::new();
        let mut at = reused;
        let from = reused;
        while at < prompt.len() {
            // stop at the boundary once, to keep the state there
            let mut end = (at + BATCH).min(prompt.len());
            if boundary > at && boundary < end {
                end = boundary;
            }
            logits = self.model.forward(&mut self.state, &prompt[at..end])?;
            self.held.extend_from_slice(&prompt[at..end]);
            at = end;
            if at == boundary
                && boundary > 0
                && !self.cache.contains(&self.adapter_tag, &prompt[..boundary])
            {
                // what reading the whole prefix costs, from what this part took
                let spent = t0.elapsed().as_secs_f64() * 1000.0;
                let build_ms = spent * boundary as f64 / (boundary - from).max(1) as f64;
                let label = if self.adapter_tag.is_empty() {
                    "system".to_string()
                } else {
                    self.adapter_tag.clone()
                };
                let tag = self.adapter_tag.clone();
                let snap = self.state.snapshot();
                self.cache.insert(
                    &tag,
                    prompt[..boundary].to_vec(),
                    &label,
                    snap,
                    build_ms,
                    false,
                );
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
        if self.draft.is_some() {
            let r = self.generate_speculative(
                req,
                &prompt,
                logits,
                matcher.as_mut(),
                &mut out,
                &mut timings,
            );
            timings.predicted_n = out.len();
            timings.predicted_ms = t1.elapsed().as_secs_f64() * 1000.0;
            return r.map(|()| (self.tok.decode(&out), timings));
        }
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

    /// Speculative rounds until the answer ends: the draft proposes, the model checks the batch.
    fn generate_speculative(
        &mut self,
        req: &Request,
        prompt: &[u32],
        mut logits: Vec<f32>,
        mut matcher: Option<&mut Matcher>,
        out: &mut Vec<u32>,
        timings: &mut Timings,
    ) -> Result<(), String> {
        let ctx = self.model.cfg.ctx;
        let recurrent = (0..self.model.cfg.n_layer).any(|l| self.model.cfg.is_recurrent(l));
        let mut draft = self.draft.take().ok_or("no draft")?;
        let r = (|| -> Result<(), String> {
            let mut draft_mark: Option<(Mark, usize)> = None;
            while out.len() < req.max_tokens {
                // where the draft and the model are: the same tokens
                let held_len = self.held.len();
                draft.sync(&self.held, draft_mark.as_ref().map(|(m, l)| (m, *l)))?;
                draft_mark = Some((draft.state.mark(), held_len));
                let model_mark = recurrent.then(|| self.state.mark());
                let pos_before = self.state.pos;
                // 1. the draft proposes up to draft_n tokens, each allowed by the grammar
                let n = self
                    .draft_n
                    .min(req.max_tokens - out.len())
                    .min(ctx.saturating_sub(pos_before + 2));
                let mut m_draft = matcher.as_deref().cloned();
                let mut drafted: Vec<u32> = Vec::with_capacity(n);
                let mut q_dists: Vec<Vec<(u32, f32)>> = Vec::with_capacity(n);
                for _ in 0..n {
                    let q = self.dist(&draft.logits, req, m_draft.as_ref());
                    if q.is_empty() {
                        break;
                    }
                    let d = self.pick(&q);
                    q_dists.push(q);
                    drafted.push(d);
                    if self.tok.eot.contains(&d) {
                        break;
                    }
                    if let Some(m) = m_draft.as_mut() {
                        m.accept_bytes(&self.tok.token_bytes(d));
                        if m.complete() && !m.can_continue() {
                            break;
                        }
                    }
                    draft.logits = draft.model.forward(&mut draft.state, &[d])?;
                    draft.held.push(d);
                }
                timings.drafted += drafted.len();
                // 2. the model checks them all in one batch: logits after each drafted token
                let all = if drafted.is_empty() {
                    vec![]
                } else {
                    self.model.forward_all(&mut self.state, &drafted)?
                };
                let nv = self.model.cfg.n_vocab;
                // 3. accept while the model agrees; the first disagreement is replaced
                let mut accepted = 0;
                let mut next: Option<u32> = None;
                let mut finished = false;
                let mut m_check = matcher.as_deref().cloned();
                let mut cur = logits.clone();
                for (i, &d) in drafted.iter().enumerate() {
                    let p = self.dist(&cur, req, m_check.as_ref());
                    let q = &q_dists[i];
                    let pd = p.iter().find(|(t, _)| *t == d).map_or(0.0, |(_, x)| *x);
                    let qd = q.iter().find(|(t, _)| *t == d).map_or(1.0, |(_, x)| *x);
                    if pd > 0.0 && (pd >= qd || self.rand() < pd / qd) {
                        accepted += 1;
                        out.push(d);
                        if self.tok.eot.contains(&d) {
                            out.pop();
                            finished = true;
                            break;
                        }
                        if let Some(m) = m_check.as_mut() {
                            m.accept_bytes(&self.tok.token_bytes(d));
                            if m.complete() && !m.can_continue() {
                                finished = true;
                                break;
                            }
                        }
                        cur = all[i * nv..(i + 1) * nv].to_vec();
                    } else {
                        // the residual p − q, as speculative sampling prescribes (greedy: p's top)
                        let mut res: Vec<(u32, f32)> = p
                            .iter()
                            .map(|(t, x)| {
                                (
                                    *t,
                                    (x - q.iter().find(|(u, _)| u == t).map_or(0.0, |(_, y)| *y))
                                        .max(0.0),
                                )
                            })
                            .collect();
                        let total: f32 = res.iter().map(|(_, x)| x).sum();
                        if total <= 0.0 {
                            res = p.clone();
                        }
                        next = Some(self.pick(&res));
                        break;
                    }
                }
                timings.accepted += accepted;
                if !finished && next.is_none() {
                    // every drafted token was right: one more from the model's last logits
                    let p = self.dist(&cur, req, m_check.as_ref());
                    if p.is_empty() {
                        finished = true;
                    } else {
                        next = Some(self.pick(&p));
                    }
                }
                // 4. the model's state: keep the accepted tokens, drop the rest
                self.held.extend_from_slice(&drafted[..accepted]);
                if accepted < drafted.len() {
                    match &model_mark {
                        Some(m) => {
                            // recurrent layers cannot step back: redo the accepted tokens from
                            // the mark, together with the replacement
                            self.state.rewind(m);
                            let mut redo = drafted[..accepted].to_vec();
                            if let Some(e) = next {
                                redo.push(e);
                            }
                            if !redo.is_empty() {
                                logits = self.model.forward(&mut self.state, &redo)?;
                            }
                            if let Some(e) = next {
                                self.held.push(e);
                            }
                        }
                        None => {
                            self.state.pos = pos_before + accepted;
                            if let Some(e) = next {
                                logits = self.model.forward(&mut self.state, &[e])?;
                                self.held.push(e);
                            }
                        }
                    }
                } else if let Some(e) = next {
                    if self.state.pos + 1 >= ctx {
                        finished = true;
                    } else {
                        logits = self.model.forward(&mut self.state, &[e])?;
                        self.held.push(e);
                    }
                }
                if let Some(e) = next {
                    if self.tok.eot.contains(&e) {
                        finished = true;
                    } else {
                        out.push(e);
                        if let Some(m) = matcher.as_deref_mut() {
                            // the matcher follows the tokens kept: the accepted, then e
                            for &t in &drafted[..accepted] {
                                m.accept_bytes(&self.tok.token_bytes(t));
                            }
                            m.accept_bytes(&self.tok.token_bytes(e));
                            if m.complete() && !m.can_continue() {
                                finished = true;
                            }
                        }
                    }
                } else if let Some(m) = matcher.as_deref_mut() {
                    for &t in &drafted[..accepted] {
                        m.accept_bytes(&self.tok.token_bytes(t));
                    }
                }
                if finished || self.state.pos + 1 >= ctx {
                    break;
                }
            }
            Ok(())
        })();
        let _ = prompt;
        self.draft = Some(draft);
        r
    }

    /// The next-token distribution `sample` draws from: top-k among what the grammar allows,
    /// softmax at the temperature (one-hot at the top when greedy). Sorted, best first.
    fn dist(&self, logits: &[f32], req: &Request, m: Option<&Matcher>) -> Vec<(u32, f32)> {
        let mut cands = self.candidates(logits, req, m);
        if cands.is_empty() {
            return cands;
        }
        cands.sort_by(|a, b| b.1.total_cmp(&a.1));
        if req.temperature <= 0.0 {
            return vec![(cands[0].0, 1.0)];
        }
        let mx = cands[0].1;
        let mut total = 0.0;
        for c in cands.iter_mut() {
            c.1 = ((c.1 - mx) / req.temperature).exp();
            total += c.1;
        }
        cands.iter_mut().for_each(|c| c.1 /= total);
        cands
    }

    /// Draw from a distribution (tokens with weights).
    fn pick(&mut self, d: &[(u32, f32)]) -> u32 {
        let total: f32 = d.iter().map(|(_, w)| w).sum();
        let mut r = self.rand() * total;
        for (t, w) in d {
            r -= w;
            if r <= 0.0 {
                return *t;
            }
        }
        d[0].0
    }

    /// The top-k tokens the grammar allows, with their logits.
    fn candidates(&self, logits: &[f32], req: &Request, m: Option<&Matcher>) -> Vec<(u32, f32)> {
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
        cands
    }

    /// Pick the next token: top-k by logit among what the grammar allows, then temperature.
    fn sample(
        &mut self,
        logits: &[f32],
        req: &Request,
        m: Option<&crate::grammar::Matcher>,
    ) -> Option<u32> {
        let mut cands = self.candidates(logits, req, m);
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
