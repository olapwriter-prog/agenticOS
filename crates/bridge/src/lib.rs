//! The bridge to the Android sandbox. Android sends events (notifications, new files, shares) and
//! receives commands (replies, opening an app, sharing a file). They move as small JSON files in a
//! spool folder, one file per event or command, written under a temporary name and renamed when
//! complete, so neither side reads half a file. No network and no sockets.
//!
//! Everything that comes from Android is data: it is shown and quoted, never run as a command,
//! and never used as a path. Ledger lines name the app, never the message text.
#[cfg(test)]
mod tests;
pub mod transport;
pub mod triage;

use agentos_action::{Action, Tier, Tool};
use agentos_executor::Connector;
use agentos_screen::{Card, Ui};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub use transport::{Sim, Spool};

pub const MAX_EVENT_BYTES: u64 = 64 * 1024;
pub const MAX_TEXT_CHARS: usize = 4000;
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
pub const INBOX_KEEP: usize = 500;
pub const FILES_KEEP: usize = 200;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Attachment {
    pub name: String,
    pub rel: String,
    pub mime: String,
    pub size: u64,
    pub too_big: bool,
}

/// One event from Android, as written by the bridge app (plan section 1.1).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Event {
    pub v: u32,
    pub id: String,
    pub kind: String,
    pub time_ms: u64,
    pub package: String,
    pub app: String,
    pub conversation: String,
    pub sender: String,
    pub text: String,
    pub reply_key: String,
    pub attachments: Vec<Attachment>,
    pub ok: bool,
    pub error: String,
    pub app_version: String,
    pub android_sdk: u32,
}

/// One command for Android (plan section 1.2).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Command {
    pub v: u32,
    pub id: String,
    pub kind: String,
    pub reply_key: String,
    pub text: String,
    pub package: String,
    pub rel: String,
    pub mime: String,
}

/// A message kept in the native inbox.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: String,
    pub time_ms: u64,
    pub package: String,
    pub app: String,
    pub conversation: String,
    pub sender: String,
    pub text: String,
    pub reply_key: String,
    /// names of the files copied into the documents folder
    pub files: Vec<String>,
    pub read: bool,
    pub replied: bool,
}

/// What the owner allowed for one app. Everything starts off.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AppRule {
    pub package: String,
    pub app: String,
    pub read: bool,
    pub reply: bool,
    pub files: bool,
}

/// A file that arrived from Android and was copied into the documents folder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Received {
    pub name: String,
    pub from_app: String,
    pub conversation: String,
    pub time_ms: u64,
    pub size: u64,
    pub mime: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub inbox: Vec<Message>,
    pub rules: Vec<AppRule>,
    pub received: Vec<Received>,
    pub rejected: u64,
    pub last_hello_ms: u64,
}

/// Where events come from and commands go to.
pub trait Transport {
    /// Every waiting event, oldest first. Each event is returned once.
    fn take_events(&mut self) -> Vec<Event>;
    fn send(&mut self, c: &Command) -> Result<(), String>;
    /// Absolute path of an attachment, if it is inside the spool and exists.
    fn file(&self, rel: &str) -> Option<PathBuf>;
    /// Copy a host file into the spool so Android can read it; returns its `rel`.
    fn offer_file(&mut self, from: &Path, name: &str) -> Result<String, String>;
    fn up(&self) -> bool;
    /// Commands Android has not picked up yet (a paused Android picks up nothing).
    fn waiting_commands(&self) -> usize {
        0
    }
    /// How many commands were sent (the simulator counts; the spool does not need to).
    fn sent_count(&self) -> usize {
        0
    }
    /// Only the simulator accepts injected events; the others refuse.
    fn inject(&mut self, _e: Event) -> bool {
        false
    }
}

/// A name a file may keep: its last component, with anything odd replaced, at most 120 characters.
pub fn clean_name(name: &str) -> Option<String> {
    let base = Path::new(name).file_name()?.to_str()?;
    if base.is_empty() || base == "." || base == ".." {
        return None;
    }
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || " ._()-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(120)
        .collect();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '.') {
        return None;
    }
    Some(cleaned)
}

/// A spool path must be `files/…`, with no `..`, no backslash and no leading slash.
pub fn rel_ok(rel: &str) -> bool {
    rel.starts_with("files/")
        && !rel.contains("..")
        && !rel.contains('\\')
        && !rel.starts_with('/')
        && rel.len() > "files/".len()
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let mut n = 2;
    loop {
        let p = dir.join(format!("{stem} ({n}){ext}"));
        if !p.exists() {
            return p;
        }
        n += 1;
    }
}

/// The bridge: pulls events from Android into the inbox, and sends commands back.
pub struct Bridge {
    pub transport: Box<dyn Transport>,
    pub state: State,
    pub state_path: PathBuf,
    /// where received files are copied: <data_dir>/documents/android
    pub docs_dir: PathBuf,
}

impl Bridge {
    pub fn load(transport: Box<dyn Transport>, data_dir: &Path) -> Bridge {
        let state_path = data_dir.join("bridge.json");
        let state = std::fs::read_to_string(&state_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let docs_dir = data_dir.join("documents").join("android");
        let _ = std::fs::create_dir_all(&docs_dir);
        Bridge {
            transport,
            state,
            state_path,
            docs_dir,
        }
    }

    /// Writes the state under a temporary name, then renames it.
    pub fn save(&self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&self.state).map_err(|e| e.to_string())?;
        let tmp = self.state_path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &self.state_path).map_err(|e| e.to_string())
    }

    pub fn rule(&self, package: &str) -> AppRule {
        self.state
            .rules
            .iter()
            .find(|r| r.package == package)
            .cloned()
            .unwrap_or_else(|| AppRule {
                package: package.to_string(),
                ..Default::default()
            })
    }

    /// Sets one permission: `what` is `read`, `reply` or `files`.
    pub fn set_rule(&mut self, package: &str, what: &str, on: bool) -> Result<(), String> {
        let idx = match self.state.rules.iter().position(|r| r.package == package) {
            Some(i) => i,
            None => {
                self.state.rules.push(AppRule {
                    package: package.to_string(),
                    app: package.to_string(),
                    ..Default::default()
                });
                self.state.rules.len() - 1
            }
        };
        let r = &mut self.state.rules[idx];
        match what {
            "read" => r.read = on,
            "reply" => r.reply = on,
            "files" => r.files = on,
            _ => {
                return Err(format!(
                    "unknown permission {what}: use read, reply or files"
                ))
            }
        }
        Ok(())
    }

    /// Delete a conversation from the inbox, and dismiss its notifications in Android.
    /// Returns how many messages went.
    pub fn delete_conversation(&mut self, conversation: &str) -> usize {
        let keys: Vec<String> = self
            .state
            .inbox
            .iter()
            .filter(|m| m.conversation == conversation && !m.reply_key.is_empty())
            .map(|m| m.reply_key.clone())
            .collect();
        let before = self.state.inbox.len();
        self.state.inbox.retain(|m| m.conversation != conversation);
        for (i, k) in keys.iter().enumerate() {
            let _ = self.transport.send(&Command {
                v: 1,
                id: format!("c-dismiss-{}-{i}", before),
                kind: "dismiss".into(),
                reply_key: k.clone(),
                ..Default::default()
            });
        }
        let _ = self.save();
        before - self.state.inbox.len()
    }

    pub fn unread(&self) -> usize {
        self.state.inbox.iter().filter(|m| !m.read).count()
    }

    /// Pulls every waiting event, applies the owner's rules and returns one ledger line per
    /// accepted event. The lines name the app and never carry message text.
    pub fn pump(&mut self, now_ms: u64) -> Vec<String> {
        let events = self.transport.take_events();
        let mut lines = Vec::new();
        let mut changed = false;
        for e in events {
            match e.kind.as_str() {
                "hello" => {
                    self.state.last_hello_ms = now_ms;
                    changed = true;
                }
                "removed" => {
                    for m in self
                        .state
                        .inbox
                        .iter_mut()
                        .filter(|m| m.reply_key == e.reply_key)
                    {
                        m.read = true;
                    }
                    changed = true;
                }
                "result" => {
                    if !e.ok {
                        lines.push("bridge: android refused a command".to_string());
                    }
                }
                "notification" => {
                    if !self.state.rules.iter().any(|r| r.package == e.package) {
                        // a new app shows in settings with everything off, and its message is dropped
                        self.state.rules.push(AppRule {
                            package: e.package.clone(),
                            app: if e.app.is_empty() {
                                e.package.clone()
                            } else {
                                e.app.clone()
                            },
                            ..Default::default()
                        });
                        changed = true;
                        continue;
                    }
                    let rule = self.rule(&e.package);
                    if !rule.read {
                        continue;
                    }
                    let app = if e.app.is_empty() {
                        e.package.clone()
                    } else {
                        e.app.clone()
                    };
                    let conversation = if e.conversation.is_empty() {
                        e.sender.clone()
                    } else {
                        e.conversation.clone()
                    };
                    let files = if rule.files {
                        self.import_all(&e, &app, &conversation)
                    } else {
                        Vec::new()
                    };
                    self.state.inbox.push(Message {
                        id: e.id.clone(),
                        time_ms: e.time_ms,
                        package: e.package.clone(),
                        app: app.clone(),
                        conversation,
                        sender: e.sender.clone(),
                        text: cut(&e.text),
                        reply_key: e.reply_key.clone(),
                        files,
                        read: false,
                        replied: false,
                    });
                    let extra = self.state.inbox.len().saturating_sub(INBOX_KEEP);
                    self.state.inbox.drain(..extra);
                    lines.push(format!("bridge: message from {app}"));
                    changed = true;
                }
                "file" | "share" => {
                    let allowed = e.kind == "share"
                        || if e.package.is_empty() {
                            self.rule("*").files
                        } else {
                            self.rule(&e.package).files
                        };
                    if !allowed || e.attachments.is_empty() {
                        continue;
                    }
                    let app = if e.app.is_empty() {
                        "Android".to_string()
                    } else {
                        e.app.clone()
                    };
                    let got = self.import_all(&e, &app, "");
                    if !got.is_empty() {
                        lines.push(format!("bridge: file from {app}"));
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        if changed {
            let _ = self.save();
        }
        lines
    }

    fn import_all(&mut self, e: &Event, from_app: &str, conversation: &str) -> Vec<String> {
        let mut names = Vec::new();
        for a in &e.attachments {
            if let Some(name) = self.import(a, e.time_ms, from_app, conversation) {
                names.push(name);
            }
        }
        names
    }

    /// Copies one attachment into the documents folder. Returns the name it was kept under.
    fn import(
        &mut self,
        a: &Attachment,
        time_ms: u64,
        from_app: &str,
        conversation: &str,
    ) -> Option<String> {
        if !rel_ok(&a.rel) {
            self.state.rejected += 1;
            return None;
        }
        let Some(src) = self.transport.file(&a.rel) else {
            self.state.rejected += 1;
            return None;
        };
        let size = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
        if size > MAX_FILE_BYTES {
            self.state.rejected += 1;
            return None;
        }
        let Some(name) = clean_name(&a.name) else {
            self.state.rejected += 1;
            return None;
        };
        let dest = unique_path(&self.docs_dir, &name);
        if std::fs::copy(&src, &dest).is_err() {
            self.state.rejected += 1;
            return None;
        }
        let kept = dest.file_name()?.to_string_lossy().to_string();
        self.state.received.push(Received {
            name: kept.clone(),
            from_app: from_app.to_string(),
            conversation: conversation.to_string(),
            time_ms,
            size,
            mime: a.mime.clone(),
        });
        let extra = self.state.received.len().saturating_sub(FILES_KEEP);
        self.state.received.drain(..extra);
        Some(kept)
    }

    /// The conversation a question names: exact conversation, then exact sender, then a part of
    /// either. More than one different match is an error that lists them.
    pub fn resolve(&self, q: &str) -> Result<String, String> {
        let q = q.trim().to_lowercase();
        if q.is_empty() {
            return Err("name a conversation".into());
        }
        let convs: Vec<&Message> = self.state.inbox.iter().collect();
        let distinct = |hits: Vec<String>| {
            let mut v: Vec<String> = Vec::new();
            for h in hits {
                if !v.contains(&h) {
                    v.push(h);
                }
            }
            v
        };
        for pass in 0..3 {
            let hits: Vec<String> = convs
                .iter()
                .filter(|m| {
                    let c = m.conversation.to_lowercase();
                    let s = m.sender.to_lowercase();
                    match pass {
                        0 => c == q,
                        1 => s == q,
                        _ => c.contains(&q) || s.contains(&q),
                    }
                })
                .map(|m| m.conversation.clone())
                .collect();
            let v = distinct(hits);
            match v.len() {
                0 => continue,
                1 => return Ok(v[0].clone()),
                _ => return Err(format!("more than one: {}", v.join(", "))),
            }
        }
        Err(format!("no conversation called {q}"))
    }

    fn rule_for_app(&self, app: &str) -> Option<AppRule> {
        let want = app.to_lowercase();
        self.state
            .rules
            .iter()
            .find(|r| r.app.to_lowercase() == want || r.package.to_lowercase() == want)
            .cloned()
    }
}

/// Cuts text to the limit, by characters.
fn cut(text: &str) -> String {
    text.chars().take(MAX_TEXT_CHARS).collect()
}

pub type SharedBridge = Rc<RefCell<Bridge>>;

/// The bridge's tools for the executor.
pub struct BridgeTools(pub SharedBridge);

fn arg(a: &Action, name: &str) -> Result<String, String> {
    a.args
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.trim().to_string())
        .ok_or_else(|| format!("missing {name}"))
}

fn short_time(ms: u64) -> String {
    let mins = (ms / 60_000) % (24 * 60);
    format!("{:02}:{:02}", mins / 60, mins % 60)
}

impl Connector for BridgeTools {
    fn name(&self) -> &str {
        "bridge"
    }
    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "android_inbox",
                args: &[],
                tier: Tier::Low,
            },
            Tool {
                name: "android_messages_from",
                args: &["conversation"],
                tier: Tier::Low,
            },
            Tool {
                name: "android_reply",
                args: &["conversation", "text"],
                tier: Tier::Medium,
            },
            Tool {
                name: "android_files",
                args: &[],
                tier: Tier::Low,
            },
            Tool {
                name: "android_share",
                args: &["app", "name"],
                tier: Tier::High,
            },
            Tool {
                name: "android_bridge_status",
                args: &[],
                tier: Tier::Low,
            },
        ]
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        let mut b = self.0.borrow_mut();
        match a.tool.as_str() {
            "android_inbox" => {
                let mut ui = Ui::new("Android messages")
                    .kind("inbox")
                    .layout("board")
                    .accent("coral");
                let mut seen: Vec<String> = Vec::new();
                for m in b.state.inbox.iter().rev() {
                    if seen.contains(&m.conversation) || seen.len() >= 12 {
                        continue;
                    }
                    seen.push(m.conversation.clone());
                    let unread = b
                        .state
                        .inbox
                        .iter()
                        .filter(|x| x.conversation == m.conversation && !x.read)
                        .count();
                    let lines: Vec<String> = b
                        .state
                        .inbox
                        .iter()
                        .rev()
                        .filter(|x| x.conversation == m.conversation)
                        .take(2)
                        .map(|x| x.text.chars().take(80).collect())
                        .collect();
                    let last = lines.last().cloned().unwrap_or_default();
                    let value = if unread > 0 {
                        format!("{unread} new")
                    } else {
                        String::new()
                    };
                    // one row per conversation: tapping it opens the thread
                    ui = ui.card(
                        Card::new(m.conversation.clone())
                            .source(m.app.clone())
                            .item(
                                m.conversation.clone(),
                                last,
                                value,
                                "message",
                                format!("android messages from {}", m.conversation),
                            )
                            .button(
                                "Reply",
                                format!("reply on android to {} saying ", m.conversation),
                            ),
                    );
                }
                if seen.is_empty() {
                    ui = ui.card(
                        Card::new("Nothing yet").subtitle("Messages from allowed apps appear here"),
                    );
                }
                Ok(ui.to_reply())
            }
            "android_messages_from" => {
                let conv = b.resolve(&arg(a, "conversation")?)?;
                let msgs: Vec<Message> = b
                    .state
                    .inbox
                    .iter()
                    .filter(|m| m.conversation == conv)
                    .cloned()
                    .collect();
                let start = msgs.len().saturating_sub(20);
                let mut ui = Ui::new(conv.clone()).kind("thread");
                for m in &msgs[start..] {
                    let mut c = Card::new(if m.sender.is_empty() {
                        conv.clone()
                    } else {
                        m.sender.clone()
                    })
                    .subtitle(short_time(m.time_ms));
                    if !m.text.is_empty() {
                        c = c.line(m.text.clone());
                    }
                    for f in &m.files {
                        c = c.button(format!("Open {f}"), format!("read document {f}"));
                    }
                    ui = ui.card(c);
                }
                for m in b.state.inbox.iter_mut().filter(|m| m.conversation == conv) {
                    m.read = true;
                }
                let _ = b.save();
                Ok(ui.to_reply())
            }
            "android_reply" => {
                let conv = b.resolve(&arg(a, "conversation")?)?;
                let text = arg(a, "text")?;
                if text.is_empty() {
                    return Err("say what to reply".into());
                }
                let target = b
                    .state
                    .inbox
                    .iter()
                    .rev()
                    .find(|m| m.conversation == conv && !m.reply_key.is_empty())
                    .cloned()
                    .ok_or_else(|| format!("nothing to reply to in {conv}"))?;
                let app = if target.app.is_empty() {
                    target.package.clone()
                } else {
                    target.app.clone()
                };
                if !b.rule(&target.package).reply {
                    return Err(format!(
                        "replies to {app} are off; turn them on in Settings > Android bridge"
                    ));
                }
                let cmd = Command {
                    v: 1,
                    id: format!("c-{}-{}", target.time_ms, b.state.inbox.len()),
                    kind: "reply".into(),
                    reply_key: target.reply_key.clone(),
                    text: cut(&text),
                    ..Default::default()
                };
                b.transport.send(&cmd)?;
                for m in b
                    .state
                    .inbox
                    .iter_mut()
                    .filter(|m| m.reply_key == target.reply_key)
                {
                    m.replied = true;
                }
                let _ = b.save();
                Ok(format!("Replied to {conv} on {app}"))
            }
            "android_files" => {
                let mut ui = Ui::new("Files from Android").kind("files");
                for r in b.state.received.iter().rev().take(20) {
                    let kb = r.size / 1024;
                    let size = if kb >= 1024 {
                        format!("{:.1} MB", kb as f64 / 1024.0)
                    } else {
                        format!("{kb} KB")
                    };
                    ui = ui.card(
                        Card::new(r.name.clone())
                            .subtitle(format!("{} · {}", r.from_app, r.conversation))
                            .badge(size)
                            .button("Open", format!("read document {}", r.name))
                            .button(
                                "Send back",
                                format!("share {} to android app {}", r.name, r.from_app),
                            ),
                    );
                }
                if b.state.received.is_empty() {
                    ui = ui.card(
                        Card::new("No files yet")
                            .subtitle("Files that arrive from allowed apps appear here"),
                    );
                }
                Ok(ui.to_reply())
            }
            "android_share" => {
                let app = arg(a, "app")?;
                let name = arg(a, "name")?;
                if clean_name(&name).as_deref() != Some(name.as_str()) {
                    return Err("a plain file name, not a path".into());
                }
                let docs = b
                    .docs_dir
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                let from = [b.docs_dir.join(&name), docs.join(&name)]
                    .into_iter()
                    .find(|p| p.is_file())
                    .ok_or_else(|| format!("no document called {name}"))?;
                let rule = b
                    .rule_for_app(&app)
                    .ok_or_else(|| format!("no Android app called {app} is allowed"))?;
                let rel = b.transport.offer_file(&from, &name)?;
                let cmd = Command {
                    v: 1,
                    id: format!("c-share-{}", b.state.received.len()),
                    kind: "share".into(),
                    package: rule.package.clone(),
                    rel,
                    mime: String::new(),
                    ..Default::default()
                };
                b.transport.send(&cmd)?;
                Ok(format!("Sent {name} to {app}"))
            }
            "android_bridge_status" => {
                let apps = b
                    .state
                    .rules
                    .iter()
                    .filter(|r| r.read || r.reply || r.files)
                    .count();
                Ok(format!(
                    "Android bridge: {}, {apps} apps allowed, {} unread, {} files",
                    if b.transport.up() { "up" } else { "down" },
                    b.unread(),
                    b.state.received.len()
                ))
            }
            other => Err(format!("unknown bridge tool {other}")),
        }
    }
}
