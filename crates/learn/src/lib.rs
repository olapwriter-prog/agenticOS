//! Learning from what the human approves and declines.
//!
//! Every request becomes an episode: what was asked, which action was chosen, and how it went.
//! Rewards come only from the human and from what really happened, never from the model:
//!   tool ran +0.3 · tool failed -0.5 · approval key pressed +1 · declined -1
//!   "that was right" +1 · "that was wrong" -1 · no usable action -1
//! A "wrong" followed within two minutes by an action that works is a correction: the working
//! action becomes the answer for the original request.
//!
//! What the agent does with it, without retraining any weights:
//!   - shortcuts: a request whose action the human approved (and never rejected) runs that action
//!     directly next time, still through the executor's tiers and the approval key;
//!   - examples: the model is shown the most similar approved requests and actions;
//!   - training data: approved episodes (SFT) and correction pairs (chosen vs rejected, for DPO)
//!     are written out for a trainer, and `evaluate` scores a model on held-out episodes so a
//!     retrained model is only used if it does better.
//!
//! Learning never changes what needs approval: that stays with the executor.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;

pub const RAN: f32 = 0.3;
pub const FAILED: f32 = -0.5;
pub const APPROVED: f32 = 1.0;
pub const DECLINED: f32 = -1.0;
pub const GOOD: f32 = 1.0;
pub const BAD: f32 = -1.0;
pub const GAVE_UP: f32 = -1.0;
/// How long after a "wrong" a working action counts as the correction.
pub const CORRECTION_MS: u64 = 120_000;
/// Episodes kept; older ones are dropped when the log is compacted.
pub const KEEP: usize = 5000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Episode {
    pub id: u64,
    pub t_ms: u64,
    pub prompt: String,
    /// The action as `{"tool":..,"args":{..}}`, or empty if none was produced.
    pub action: String,
    /// "intent", "model", "shortcut" or "function".
    pub source: String,
    pub reward: f32,
    /// Human signals seen: approved, declined, good, bad.
    pub signals: Vec<String>,
    /// Set when a later action corrected this one: the action it should have been.
    pub corrected_to: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "e")]
enum Event {
    #[serde(rename = "ep")]
    Episode(Episode),
    #[serde(rename = "r")]
    Reward {
        id: u64,
        delta: f32,
        signal: Option<String>,
    },
    #[serde(rename = "fix")]
    Correct { id: u64, action: String },
}

/// Lowercase words, without punctuation: how requests are compared.
pub fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect()
}
fn normal(s: &str) -> String {
    words(s).join(" ")
}
fn similarity(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f32 {
    let inter = a.intersection(b).count() as f32;
    let union = a.union(b).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

pub struct Learner {
    path: Option<PathBuf>,
    pub episodes: Vec<Episode>,
    next_id: u64,
    /// The approval-queue id of a High action -> the episode that asked for it.
    pending: BTreeMap<u64, u64>,
    /// The last episode the human was told about, for "that was right/wrong".
    last: Option<u64>,
    /// An episode the human said was wrong, waiting for its correction: (id, when).
    wrong: Option<(u64, u64)>,
}

impl Learner {
    pub fn in_memory() -> Self {
        Learner {
            path: None,
            episodes: Vec::new(),
            next_id: 1,
            pending: BTreeMap::new(),
            last: None,
            wrong: None,
        }
    }

    /// An append-only log of events in `path`, replayed at start and compacted when large.
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let mut l = Self::in_memory();
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                // a torn last line (power cut while writing) is skipped, not fatal
                if let Ok(ev) = serde_json::from_str::<Event>(line) {
                    l.apply(ev);
                }
            }
        }
        l.path = Some(path);
        if l.episodes.len() > KEEP {
            l.compact()?;
        }
        Ok(l)
    }

    fn apply(&mut self, ev: Event) {
        match ev {
            Event::Episode(e) => {
                self.next_id = self.next_id.max(e.id + 1);
                self.episodes.push(e);
            }
            Event::Reward { id, delta, signal } => {
                if let Some(e) = self.get_mut(id) {
                    e.reward += delta;
                    if let Some(s) = signal {
                        e.signals.push(s);
                    }
                }
            }
            Event::Correct { id, action } => {
                if let Some(e) = self.get_mut(id) {
                    e.corrected_to = Some(action);
                }
            }
        }
    }

    fn record(&mut self, ev: Event) {
        if let Some(p) = &self.path {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
            {
                let _ = writeln!(f, "{}", serde_json::to_string(&ev).unwrap_or_default());
            }
        }
        self.apply(ev);
    }

    fn compact(&mut self) -> Result<(), String> {
        let drop = self.episodes.len().saturating_sub(KEEP);
        self.episodes.drain(..drop);
        let Some(p) = &self.path else { return Ok(()) };
        let tmp = p.with_extension("tmp");
        let mut out = String::new();
        for e in &self.episodes {
            out.push_str(
                &serde_json::to_string(&Event::Episode(e.clone())).map_err(|e| e.to_string())?,
            );
            out.push('\n');
        }
        std::fs::write(&tmp, out).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, p).map_err(|e| e.to_string())
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut Episode> {
        self.episodes.iter_mut().rev().find(|e| e.id == id)
    }
    pub fn get(&self, id: u64) -> Option<&Episode> {
        self.episodes.iter().rev().find(|e| e.id == id)
    }

    /// A request was handled. Returns the episode id. If the human had just said "wrong" and
    /// this one worked, it is taken as the correction.
    pub fn begin(&mut self, t_ms: u64, prompt: &str, action: &str, source: &str) -> u64 {
        let id = self.next_id;
        self.record(Event::Episode(Episode {
            id,
            t_ms,
            prompt: prompt.to_string(),
            action: action.to_string(),
            source: source.to_string(),
            reward: 0.0,
            signals: vec![],
            corrected_to: None,
        }));
        self.last = Some(id);
        id
    }

    fn reward(&mut self, id: u64, delta: f32, signal: Option<&str>) {
        self.record(Event::Reward {
            id,
            delta,
            signal: signal.map(String::from),
        });
    }

    /// How the action went: ran, failed, or is waiting for the key (with its queue id).
    pub fn ran(&mut self, id: u64, t_ms: u64, ok: bool) {
        self.reward(id, if ok { RAN } else { FAILED }, None);
        if ok {
            self.maybe_correct(id, t_ms);
        }
    }
    pub fn gave_up(&mut self, id: u64) {
        self.reward(id, GAVE_UP, None);
    }
    pub fn waiting(&mut self, id: u64, queue_id: u64) {
        self.pending.insert(queue_id, id);
    }
    pub fn approved(&mut self, queue_id: u64, t_ms: u64) {
        if let Some(id) = self.pending.remove(&queue_id) {
            self.reward(id, APPROVED, Some("approved"));
            self.maybe_correct(id, t_ms);
        }
    }
    pub fn declined(&mut self, queue_id: u64) {
        if let Some(id) = self.pending.remove(&queue_id) {
            self.reward(id, DECLINED, Some("declined"));
        }
    }

    /// "That was right" or "that was wrong", about the last thing done. Returns its prompt.
    pub fn feedback(&mut self, good: bool, t_ms: u64) -> Option<String> {
        let id = self.last?;
        self.reward(
            id,
            if good { GOOD } else { BAD },
            Some(if good { "good" } else { "bad" }),
        );
        if !good {
            self.wrong = Some((id, t_ms));
        }
        self.get(id).map(|e| e.prompt.clone())
    }

    fn maybe_correct(&mut self, by: u64, t_ms: u64) {
        let Some((wrong, at)) = self.wrong else {
            return;
        };
        if by == wrong || t_ms.saturating_sub(at) > CORRECTION_MS {
            return;
        }
        let action = match self.get(by) {
            Some(e) if !e.action.is_empty() => e.action.clone(),
            _ => return,
        };
        if self.get(wrong).is_some_and(|w| w.action == action) {
            return;
        }
        self.record(Event::Correct { id: wrong, action });
        self.wrong = None;
    }

    /// The action to use for a request, learned from what the human approved: an exact match
    /// (same words) whose answer was approved or corrected, and never rejected since.
    pub fn shortcut(&self, prompt: &str) -> Option<String> {
        let key = normal(prompt);
        let mut score: BTreeMap<&str, f32> = BTreeMap::new();
        let mut rejected: BTreeSet<&str> = BTreeSet::new();
        for e in self.episodes.iter().filter(|e| normal(&e.prompt) == key) {
            if let Some(fix) = &e.corrected_to {
                *score.entry(fix.as_str()).or_default() += 1.0;
                rejected.insert(e.action.as_str());
            } else if e.signals.iter().any(|s| s == "approved" || s == "good") {
                *score.entry(e.action.as_str()).or_default() += 1.0;
            }
            if e.signals.iter().any(|s| s == "declined" || s == "bad") && e.corrected_to.is_none() {
                rejected.insert(e.action.as_str());
            }
        }
        score
            .into_iter()
            .filter(|(a, s)| *s >= 1.0 && !a.is_empty() && !rejected.contains(a))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(a, _)| a.to_string())
    }

    /// What the request should have produced, by the human's judgement: the correction if any,
    /// else the action if the human approved it or said it was right. A model's answer that only
    /// ran without error is not a label: that would be the model marking its own work.
    /// Phrase-table and shortcut answers come from the human already, so running is enough.
    pub fn target(e: &Episode) -> Option<&str> {
        if let Some(fix) = &e.corrected_to {
            return Some(fix);
        }
        let rejected = e.signals.iter().any(|s| s == "declined" || s == "bad");
        let confirmed = e.signals.iter().any(|s| s == "approved" || s == "good");
        let from_human = e.source == "intent" || e.source == "shortcut";
        let ok = confirmed || (from_human && e.reward > 0.0);
        (!e.action.is_empty() && ok && !rejected).then_some(e.action.as_str())
    }

    /// Up to `k` approved (prompt, action) pairs most like this request, best first.
    pub fn examples(&self, prompt: &str, k: usize) -> Vec<(String, String)> {
        let q: BTreeSet<String> = words(prompt).into_iter().collect();
        let mut seen = BTreeSet::new();
        let mut scored: Vec<(f32, String, String)> = self
            .episodes
            .iter()
            .rev()
            .filter_map(|e| Self::target(e).map(|t| (e, t)))
            .filter(|(e, _)| seen.insert(normal(&e.prompt)))
            .map(|(e, t)| {
                let w: BTreeSet<String> = words(&e.prompt).into_iter().collect();
                (similarity(&q, &w), e.prompt.clone(), t.to_string())
            })
            .filter(|(s, _, _)| *s > 0.0)
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored.into_iter().take(k).map(|(_, p, a)| (p, a)).collect()
    }

    /// Training data. SFT: one chat per approved episode. DPO: corrections, as the action the
    /// human wanted (chosen) against the one they rejected.
    pub fn datasets(&self, system: &str) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
        let mut sft = Vec::new();
        let mut dpo = Vec::new();
        for e in &self.episodes {
            if let Some(t) = Self::target(e) {
                sft.push(serde_json::json!({"messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": e.prompt},
                    {"role": "assistant", "content": t}
                ]}));
            }
            if let Some(fix) = &e.corrected_to {
                if !e.action.is_empty() {
                    dpo.push(serde_json::json!({
                        "prompt": e.prompt, "chosen": fix, "rejected": e.action
                    }));
                }
            }
        }
        (sft, dpo)
    }

    /// Held-out cases for judging a model: (prompt, expected action), every `nth` labelled one.
    pub fn eval_set(&self, nth: usize) -> Vec<(String, String)> {
        self.episodes
            .iter()
            .filter(|e| e.source == "model" || e.corrected_to.is_some())
            .filter_map(|e| Self::target(e).map(|t| (e.prompt.clone(), t.to_string())))
            .enumerate()
            .filter(|(i, _)| i % nth.max(1) == 0)
            .map(|(_, x)| x)
            .collect()
    }

    pub fn stats(&self) -> String {
        let n = self.episodes.len();
        let labelled = self
            .episodes
            .iter()
            .filter(|e| Self::target(e).is_some())
            .count();
        let fixes = self
            .episodes
            .iter()
            .filter(|e| e.corrected_to.is_some())
            .count();
        let by = |src: &str| {
            let v: Vec<&Episode> = self.episodes.iter().filter(|e| e.source == src).collect();
            let good = v.iter().filter(|e| e.reward > 0.0).count();
            format!("{src} {good}/{}", v.len())
        };
        format!(
            "{n} episodes, {labelled} usable for learning, {fixes} corrections; rewarded/total by source: {}, {}, {}, {}, {}",
            by("intent"),
            by("shortcut"),
            by("classifier"),
            by("model"),
            by("function")
        )
    }
}

/// Does this model answer held-out requests the way the human wanted? `ask` sends one request
/// to the model and returns its action JSON. Scores 1 for the exact action, 0.5 for the right
/// tool with other arguments. Returns the mean.
pub fn evaluate(cases: &[(String, String)], ask: &mut dyn FnMut(&str) -> String) -> f32 {
    if cases.is_empty() {
        return 0.0;
    }
    let parse = |s: &str| serde_json::from_str::<serde_json::Value>(s.trim()).ok();
    let total: f32 = cases
        .iter()
        .map(|(prompt, want)| {
            let got = ask(prompt);
            match (parse(&got), parse(want)) {
                (Some(g), Some(w)) if g == w => 1.0,
                (Some(g), Some(w)) if g["tool"] == w["tool"] => 0.5,
                _ => 0.0,
            }
        })
        .sum();
    total / cases.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    const CALL: &str = r#"{"tool":"call","args":{"to":"mother"}}"#;
    const TORCH: &str = r#"{"tool":"flashlight","args":{"on":"on"}}"#;
    const VOLUME: &str = r#"{"tool":"set_volume","args":{"percent":"100"}}"#;

    #[test]
    fn an_approved_high_action_becomes_a_shortcut() {
        let mut l = Learner::in_memory();
        let id = l.begin(0, "Could you ring my mother?", CALL, "model");
        l.waiting(id, 7);
        assert_eq!(
            l.shortcut("could you ring my mother"),
            None,
            "not before the key"
        );
        l.approved(7, 10);
        assert_eq!(l.get(id).unwrap().reward, APPROVED);
        assert_eq!(
            l.shortcut("could you ring my mother").as_deref(),
            Some(CALL)
        );
        assert_eq!(l.shortcut("ring my mother"), None, "exact requests only");
    }
    #[test]
    fn a_declined_action_is_never_a_shortcut() {
        let mut l = Learner::in_memory();
        let a = l.begin(0, "ring mum", CALL, "model");
        l.waiting(a, 1);
        l.approved(1, 1);
        let b = l.begin(5, "ring mum", CALL, "shortcut");
        l.waiting(b, 2);
        l.declined(2);
        assert_eq!(l.shortcut("ring mum"), None);
    }
    #[test]
    fn wrong_then_a_working_action_is_a_correction() {
        let mut l = Learner::in_memory();
        let bad = l.begin(0, "switch the torch on", VOLUME, "model");
        l.ran(bad, 0, true);
        assert_eq!(
            l.feedback(false, 1_000).as_deref(),
            Some("switch the torch on")
        );
        let fix = l.begin(5_000, "flashlight on", TORCH, "intent");
        l.ran(fix, 5_000, true);
        assert_eq!(l.get(bad).unwrap().corrected_to.as_deref(), Some(TORCH));
        assert_eq!(l.shortcut("Switch the torch on!").as_deref(), Some(TORCH));
        let (sft, dpo) = l.datasets("sys");
        assert_eq!(dpo.len(), 1);
        assert_eq!(dpo[0]["chosen"], TORCH);
        assert_eq!(dpo[0]["rejected"], VOLUME);
        assert!(sft
            .iter()
            .any(|c| c["messages"][1]["content"] == "switch the torch on"
                && c["messages"][2]["content"] == TORCH));
    }
    #[test]
    fn a_late_action_is_not_taken_as_a_correction() {
        let mut l = Learner::in_memory();
        let bad = l.begin(0, "switch the torch on", VOLUME, "model");
        l.feedback(false, 0);
        let later = l.begin(CORRECTION_MS + 1, "flashlight on", TORCH, "intent");
        l.ran(later, CORRECTION_MS + 1, true);
        assert!(l.get(bad).unwrap().corrected_to.is_none());
    }
    #[test]
    fn similar_approved_requests_are_offered_as_examples() {
        let mut l = Learner::in_memory();
        let a = l.begin(0, "please switch the torch on", TORCH, "model");
        l.feedback(true, 0);
        let b = l.begin(1, "ring my mother now", CALL, "model");
        l.waiting(b, 3);
        l.approved(3, 2);
        let failed = l.begin(2, "torch please", VOLUME, "model");
        l.ran(failed, 2, false);
        let ex = l.examples("turn the torch on", 3);
        assert_eq!(
            ex[0],
            ("please switch the torch on".to_string(), TORCH.to_string())
        );
        assert!(
            ex.iter().all(|(_, act)| act != VOLUME),
            "failures are never examples"
        );
        let _ = a;
    }
    #[test]
    fn the_log_survives_a_restart_and_a_torn_line() {
        let p = std::env::temp_dir().join(format!("learn-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&p);
        {
            let mut l = Learner::open(p.clone()).unwrap();
            let id = l.begin(0, "ring mum", CALL, "model");
            l.waiting(id, 1);
            l.approved(1, 1);
        }
        std::fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(b"{\"e\":\"r\",\"id\":")
            .unwrap();
        let l = Learner::open(p.clone()).unwrap();
        assert_eq!(l.shortcut("ring mum").as_deref(), Some(CALL));
        let mut l = l;
        assert_eq!(l.begin(0, "x", "", "model"), 2, "ids continue");
        std::fs::remove_file(p).ok();
    }
    #[test]
    fn a_model_answer_that_merely_ran_is_not_a_label() {
        let mut l = Learner::in_memory();
        let m = l.begin(0, "switch the torch on", VOLUME, "model");
        l.ran(m, 0, true);
        assert!(Learner::target(l.get(m).unwrap()).is_none());
        assert!(l.examples("torch on", 3).is_empty());
        assert!(l.eval_set(1).is_empty());
        let i = l.begin(1, "flashlight on", TORCH, "intent");
        l.ran(i, 1, true);
        assert_eq!(Learner::target(l.get(i).unwrap()), Some(TORCH));
        l.feedback(true, 2);
        assert_eq!(l.datasets("s").0.len(), 1);
    }
    #[test]
    fn evaluation_scores_exact_actions_and_right_tools() {
        let cases = vec![
            ("ring mum".to_string(), CALL.to_string()),
            ("torch".to_string(), TORCH.to_string()),
        ];
        let perfect = evaluate(&cases, &mut |p| {
            if p == "ring mum" {
                CALL.into()
            } else {
                TORCH.into()
            }
        });
        assert_eq!(perfect, 1.0);
        let half = evaluate(&cases, &mut |p| {
            if p == "ring mum" {
                r#"{"tool":"call","args":{"to":"dad"}}"#.into()
            } else {
                "nonsense".into()
            }
        });
        assert_eq!(half, 0.25);
    }
    #[test]
    fn stats_count_by_source() {
        let mut l = Learner::in_memory();
        let a = l.begin(0, "x", TORCH, "intent");
        l.ran(a, 0, true);
        let b = l.begin(0, "y", "", "model");
        l.gave_up(b);
        let s = l.stats();
        assert!(
            s.contains("2 episodes") && s.contains("intent 1/1") && s.contains("model 0/1"),
            "{s}"
        );
    }
}
