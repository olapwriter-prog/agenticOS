//! The learning lab: does the local model get better at *this* owner's requests?
//!
//! - A **random test suite**, drawn fresh each run from a seed: held-out requests the owner
//!   confirmed, plus variants of confirmed requests (reworded: politeness added, words dropped,
//!   a typo, different case). A model that only memorised its training data fails the variants.
//! - **Scores per domain** (phone, messages, money...), kept as a history, so a fine-tune can be
//!   compared with the run before it.
//! - **Experts**: a dense 8B model cannot be turned into a real mixture of experts on a phone
//!   (sparse upcycling needs a GPU cluster). What a phone can do is a *mixture of adapters*: one
//!   small LoRA adapter per domain, trained on that domain's confirmed requests, and a **router**
//!   that picks the adapter for each request. The router here is a naive Bayes classifier learned
//!   from the same confirmed requests; its accuracy is part of every run.
use crate::{Episode, Learner};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// The domain a tool belongs to: the unit an expert adapter is trained for.
pub fn domain_of(tool: &str) -> &'static str {
    let t = tool;
    if t.starts_with("android_inbox") || t.starts_with("android_messages") || t == "android_reply" {
        return "messages";
    }
    match t {
        "call" | "answer_call" | "hangup" | "list_calls" | "call_log" | "find_contact"
        | "add_contact" | "list_contacts" | "ussd" => "phone",
        "send_sms" | "read_messages" | "all_messages" | "messages_from" | "send_message"
        | "telegram_send" | "gmail_send" => "messages",
        "read_calendar" | "add_event" | "set_reminder" | "gcal_list" | "gcal_add" => "calendar",
        "camera_take" | "photo_list" | "photo_show" | "photo_edit" => "media",
        "documents" | "read_document" | "android_files" | "android_share" => "documents",
        "flashlight" | "wifi_scan" | "wifi_connect" | "airplane_mode" | "mobile_data"
        | "set_volume" | "ring_phone" | "phone_status" | "where_am_i" => "device",
        "weather" | "air_quality" | "time_in" | "places" | "directions" | "wiki" | "define"
        | "holidays" | "books" | "radio" | "podcasts" | "recipe" | "translate" => "info",
        "convert_currency" | "crypto" => "money",
        _ if t.starts_with("invoice") || t.starts_with("upi") || t.starts_with("pay") => "money",
        _ if t.starts_with("android_") => "android",
        _ => "other",
    }
}

/// The tool of an action JSON (`{"tool":..}`).
pub fn tool_of(action: &str) -> String {
    serde_json::from_str::<serde_json::Value>(action)
        .ok()
        .and_then(|v| v["tool"].as_str().map(String::from))
        .unwrap_or_default()
}

// ---- the router -----------------------------------------------------------------------------

/// Picks a domain for a request: multinomial naive Bayes over words, with add-one smoothing,
/// learned from the owner's confirmed requests.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Router {
    /// domain -> (word -> count)
    words: HashMap<String, HashMap<String, u32>>,
    /// domain -> number of requests
    docs: HashMap<String, u32>,
    vocab: HashMap<String, ()>,
}

impl Router {
    pub fn train(examples: &[(String, String)]) -> Router {
        let mut r = Router::default();
        for (prompt, action) in examples {
            let d = domain_of(&tool_of(action)).to_string();
            *r.docs.entry(d.clone()).or_default() += 1;
            let w = r.words.entry(d).or_default();
            for word in crate::words(prompt) {
                *w.entry(word.clone()).or_default() += 1;
                r.vocab.insert(word, ());
            }
        }
        r
    }

    pub fn domains(&self) -> usize {
        self.docs.len()
    }

    /// The most likely domain and how sure the router is (0..1). None before any training.
    pub fn route(&self, prompt: &str) -> Option<(String, f32)> {
        let total: u32 = self.docs.values().sum();
        if total == 0 {
            return None;
        }
        let v = self.vocab.len().max(1) as f64;
        let words = crate::words(prompt);
        let mut scores: Vec<(String, f64)> = self
            .docs
            .iter()
            .map(|(d, n)| {
                let counts = &self.words[d];
                let size: u32 = counts.values().sum();
                let mut s = (*n as f64 / total as f64).ln();
                for w in &words {
                    let c = *counts.get(w).unwrap_or(&0) as f64;
                    s += ((c + 1.0) / (size as f64 + v)).ln();
                }
                (d.clone(), s)
            })
            .collect();
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        // softmax of the top two, for a confidence
        let best = scores[0].1;
        let sum: f64 = scores.iter().map(|(_, s)| (s - best).exp()).sum();
        Some((scores[0].0.clone(), (1.0 / sum) as f32))
    }
}

// ---- the random test suite ------------------------------------------------------------------

/// A small seeded generator, so a run can be repeated exactly.
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407)
            | 1)
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Case {
    pub prompt: String,
    /// the action the owner confirmed, as JSON
    pub expect: String,
    pub domain: String,
    /// "held-out" (as the owner said it) or "variant" (reworded)
    pub kind: String,
}

/// Reword a request the way people do: politeness, a dropped word, a typo, case.
pub fn variant(prompt: &str, rng: &mut Rng) -> String {
    let mut words: Vec<String> = prompt.split_whitespace().map(String::from).collect();
    if words.is_empty() {
        return prompt.to_string();
    }
    match rng.below(5) {
        0 => {
            let pre = ["please", "can you", "could you", "hey,", "quickly"][rng.below(5)];
            words.insert(0, pre.into());
        }
        1 if words.len() > 3 => {
            // drop a small word
            let small: Vec<usize> = (0..words.len())
                .filter(|i| {
                    ["the", "a", "my", "to", "for", "please"]
                        .contains(&words[*i].to_lowercase().as_str())
                })
                .collect();
            if let Some(&i) = small.get(rng.below(small.len().max(1))) {
                words.remove(i);
            }
        }
        2 => {
            // a typo: swap two letters inside one longer word
            let long: Vec<usize> = (0..words.len())
                .filter(|i| words[*i].chars().count() > 4)
                .collect();
            if let Some(&i) = long.get(rng.below(long.len().max(1))) {
                let mut c: Vec<char> = words[i].chars().collect();
                let j = 1 + rng.below(c.len() - 3);
                c.swap(j, j + 1);
                words[i] = c.into_iter().collect();
            }
        }
        3 => words = words.iter().map(|w| w.to_uppercase()).collect(),
        _ => words.push(["please", "now", "thanks"][rng.below(3)].into()),
    }
    words.join(" ")
}

/// Draw a suite: up to `n` held-out cases and as many variants, in a random order.
pub fn random_suite(learner: &Learner, seed: u64, n: usize) -> Vec<Case> {
    let mut rng = Rng::new(seed);
    let mut pool = learner.confirmed();
    // shuffle (Fisher-Yates) and take n
    for i in (1..pool.len()).rev() {
        let j = rng.below(i + 1);
        pool.swap(i, j);
    }
    pool.truncate(n);
    let mut cases = Vec::new();
    for (p, a) in &pool {
        let domain = domain_of(&tool_of(a)).to_string();
        cases.push(Case {
            prompt: p.clone(),
            expect: a.clone(),
            domain: domain.clone(),
            kind: "held-out".into(),
        });
        let v = variant(p, &mut rng);
        if v != *p {
            cases.push(Case {
                prompt: v,
                expect: a.clone(),
                domain,
                kind: "variant".into(),
            });
        }
    }
    for i in (1..cases.len()).rev() {
        let j = rng.below(i + 1);
        cases.swap(i, j);
    }
    cases
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DomainScore {
    pub domain: String,
    pub n: usize,
    pub score: f32,
}

/// One run of the suite.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub at_ms: u64,
    /// what was tested: "phrase table", "small model", "small model + adapter 3"...
    pub label: String,
    pub seed: u64,
    pub n: usize,
    /// mean score, 0..1: 1 for the exact action, 0.5 for the right tool
    pub score: f32,
    pub held_out: f32,
    pub variants: f32,
    /// how often the router picked the right domain
    pub router: f32,
    pub by_domain: Vec<DomainScore>,
}

fn grade(got: &str, want: &str) -> f32 {
    let parse = |s: &str| serde_json::from_str::<serde_json::Value>(s.trim()).ok();
    match (parse(got), parse(want)) {
        (Some(g), Some(w)) if g == w => 1.0,
        (Some(g), Some(w)) if g["tool"] == w["tool"] => 0.5,
        _ => 0.0,
    }
}

/// Run a suite: `ask` gives the action JSON for a request ("" for none).
pub fn run_suite(
    cases: &[Case],
    router: &Router,
    ask: &mut dyn FnMut(&str) -> String,
    label: &str,
    seed: u64,
    at_ms: u64,
) -> Run {
    let mut by: HashMap<String, (usize, f32)> = HashMap::new();
    let (mut sum, mut held, mut held_n, mut var, mut var_n, mut routed) = (0.0, 0.0, 0, 0.0, 0, 0);
    for c in cases {
        let s = grade(&ask(&c.prompt), &c.expect);
        sum += s;
        if c.kind == "variant" {
            var += s;
            var_n += 1;
        } else {
            held += s;
            held_n += 1;
        }
        let e = by.entry(c.domain.clone()).or_default();
        e.0 += 1;
        e.1 += s;
        if router.route(&c.prompt).is_some_and(|(d, _)| d == c.domain) {
            routed += 1;
        }
    }
    let mean = |s: f32, n: usize| if n == 0 { 0.0 } else { s / n as f32 };
    let mut by_domain: Vec<DomainScore> = by
        .into_iter()
        .map(|(domain, (n, s))| DomainScore {
            domain,
            n,
            score: mean(s, n),
        })
        .collect();
    by_domain.sort_by(|a, b| b.n.cmp(&a.n).then(a.domain.cmp(&b.domain)));
    Run {
        at_ms,
        label: label.to_string(),
        seed,
        n: cases.len(),
        score: mean(sum, cases.len()),
        held_out: mean(held, held_n),
        variants: mean(var, var_n),
        router: mean(routed as f32, cases.len()),
        by_domain,
    }
}

// ---- experts --------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Expert {
    pub domain: String,
    /// confirmed requests in this domain
    pub examples: usize,
    /// enough to train its own adapter
    pub ready: bool,
}

/// One expert per domain with confirmed requests, most data first.
pub fn experts(learner: &Learner, min_examples: usize) -> Vec<Expert> {
    let mut count: HashMap<&'static str, usize> = HashMap::new();
    for e in &learner.episodes {
        if let Some(t) = Learner::target(e) {
            *count.entry(domain_of(&tool_of(t))).or_default() += 1;
        }
    }
    let mut v: Vec<Expert> = count
        .into_iter()
        .map(|(d, n)| Expert {
            domain: d.into(),
            examples: n,
            ready: n >= min_examples,
        })
        .collect();
    v.sort_by(|a, b| b.examples.cmp(&a.examples).then(a.domain.cmp(&b.domain)));
    v
}

/// The training chats for one domain's adapter.
pub fn expert_dataset(learner: &Learner, system: &str, domain: &str) -> Vec<serde_json::Value> {
    learner
        .episodes
        .iter()
        .filter_map(|e: &Episode| Learner::target(e).map(|t| (e, t)))
        .filter(|(_, t)| domain_of(&tool_of(t)) == domain)
        .map(|(e, t)| {
            serde_json::json!({"messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": e.prompt},
                {"role": "assistant", "content": t}
            ]})
        })
        .collect()
}

// ---- history --------------------------------------------------------------------------------

/// Every run, kept in `<data_dir>/lab.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct History {
    pub runs: Vec<Run>,
}

impl History {
    pub fn load(path: &Path) -> History {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_string_pretty(self).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }
    pub fn push(&mut self, r: Run) {
        self.runs.push(r);
        let extra = self.runs.len().saturating_sub(100);
        self.runs.drain(..extra);
    }
    /// The change from the run before, in points (score × 100).
    pub fn delta(&self) -> Option<f32> {
        let n = self.runs.len();
        (n >= 2).then(|| (self.runs[n - 1].score - self.runs[n - 2].score) * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CALL: &str = r#"{"tool":"call","args":{"to":"mum"}}"#;
    const SMS: &str = r#"{"tool":"send_sms","args":{"to":"ravi","text":"late"}}"#;
    const TORCH: &str = r#"{"tool":"flashlight","args":{"on":"on"}}"#;

    fn confirmed() -> Learner {
        let mut l = Learner::in_memory();
        let mut q = 0;
        for (p, a) in [
            ("call mum please", CALL),
            ("ring my mother", CALL),
            ("phone mum now", CALL),
            ("text ravi i am late", SMS),
            ("send ravi a message saying late", SMS),
            ("message ravi late", SMS),
            ("torch on", TORCH),
            ("switch the flashlight on", TORCH),
            ("turn on the torch", TORCH),
        ] {
            let id = l.begin(q, p, a, "model");
            q += 1;
            l.waiting(id, q);
            l.approved(q, q + 1);
        }
        l
    }

    #[test]
    fn domains_group_tools() {
        assert_eq!(domain_of("call"), "phone");
        assert_eq!(domain_of("send_sms"), "messages");
        assert_eq!(domain_of("android_reply"), "messages");
        assert_eq!(domain_of("invoice_create"), "money");
        assert_eq!(domain_of("android_show"), "android");
        assert_eq!(domain_of("whatever"), "other");
    }

    #[test]
    fn the_router_learns_domains_from_confirmed_requests() {
        let l = confirmed();
        let r = Router::train(&l.confirmed());
        assert_eq!(r.domains(), 3);
        assert_eq!(r.route("please call my mum").unwrap().0, "phone");
        assert_eq!(r.route("send a message to ravi").unwrap().0, "messages");
        assert_eq!(r.route("turn the torch on now").unwrap().0, "device");
        assert!(Router::default().route("x").is_none());
    }

    #[test]
    fn the_suite_is_random_but_repeatable_and_has_variants() {
        let l = confirmed();
        let a = random_suite(&l, 7, 5);
        let b = random_suite(&l, 7, 5);
        let c = random_suite(&l, 8, 5);
        assert_eq!(a, b, "the same seed draws the same suite");
        assert_ne!(a, c, "another seed draws another");
        assert!(a.iter().any(|c| c.kind == "variant"));
        assert!(a.iter().filter(|c| c.kind == "held-out").count() <= 5);
    }

    #[test]
    fn a_run_scores_per_domain_and_the_router() {
        let l = confirmed();
        let cases = random_suite(&l, 1, 9);
        let router = Router::train(&l.confirmed());
        // a model that knows only calls
        let run = run_suite(
            &cases,
            &router,
            &mut |p: &str| {
                if p.to_lowercase().contains("mum") || p.to_lowercase().contains("mother") {
                    CALL.into()
                } else {
                    String::new()
                }
            },
            "test",
            1,
            0,
        );
        let phone = run.by_domain.iter().find(|d| d.domain == "phone").unwrap();
        assert!(phone.score > 0.9, "{run:?}");
        assert!(run.score < 0.6, "{run:?}");
        assert!(run.router > 0.5, "{run:?}");
    }

    #[test]
    fn experts_count_examples_per_domain_and_export_their_data() {
        let l = confirmed();
        let ex = experts(&l, 3);
        assert_eq!(ex.len(), 3);
        assert!(ex.iter().all(|e| e.ready && e.examples == 3));
        assert_eq!(expert_dataset(&l, "sys", "phone").len(), 3);
        assert!(experts(&l, 4).iter().all(|e| !e.ready));
    }

    #[test]
    fn history_keeps_runs_and_the_change() {
        let dir = std::env::temp_dir().join(format!("lab-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("lab.json");
        let mut h = History::default();
        h.push(Run {
            score: 0.5,
            ..Default::default()
        });
        h.push(Run {
            score: 0.75,
            ..Default::default()
        });
        h.save(&p).unwrap();
        let h2 = History::load(&p);
        assert_eq!(h2.runs.len(), 2);
        assert_eq!(h2.delta(), Some(25.0));
    }
}
