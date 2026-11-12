//! The two transports: a spool folder shared with the Android side, and an in-memory simulator
//! for tests and for `bridge = "sim"`.
use crate::{clean_name, rel_ok, Command, Event, Transport, MAX_EVENT_BYTES, MAX_TEXT_CHARS};
use std::path::{Path, PathBuf};

/// The spool folder: `out/` (Android writes events), `in/` (we write commands), `files/`
/// (attachments either side has copied for the other).
pub struct Spool {
    pub root: PathBuf,
}

impl Spool {
    pub fn new(root: PathBuf) -> Result<Spool, String> {
        for sub in ["out", "in", "files"] {
            std::fs::create_dir_all(root.join(sub))
                .map_err(|e| format!("{}: {e}", root.display()))?;
        }
        Ok(Spool { root })
    }
}

impl Transport for Spool {
    fn take_events(&mut self) -> Vec<Event> {
        let dir = self.root.join("out");
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        paths.sort();
        let mut events = Vec::new();
        for p in paths {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            // a file still being written has a .tmp name; leave it alone
            if !name.ends_with(".json") {
                continue;
            }
            let too_big = std::fs::metadata(&p)
                .map(|m| m.len() > MAX_EVENT_BYTES)
                .unwrap_or(true);
            if too_big {
                let _ = std::fs::remove_file(&p);
                continue;
            }
            let parsed = std::fs::read_to_string(&p)
                .ok()
                .and_then(|t| serde_json::from_str::<Event>(&t).ok());
            let _ = std::fs::remove_file(&p);
            if let Some(mut e) = parsed {
                e.text = e.text.chars().take(MAX_TEXT_CHARS).collect();
                events.push(e);
            }
        }
        events
    }

    fn send(&mut self, c: &Command) -> Result<(), String> {
        if c.id.is_empty()
            || !c
                .id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        {
            return Err("command id must be letters, digits, - or _".into());
        }
        let dir = self.root.join("in");
        let body = serde_json::to_string(c).map_err(|e| e.to_string())?;
        let tmp = dir.join(format!("{}.json.tmp", c.id));
        std::fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, dir.join(format!("{}.json", c.id))).map_err(|e| e.to_string())
    }

    fn file(&self, rel: &str) -> Option<PathBuf> {
        if !rel_ok(rel) {
            return None;
        }
        let p = self.root.join(rel);
        p.is_file().then_some(p)
    }

    fn offer_file(&mut self, from: &Path, name: &str) -> Result<String, String> {
        let clean = clean_name(name).ok_or("a plain file name")?;
        let rel = format!("files/out-{clean}");
        std::fs::copy(from, self.root.join(&rel))
            .map_err(|e| format!("{}: {e}", from.display()))?;
        Ok(rel)
    }

    fn up(&self) -> bool {
        self.root.join("out").is_dir()
    }
}

/// The simulator. Events are injected by `agentctl bridge sim`; commands are recorded for tests.
pub struct Sim {
    pub root: PathBuf,
    pub events: Vec<Event>,
    pub sent: Vec<Command>,
}

impl Sim {
    pub fn new(root: PathBuf) -> Result<Sim, String> {
        std::fs::create_dir_all(root.join("files"))
            .map_err(|e| format!("{}: {e}", root.display()))?;
        Ok(Sim {
            root,
            events: Vec::new(),
            sent: Vec::new(),
        })
    }
}

impl Transport for Sim {
    fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }
    fn send(&mut self, c: &Command) -> Result<(), String> {
        self.sent.push(c.clone());
        Ok(())
    }
    fn file(&self, rel: &str) -> Option<PathBuf> {
        if !rel_ok(rel) {
            return None;
        }
        let p = self.root.join(rel);
        p.is_file().then_some(p)
    }
    fn offer_file(&mut self, from: &Path, name: &str) -> Result<String, String> {
        let clean = clean_name(name).ok_or("a plain file name")?;
        let rel = format!("files/out-{clean}");
        std::fs::copy(from, self.root.join(&rel))
            .map_err(|e| format!("{}: {e}", from.display()))?;
        Ok(rel)
    }
    fn up(&self) -> bool {
        true
    }
    fn sent_count(&self) -> usize {
        self.sent.len()
    }
    fn inject(&mut self, e: Event) -> bool {
        self.events.push(e);
        true
    }
}
