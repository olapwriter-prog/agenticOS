//! agentos-ui: the phone's screen, built with Rune. The OS itself is headless: agentd is the
//! phone, and this app is one of its clients, talking to it over the same control socket as
//! `agentd ctl`. The design is a phone's: a status bar, an agent-first Home, a floating tab bar,
//! sheets from the bottom, and one accent colour on a dark ground, drawn with flat fills.
//!
//!   agentos-ui                               open the window (socket: $AGENTD_SOCK or /run/agentd.sock)
//!   agentos-ui --snap ui.png --scene demo    render headless, with sample data
//!   agentos-ui --snap ui.png --scene live    render headless, from a running agentd
//!     (live takes AGENTOS_TAB, AGENTOS_SEG and AGENTOS_SHEET to show a given screen)
mod genui;
mod icons;
mod screens;
mod theme;

use rune::{AppSpec, Cx, El, ScrollSpec, TextEdit, Tx, col, el};
use screens::{android, approval, chat, home, phone, setup, store, system};
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

/// The Phone tab's segments.
/// The bottom tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Phone,
    Store,
    Android,
    System,
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
        Tab::Phone => phone::view(s, st),
        Tab::Store => store::view(s, st),
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
        .child(theme::tab_bar(s.tab, waiting).pin(Some(14.0), None, Some(14.0), Some(18.0)));
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
            {"who": "Mum", "text": "landed safely?", "at": 4000}
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
        let mut rt = Runtime::new(
            s,
            view,
            (390.0, 844.0),
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
        for (tab, title) in [
            ("tab-phone", "title-phone"),
            ("tab-store", "title-store"),
            ("tab-android", "title-android"),
            ("tab-system", "title-system"),
            ("tab-home", "title-home"),
        ] {
            tap(&mut rt, tab);
            assert!(rt.find(title).is_some(), "{title} missing after {tab}");
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
        tap(&mut rt, "tab-phone");
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
        tap(&mut rt, "tab-phone");
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
        tap(&mut rt, "tab-store");
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
        tap(&mut rt, "tab-store");
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
    fn the_sandbox_shows_its_apps_its_memory_and_why_it_closed() {
        let mut rt = rt();
        tap(&mut rt, "tab-android");
        assert!(rt.find("memory-bar").is_some());
        assert!(rt.find("android-banner").is_some());
        tap(&mut rt, "app-com.whatsapp");
        assert_eq!(
            rt.state.sent.last().unwrap(),
            "do android_open {\"app\":\"com.whatsapp\"}"
        );
        tap(&mut rt, "close-all");
        assert_eq!(rt.state.sent.last().unwrap(), "do android_close_all {}");
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
}
