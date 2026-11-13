//! The predictive cache, agentd's side: talks to the model server's cache (`GET /cache`,
//! `POST /cache/policy`, `POST /cache/warm`), keeps the manager's record of which warm-ups were
//! used, and runs warm-ups on a thread so the daemon never waits on a model reading a prompt.
//! The decisions themselves are `agentos_learn::predict::plan`.
use agentos_learn::predict::{kind_of, Cached, Plan, Record};
use agentos_learn::Episode;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

/// A warm-up counts as used when a request of its kind comes within this long.
pub const WARM_WINDOW_MS: u64 = 20 * 60_000;

pub struct CacheMgr {
    path: PathBuf,
    pub last_ms: u64,
    /// on/off from `cache on|off`; None follows the config
    pub enabled: Option<bool>,
    pub record: Record,
    pub plan: Option<Plan>,
    /// the server's last report
    pub server: Option<Value>,
    pub error: Option<String>,
    /// warm-ups not yet used: adapter and when
    pub warm_log: Vec<(String, u64)>,
    /// episodes up to this id have been credited
    pub seen_episode: u64,
    /// the local hour (days*24+hour) Android was last woken ahead for
    pub woke_android: Option<i64>,
    /// recent decisions, newest last
    pub log: Vec<(u64, String)>,
    warming: Option<Receiver<(String, Result<Value, String>)>>,
}

pub fn get(url: &str, timeout: Duration) -> Result<Value, String> {
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    agent
        .get(&format!("{url}/cache"))
        .call()
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())
}

pub fn post(url: &str, path: &str, body: &Value, timeout: Duration) -> Result<Value, String> {
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    agent
        .post(&format!("{url}{path}"))
        .send_json(body.clone())
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())
}

/// Free memory (MemAvailable), where /proc can be read.
pub fn mem_free_mb(proc_root: &Path) -> Option<u64> {
    let t = std::fs::read_to_string(proc_root.join("meminfo")).ok()?;
    t.lines()
        .find(|l| l.starts_with("MemAvailable:"))?
        .split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()
        .map(|kb| kb / 1024)
}

/// The cached states in a server report.
pub fn entries(server: &Value) -> Vec<Cached> {
    server["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| serde_json::from_value(e.clone()).ok())
        .collect()
}

/// The experts (named adapters) a server can switch to.
pub fn experts(server: &Value) -> Vec<String> {
    server["adapters"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a.as_str())
        .filter(|a| !a.is_empty())
        .map(String::from)
        .collect()
}

impl CacheMgr {
    pub fn open(dir: &Path) -> CacheMgr {
        let path = dir.join("cache.json");
        CacheMgr {
            record: Record::load(&path),
            path,
            last_ms: 0,
            enabled: None,
            plan: None,
            server: None,
            error: None,
            warm_log: vec![],
            seen_episode: 0,
            woke_android: None,
            log: vec![],
            warming: None,
        }
    }

    pub fn note(&mut self, now: u64, line: String) {
        self.log.push((now, line));
        if self.log.len() > 30 {
            self.log.remove(0);
        }
    }

    /// Credit warm-ups that a request then used; forget those that went unused too long.
    /// Returns how many were credited.
    pub fn credit(&mut self, episodes: &[Episode], experts: &[String], now: u64) -> usize {
        let mut n = 0;
        let seen = self.seen_episode;
        for e in episodes.iter().filter(|e| e.id > seen) {
            self.seen_episode = self.seen_episode.max(e.id);
            if e.source != "model" {
                continue;
            }
            let kind = kind_of(e);
            let adapter = if experts.contains(&kind) {
                kind
            } else {
                String::new()
            };
            // the expert's own warm-up, and the base prompt's (every model request reads it)
            for want in [adapter.clone(), String::new()] {
                if let Some(i) = self
                    .warm_log
                    .iter()
                    .position(|(a, t)| *a == want && *t <= e.t_ms && e.t_ms - t <= WARM_WINDOW_MS)
                {
                    self.warm_log.remove(i);
                    *self.record.used.entry(want).or_default() += 1;
                    n += 1;
                }
                if adapter.is_empty() {
                    break;
                }
            }
        }
        self.warm_log
            .retain(|(_, t)| now.saturating_sub(*t) <= WARM_WINDOW_MS);
        if n > 0 {
            let _ = self.record.save(&self.path);
        }
        n
    }

    pub fn warming(&self) -> bool {
        self.warming.is_some()
    }

    /// Read the system prompt for each adapter on a thread (the server does one at a time).
    pub fn start_warm(
        &mut self,
        url: &str,
        system: &str,
        adapters: Vec<String>,
        timeout: Duration,
    ) {
        if self.warming.is_some() || adapters.is_empty() {
            return;
        }
        let (tx, rx) = channel();
        let (url, system) = (url.to_string(), system.to_string());
        std::thread::spawn(move || {
            for a in adapters {
                let body = json!({
                    "messages": [{"role": "system", "content": system}],
                    "adapter": a,
                    "label": if a.is_empty() { "system".to_string() } else { a.clone() },
                });
                let r = post(&url, "/cache/warm", &body, timeout);
                if tx.send((a, r)).is_err() {
                    break;
                }
            }
        });
        self.warming = Some(rx);
    }

    /// Collect finished warm-ups. Returns lines for the ledger.
    pub fn poll_warm(&mut self, now: u64) -> Vec<String> {
        let mut lines = vec![];
        let Some(rx) = self.warming.take() else {
            return lines;
        };
        let mut done = false;
        loop {
            match rx.try_recv() {
                Ok((a, Ok(v))) => {
                    let name = if a.is_empty() {
                        "system prompt".to_string()
                    } else {
                        format!("{a} expert")
                    };
                    let w = &v["warmed"];
                    let line = if w["already"].as_bool() == Some(true) {
                        format!("cache: {name} already warm")
                    } else {
                        *self.record.warmed.entry(a.clone()).or_default() += 1;
                        self.warm_log.push((a.clone(), now));
                        format!(
                            "cache: warmed the {name}: {} tokens, {} MB, {:.1} s{}",
                            w["tokens"].as_u64().unwrap_or(0),
                            w["bytes"].as_u64().unwrap_or(0) >> 20,
                            w["ms"].as_f64().unwrap_or(0.0) / 1000.0,
                            if w["kept"].as_bool() == Some(false) {
                                " (did not fit the budget)"
                            } else {
                                ""
                            }
                        )
                    };
                    self.server = Some(v);
                    self.note(now, line.clone());
                    lines.push(line);
                }
                Ok((a, Err(e))) => {
                    let line = format!(
                        "cache: could not warm {}: {e}",
                        if a.is_empty() {
                            "the system prompt"
                        } else {
                            &a
                        }
                    );
                    self.note(now, line.clone());
                    lines.push(line);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }
        if done {
            let _ = self.record.save(&self.path);
        } else {
            self.warming = Some(rx);
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(id: u64, t_ms: u64, tool: &str, source: &str) -> Episode {
        Episode {
            id,
            t_ms,
            prompt: tool.into(),
            action: format!(r#"{{"tool":"{tool}","args":{{}}}}"#),
            source: source.into(),
            reward: 1.0,
            signals: vec![],
            corrected_to: None,
        }
    }

    #[test]
    fn a_request_soon_after_a_warm_up_credits_it_and_old_ones_expire() {
        let d = std::env::temp_dir().join(format!("agentd-cache-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut m = CacheMgr::open(&d);
        let t = 10_000_000;
        m.warm_log = vec![
            (String::new(), t),
            ("messages".into(), t),
            ("money".into(), t),
        ];
        let experts = vec!["messages".to_string(), "money".to_string()];
        let eps = vec![
            ep(1, t + 60_000, "send_sms", "model"),
            ep(2, t + 70_000, "flashlight", "intent"),
        ];
        assert_eq!(m.credit(&eps, &experts, t + 80_000), 2);
        assert_eq!(m.record.used.get("messages"), Some(&1));
        assert_eq!(m.record.used.get(""), Some(&1));
        // nothing is credited twice; the unused money warm-up expires
        assert_eq!(m.credit(&eps, &experts, t + WARM_WINDOW_MS + 1), 0);
        assert!(m.warm_log.is_empty());
        // the record survives a restart
        assert_eq!(CacheMgr::open(&d).record.used.get("messages"), Some(&1));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn reports_are_read_into_entries_and_experts() {
        let v = json!({"adapters": ["", "messages"], "entries": [
            {"id": 3, "adapter": "", "label": "system", "tokens": 900, "bytes": 1048576, "hits": 2, "idle": 1, "priority": 0.5, "warmed": true}
        ]});
        assert_eq!(experts(&v), vec!["messages".to_string()]);
        let e = entries(&v);
        assert_eq!((e[0].id, e[0].tokens, e[0].hits), (3, 900, 2));
        let d = std::env::temp_dir().join(format!("agentd-mem-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("meminfo"),
            "MemTotal: 8000000 kB\nMemAvailable:    2048000 kB\n",
        )
        .unwrap();
        assert_eq!(mem_free_mb(&d), Some(2000));
        let _ = std::fs::remove_dir_all(&d);
    }
}
