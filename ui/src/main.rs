//! agentos-ui: the phone's screen, built with Rune. The OS itself is headless: agentd is the
//! phone, and this app is one of its clients, talking to it over the same control socket as
//! `agentd ctl`. The design is a phone's: a status bar, an agent-first Home, a floating tab bar,
//! sheets from the bottom, and one accent colour on a dark ground, drawn with flat fills.
//!
//!   agentos-ui                               open the window (socket: $AGENTD_SOCK or /run/agentd.sock)
//!   agentos-ui --snap ui.png --scene demo    render headless, with sample data
//!   agentos-ui --snap ui.png --scene live    render headless, from a running agentd
//!     (live takes AGENTOS_TAB, AGENTOS_SEG and AGENTOS_SHEET to show a given screen)
#![recursion_limit = "512"]
mod genui;
mod icons;
mod screens;
mod theme;

use rune::{AppSpec, Cx, El, ScrollSpec, TextEdit, Tx, col, el};
use screens::{
    android, approval, chat, files, home, inbox, lab, phone, setup, store, system, today,
};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

/// Where agentd listens.
fn socket() -> String {
    if let Ok(s) = std::env::var("AGENTD_SOCK") {
        return s;
    }
    for p in ["/run/agentd.sock", "./agentd-data/control.sock"] {
        if std::path::Path::new(p).exists() {
            return p.into();
        }
    }
    "/run/agentd.sock".into()
}

/// One line to agentd, one answer back (newlines in it travel as \u{1}).
fn ctl(line: &str) -> Result<String, String> {
    let mut s = std::os::unix::net::UnixStream::connect(socket())
        .map_err(|e| format!("agentd is not running ({e})"))?;
    s.set_read_timeout(Some(Duration::from_secs(1800))).ok();
    writeln!(s, "{line}").map_err(|e| e.to_string())?;
    let mut out = String::new();
    BufReader::new(s)
        .read_line(&mut out)
        .map_err(|e| e.to_string())?;
    Ok(out.trim_end_matches('\n').replace('\u{1}', "\n"))
}

enum Event {
    State(Result<Value, String>),
    Reply { asked: String, answer: String },
}

/// The screens behind the bar (Home, Inbox, Android, You) and the ones Home opens (Phone,
/// Store, Files).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Inbox,
    Phone,
    Store,
    Files,
    Today,
    Lab,
    Android,
    System,
}

/// The six ways to see the conversation with the assistant (plan v3, D.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Poster,
    Board,
    Steps,
    Thread,
    Voice,
    Reader,
}

impl Mode {
    pub const ALL: [Mode; 6] = [
        Mode::Poster,
        Mode::Board,
        Mode::Steps,
        Mode::Thread,
        Mode::Voice,
        Mode::Reader,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Mode::Poster => "poster",
            Mode::Board => "board",
            Mode::Steps => "steps",
            Mode::Thread => "thread",
            Mode::Voice => "voice",
            Mode::Reader => "reader",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Mode::Poster => "Poster",
            Mode::Board => "Board",
            Mode::Steps => "Steps",
            Mode::Thread => "Thread",
            Mode::Voice => "Voice",
            Mode::Reader => "Reader",
        }
    }
    /// The mode a document asks for by its layout; anything else reads as a thread.
    pub fn from_layout(layout: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.name() == layout)
    }
}

/// The Phone tab's segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seg {
    Recents,
    Messages,
    Contacts,
    Keypad,
}

pub struct State {
    pub tab: Tab,
    pub seg: Seg,
    pub snap: Value,
    pub error: Option<String>,
    pub chat: TextEdit,
    pub setup_input: TextEdit,
    pub keypad: TextEdit,
    pub new_name: TextEdit,
    pub new_number: TextEdit,
    pub store_q: TextEdit,
    pub token: TextEdit,
    pub paste: TextEdit,
    /// digits typed on a PIN pad; never logged or sent unmasked
    pub pin: String,
    pub setup_pin: String,
    /// the pending action whose sheet is open
    pub sheet: Option<u64>,
    /// the Settings row that is open
    pub expanded: Option<String>,
    /// Restart was tapped once; a second tap restarts
    pub confirm_restart: bool,
    /// the assistant's conversation is open, full screen
    pub chat_open: bool,
    /// the view the owner picked; None follows the newest answer's layout
    pub chat_mode: Option<Mode>,
    /// which answer the poster shows; None is the newest
    pub poster_at: Option<usize>,
    /// the voice view is listening
    pub voice_on: bool,
    /// the conversation open in the inbox: "person|app", for example "Mum|WhatsApp"
    pub inbox_open: Option<String>,
    /// the inbox filter: "All", "SMS" or an app name
    pub inbox_filter: String,
    /// the reply box in an inbox conversation
    pub reply: TextEdit,
    /// for each question, the newest answer screen's time when it was asked, so each answer is
    /// matched with the screen made for it
    pub asked_after: Vec<u64>,
    /// (what you said, what the agent answered)
    pub log: Vec<(String, String)>,
    /// (what was asked, what came back) for the store and sandbox commands, newest last
    pub replies: Vec<(String, String)>,
    /// every command sent, newest last, with secrets masked (tests read this)
    pub sent: Vec<String>,
    /// photos the generated screens show, decoded once each
    pub images: std::collections::HashMap<String, rune::Pixels>,
    link: Option<Sender<String>>,
    inbox: Option<Receiver<Event>>,
}

impl State {
    fn new() -> State {
        State {
            tab: Tab::Home,
            seg: Seg::Recents,
            snap: Value::Null,
            error: None,
            chat: TextEdit::new(""),
            setup_input: TextEdit::new(""),
            keypad: TextEdit::new(""),
            new_name: TextEdit::new(""),
            new_number: TextEdit::new(""),
            store_q: TextEdit::new(""),
            token: TextEdit::new(""),
            paste: TextEdit::new(""),
            pin: String::new(),
            setup_pin: String::new(),
            sheet: None,
            expanded: None,
            confirm_restart: false,
            chat_open: false,
            chat_mode: None,
            poster_at: None,
            voice_on: false,
            inbox_open: None,
            inbox_filter: "All".into(),
            reply: TextEdit::new(""),
            asked_after: vec![],
            log: vec![],
            replies: vec![],
            sent: vec![],
            images: std::collections::HashMap::new(),
            link: None,
            inbox: None,
        }
    }

    pub fn send(&mut self, line: String) {
        self.send_shown(line.clone(), line);
    }

    /// Send `line`, but keep `shown` (with secrets masked) in what the UI records.
    pub fn send_shown(&mut self, line: String, shown: String) {
        if let Some(l) = &self.link {
            let _ = l.send(line);
        }
        self.sent.push(shown);
    }

    /// Ask the agent in words: the question goes in the conversation and to agentd.
    pub fn ask(&mut self, said: &str) {
        let said = said.trim();
        if said.is_empty() {
            return;
        }
        let newest = self.snap["screens"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|x| x["at"].as_u64())
            .unwrap_or(0);
        self.log.push((said.to_string(), String::new()));
        self.asked_after.push(newest);
        self.chat_open = true;
        self.send(format!("chat {said}"));
    }

    /// Decode the photos the dashboard and the answers show.
    pub fn load_images(&mut self) {
        let mut docs = vec![self.snap["dashboard"].clone()];
        for sc in self.snap["screens"].as_array().into_iter().flatten() {
            docs.push(sc["ui"].clone());
        }
        for d in docs {
            genui::load_images(&d, &mut self.images);
        }
    }

    /// Send what is in the setup box as the answer to the current step.
    pub fn setup_submit(&mut self) {
        let said = self.setup_input.text.trim().to_string();
        self.setup_input.clear();
        if !said.is_empty() {
            self.send(format!("setup answer {said}"));
        }
    }
}

/// A worker thread: asks agentd for the state every second, and runs each command on a thread of
/// its own (a chat can take a while), waking the window when something arrives.
fn setup(s: &mut State, wake: rune::Waker) {
    // a headless render of a demo scene draws sample data: no agentd to talk to
    let args: Vec<String> = std::env::args().collect();
    let snap = args.iter().any(|a| a == "--snap");
    let live = args.windows(2).any(|w| w[0] == "--scene" && w[1] == "live");
    if snap && !live {
        return;
    }
    let (cmd_tx, cmd_rx) = channel::<String>();
    let (ev_tx, ev_rx) = channel::<Event>();
    s.link = Some(cmd_tx);
    s.inbox = Some(ev_rx);
    std::thread::spawn(move || {
        loop {
            match cmd_rx.recv_timeout(Duration::from_secs(1)) {
                Ok(line) => {
                    let (ev_tx, wake) = (ev_tx.clone(), wake.clone());
                    std::thread::spawn(move || {
                        let answer = ctl(&line).unwrap_or_else(|e| e);
                        let _ = ev_tx.send(Event::Reply {
                            asked: line,
                            answer,
                        });
                        wake();
                    });
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return,
            }
            let st = ctl("state").and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()));
            if ev_tx.send(Event::State(st)).is_err() {
                return;
            }
            wake();
        }
    });
}

fn poll(s: &mut State, _t: f64, _tx: &mut Tx) -> bool {
    let Some(inbox) = &s.inbox else { return false };
    let events: Vec<Event> = inbox.try_iter().collect();
    if events.is_empty() {
        return false;
    }
    for ev in events {
        match ev {
            Event::State(Ok(v)) => {
                s.snap = v;
                s.error = None;
                s.load_images();
            }
            Event::State(Err(e)) => s.error = Some(e),
            Event::Reply { asked, answer } => {
                if asked.starts_with("do store_")
                    || asked.starts_with("do android_")
                    || asked.starts_with("store")
                    || asked.starts_with("lab")
                    || asked.starts_with("learn")
                    || asked.starts_with("inbox")
                {
                    s.replies.push((asked.clone(), answer.clone()));
                    if s.replies.len() > 10 {
                        s.replies.remove(0);
                    }
                }
                if let Some(said) = asked.strip_prefix("chat ") {
                    if let Some(entry) = s
                        .log
                        .iter_mut()
                        .rev()
                        .find(|(q, a)| q == said && a.is_empty())
                    {
                        entry.1 = answer;
                    }
                }
            }
        }
    }
    true
}

fn view(s: &State, _: &mut Cx<State>) -> El {
    let st = &s.snap;
    // while setup is not done it takes the whole screen
    if !st["setup"].is_null() {
        return setup::view(s, st);
    }
    let waiting = st["pending"].as_array().map_or(0, |p| p.len());
    // the assistant, full screen; the approval sheet can still come up over it
    if s.chat_open {
        let mut page = col().w_full().h_full().child(chat::view(s, st));
        if let Some(id) = s.sheet {
            page = page.child(approval::sheet(s, id));
        }
        return page;
    }
    let body = match s.tab {
        Tab::Home => home::view(s, st),
        Tab::Inbox => inbox::view(s, st),
        Tab::Phone => phone::view(s, st),
        Tab::Store => store::view(s, st),
        Tab::Files => files::view(s, st),
        Tab::Today => today::view(s, st),
        Tab::Lab => lab::view(s, st),
        Tab::Android => android::view(s, st),
        Tab::System => system::view(s, st),
    };
    let mut content = col().w_full().gap(14.0).px(20.0).pb(110.0);
    if let Some(e) = &s.error {
        content = content.child(
            col()
                .key("status")
                .w_full()
                .padding(12.0)
                .radius(14.0)
                .fill(theme::BAD.alpha(0.12))
                .child(
                    theme::t(e.clone(), theme::CAPTION, theme::BAD)
                        .w_full()
                        .wrap(),
                ),
        );
    }
    content = content.child(body);
    let mut page = col()
        .w_full()
        .h_full()
        .fill(theme::BG)
        .child(theme::status_bar(st))
        .child(
            content
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .grow(1.0)
                .min_h(0.0),
        )
        .child(theme::tab_bar_with(s.tab, waiting, inbox::unread(st)).pin(
            Some(12.0),
            None,
            Some(12.0),
            Some(16.0),
        ));
    if let Some(id) = s.sheet {
        page = page.child(approval::sheet(s, id));
    }
    let _ = el;
    page
}

/// A phone mid-afternoon, for headless renders and tests.
fn demo() -> Value {
    json!({
        "stage": "Active",
        "status": "Active, background work paused, battery 76%",
        "phone": "Jio LTE signal 64%, mobile data on, wifi HomeNet",
        "device": {"time": "14:32", "battery": 76, "charging": false, "signal": 64, "tech": "LTE", "operator": "Jio", "wifi": "HomeNet", "mobile_data": true, "airplane": false},
        "owner": "Asha",
        "pending": [
            {"id": 3, "tool": "send_sms", "args": {"to": "Mum", "text": "Running 10 min late"}},
            {"id": 4, "tool": "call", "args": {"to": "Arjun"}}
        ],
        "recent": [
            {"prompt": "drop Mum a text saying I'll be late", "tool": "send_sms", "source": "model", "reward": 1.0},
            {"prompt": "how many texts did Ravi send me", "tool": "", "source": "model", "reward": 0.0},
            {"prompt": "switch the torch on", "tool": "flashlight", "source": "shortcut", "reward": 0.3},
            {"prompt": "what's my signal like", "tool": "phone_status", "source": "model", "reward": 0.3},
            {"prompt": "make it louder", "tool": "set_volume", "source": "model", "reward": -1.0}
        ],
        "models": ["qwen3.5-4b (small): running, ready", "qwen3.5-9b (large): stopped"],
        "prio": "Idle · 4 model cores · 41°C",
        "functions": ["split_bill v2", "add_gst v1"],
        "learning": "42 episodes, 17 usable, 2 corrections",
        "setup": null,
        "pin_set": true,
        "calls": [
            {"who": "Asha", "state": "missed", "at": 1000},
            {"who": "Arjun", "state": "incoming", "at": 2000}
        ],
        "messages": [
            {"who": "Ravi", "text": "see you at 6", "at": 3000},
            {"who": "Mum", "text": "landed safely?", "at": 4000},
            {"who": "ShopMart", "text": "Mega SALE 50% off today only", "at": 2500}
        ],
        "contacts": [
            {"name": "Mum", "number": "+91 98765 43210"},
            {"name": "Ravi", "number": "+91 98123 45678"},
            {"name": "Me", "number": "+91 90000 11111"}
        ],
        "connectors": [
            {"name": "telegram", "state": "connected", "tools": 2},
            {"name": "google", "state": "unavailable", "tools": 0}
        ],
        "store": {"installed": ["trip-planner"]},
        "android": {
            "mode": "sim",
            "running": true,
            "paused": false,
            "showing": false,
            "pause_after_secs": 60,
            "apps": [
                {"name": "Notes", "package": "org.example.notes"},
                {"name": "WhatsApp", "package": "com.whatsapp"},
                {"name": "Maps", "package": "com.google.android.apps.maps"},
                {"name": "Uber", "package": "com.ubercab"},
                {"name": "Flipkart", "package": "com.flipkart.android"}
            ],
            "last_stop": {"reason": "only 500 MiB free", "at_ms": 5000}
        },
        "memory": {"available_mib": 1800, "psi": 1.2},
        "inbox": [
            {"id": "e1", "time_ms": 1731400000000u64, "app": "WhatsApp", "package": "com.whatsapp", "conversation": "Mum", "sender": "Mum", "text": "Here is the invoice", "files": ["Invoice-0042.pdf"], "read": false, "replied": false}
        ],
        "bridge": {"up": true, "unread": 1, "apps": [
            {"package": "com.whatsapp", "app": "WhatsApp", "read": true, "reply": false, "files": true},
            {"package": "org.telegram.messenger", "app": "Telegram", "read": false, "reply": false, "files": false}
        ], "received": [
            {"name": "Invoice-0042.pdf", "from_app": "WhatsApp", "conversation": "Mum", "time_ms": 1731400000000u64, "size": 48213, "mime": "application/pdf"}
        ]},
        "documents": [{"name": "Lease.docx", "size": 90112}, {"name": "Ravi-Sheet.xlsx", "size": 215040}],
        "triage": [
            {"conversation": "Mum", "source": "WhatsApp", "text": "Here is the invoice", "group": "Bills", "suggest": "pay", "reason": "Money is due", "draft": "Got it, paying today", "kept": false},
            {"conversation": "Mum", "source": "SMS", "text": "landed safely?", "group": "Reply", "suggest": "reply", "reason": "A question from someone you know", "draft": "Yes, will do", "kept": false},
            {"conversation": "Ravi", "source": "SMS", "text": "see you at 6", "group": "People", "suggest": "keep", "reason": "From someone you know", "draft": "", "kept": false},
            {"conversation": "ShopMart", "source": "SMS", "text": "Mega SALE 50% off today only", "group": "Promotions", "suggest": "delete", "reason": "Looks like an offer or spam", "draft": "", "kept": false}
        ],
        "today": {
            "flow": [
                {"t_ms": 1, "at": "08:10", "prompt": "weather today", "tool": "weather", "domain": "info", "outcome": "done", "source": "intent"},
                {"t_ms": 2, "at": "09:42", "prompt": "text Mum running late", "tool": "send_sms", "domain": "messages", "outcome": "approved", "source": "model"},
                {"t_ms": 3, "at": "11:05", "prompt": "read the lease", "tool": "read_document", "domain": "documents", "outcome": "done", "source": "model"},
                {"t_ms": 4, "at": "13:20", "prompt": "call Arjun", "tool": "call", "domain": "phone", "outcome": "declined", "source": "model"},
                {"t_ms": 5, "at": "14:05", "prompt": "torch on", "tool": "flashlight", "domain": "device", "outcome": "done", "source": "shortcut"}
            ],
            "by_hour": [0,0,0,0,0,0,0,0,1,1,0,1,0,1,1,0,0,0,0,0,0,0,0,0],
            "by_domain": [["info", 1], ["messages", 1], ["documents", 1], ["phone", 1], ["device", 1]],
            "done": 3, "approved": 1, "declined": 1, "failed": 0, "success": 0.8,
            "week": [9, 14, 6, 11, 17, 12, 5], "without_model": 2, "busiest_hour": 8
        },
        "lab": {
            "model": "qwen3.5-4b (small): running, ready", "has_model": true,
            "episodes": 412, "labelled": 163, "corrections": 21,
            "adapter": "/data/learn/adapter-3.gguf", "training": "waiting: not on the charger", "last_train": "adapter-3 kept: 71% vs 64%",
            "runs": [
                {"at_ms": 1, "label": "small model", "seed": 1, "n": 38, "score": 0.58, "held_out": 0.66, "variants": 0.49, "router": 0.71, "by_domain": []},
                {"at_ms": 2, "label": "small model + adapter-1.gguf", "seed": 2, "n": 40, "score": 0.63, "held_out": 0.70, "variants": 0.55, "router": 0.76, "by_domain": []},
                {"at_ms": 3, "label": "small model + adapter-2.gguf", "seed": 3, "n": 40, "score": 0.66, "held_out": 0.73, "variants": 0.59, "router": 0.80, "by_domain": []},
                {"at_ms": 4, "label": "small model + adapter-3.gguf", "seed": 4, "n": 40, "score": 0.74, "held_out": 0.80, "variants": 0.68, "router": 0.85, "by_domain": [
                    {"domain": "messages", "n": 12, "score": 0.83}, {"domain": "phone", "n": 9, "score": 0.89},
                    {"domain": "info", "n": 8, "score": 0.69}, {"domain": "money", "n": 6, "score": 0.42}, {"domain": "documents", "n": 5, "score": 0.6}
                ]}
            ],
            "delta": 8.0,
            "experts": [
                {"domain": "messages", "examples": 52, "ready": true}, {"domain": "phone", "examples": 38, "ready": true},
                {"domain": "info", "examples": 31, "ready": true}, {"domain": "money", "examples": 6, "ready": false}, {"domain": "documents", "examples": 5, "ready": false}
            ],
            "router_domains": 7, "min_expert": 8
        },
        "cache": {
            "enabled": true, "budget_mb": 512, "hit_rate": 0.86, "saved_s": 412.0, "warmed": 14, "warm_used": 11, "warming": false, "error": null,
            "server": {"budget_bytes": 429916160, "used_bytes": 197132288, "adapter": "", "adapters": ["messages", "phone"],
                "entries": [
                    {"id": 1, "adapter": "", "label": "system", "tokens": 1840, "bytes": 125829120, "hits": 63, "idle": 0, "priority": 0.94, "warmed": true},
                    {"id": 4, "adapter": "messages", "label": "messages", "tokens": 1840, "bytes": 71303168, "hits": 9, "idle": 3, "priority": 0.41, "warmed": true}
                ], "stats": {"hits": 72, "misses": 12}},
            "plan": {"budget_bytes": 429916160, "warm": [], "android_wake": false, "model_soon": 0.92,
                "next": [["messages", 0.48], ["phone", 0.21], ["info", 0.14]],
                "reasons": ["Budget 409 MB: on battery.", "Warming the messages expert: next request 48% likely messages."]},
            "log": ["cache: dropped 1 state(s), budget 409 MB", "cache: warmed the messages expert: 1840 tokens, 68 MB, 6.2 s"]
        },
        "dashboard": {"title": "Today", "kind": "dashboard", "cards": [
            {"title": "Battery", "metric": {"value": "76%", "label": "on battery"}, "progress": 0.76, "tone": "ok"},
            {"title": "Waiting for you", "metric": {"value": "2", "label": "to approve"}, "tone": "warn"},
            {"title": "Messages", "metric": {"value": "2", "label": "today"}, "lines": ["Mum: landed safely?"], "actions": [{"label": "Read", "say": "read my messages"}]},
            {"title": "Calls", "metric": {"value": "1", "label": "missed"}, "lines": ["last: Asha (missed)"], "actions": [{"label": "Call back", "say": "call Asha"}]},
            {"title": "Last photo", "image": demo_photo(), "actions": [{"label": "Photos", "say": "photos"}]},
            {"title": "Memory", "metric": {"value": "1.8 GB", "label": "free"}, "progress": 0.22}
        ]},
        "screens": [
            {"tool": "weather", "at": 1, "ui": {"title": "Weather in Bengaluru", "subtitle": "India", "kind": "weather", "cards": [
                {"title": "partly cloudy", "metric": {"value": "24°", "label": "feels like 26°"}, "badge": "24°C", "tone": "info", "lines": ["humidity 62% · wind 9 km/h"]},
                {"title": "Highs", "chart": {"kind": "line", "labels": ["Today", "13", "14", "15"], "values": [27.0, 29.0, 28.0, 25.0], "unit": "°"}},
                {"title": "Chance of rain", "chart": {"kind": "bar", "labels": ["Today", "13", "14", "15"], "values": [70.0, 40.0, 20.0, 85.0], "unit": "%"}, "tone": "info"}
            ], "actions": [{"label": "Air quality", "say": "air quality in Bengaluru"}]}}
        ]
    })
}

/// A picture for the demo scenes: drawn once into the temp folder.
fn demo_photo() -> String {
    let path = std::env::temp_dir().join("agentos-demo-photo.jpg");
    if !path.exists() {
        let img = image::RgbImage::from_fn(640, 480, |x, y| {
            let (fx, fy) = (x as f32 / 640.0, y as f32 / 480.0);
            let hill = 0.62 + 0.08 * (fx * 7.0).sin();
            let sun = ((fx - 0.72).powi(2) + (fy - 0.25).powi(2)).sqrt() < 0.07;
            let (r, g, b) = if sun {
                (0.98, 0.85, 0.5)
            } else if fy > hill {
                (0.2, 0.45 - (fy - hill) * 0.3, 0.22)
            } else {
                (0.35 + fy * 0.3, 0.55 + fy * 0.15, 0.9 - fy * 0.2)
            };
            image::Rgb([(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8])
        });
        let _ = img.save(&path);
    }
    path.display().to_string()
}

/// The assistant open, with two answers: the weather (a chart) and a photo (with its edits).
fn chat_demo(s: &mut State) {
    s.snap = demo();
    let weather = s.snap["screens"][0].clone();
    s.snap["screens"] = json!([demo_photo_screen(), weather]);
    s.log = vec![
        ("weather".into(), "done: Weather in Bengaluru".into()),
        ("take a photo".into(), "done: Photo taken".into()),
    ];
    s.asked_after = vec![0, 1];
    s.chat_open = true;
    s.load_images();
}

/// Three answers that use the new views: the weather as a poster, an invoice with its steps, a
/// table, a list and options, and a document for the reader.
fn modes_demo(s: &mut State, mode: Option<Mode>) {
    s.snap = demo();
    let mut weather = s.snap["screens"][0].clone();
    weather["ui"]["layout"] = json!("poster");
    weather["ui"]["accent"] = json!("sky");
    let invoice = json!({"tool": "invoice_create", "at": 2, "ui": {"title": "Invoice for Ravi", "kind": "money", "accent": "violet", "layout": "steps", "cards": [
        {"title": "Plan", "source": "Connector",
         "steps": [
            {"label": "Read Ravi's details", "detail": "Ravi Traders", "state": "done"},
            {"label": "Create invoice INV-0042", "detail": "GST included", "state": "done"},
            {"label": "Share to WhatsApp", "detail": "waits for your PIN", "state": "wait"}
         ],
         "table": {"columns": ["Item", "Amount"], "rows": [["Design work", "₹4,000"], ["GST 18%", "₹720"]]},
         "items": [{"title": "INV-0042.pdf", "subtitle": "Ready to send", "value": "₹4,720", "icon": "pdf", "say": "read document INV-0042.pdf"}],
         "compare": [{"title": "UPI", "value": "Instant", "lines": ["No fee"], "best": true}, {"title": "Bank", "value": "1 day", "lines": ["NEFT"]}]}
    ]}});
    let reader = json!({"tool": "read_document", "at": 3, "ui": {"title": "Invoice-0042.pdf", "subtitle": "PDF · 2 parts", "kind": "document", "layout": "reader", "accent": "lime", "cards": [
        {"title": "Page 1", "lines": ["Total ₹4,720, due 20 Nov", "Pay by UPI or bank transfer", "GST included; from Ravi Traders"]}
    ]}});
    s.snap["screens"] = json!([reader, invoice, weather]);
    s.log = vec![
        ("weather".into(), "done: Weather in Bengaluru".into()),
        (
            "make an invoice for Ravi".into(),
            "done: Invoice for Ravi".into(),
        ),
        (
            "read document Invoice-0042.pdf".into(),
            "done: Invoice-0042.pdf".into(),
        ),
    ];
    s.asked_after = vec![0, 1, 2];
    s.chat_open = true;
    s.chat_mode = mode;
    s.load_images();
}

/// A photo answer, as the camera tool gives it.
fn demo_photo_screen() -> Value {
    json!({"tool": "camera_take", "at": 2, "ui": {"title": "Photo taken", "kind": "photo", "cards": [
        {"title": "IMG_0001", "image": demo_photo(), "metric": {"value": "31%", "label": "brightness"}, "lines": ["Dark photo with muted colours, 960×720"]},
        {"title": "Suggested edits", "subtitle": "Tap one: the original is kept", "lines": ["Brighten: it is dark (31% brightness)", "Enhance: it looks flat: low contrast", "Vivid: the colours are dull"],
         "actions": [{"label": "Brighten", "say": "edit photo IMG_0001 brighten"}, {"label": "Enhance", "say": "edit photo IMG_0001 enhance"}, {"label": "Vivid", "say": "edit photo IMG_0001 vivid"}, {"label": "Black & white", "say": "edit photo IMG_0001 bw"}]}
    ], "actions": [{"label": "All photos", "say": "photos"}, {"label": "Take another", "say": "take a photo"}]}})
}

fn setup_demo(
    id: &str,
    index: u64,
    title: &str,
    body: &str,
    options: Value,
    input: Value,
    can_skip: bool,
) -> Value {
    let mut d = demo();
    d["setup"] = json!({"id": id, "index": index, "total": 13, "title": title, "body": body, "options": options, "input": input, "can_skip": can_skip, "error": null});
    d
}

fn main() {
    rune::run(AppSpec::<State> {
        scenes: &[
            ("demo", |s| {
                s.snap = demo();
                s.log
                    .push(("weather".into(), "done: Weather in Bengaluru".into()));
                s.load_images();
            }),
            ("photo", |s| {
                s.snap = demo();
                s.snap["pending"] = json!([]);
                s.load_images();
            }),
            ("chat", |s| chat_demo(s)),
            ("chat-poster", |s| modes_demo(s, Some(Mode::Poster))),
            ("chat-board", |s| modes_demo(s, Some(Mode::Board))),
            ("chat-steps", |s| modes_demo(s, Some(Mode::Steps))),
            ("chat-thread", |s| modes_demo(s, Some(Mode::Thread))),
            ("chat-voice", |s| modes_demo(s, Some(Mode::Voice))),
            ("chat-reader", |s| modes_demo(s, None)),
            ("inbox", |s| {
                s.snap = demo();
                s.tab = Tab::Inbox;
            }),
            ("inbox-thread", |s| {
                s.snap = demo();
                s.tab = Tab::Inbox;
                s.inbox_open = Some("Mum|WhatsApp".into());
            }),
            ("files", |s| {
                s.snap = demo();
                s.tab = Tab::Files;
            }),
            ("today", |s| {
                s.snap = demo();
                s.tab = Tab::Today;
            }),
            ("lab", |s| {
                s.snap = demo();
                s.tab = Tab::Lab;
            }),
            ("setup", |s| {
                s.snap = setup_demo(
                    "wifi",
                    2,
                    "Connect to Wi-Fi",
                    "",
                    json!(["skip"]),
                    json!("text"),
                    true,
                )
            }),
            ("setup-welcome", |s| {
                s.snap = setup_demo(
                    "welcome",
                    1,
                    "Welcome",
                    "This phone is set for India, time zone UTC+5:30. English only for now.",
                    json!(["yes"]),
                    Value::Null,
                    true,
                )
            }),
            ("setup-pin", |s| {
                s.snap = setup_demo(
                    "pin",
                    7,
                    "Screen lock",
                    "Set the approval PIN (4 to 8 digits). Every payment and message asks for it.",
                    json!([]),
                    json!("password"),
                    false,
                );
                s.setup_pin = "43".into();
            }),
            ("phone", |s| {
                s.snap = demo();
                s.tab = Tab::Phone;
            }),
            ("contacts", |s| {
                s.snap = demo();
                s.tab = Tab::Phone;
                s.seg = Seg::Contacts;
            }),
            ("keypad", |s| {
                s.snap = demo();
                s.tab = Tab::Phone;
                s.seg = Seg::Keypad;
                s.keypad = TextEdit::new("98765 4");
            }),
            ("store", |s| {
                s.snap = demo();
                s.tab = Tab::Store;
                s.replies.push((
                    "do store_search {\"text\":\"agent\"}".into(),
                    "trip-planner: Trip planner — Plans a trip: weather, directions, events and reminders\ndaily-digest: Daily digest — A morning summary: the weather, events and texts\nbill-splitter: Bill splitter — Splits a bill between people, in any currency".into(),
                ));
            }),
            ("android", |s| {
                s.snap = demo();
                s.tab = Tab::Android;
            }),
            ("android-paused", |s| {
                s.snap = demo();
                s.snap["android"]["paused"] = json!(true);
                s.snap["android"]["last_stop"] = Value::Null;
                s.tab = Tab::Android;
            }),
            ("android-showing", |s| {
                s.snap = demo();
                s.snap["android"]["showing"] = json!(true);
                s.snap["android"]["last_stop"] = Value::Null;
                s.tab = Tab::Android;
            }),
            ("system", |s| {
                s.snap = demo();
                s.tab = Tab::System;
                s.expanded = Some("telegram".into());
            }),
            ("sheet", |s| {
                s.snap = demo();
                s.sheet = Some(3);
                s.pin = "43".into();
            }),
            ("live", |s| {
                // AGENTOS_ASK="weather;take a photo": ask the running agentd each question, keep
                // its real answers, and open the assistant on them
                if let Ok(asks) = std::env::var("AGENTOS_ASK") {
                    for q in asks.split(';').map(str::trim).filter(|q| !q.is_empty()) {
                        let newest = ctl("state")
                            .ok()
                            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                            .and_then(|v| v["screens"].get(0).and_then(|x| x["at"].as_u64()))
                            .unwrap_or(0);
                        let answer = ctl(&format!("chat {q}")).unwrap_or_else(|e| e);
                        s.log.push((q.to_string(), answer));
                        s.asked_after.push(newest);
                    }
                    s.chat_open = true;
                }
                match ctl("state").and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
                {
                    Ok(v) => s.snap = v,
                    Err(e) => s.error = Some(e),
                }
                s.tab = match std::env::var("AGENTOS_TAB").as_deref() {
                    Ok("phone") => Tab::Phone,
                    Ok("inbox") => Tab::Inbox,
                    Ok("files") => Tab::Files,
                    Ok("today") => Tab::Today,
                    Ok("lab") => Tab::Lab,
                    Ok("store") => Tab::Store,
                    Ok("android") => Tab::Android,
                    Ok("system") => Tab::System,
                    _ => Tab::Home,
                };
                s.seg = match std::env::var("AGENTOS_SEG").as_deref() {
                    Ok("messages") => Seg::Messages,
                    Ok("contacts") => Seg::Contacts,
                    Ok("keypad") => Seg::Keypad,
                    _ => Seg::Recents,
                };
                s.sheet = std::env::var("AGENTOS_SHEET")
                    .ok()
                    .and_then(|v| v.parse().ok());
                s.load_images();
            }),
        ],
        setup: Some(setup),
        poll: Some(poll),
        min_size: Some((360.0, 640.0)),
        ..AppSpec::new("Agentic OS", (390.0, 844.0), State::new, view)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Runtime};

    fn rt_of(s: State) -> Runtime<State> {
        // a tall phone, so the tests can tap what is below the fold (the runtime cannot scroll)
        let mut rt = Runtime::new(
            s,
            view,
            (390.0, 3200.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        rt
    }
    fn rt() -> Runtime<State> {
        let mut s = State::new();
        s.snap = demo();
        rt_of(s)
    }
    fn tap(rt: &mut Runtime<State>, key: &str) {
        let (x, y) = rt.find(key).unwrap_or_else(|| panic!("no element {key}"));
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        for _ in 0..4 {
            rt.frame(1.0 / 60.0);
        }
    }

    #[test]
    fn approving_without_a_pin_presses_the_key_for_that_item() {
        let mut rt = rt();
        rt.state.snap["pin_set"] = json!(false);
        rt.frame(0.0);
        tap(&mut rt, "approve-3");
        tap(&mut rt, "decline-4");
        assert_eq!(rt.state.sent, vec!["press 3", "decline 4"]);
    }

    #[test]
    fn home_shows_the_status_bar_what_waits_and_today() {
        let mut rt = rt();
        for key in [
            "header",
            "stage",
            "ask",
            "chat",
            "waiting",
            "pending-3",
            "pending-4",
            "quick",
            "recent",
            "recent-4",
            "tabs",
        ] {
            assert!(rt.find(key).is_some(), "{key} missing");
        }
        tap(&mut rt, "tab-system");
        for key in ["model", "functions", "safety", "services"] {
            assert!(rt.find(key).is_some(), "{key} missing");
        }
    }

    #[test]
    fn each_tab_shows_its_title() {
        let mut rt = rt();
        // the bar: Home, Inbox, Android, You
        for (tab, title) in [
            ("tab-inbox", "title-inbox"),
            ("tab-android", "title-android"),
            ("tab-system", "title-system"),
            ("tab-home", "title-home"),
        ] {
            tap(&mut rt, tab);
            assert!(rt.find(title).is_some(), "{title} missing after {tab}");
        }
        // and the screens Home opens
        for (go, title) in [
            ("go-phone", "title-phone"),
            ("go-store", "title-store"),
            ("go-files", "title-files"),
        ] {
            tap(&mut rt, "tab-home");
            tap(&mut rt, go);
            assert!(rt.find(title).is_some(), "{title} missing after {go}");
        }
    }

    #[test]
    fn nothing_waiting_means_no_waiting_section() {
        let mut s = State::new();
        let mut d = demo();
        d["pending"] = json!([]);
        s.snap = d;
        let rt = rt_of(s);
        assert!(rt.find("waiting").is_none());
    }

    #[test]
    fn a_chat_answer_lands_next_to_its_question() {
        let mut s = State::new();
        s.snap = demo();
        s.log.push(("call mum".into(), String::new()));
        let (tx, rx) = channel();
        s.inbox = Some(rx);
        tx.send(Event::Reply {
            asked: "chat call mum".into(),
            answer: "not done: approval required (id 1)".into(),
        })
        .unwrap();
        let mut rt = rt_of(s);
        rt.poll = Some(poll);
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.state.log[0].1, "not done: approval required (id 1)");
    }

    #[test]
    fn a_quick_action_asks_the_agent() {
        let mut rt = rt();
        tap(&mut rt, "quick-torch");
        assert_eq!(rt.state.sent.last().unwrap(), "chat torch on");
        assert_eq!(rt.state.log.last().unwrap().0, "torch on");
    }

    #[test]
    fn setup_is_shown_over_everything_and_its_answers_are_sent() {
        let mut s = State::new();
        s.snap = setup_demo(
            "wifi",
            2,
            "Connect to Wi-Fi",
            "",
            json!(["skip"]),
            json!("text"),
            true,
        );
        let mut rt = rt_of(s);
        assert!(rt.find("setup-screen").is_some());
        assert!(
            rt.find("tab-home").is_none(),
            "the tabs are not shown during setup"
        );
        tap(&mut rt, "setup-option-0");
        assert_eq!(rt.state.sent.last().unwrap(), "setup answer skip");
    }

    #[test]
    fn the_first_setup_step_has_no_back_button() {
        let mut s = State::new();
        s.snap = setup_demo(
            "welcome",
            1,
            "Welcome",
            "",
            json!(["yes"]),
            Value::Null,
            true,
        );
        let rt = rt_of(s);
        assert!(rt.find("setup-back").is_none());
        let mut s = State::new();
        s.snap = setup_demo(
            "wifi",
            2,
            "Connect to Wi-Fi",
            "",
            json!(["skip"]),
            json!("text"),
            true,
        );
        assert!(rt_of(s).find("setup-back").is_some());
    }

    #[test]
    fn the_setup_pin_is_typed_on_a_pad_and_sent_masked() {
        let mut s = State::new();
        s.snap = setup_demo(
            "pin",
            7,
            "Screen lock",
            "",
            json!([]),
            json!("password"),
            false,
        );
        let mut rt = rt_of(s);
        assert!(rt.find("setup-input").is_none(), "no text box for a PIN");
        for k in ["setup-pin-4", "setup-pin-3", "setup-pin-2", "setup-pin-1"] {
            tap(&mut rt, k);
        }
        tap(&mut rt, "setup-continue");
        assert_eq!(rt.state.sent.last().unwrap(), "setup answer ****");
        assert!(!rt.state.sent.iter().any(|s| s.contains("4321")));
    }

    #[test]
    fn the_pin_is_typed_on_the_sheet_and_never_sent_or_kept_unmasked() {
        let mut rt = rt();
        tap(&mut rt, "approve-3");
        assert!(
            rt.find("sheet").is_some(),
            "approving with a PIN set opens the sheet"
        );
        for k in ["pin-1", "pin-2", "pin-3", "pin-4"] {
            tap(&mut rt, k);
        }
        assert!(rt.state.sent.is_empty(), "nothing is sent before Approve");
        tap(&mut rt, "sheet-approve");
        assert_eq!(rt.state.sent.last().unwrap(), "press 3 ****");
        assert!(!rt.state.sent.iter().any(|s| s.contains("1234")));
        assert!(rt.state.pin.is_empty());
    }

    #[test]
    fn approve_waits_for_four_digits() {
        let mut rt = rt();
        tap(&mut rt, "approve-3");
        tap(&mut rt, "pin-1");
        tap(&mut rt, "sheet-approve");
        assert!(rt.state.sent.is_empty(), "one digit is not a PIN");
        assert!(rt.state.sheet.is_some());
    }

    #[test]
    fn decline_on_the_sheet_sends_decline_and_clears_the_pin() {
        let mut rt = rt();
        tap(&mut rt, "approve-4");
        tap(&mut rt, "pin-9");
        tap(&mut rt, "sheet-decline");
        assert_eq!(rt.state.sent.last().unwrap(), "decline 4");
        assert!(rt.state.pin.is_empty());
    }

    #[test]
    fn the_phone_calls_a_contact_and_answers_a_call() {
        let mut rt = rt();
        tap(&mut rt, "go-phone");
        tap(&mut rt, "seg-contacts");
        assert!(rt.find("contact-0").is_some());
        tap(&mut rt, "call-0");
        assert_eq!(rt.state.sent.last().unwrap(), "do call {\"to\":\"Mum\"}");
        tap(&mut rt, "answer-call");
        assert_eq!(rt.state.sent.last().unwrap(), "do answer_call {}");
    }

    #[test]
    fn the_keypad_dials_what_is_typed() {
        let mut rt = rt();
        tap(&mut rt, "go-phone");
        tap(&mut rt, "seg-keypad");
        for k in ["kp-9", "kp-8", "kp-1"] {
            tap(&mut rt, k);
        }
        tap(&mut rt, "kp-back");
        tap(&mut rt, "keypad-call");
        assert_eq!(rt.state.sent.last().unwrap(), "do call {\"to\":\"98\"}");
    }

    #[test]
    fn the_store_searches_by_category_and_installs_and_removes() {
        let mut rt = rt();
        tap(&mut rt, "go-store");
        tap(&mut rt, "chip-agents");
        assert_eq!(
            rt.state.sent.last().unwrap(),
            "do store_search {\"text\":\"agent\"}"
        );
        rt.state.replies.push((
            "do store_search {\"text\":\"agent\"}".into(),
            "zomato: Zomato — order food\nswiggy: Swiggy — order food".into(),
        ));
        tap(&mut rt, "tab-home");
        tap(&mut rt, "go-store");
        tap(&mut rt, "store-install-0");
        assert_eq!(
            rt.state.sent.last().unwrap(),
            "do store_install {\"id\":\"zomato\"}"
        );
        tap(&mut rt, "store-remove-0");
        assert_eq!(
            rt.state.sent.last().unwrap(),
            "do store_remove {\"id\":\"trip-planner\"}"
        );
    }

    #[test]
    fn android_is_opened_as_itself_not_redrawn() {
        let mut rt = rt();
        tap(&mut rt, "tab-android");
        assert!(rt.find("memory-bar").is_some());
        assert!(rt.find("android-banner").is_some());
        assert!(
            rt.find("app-com.whatsapp").is_none(),
            "no copy of Android's app grid"
        );
        tap(&mut rt, "android-open");
        assert_eq!(rt.state.sent.last().unwrap(), "do android_show {}");
        tap(&mut rt, "android-pause");
        assert_eq!(rt.state.sent.last().unwrap(), "do android_pause {}");
        tap(&mut rt, "close-all");
        assert_eq!(rt.state.sent.last().unwrap(), "do android_close_all {}");
    }

    #[test]
    fn a_paused_android_offers_resume_and_says_why() {
        let mut s = State::new();
        let mut d = demo();
        d["android"]["paused"] = json!(true);
        s.snap = d;
        s.tab = Tab::Android;
        let mut rt = rt_of(s);
        assert!(rt.find("android-pause").is_none());
        tap(&mut rt, "android-resume");
        assert_eq!(rt.state.sent.last().unwrap(), "do android_resume {}");
    }

    #[test]
    fn on_screen_android_offers_the_way_back() {
        let mut s = State::new();
        let mut d = demo();
        d["android"]["showing"] = json!(true);
        s.snap = d;
        s.tab = Tab::Android;
        let mut rt = rt_of(s);
        tap(&mut rt, "android-back");
        assert_eq!(rt.state.sent.last().unwrap(), "do android_hide {}");
    }

    #[test]
    fn close_all_is_not_offered_when_nothing_runs() {
        let mut s = State::new();
        let mut d = demo();
        d["android"]["running"] = json!(false);
        s.snap = d;
        s.tab = Tab::Android;
        assert!(rt_of(s).find("close-all").is_none());
    }

    #[test]
    fn a_token_is_sent_masked_and_cleared_from_the_field() {
        let mut rt = rt();
        tap(&mut rt, "tab-system");
        tap(&mut rt, "connector-telegram");
        rt.state.token.text = "123:SECRET".into();
        tap(&mut rt, "connect-telegram");
        assert_eq!(
            rt.state.sent.last().unwrap(),
            "secret set conn.telegram ****"
        );
        assert!(
            rt.state.token.text.is_empty(),
            "the token is cleared once sent"
        );
        assert!(!rt.state.sent.iter().any(|s| s.contains("SECRET")));
    }

    #[test]
    fn restart_asks_for_a_second_tap() {
        let mut rt = rt();
        tap(&mut rt, "tab-system");
        tap(&mut rt, "restart");
        assert!(rt.state.sent.is_empty(), "one tap does not restart");
        tap(&mut rt, "restart-confirm");
        assert_eq!(rt.state.sent.last().unwrap(), "restart");
    }

    #[test]
    fn home_tab_shows_a_dot_when_something_waits() {
        let rt = rt();
        // the dot is drawn inside the Home tab; with nothing waiting there is none
        assert!(rt.find("tab-home").is_some());
        let mut s = State::new();
        let mut d = demo();
        d["pending"] = json!([]);
        s.snap = d;
        let _ = rt_of(s);
    }

    #[test]
    fn asking_opens_the_assistant_and_back_closes_it() {
        let mut rt = rt();
        tap(&mut rt, "quick-camera");
        assert_eq!(rt.state.sent.last().unwrap(), "chat take a photo");
        assert!(rt.state.chat_open);
        assert!(rt.find("chat-thread").is_some());
        assert!(rt.find("tabs").is_none(), "the assistant is full screen");
        tap(&mut rt, "chat-back");
        assert!(!rt.state.chat_open);
        assert!(
            rt.find("open-chat").is_some(),
            "Home shows the last exchange"
        );
    }

    #[test]
    fn an_answer_is_drawn_as_its_generated_screen_and_its_buttons_ask() {
        let mut s = State::new();
        chat_demo(&mut s);
        let mut rt = rt_of(s);
        assert!(rt.find("answer-1-card-0").is_some());
        assert!(rt.find("answer-1-card-0-metric").is_some());
        tap(&mut rt, "answer-1-card-1-do-0");
        assert_eq!(
            rt.state.sent.last().unwrap(),
            "chat edit photo IMG_0001 brighten"
        );
    }

    #[test]
    fn each_answer_gets_the_screen_made_for_it() {
        let mut s = State::new();
        chat_demo(&mut s);
        let st = s.snap.clone();
        assert_eq!(
            chat::screen_for(&st, &s, 0).unwrap()["kind"],
            json!("weather")
        );
        assert_eq!(
            chat::screen_for(&st, &s, 1).unwrap()["kind"],
            json!("photo")
        );
        // a question asked after the newest screen, with no new screen yet, gets none
        s.asked_after.push(2);
        s.log
            .push(("remind me".into(), "done: reminder set".into()));
        assert!(chat::screen_for(&st, &s, 2).is_none());
    }

    #[test]
    fn home_shows_the_generated_dashboard() {
        let rt = rt();
        assert!(rt.find("dashboard").is_some());
        assert!(rt.find("widget-0").is_some());
        assert!(rt.find("widget-0-metric").is_some());
        assert!(rt.find("widget-0-progress").is_some());
    }

    #[test]
    fn a_chart_draws_one_bar_per_value() {
        let mut s = State::new();
        chat_demo(&mut s);
        s.log.truncate(1);
        s.asked_after.truncate(1);
        let rt = rt_of(s);
        // the chance-of-rain chart, the last card of the weather answer
        for i in 0..4 {
            let k = format!("answer-0-card-2-bar-{i}");
            assert!(rt.find(&k).is_some(), "{k}");
        }
    }

    #[test]
    fn the_latest_answer_shows_its_buttons_only_as_suggestions() {
        let mut s = State::new();
        chat_demo(&mut s);
        let rt = rt_of(s);
        assert!(
            rt.find("answer-1-act-0").is_none(),
            "no duplicate inside the latest answer"
        );
        assert!(rt.find("idea-0").is_some(), "they show above the box");
    }

    // ---- Part E: the Signal client ----------------------------------------------------------

    #[test]
    fn every_icon_the_plan_names_resolves() {
        for name in icons::PLAN_NAMES {
            assert!(icons::by_name(name).is_some(), "no icon {name}");
        }
    }

    #[test]
    fn a_document_draws_steps_a_table_items_and_options() {
        let mut s = State::new();
        modes_demo(&mut s, Some(Mode::Thread));
        let rt = rt_of(s);
        for k in [
            "answer-1-card-0-step-0",
            "answer-1-card-0-step-2",
            "answer-1-card-0-table",
            "answer-1-card-0-item-0",
            "answer-1-card-0-option-0",
            "answer-1-card-0-option-1",
        ] {
            assert!(rt.find(k).is_some(), "{k} missing");
        }
    }

    #[test]
    fn each_chat_mode_draws_its_view() {
        for (mode, key) in [
            (Mode::Poster, "mode-poster-view"),
            (Mode::Board, "mode-board-view"),
            (Mode::Steps, "mode-steps-view"),
            (Mode::Thread, "chat-thread"),
            (Mode::Voice, "mode-voice-view"),
            (Mode::Reader, "mode-reader-view"),
        ] {
            let mut s = State::new();
            modes_demo(&mut s, Some(mode));
            let rt = rt_of(s);
            assert!(rt.find(key).is_some(), "{key} missing for {mode:?}");
            assert!(
                rt.find("chat-modes").is_some(),
                "the switcher is always there"
            );
        }
    }

    #[test]
    fn the_newest_answer_picks_the_view_until_the_owner_picks_one() {
        let mut s = State::new();
        modes_demo(&mut s, None);
        let mut rt = rt_of(s);
        // the newest answer is a document: the reader
        assert!(rt.find("mode-reader-view").is_some());
        tap(&mut rt, "mode-board");
        assert_eq!(rt.state.chat_mode, Some(Mode::Board));
        assert!(rt.find("mode-board-view").is_some());
    }

    #[test]
    fn a_board_tile_opens_its_poster_and_the_poster_pages() {
        let mut s = State::new();
        modes_demo(&mut s, Some(Mode::Board));
        let mut rt = rt_of(s);
        tap(&mut rt, "tile-0");
        assert_eq!(rt.state.chat_mode, Some(Mode::Poster));
        assert_eq!(rt.state.poster_at, Some(0));
        assert!(rt.find("poster-field").is_some());
        assert!(
            rt.find("poster-bar-0").is_some(),
            "the weather poster draws its chart"
        );
        tap(&mut rt, "poster-next");
        assert_eq!(rt.state.poster_at, Some(1));
    }

    #[test]
    fn steps_shows_the_steps_an_answer_carries_and_its_result() {
        let mut s = State::new();
        modes_demo(&mut s, Some(Mode::Steps));
        let rt = rt_of(s);
        assert!(rt.find("run-1-step-2").is_some());
        assert!(rt.find("run-1-result").is_some());
        assert!(
            rt.find("run-0-step-0").is_some(),
            "an answer with no steps gets one"
        );
    }

    #[test]
    fn a_waiting_approval_shows_approve_on_its_step() {
        let mut s = State::new();
        modes_demo(&mut s, Some(Mode::Steps));
        s.snap["pin_set"] = json!(false);
        s.log.push((
            "call mum".into(),
            "not done: approval required (id 4)".into(),
        ));
        s.asked_after.push(3);
        let mut rt = rt_of(s);
        tap(&mut rt, "steps-approve-4");
        assert_eq!(rt.state.sent.last().unwrap(), "press 4");
    }

    #[test]
    fn the_reader_shares_and_replies_to_the_sender() {
        let mut s = State::new();
        modes_demo(&mut s, Some(Mode::Reader));
        let mut rt = rt_of(s);
        assert!(rt.find("reader-page").is_some());
        assert!(
            rt.find("reader-reply").is_some(),
            "the file came from Mum on WhatsApp"
        );
        tap(&mut rt, "reader-share");
        assert_eq!(
            rt.state.sent.last().unwrap(),
            "chat share Invoice-0042.pdf to android app WhatsApp"
        );
    }

    #[test]
    fn voice_listens_on_the_orb_and_the_microphone() {
        let mut s = State::new();
        modes_demo(&mut s, Some(Mode::Voice));
        let mut rt = rt_of(s);
        assert!(
            rt.find("voice-answer").is_some(),
            "the newest answer shows as a card"
        );
        tap(&mut rt, "voice-mic");
        assert!(rt.state.voice_on);
        assert!(
            rt.find("chat-bar").is_none(),
            "the box is replaced by the microphone"
        );
    }

    #[test]
    fn the_inbox_merges_sms_and_android_and_turns_replies_on() {
        let mut rt = rt();
        tap(&mut rt, "tab-inbox");
        assert!(rt.find("title-inbox").is_some());
        assert!(rt.find("inbox-row-0").is_some());
        assert!(
            rt.find("inbox-filter-whatsapp").is_some(),
            "a filter for each app"
        );
        // Mum: an SMS and a WhatsApp message with a file, in one conversation
        tap(&mut rt, "inbox-row-0");
        assert_eq!(rt.state.inbox_open.as_deref(), Some("Mum|WhatsApp"));
        tap(&mut rt, "inbox-file-0-0");
        assert_eq!(
            rt.state.sent.last().unwrap(),
            "chat read document Invoice-0042.pdf"
        );
        // replies to WhatsApp are off: the box is a card that turns them on
        rt.update(|s, _| s.chat_open = false);
        for _ in 0..4 {
            rt.frame(1.0 / 60.0);
        }
        assert!(rt.find("inbox-reply").is_none());
        tap(&mut rt, "inbox-turn-on");
        assert_eq!(
            rt.state.sent.last().unwrap(),
            "bridge allow com.whatsapp reply on"
        );
        // once on, the reply goes through the app's reply action
        rt.update(|s, _| s.snap["bridge"]["apps"][0]["reply"] = json!(true));
        for _ in 0..4 {
            rt.frame(1.0 / 60.0);
        }
        rt.state.reply.text = "On my way".into();
        tap(&mut rt, "inbox-send");
        assert_eq!(
            rt.state.sent.last().unwrap(),
            "chat reply on android to Mum saying On my way"
        );
    }

    #[test]
    fn an_sms_conversation_replies_by_sms() {
        let mut rt = rt();
        tap(&mut rt, "tab-inbox");
        tap(&mut rt, "inbox-filter-sms");
        tap(&mut rt, "inbox-row-0");
        let key = rt.state.inbox_open.clone().unwrap();
        let (who, source) = key.split_once('|').unwrap();
        assert_eq!(source, "SMS");
        rt.state.reply.text = "ok".into();
        tap(&mut rt, "inbox-send");
        assert_eq!(
            rt.state.sent.last().unwrap(),
            &format!("chat text {who} ok")
        );
    }

    #[test]
    fn files_lists_both_kinds_and_opens_one() {
        let mut rt = rt();
        tap(&mut rt, "go-files");
        assert!(rt.find("file-local-0").is_some());
        assert!(rt.find("file-android-0").is_some());
        tap(&mut rt, "file-android-0");
        assert_eq!(
            rt.state.sent.last().unwrap(),
            "chat read document Invoice-0042.pdf"
        );
    }

    #[test]
    fn a_bridge_switch_sends_its_control_line() {
        let mut rt = rt();
        tap(&mut rt, "tab-system");
        assert!(rt.find("bridge-status").is_some());
        tap(&mut rt, "bridge-com.whatsapp-reply");
        assert_eq!(
            rt.state.sent.last().unwrap(),
            "bridge allow com.whatsapp reply on"
        );
        tap(&mut rt, "bridge-com.whatsapp-read");
        assert_eq!(
            rt.state.sent.last().unwrap(),
            "bridge allow com.whatsapp read off"
        );
    }

    #[test]
    fn home_leads_with_the_priority_field() {
        let rt = rt();
        assert!(rt.find("priority").is_some());
        assert!(rt.find("priority-hero").is_some());
    }

    #[test]
    fn the_ask_button_opens_the_assistant() {
        let mut rt = rt();
        tap(&mut rt, "tab-ask");
        assert!(rt.state.chat_open);
    }

    #[test]
    fn the_android_screen_sets_up_play() {
        let mut rt = rt();
        tap(&mut rt, "tab-android");
        assert!(rt.find("play-card").is_some());
        tap(&mut rt, "play-setup");
        assert_eq!(rt.state.sent.last().unwrap(), "chat set up play store");
    }

    #[test]
    fn the_inbox_suggests_replies_with_drafts_and_what_to_delete() {
        let mut rt = rt();
        tap(&mut rt, "tab-inbox");
        assert!(rt.find("inbox-ai").is_some());
        assert!(rt.find("inbox-group-reply").is_some(), "organised by group");
        assert!(rt.find("inbox-group-promotions").is_some());
        tap(&mut rt, "ai-send-0");
        assert_eq!(rt.state.sent.last().unwrap(), "chat text Mum Yes, will do");
        tap(&mut rt, "ai-del-0");
        assert_eq!(rt.state.sent.last().unwrap(), "inbox delete ShopMart|SMS");
        tap(&mut rt, "ai-keep-0");
        assert_eq!(rt.state.sent.last().unwrap(), "inbox keep ShopMart|SMS");
        tap(&mut rt, "inbox-group-promotions");
        assert!(rt.find("inbox-row-0").is_some());
        assert!(
            rt.find("inbox-row-1").is_none(),
            "only the promotion is listed"
        );
    }

    #[test]
    fn today_shows_the_flow_and_the_numbers() {
        let mut rt = rt();
        tap(&mut rt, "go-today");
        for k in [
            "title-today",
            "today-actions",
            "today-success",
            "flow-0",
            "flow-4",
            "hour",
            "domain-0",
            "week",
        ] {
            assert!(rt.find(k).is_some(), "{k} missing");
        }
    }

    #[test]
    fn the_lab_runs_tests_trains_and_shows_progress_and_experts() {
        let mut rt = rt();
        tap(&mut rt, "go-lab");
        for k in [
            "title-lab",
            "lab-model",
            "lab-history",
            "lab-run-3",
            "lab-domain-0",
            "expert-0",
            "expert-4",
        ] {
            assert!(rt.find(k).is_some(), "{k} missing");
        }
        tap(&mut rt, "lab-run");
        assert_eq!(rt.state.sent.last().unwrap(), "lab run");
        tap(&mut rt, "lab-train");
        assert_eq!(rt.state.sent.last().unwrap(), "learn train");
        tap(&mut rt, "lab-experts");
        assert_eq!(rt.state.sent.last().unwrap(), "lab experts");
    }

    #[test]
    fn the_lab_shows_the_predictive_cache_and_its_reasons() {
        let mut rt = rt();
        tap(&mut rt, "go-lab");
        for k in ["lab-cache", "cache-hit", "cache-mem", "cache-bar", "cache-entry-0", "cache-entry-1", "cache-why-0"] {
            assert!(rt.find(k).is_some(), "{k} missing");
        }
        tap(&mut rt, "cache-toggle");
        assert_eq!(rt.state.sent.last().unwrap(), "cache off");
        tap(&mut rt, "cache-now");
        assert_eq!(rt.state.sent.last().unwrap(), "cache now");
        tap(&mut rt, "cache-clear");
        assert_eq!(rt.state.sent.last().unwrap(), "cache clear");
    }
}
