//! The predictive cache: answers prepared before they are asked for.
//!
//! People ask a phone the same read-only things at the same times: the dashboard after the
//! lunch rush, the riders' sheet at closing, then the plan. Each such request is remembered
//! with the hour and the request before it. From that, the few most likely next requests are
//! run ahead of time and their answers kept. An answer is used only while the data it was made
//! from is unchanged (the owner of the data gives a revision number), so a prepared answer is
//! never stale. Only tools that read are ever run ahead: nothing is sent, changed or approved
//! by prediction.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Default, Serialize, Deserialize)]
struct Habits {
    /// "previous action|hour" -> action -> times seen. Also "*|hour" and "previous|*".
    seen: BTreeMap<String, BTreeMap<String, u32>>,
}

struct Entry {
    rev: u64,
    answer: String,
    ahead: bool,
}

#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    /// Answered from an answer prepared ahead of time.
    pub ahead_hits: u64,
    /// Answered from an answer kept from an earlier identical request.
    pub repeat_hits: u64,
    pub misses: u64,
    /// Answers prepared ahead.
    pub prepared: u64,
    /// Prepared answers that went stale (the data changed) before anyone asked.
    pub stale: u64,
}

pub struct Predictor {
    path: Option<PathBuf>,
    habits: Habits,
    cache: BTreeMap<String, Entry>,
    last: String,
    pub stats: Stats,
}

/// How many requests are prepared ahead at most.
pub const AHEAD: usize = 3;

impl Predictor {
    pub fn open(path: Option<PathBuf>) -> Self {
        let habits = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self { path, habits, cache: BTreeMap::new(), last: String::new(), stats: Stats::default() }
    }

    /// A read-only request was made at this hour.
    pub fn observe(&mut self, action: &str, hour: usize) {
        for key in [format!("{}|{hour}", self.last), format!("*|{hour}"), format!("{}|*", self.last)] {
            *self.habits.seen.entry(key).or_default().entry(action.to_string()).or_default() += 1;
        }
        self.last = action.to_string();
        if let Some(p) = &self.path {
            if let Ok(t) = serde_json::to_string(&self.habits) {
                let _ = std::fs::write(p, t);
            }
        }
    }

    /// The requests most likely to come next at this hour, best first. A request must have
    /// been seen at least twice in a matching situation to be worth preparing.
    pub fn likely(&self, hour: usize) -> Vec<String> {
        let mut score: BTreeMap<&str, f32> = BTreeMap::new();
        // the same hour on other days, and the hours beside it (people are not that punctual)
        let hours = [(hour, 1.0), ((hour + 23) % 24, 0.5), ((hour + 1) % 24, 0.5)];
        for (h, w) in hours {
            for (key, weight) in [(format!("{}|{h}", self.last), 2.0 * w), (format!("*|{h}"), w)] {
                for (a, n) in self.habits.seen.get(&key).into_iter().flatten() {
                    *score.entry(a.as_str()).or_default() += weight * *n as f32;
                }
            }
        }
        for (a, n) in self.habits.seen.get(&format!("{}|*", self.last)).into_iter().flatten() {
            *score.entry(a.as_str()).or_default() += 0.5 * *n as f32;
        }
        // how often it was asked around this hour at all, whatever came before
        let times = |a: &str| -> u32 {
            hours.iter().map(|(h, _)| self.habits.seen.get(&format!("*|{h}")).and_then(|m| m.get(a)).copied().unwrap_or(0)).sum()
        };
        let mut v: Vec<(&str, f32)> = score.into_iter().filter(|(a, _)| times(a) >= 2 && *a != self.last).collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));
        v.into_iter().take(AHEAD).map(|(a, _)| a.to_string()).collect()
    }

    /// The kept answer, if the data has not changed since it was made.
    pub fn get(&mut self, action: &str, rev: u64) -> Option<String> {
        match self.cache.get(action) {
            Some(e) if e.rev == rev => {
                if e.ahead {
                    self.stats.ahead_hits += 1;
                } else {
                    self.stats.repeat_hits += 1;
                }
                Some(e.answer.clone())
            }
            _ => {
                self.stats.misses += 1;
                None
            }
        }
    }
    pub fn fresh(&self, action: &str, rev: u64) -> bool {
        self.cache.get(action).is_some_and(|e| e.rev == rev)
    }
    pub fn put(&mut self, action: &str, rev: u64, answer: String, ahead: bool) {
        if ahead {
            self.stats.prepared += 1;
        }
        if let Some(old) = self.cache.insert(action.to_string(), Entry { rev, answer, ahead }) {
            if old.ahead && old.rev != rev {
                self.stats.stale += 1;
            }
        }
    }
    /// A request was served: its kept answer is now an ordinary one (counted once as "ahead").
    pub fn used(&mut self, action: &str) {
        if let Some(e) = self.cache.get_mut(action) {
            e.ahead = false;
        }
    }
    pub fn habits(&self) -> usize {
        self.habits.seen.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_prepares_what_usually_follows_and_never_serves_a_stale_answer() {
        let mut p = Predictor::open(None);
        // three evenings: dashboard, then riders, then the plan
        for _ in 0..3 {
            p.observe("dashboard", 22);
            p.observe("riders", 22);
            p.observe("plan", 22);
        }
        // nothing learned about the morning
        p.last.clear();
        assert!(p.likely(9).is_empty());
        // in the evening, with nothing asked yet, the three regulars are likely
        assert_eq!(p.likely(22).len(), 3);
        p.observe("dashboard", 22);
        assert_eq!(p.likely(22)[0], "riders", "what follows the dashboard comes first");
        // a quarter of an hour late is still the same habit
        assert_eq!(p.likely(23)[0], "riders");

        p.put("riders", 7, "Ravi 104".into(), true);
        assert_eq!(p.get("riders", 7).as_deref(), Some("Ravi 104"));
        // the book changed: the prepared answer is not used
        assert_eq!(p.get("riders", 8), None);
        assert_eq!((p.stats.ahead_hits, p.stats.misses, p.stats.prepared), (1, 1, 1));
        p.put("riders", 8, "Ravi 156".into(), true);
        assert_eq!(p.stats.stale, 1);
    }

    #[test]
    fn a_request_seen_once_is_not_worth_preparing() {
        let mut p = Predictor::open(None);
        p.observe("gst", 10);
        p.last.clear();
        assert!(p.likely(10).is_empty());
    }

    #[test]
    fn habits_survive_a_restart() {
        let f = std::env::temp_dir().join(format!("predict-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&f);
        {
            let mut p = Predictor::open(Some(f.clone()));
            for _ in 0..3 {
                p.observe("dashboard", 14);
                p.last.clear();
            }
        }
        let p = Predictor::open(Some(f.clone()));
        assert_eq!(p.likely(14), vec!["dashboard".to_string()]);
        let _ = std::fs::remove_file(&f);
    }
}
