//! The day as a flow: what was asked, what the agent did and how it ended, hour by hour, with
//! the numbers behind it (per domain, approvals, the last seven days). Built from the episodes the
//! learner keeps, so it needs no extra logging.
use crate::lab::{domain_of, tool_of};
use crate::Episode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const DAY_MS: u64 = 86_400_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub t_ms: u64,
    /// local "HH:MM"
    pub at: String,
    pub prompt: String,
    pub tool: String,
    pub domain: String,
    /// "done", "approved", "declined", "failed" or "open"
    pub outcome: String,
    /// "intent", "model", "shortcut", ...
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Day {
    pub flow: Vec<Step>,
    /// actions per local hour, 0..24
    pub by_hour: Vec<u32>,
    pub by_domain: Vec<(String, u32)>,
    pub done: u32,
    pub approved: u32,
    pub declined: u32,
    pub failed: u32,
    /// share of finished actions that went well, 0..1
    pub success: f32,
    /// actions on each of the last seven local days, oldest first (today last)
    pub week: Vec<u32>,
    /// answered without the model (phrases, shortcuts): fast and private
    pub without_model: u32,
    pub busiest_hour: Option<u32>,
}

fn outcome(e: &Episode) -> &'static str {
    if e.signals.iter().any(|s| s == "declined") {
        "declined"
    } else if e.signals.iter().any(|s| s == "approved") {
        "approved"
    } else if e.reward < 0.0 {
        "failed"
    } else if e.reward > 0.0 {
        "done"
    } else {
        "open"
    }
}

/// The local day that `now_ms` falls in, with the clock `offset_min` minutes from UTC.
pub fn day_report(episodes: &[Episode], now_ms: u64, offset_min: i32) -> Day {
    let off = offset_min as i64 * 60_000;
    let local = |t: u64| (t as i64 + off).max(0) as u64;
    let today = local(now_ms) / DAY_MS;
    let mut d = Day {
        by_hour: vec![0; 24],
        week: vec![0; 7],
        ..Default::default()
    };
    let mut domains: HashMap<String, u32> = HashMap::new();
    for e in episodes {
        let lt = local(e.t_ms);
        let day = lt / DAY_MS;
        if day <= today && today - day < 7 {
            d.week[(6 - (today - day)) as usize] += 1;
        }
        if day != today {
            continue;
        }
        let tool = tool_of(&e.action);
        let domain = if tool.is_empty() {
            "answer".to_string()
        } else {
            domain_of(&tool).to_string()
        };
        let mins = (lt % DAY_MS) / 60_000;
        let o = outcome(e);
        match o {
            "done" => d.done += 1,
            "approved" => d.approved += 1,
            "declined" => d.declined += 1,
            "failed" => d.failed += 1,
            _ => {}
        }
        if e.source != "model" {
            d.without_model += 1;
        }
        d.by_hour[(mins / 60) as usize] += 1;
        *domains.entry(domain.clone()).or_default() += 1;
        d.flow.push(Step {
            t_ms: e.t_ms,
            at: format!("{:02}:{:02}", mins / 60, mins % 60),
            prompt: e.prompt.clone(),
            tool,
            domain,
            outcome: o.into(),
            source: e.source.clone(),
        });
    }
    d.flow.sort_by_key(|s| s.t_ms);
    let mut by: Vec<(String, u32)> = domains.into_iter().collect();
    by.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    d.by_domain = by;
    let finished = d.done + d.approved + d.declined + d.failed;
    d.success = if finished == 0 {
        0.0
    } else {
        (d.done + d.approved) as f32 / finished as f32
    };
    d.busiest_hour = d
        .by_hour
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .max_by_key(|(h, n)| (**n, std::cmp::Reverse(*h)))
        .map(|(h, _)| h as u32);
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(
        t_ms: u64,
        prompt: &str,
        tool: &str,
        reward: f32,
        signals: &[&str],
        source: &str,
    ) -> Episode {
        Episode {
            id: t_ms,
            t_ms,
            prompt: prompt.into(),
            action: if tool.is_empty() {
                String::new()
            } else {
                format!(r#"{{"tool":"{tool}","args":{{}}}}"#)
            },
            source: source.into(),
            reward,
            signals: signals.iter().map(|s| s.to_string()).collect(),
            corrected_to: None,
        }
    }

    #[test]
    fn today_is_a_flow_with_outcomes_hours_and_domains() {
        // 2026-11-12 09:00 UTC is 14:30 in India (+5:30)
        let now = 1_762_938_000_000u64;
        let h = 3_600_000;
        let eps = vec![
            ep(now - 5 * h, "weather", "weather", 0.3, &[], "intent"),
            ep(now - 2 * h, "call mum", "call", 1.0, &["approved"], "model"),
            ep(
                now - h,
                "text arjun",
                "send_sms",
                -1.0,
                &["declined"],
                "model",
            ),
            ep(
                now - 30 * 60_000,
                "torch on",
                "flashlight",
                0.3,
                &[],
                "shortcut",
            ),
            ep(now - 3 * 24 * h, "old", "weather", 0.3, &[], "intent"),
            ep(now - 9 * 24 * h, "too old", "weather", 0.3, &[], "intent"),
        ];
        let d = day_report(&eps, now, 330);
        assert_eq!(d.flow.len(), 4);
        assert_eq!(d.flow[0].prompt, "weather");
        assert_eq!(d.flow[0].at, "09:30");
        assert_eq!(d.flow[1].outcome, "approved");
        assert_eq!(d.flow[2].outcome, "declined");
        assert_eq!((d.done, d.approved, d.declined), (2, 1, 1));
        assert!((d.success - 0.75).abs() < 1e-6);
        assert_eq!(d.without_model, 2);
        assert_eq!(d.week, vec![0, 0, 0, 1, 0, 0, 4]);
        assert_eq!(d.by_hour.iter().sum::<u32>(), 4);
        assert!(d.by_domain.iter().any(|(k, n)| k == "phone" && *n == 1));
    }

    #[test]
    fn an_empty_day_is_empty() {
        let d = day_report(&[], 1_762_938_000_000, 330);
        assert!(d.flow.is_empty());
        assert_eq!(d.success, 0.0);
        assert_eq!(d.busiest_hour, None);
    }
}
