//! The predictive cache's brain: what the owner is likely to ask next, and from that, what the
//! model server should keep in memory, drop, or prepare ahead of time.
//!
//! The predictor is learned from the episodes the learner already keeps: for each hour of the
//! day, how likely a request is and which kind (domain) it will be, and which kind tends to follow
//! which. Old days count less (a two-week half-life), so it follows changes in routine. The
//! manager turns predictions, the cache's contents, the battery and free memory into a plan: a
//! memory budget, a priority per cached state, states to drop, and states to warm. It also learns
//! from its own record: a domain whose warm-ups keep going unused needs more evidence before it
//! is warmed again. Plans only prepare: nothing is ever sent or done on the owner's behalf.
use crate::lab::{domain_of, tool_of};
use crate::Episode;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

const DAY_MS: u64 = 86_400_000;
const HALF_LIFE_DAYS: f64 = 14.0;
const MB: u64 = 1 << 20;

/// The kind of request an episode was: its tool's domain, or "answer" when no tool ran.
pub fn kind_of(e: &Episode) -> String {
    let tool = tool_of(&e.action);
    if tool.is_empty() {
        "answer".into()
    } else {
        domain_of(&tool).to_string()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Predictor {
    /// weighted requests per local hour and kind
    by_hour: Vec<HashMap<String, f64>>,
    /// weighted kind → next kind (within an hour of each other)
    after: HashMap<String, HashMap<String, f64>>,
    /// per hour: weighted days with a model request / with Android used
    model_days: Vec<f64>,
    android_days: Vec<f64>,
    /// weighted number of days seen
    days: f64,
    pub episodes: usize,
}

impl Predictor {
    /// Learn from history. `now_ms` sets the age of each episode; `offset_min` the local clock.
    pub fn learn(episodes: &[Episode], now_ms: u64, offset_min: i32) -> Predictor {
        let off = offset_min as i64 * 60_000;
        let local = |t: u64| (t as i64 + off).max(0) as u64;
        let weight = |t: u64| {
            let age = now_ms.saturating_sub(t) as f64 / DAY_MS as f64;
            0.5f64.powf(age / HALF_LIFE_DAYS)
        };
        let mut p = Predictor {
            by_hour: vec![HashMap::new(); 24],
            model_days: vec![0.0; 24],
            android_days: vec![0.0; 24],
            episodes: episodes.len(),
            ..Default::default()
        };
        let mut sorted: Vec<&Episode> = episodes.iter().filter(|e| e.t_ms <= now_ms).collect();
        sorted.sort_by_key(|e| e.t_ms);
        let mut days: HashSet<u64> = HashSet::new();
        let mut model_hours: HashSet<(u64, usize)> = HashSet::new();
        let mut android_hours: HashSet<(u64, usize)> = HashSet::new();
        let mut prev: Option<(&Episode, String)> = None;
        for e in sorted {
            let lt = local(e.t_ms);
            let (day, hour) = (lt / DAY_MS, ((lt % DAY_MS) / 3_600_000) as usize);
            let w = weight(e.t_ms);
            let k = kind_of(e);
            if days.insert(day) {
                p.days += w;
            }
            *p.by_hour[hour].entry(k.clone()).or_default() += w;
            if e.source == "model" && model_hours.insert((day, hour)) {
                p.model_days[hour] += w;
            }
            if (k == "android" || tool_of(&e.action).starts_with("android"))
                && android_hours.insert((day, hour))
            {
                p.android_days[hour] += w;
            }
            if let Some((pe, pk)) = &prev {
                if e.t_ms.saturating_sub(pe.t_ms) <= 3_600_000 {
                    *p.after
                        .entry(pk.clone())
                        .or_default()
                        .entry(k.clone())
                        .or_default() += w;
                }
            }
            prev = Some((e, k));
        }
        p
    }

    fn norm(m: &HashMap<String, f64>) -> Vec<(String, f32)> {
        let total: f64 = m.values().sum();
        if total <= 0.0 {
            return vec![];
        }
        let mut v: Vec<(String, f32)> = m
            .iter()
            .map(|(k, x)| (k.clone(), (x / total) as f32))
            .collect();
        v.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        v
    }

    /// What kind of request comes next, most likely first, summing to 1: this hour and the next
    /// (smoothed), mixed with what usually follows the last request.
    pub fn next(&self, hour: usize, prev: Option<&str>) -> Vec<(String, f32)> {
        let mut mix: HashMap<String, f64> = HashMap::new();
        for (h, w) in [
            (hour % 24, 0.6),
            ((hour + 1) % 24, 0.3),
            ((hour + 23) % 24, 0.1),
        ] {
            for (k, p) in Self::norm(&self.by_hour[h]) {
                *mix.entry(k).or_default() += w * p as f64;
            }
        }
        if let Some(after) = prev.and_then(|k| self.after.get(k)) {
            let has_hour = !mix.is_empty();
            for (k, p) in Self::norm(after) {
                *mix.entry(k).or_default() += if has_hour { 0.8 } else { 1.0 } * p as f64;
            }
        }
        if mix.is_empty() {
            // nothing for this hour: the day overall
            for m in &self.by_hour {
                for (k, x) in m {
                    *mix.entry(k.clone()).or_default() += x;
                }
            }
        }
        Self::norm(&mix)
    }

    fn rate(&self, per_hour: &[f64], hour: usize) -> f32 {
        if self.days <= 0.0 {
            return 0.0;
        }
        let at = |h: usize| (per_hour[h % 24] / self.days).min(1.0);
        at(hour).max(at(hour + 1)) as f32
    }

    /// How likely a request that needs the model is, this hour or the next (0..1).
    pub fn model_soon(&self, hour: usize) -> f32 {
        self.rate(&self.model_days, hour)
    }

    /// How likely Android is needed, this hour or the next.
    pub fn android_soon(&self, hour: usize) -> f32 {
        self.rate(&self.android_days, hour)
    }
}

/// One cached state as the model server reports it (`GET /cache`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cached {
    pub id: u64,
    pub adapter: String,
    pub label: String,
    pub tokens: usize,
    pub bytes: u64,
    pub hits: u32,
    pub idle: u64,
    pub priority: f32,
    pub warmed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Power {
    pub battery: u8,
    pub charging: bool,
    /// free memory, when it can be read
    pub mem_free_mb: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    /// the most the cache may use with power and memory to spare
    pub budget_mb: u64,
    /// warm states ahead of time (else only manage what requests made)
    pub warm: bool,
}

/// The manager's own record: per adapter ("" = the base model), warm-ups and how many a request
/// then used. Kept across restarts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    pub warmed: BTreeMap<String, u32>,
    pub used: BTreeMap<String, u32>,
}

impl Record {
    /// Share of warm-ups that were used, once there are enough to judge.
    pub fn precision(&self, adapter: &str) -> Option<f32> {
        let w = *self.warmed.get(adapter).unwrap_or(&0);
        (w >= 5).then(|| *self.used.get(adapter).unwrap_or(&0) as f32 / w as f32)
    }
    pub fn load(path: &std::path::Path) -> Record {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub budget_bytes: u64,
    pub priorities: Vec<(u64, f32)>,
    pub drop: Vec<u64>,
    /// adapters whose shared prompt to read now ("" = the base model)
    pub warm: Vec<String>,
    pub android_wake: bool,
    pub model_soon: f32,
    pub next: Vec<(String, f32)>,
    /// why, in plain words, for the lab screen and the ledger
    pub reasons: Vec<String>,
}

/// The adapter that serves a kind of request, when there is an expert for it.
fn adapter_for<'a>(kind: &str, experts: &'a [String]) -> Option<&'a String> {
    experts.iter().find(|e| e.as_str() == kind)
}

/// Decide what the cache should hold now.
pub fn plan(
    pred: &Predictor,
    hour: usize,
    prev: Option<&str>,
    cached: &[Cached],
    experts: &[String],
    power: Power,
    s: &Settings,
    rec: &Record,
) -> Plan {
    let mut p = Plan {
        model_soon: pred.model_soon(hour),
        next: pred.next(hour, prev),
        ..Default::default()
    };
    if !s.enabled {
        p.budget_bytes = 0;
        p.drop = cached.iter().map(|c| c.id).collect();
        p.reasons
            .push("Predictive cache is off: nothing kept.".into());
        return p;
    }
    // 1. the budget: power first, then free memory
    let (factor, why) = if power.charging {
        (1.0, "on the charger")
    } else if power.battery < 15 {
        (0.0, "battery under 15%")
    } else if power.battery < 30 {
        (0.35, "battery under 30%")
    } else if power.battery < 50 {
        (0.6, "battery under 50%")
    } else {
        (0.8, "on battery")
    };
    let mut budget = (s.budget_mb as f64 * factor) as u64 * MB;
    p.reasons.push(format!("Budget {} MB: {why}.", budget / MB));
    if let Some(free) = power.mem_free_mb {
        if free < 300 {
            budget = 0;
            p.reasons.push(format!(
                "Only {free} MB free: cache emptied to give memory back."
            ));
        } else if budget > free * MB / 4 {
            budget = free * MB / 4;
            p.reasons.push(format!(
                "Capped at a quarter of free memory ({} MB).",
                budget / MB
            ));
        }
    }
    p.budget_bytes = budget;

    // 2. a priority for each cached state; drop what is not worth its memory
    let prob = |k: &str| {
        p.next
            .iter()
            .find(|(d, _)| d == k)
            .map(|x| x.1)
            .unwrap_or(0.0)
    };
    for c in cached {
        if !c.adapter.is_empty() && !experts.contains(&c.adapter) {
            p.drop.push(c.id);
            p.reasons.push(format!(
                "Dropped {}: that expert is no longer served.",
                c.label
            ));
            continue;
        }
        let mut pr = if c.adapter.is_empty() {
            // the base prompt serves every model request
            p.model_soon.max(0.3)
        } else {
            prob(&c.adapter) * p.model_soon.max(0.2) * 1.5
        };
        if let Some(prec) = rec.precision(&c.adapter) {
            pr *= 0.5 + prec;
        }
        let pr = pr.clamp(0.0, 1.0);
        if (c.hits == 0 && c.idle > 50) || (pr < 0.05 && c.idle > 10) {
            p.drop.push(c.id);
            p.reasons.push(format!(
                "Dropped {} ({} MB): not used for {} requests.",
                c.label,
                c.bytes / MB,
                c.idle
            ));
            continue;
        }
        p.priorities.push((c.id, pr));
    }

    // 3. warm what is likely soon, when there is power and room
    let has = |a: &str| {
        cached
            .iter()
            .any(|c| c.adapter == a && !p.drop.contains(&c.id))
    };
    let power_ok = power.charging || power.battery >= 40;
    if s.warm && budget > 0 && power_ok {
        if p.model_soon >= 0.25 && !has("") {
            p.warm.push(String::new());
            p.reasons.push(format!(
                "Warming the system prompt: a model request is {:.0}% likely within the hour.",
                p.model_soon * 100.0
            ));
        }
        if let Some((k, pk)) = p
            .next
            .iter()
            .find(|(k, _)| adapter_for(k, experts).is_some())
        {
            // a domain whose warm-ups go unused needs more evidence
            let need = match rec.precision(k) {
                Some(prec) if prec < 0.3 => 0.6,
                _ => 0.35,
            };
            if *pk >= need && p.model_soon >= 0.25 && !has(k) {
                p.warm.push(k.clone());
                p.reasons.push(format!(
                    "Warming the {k} expert: next request {:.0}% likely {k}.",
                    pk * 100.0
                ));
            }
        }
    } else if s.warm && !power_ok {
        p.reasons
            .push("Not warming anything: battery under 40% and not charging.".into());
    }
    if pred.android_soon(hour) >= 0.5 && power_ok {
        p.android_wake = true;
        p.reasons.push(format!(
            "Waking Android ahead: used this hour on {:.0}% of days.",
            pred.android_soon(hour) * 100.0
        ));
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(t_ms: u64, tool: &str, source: &str) -> Episode {
        Episode {
            id: t_ms,
            t_ms,
            prompt: tool.into(),
            action: if tool.is_empty() {
                String::new()
            } else {
                format!(r#"{{"tool":"{tool}","args":{{}}}}"#)
            },
            source: source.into(),
            reward: 1.0,
            signals: vec![],
            corrected_to: None,
        }
    }

    const H: u64 = 3_600_000;
    // 2026-11-12 00:00 UTC
    const DAY0: u64 = 1_762_905_600_000;

    /// Ten days: messages at 9 (a call right after on some days), weather at 13, Android at 20.
    fn history() -> (Vec<Episode>, u64) {
        let mut v = vec![];
        for d in 0..10 {
            let base = DAY0 + d * DAY_MS;
            v.push(ep(base + 9 * H, "send_sms", "model"));
            if d % 3 == 0 {
                v.push(ep(base + 9 * H + 600_000, "call", "model"));
            }
            v.push(ep(base + 13 * H, "weather", "intent"));
            v.push(ep(base + 20 * H, "android_open", "intent"));
        }
        (v, DAY0 + 10 * DAY_MS + 8 * H + 30 * 60_000)
    }

    fn settings() -> Settings {
        Settings {
            enabled: true,
            budget_mb: 512,
            warm: true,
        }
    }

    #[test]
    fn it_learns_the_routine() {
        let (eps, now) = history();
        let p = Predictor::learn(&eps, now, 0);
        let next = p.next(9, None);
        assert_eq!(next[0].0, "messages");
        // after a message, a call is likely
        let after = p.next(15, Some("messages"));
        assert_eq!(after[0].0, "phone");
        assert!(p.model_soon(8) > 0.8, "{}", p.model_soon(8));
        assert!(p.model_soon(13) < 0.1);
        assert!(p.android_soon(19) > 0.8);
        assert!(p.android_soon(9) < 0.1);
    }

    #[test]
    fn before_the_morning_it_warms_the_prompt_and_the_likely_expert() {
        let (eps, now) = history();
        let pred = Predictor::learn(&eps, now, 0);
        let power = Power {
            battery: 80,
            charging: false,
            mem_free_mb: Some(4000),
        };
        let experts = vec!["messages".to_string(), "phone".to_string()];
        let pl = plan(
            &pred,
            8,
            None,
            &[],
            &experts,
            power,
            &settings(),
            &Record::default(),
        );
        assert_eq!(pl.warm, vec!["".to_string(), "messages".to_string()]);
        assert_eq!(pl.budget_bytes, 409 * MB);
        assert!(!pl.android_wake);
        // in the afternoon nothing needs the model: nothing warmed
        let pl = plan(
            &pred,
            14,
            None,
            &[],
            &experts,
            power,
            &settings(),
            &Record::default(),
        );
        assert!(pl.warm.is_empty(), "{:?}", pl.reasons);
        // the evening: Android ahead of time
        assert!(
            plan(
                &pred,
                19,
                None,
                &[],
                &experts,
                power,
                &settings(),
                &Record::default()
            )
            .android_wake
        );
    }

    #[test]
    fn low_battery_and_memory_pressure_shrink_or_empty_the_cache() {
        let (eps, now) = history();
        let pred = Predictor::learn(&eps, now, 0);
        let low = Power {
            battery: 25,
            charging: false,
            mem_free_mb: Some(4000),
        };
        let pl = plan(
            &pred,
            8,
            None,
            &[],
            &[],
            low,
            &settings(),
            &Record::default(),
        );
        assert_eq!(pl.budget_bytes, 179 * MB);
        assert!(pl.warm.is_empty(), "no warming under 40% off the charger");
        let tight = Power {
            battery: 90,
            charging: true,
            mem_free_mb: Some(600),
        };
        assert_eq!(
            plan(
                &pred,
                8,
                None,
                &[],
                &[],
                tight,
                &settings(),
                &Record::default()
            )
            .budget_bytes,
            150 * MB
        );
        let none = Power {
            battery: 90,
            charging: true,
            mem_free_mb: Some(200),
        };
        assert_eq!(
            plan(
                &pred,
                8,
                None,
                &[],
                &[],
                none,
                &settings(),
                &Record::default()
            )
            .budget_bytes,
            0
        );
    }

    #[test]
    fn stale_and_orphaned_states_are_dropped_and_priorities_follow_predictions() {
        let (eps, now) = history();
        let pred = Predictor::learn(&eps, now, 0);
        let power = Power {
            battery: 80,
            charging: true,
            mem_free_mb: None,
        };
        let experts = vec!["messages".to_string(), "money".to_string()];
        let c = |id, adapter: &str, hits, idle| Cached {
            id,
            adapter: adapter.into(),
            label: adapter.into(),
            bytes: 100 * MB,
            hits,
            idle,
            ..Default::default()
        };
        let cached = vec![
            c(1, "", 5, 0),
            c(2, "messages", 3, 1),
            c(3, "money", 0, 60),
            c(4, "photos", 2, 0),
        ];
        let pl = plan(
            &pred,
            9,
            None,
            &cached,
            &experts,
            power,
            &settings(),
            &Record::default(),
        );
        assert!(pl.drop.contains(&3) && pl.drop.contains(&4));
        let pr = |id| pl.priorities.iter().find(|x| x.0 == id).unwrap().1;
        assert!(pr(1) > 0.8 && pr(2) > 0.5, "{:?}", pl.priorities);
        // nothing already held is warmed again
        assert!(pl.warm.is_empty(), "{:?}", pl.warm);
    }

    #[test]
    fn warm_ups_that_go_unused_raise_the_bar() {
        let (mut eps, now) = history();
        // a weaker habit: money at 8 on some days
        for d in 0..10 {
            if d % 2 == 0 {
                eps.push(ep(DAY0 + d * DAY_MS + 8 * H + 20 * 60_000, "pay", "model"));
            }
            if d % 3 == 0 {
                eps.push(ep(
                    DAY0 + d * DAY_MS + 8 * H + 5 * 60_000,
                    "weather",
                    "intent",
                ));
            }
        }
        let pred = Predictor::learn(&eps, now, 0);
        let power = Power {
            battery: 90,
            charging: true,
            mem_free_mb: None,
        };
        let experts = vec!["money".to_string()];
        let warms = |rec: &Record| {
            plan(&pred, 8, None, &[], &experts, power, &settings(), rec)
                .warm
                .contains(&"money".to_string())
        };
        let p_money = pred
            .next(8, None)
            .iter()
            .find(|x| x.0 == "money")
            .unwrap()
            .1;
        assert!(p_money >= 0.35 && p_money < 0.6, "{p_money}");
        assert!(warms(&Record::default()));
        let mut rec = Record::default();
        rec.warmed.insert("money".into(), 10);
        rec.used.insert("money".into(), 1);
        assert!(!warms(&rec), "a 10% record needs 60% confidence");
        let off = Settings {
            enabled: false,
            ..settings()
        };
        let pl = plan(
            &pred,
            8,
            None,
            &[Cached {
                id: 7,
                ..Default::default()
            }],
            &experts,
            power,
            &off,
            &Record::default(),
        );
        assert_eq!((pl.budget_bytes, pl.drop.clone()), (0, vec![7]));
    }
}
