//! Overnight training: teach the local model the requests the human confirmed.
//!
//! On the charger, in the Deep stage, with enough battery, a cool phone and enough new confirmed
//! requests since the last run, agentd exports the training data and runs `agentos-llm train` in
//! the model's cgroup. The job checkpoints; when the phone is unplugged, gets hot or is needed it
//! is stopped and picks up from the checkpoint next time. A finished job keeps its adapter only
//! if it answers held-out requests better than the model without it; then the model server is
//! restarted with it. The previous adapter is kept for `learn rollback`. Every step is recorded
//! in the ledger.
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// Why training may not run now; None: it may.
pub struct Conditions {
    pub deep: bool,
    pub charging: bool,
    pub battery: u8,
    pub min_battery: u8,
    pub temperature: Option<f32>,
    pub cool_c: f32,
    pub busy: bool,
    pub new_examples: usize,
    pub min_new: usize,
    pub forced: bool,
}

impl Conditions {
    /// What stops training, if anything. A forced run (`learn train`) skips the schedule
    /// (Deep stage, the count of new requests), never the battery or the heat.
    pub fn blocker(&self) -> Option<String> {
        if !self.charging {
            return Some("not on the charger".into());
        }
        if self.battery < self.min_battery {
            return Some(format!("battery {}% (needs {}%)", self.battery, self.min_battery));
        }
        if let Some(t) = self.temperature.filter(|t| *t >= self.cool_c) {
            return Some(format!("phone at {t:.0}°C (needs below {:.0}°C)", self.cool_c));
        }
        if self.busy {
            return Some("the model is busy".into());
        }
        if self.forced {
            return None;
        }
        if !self.deep {
            return Some("not in the Deep stage".into());
        }
        if self.new_examples < self.min_new {
            return Some(format!(
                "{} new confirmed requests since the last run (needs {})",
                self.new_examples, self.min_new
            ));
        }
        None
    }
}

pub struct Running {
    child: Child,
    out: PathBuf,
    /// labelled examples in the data this run trains on
    examples: usize,
}

pub struct Trainer {
    dir: PathBuf,
    pub running: Option<Running>,
    /// how many labelled examples the last finished (or failed) run had
    pub trained_on: usize,
    pub last: Option<String>,
    pub forced: bool,
}

/// What happened, for the ledger and for `learn status`.
pub enum Outcome {
    Kept { adapter: PathBuf, base: f64, tuned: f64 },
    Discarded { base: f64, tuned: f64 },
    Paused { step: u64 },
    Failed(String),
}

impl Trainer {
    pub fn new(data_dir: &Path) -> Trainer {
        let dir = data_dir.join("learn");
        let trained_on = std::fs::read_to_string(dir.join("trained_on"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        Trainer {
            dir,
            running: None,
            trained_on,
            last: None,
            forced: false,
        }
    }

    pub fn current(&self) -> PathBuf {
        self.dir.join("adapter.lora")
    }
    fn previous(&self) -> PathBuf {
        self.dir.join("adapter.prev.lora")
    }
    fn candidate(&self) -> PathBuf {
        self.dir.join("adapter.new.lora")
    }

    /// The adapter the model should be served with, if there is one.
    pub fn active(&self) -> Option<PathBuf> {
        let p = self.current();
        p.exists().then_some(p)
    }

    /// Start a job on the exported data (sft.jsonl, grammar.gbnf in the learn directory).
    pub fn start(&mut self, bin: &Path, model: &Path, examples: usize, steps: usize, chunk_secs: u64) -> Result<u32, String> {
        let out = self.dir.join("train.out");
        let log = std::fs::File::create(self.dir.join("train.log")).map_err(|e| e.to_string())?;
        let stdout = std::fs::File::create(&out).map_err(|e| e.to_string())?;
        let mut cmd = Command::new(bin);
        cmd.arg("train")
            .arg("-m")
            .arg(model)
            .arg("--data")
            .arg(self.dir.join("sft.jsonl"))
            .arg("--grammar")
            .arg(self.dir.join("grammar.gbnf"))
            .arg("--out")
            .arg(self.candidate())
            .arg("--steps")
            .arg(steps.to_string())
            .arg("--max-seconds")
            .arg(chunk_secs.to_string())
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(log);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                cmd.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                    Ok(())
                });
            }
        }
        let child = cmd.spawn().map_err(|e| format!("cannot start {}: {e}", bin.display()))?;
        let pid = child.id();
        self.running = Some(Running { child, out, examples });
        Ok(pid)
    }

    /// Stop a running job. Its checkpoint stays, so the next start continues it.
    pub fn stop(&mut self) -> bool {
        match self.running.take() {
            Some(mut r) => {
                let _ = r.child.kill();
                let _ = r.child.wait();
                true
            }
            None => false,
        }
    }

    /// Has the job finished? Then what came of it.
    pub fn poll(&mut self) -> Option<Outcome> {
        let r = self.running.as_mut()?;
        let status = match r.child.try_wait() {
            Ok(None) => return None,
            Ok(Some(s)) => s,
            Err(e) => {
                self.running = None;
                return Some(Outcome::Failed(e.to_string()));
            }
        };
        let r = self.running.take().unwrap();
        let text = std::fs::read_to_string(&r.out).unwrap_or_default();
        let summary = text
            .lines()
            .rev()
            .find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok());
        let outcome = match summary {
            None => {
                let log = std::fs::read_to_string(self.dir.join("train.log")).unwrap_or_default();
                let why = log.lines().last().unwrap_or("").to_string();
                Outcome::Failed(format!("exited {status}: {why}"))
            }
            Some(v) if v["done"] == false => Outcome::Paused {
                step: v["step"].as_u64().unwrap_or(0),
            },
            Some(v) => {
                let (base, tuned) = (v["base"].as_f64().unwrap_or(0.0), v["tuned"].as_f64().unwrap_or(0.0));
                if v["kept"] == true {
                    match self.promote() {
                        Ok(adapter) => Outcome::Kept { adapter, base, tuned },
                        Err(e) => Outcome::Failed(e),
                    }
                } else {
                    Outcome::Discarded { base, tuned }
                }
            }
        };
        if !matches!(outcome, Outcome::Paused { .. }) {
            self.forced = false;
            self.trained_on = r.examples;
            let _ = std::fs::write(self.dir.join("trained_on"), r.examples.to_string());
        }
        Some(outcome)
    }

    /// The new adapter becomes the current one; the current one becomes the previous one.
    fn promote(&self) -> Result<PathBuf, String> {
        if self.current().exists() {
            std::fs::rename(self.current(), self.previous()).map_err(|e| e.to_string())?;
        }
        std::fs::rename(self.candidate(), self.current()).map_err(|e| e.to_string())?;
        Ok(self.current())
    }

    /// Go back to the previous adapter, or to none. Returns what is active now.
    pub fn rollback(&self) -> Result<Option<PathBuf>, String> {
        if !self.current().exists() {
            return Err("no trained adapter is in use".into());
        }
        if self.previous().exists() {
            std::fs::rename(self.previous(), self.current()).map_err(|e| e.to_string())?;
            Ok(Some(self.current()))
        } else {
            std::fs::remove_file(self.current()).map_err(|e| e.to_string())?;
            Ok(None)
        }
    }
}

impl Drop for Trainer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> Conditions {
        Conditions {
            deep: true,
            charging: true,
            battery: 90,
            min_battery: 50,
            temperature: Some(40.0),
            cool_c: 70.0,
            busy: false,
            new_examples: 30,
            min_new: 20,
            forced: false,
        }
    }

    #[test]
    fn trains_only_when_the_phone_can_spare_it() {
        assert!(ok().blocker().is_none());
        assert!(Conditions { charging: false, ..ok() }.blocker().unwrap().contains("charger"));
        assert!(Conditions { battery: 30, ..ok() }.blocker().unwrap().contains("battery"));
        assert!(Conditions { temperature: Some(75.0), ..ok() }.blocker().unwrap().contains("°C"));
        assert!(Conditions { deep: false, ..ok() }.blocker().unwrap().contains("Deep"));
        assert!(Conditions { new_examples: 5, ..ok() }.blocker().unwrap().contains("new confirmed"));
        // asked for: the schedule is skipped, never the battery or the heat
        let forced = Conditions { deep: false, new_examples: 0, forced: true, ..ok() };
        assert!(forced.blocker().is_none());
        assert!(Conditions { temperature: Some(75.0), ..forced }.blocker().is_some());
    }

    #[test]
    fn adapters_are_promoted_and_rolled_back() {
        let d = std::env::temp_dir().join(format!("agentos-trainer-{}", std::process::id()));
        std::fs::create_dir_all(d.join("learn")).unwrap();
        let t = Trainer::new(&d);
        std::fs::write(t.candidate(), "one").unwrap();
        t.promote().unwrap();
        std::fs::write(t.candidate(), "two").unwrap();
        t.promote().unwrap();
        assert_eq!(std::fs::read_to_string(t.current()).unwrap(), "two");
        assert_eq!(t.rollback().unwrap(), Some(t.current()));
        assert_eq!(std::fs::read_to_string(t.current()).unwrap(), "one");
        assert_eq!(t.rollback().unwrap(), None);
        assert!(t.active().is_none());
        assert!(t.rollback().is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
