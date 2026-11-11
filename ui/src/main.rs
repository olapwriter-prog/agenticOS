//! agentos-ui: the phone's screen, built with Rune. The OS itself is headless: agentd is the
//! phone, and this app is only one of its clients, talking to it over the same control socket as
//! `agentd ctl`. It shows what the agent is doing and lets you answer it: approve or decline a
//! call or text, see recent actions, the model and the chip, your functions, and chat.
//!
//!   agentos-ui                         open the window (socket: $AGENTD_SOCK or /run/agentd.sock)
//!   agentos-ui --snap ui.png --scene demo    render headless, with sample data
//!   agentos-ui --snap ui.png --scene live    render headless, from a running agentd
mod screens;
mod widgets;

use rune::tok::{self, type_};
use rune::{AppSpec, Cx, El, ScrollSpec, TextEdit, Tx, col, row, text};
use screens::{android, approval, home, phone, setup, store, system};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;
use widgets::{button, str_of};

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
    /// text fields
    pub setup_input: TextEdit,
    pub keypad: TextEdit,
    pub new_name: TextEdit,
    pub new_number: TextEdit,
    pub store_q: TextEdit,
    pub token: TextEdit,
    pub paste: TextEdit,
    /// the PIN being typed on the approval sheet (never logged or sent unmasked)
    pub pin: String,
    /// the pending action whose sheet is open
    pub sheet: Option<u64>,
    /// (what was asked, what came back) for the store and sandbox commands, newest last
    pub replies: Vec<(String, String)>,
    snap: Value,
    error: Option<String>,
    pub chat: TextEdit,
    /// (what you said, what the agent answered)
    pub log: Vec<(String, String)>,
    /// every command sent, newest last (tests read this)
    pub sent: Vec<String>,
    link: Option<Sender<String>>,
    inbox: Option<Receiver<Event>>,
}

impl State {
    fn new() -> State {
        State {
            tab: Tab::Home,
            seg: Seg::Recents,
            setup_input: TextEdit::new(""),
            keypad: TextEdit::new(""),
            new_name: TextEdit::new(""),
            new_number: TextEdit::new(""),
            store_q: TextEdit::new(""),
            token: TextEdit::new(""),
            paste: TextEdit::new(""),
            pin: String::new(),
            sheet: None,
            replies: vec![],
            snap: Value::Null,
            error: None,
            chat: TextEdit::new(""),
            log: vec![],
            sent: vec![],
            link: None,
            inbox: None,
        }
    }
    /// Send a control command; the answer arrives later through `poll`.
    pub fn send(&mut self, line: String) {
        self.send_shown(line.clone(), line);
    }

    /// Send `line`, but record `shown` (with secrets masked) in what the UI keeps.
    pub fn send_shown(&mut self, line: String, shown: String) {
        if let Some(l) = &self.link {
            let _ = l.send(line);
        }
        self.sent.push(shown);
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

// ---- look -----------------------------------------------------------------------------------

const H1: rune::tok::TextStyle = rune::tok::TextStyle::new(22.0, 700.0);

/// The bottom tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Phone,
    Store,
    Android,
    System,
}

fn view(s: &State, _: &mut Cx<State>) -> El {
    let t = tok::theme(true);
    let accent = tok::accent(0, true);
    let st = &s.snap;

    let stage = str_of(&st["stage"]);
    let header = row()
        .key("header")
        .w_full()
        .center_y()
        .gap(10.0)
        .child(text("Agentic OS").font(H1).color(t.text).grow(1.0))
        .child(
            row()
                .key("stage")
                .h(24.0)
                .px(10.0)
                .center()
                .radius(12.0)
                .fill(if stage == "Active" {
                    accent
                } else {
                    t.control_off
                })
                .child(
                    text(if stage.is_empty() {
                        "…".to_string()
                    } else {
                        stage.clone()
                    })
                    .font(type_::LABEL)
                    .color(t.text),
                ),
        );
    let status = widgets::quiet(
        match &s.error {
            Some(e) => e.clone(),
            None => format!("{}  ·  {}", str_of(&st["status"]), str_of(&st["phone"])),
        },
        t,
    )
    .key("status")
    .w_full()
    .wrap();

    let (key, title) = match s.tab {
        Tab::Home => ("home", "Home"),
        Tab::Phone => ("phone", "Phone"),
        Tab::Store => ("store", "Store"),
        Tab::Android => ("android", "Android"),
        Tab::System => ("system", "System"),
    };
    let title_el = text(title)
        .key(format!("title-{key}"))
        .font(H1)
        .color(t.text);

    let body = match s.tab {
        Tab::Home => home::cards(st, t, accent),
        Tab::System => system::view(s, st, t, accent),
        Tab::Phone => phone::view(s, st, t, accent),
        Tab::Store => store::view(s, st, t, accent),
        Tab::Android => android::view(s, st, t, accent),
    };

    // the tab bar: five tabs, the one shown in the accent colour
    let mut bar = row().key("tabs").w_full().gap(6.0);
    for (tab, label, k) in [
        (Tab::Home, "Home", "tab-home"),
        (Tab::Phone, "Phone", "tab-phone"),
        (Tab::Store, "Store", "tab-store"),
        (Tab::Android, "Android", "tab-android"),
        (Tab::System, "System", "tab-system"),
    ] {
        let active = s.tab == tab;
        bar = bar.child(
            button(
                k.to_string(),
                label,
                if active { accent } else { t.control_off },
                t.text,
            )
            .grow(1.0)
            .on_press(move |s: &mut State| s.tab = tab),
        );
    }

    // while setup is not done it is shown over everything
    if !st["setup"].is_null() {
        return setup::view(s, st, t, accent);
    }
    let mut page = col()
        .w_full()
        .h_full()
        .padding(16.0)
        .gap(12.0)
        .fill(t.bg)
        .child(header)
        .child(status)
        .child(title_el)
        .child(
            body.scroll(ScrollSpec {
                stick_bottom: false,
            })
            .grow(1.0)
            .min_h(0.0),
        );
    if s.tab == Tab::Home {
        page = page.child(screens::home::chat(s, t, accent));
    }
    page = page.child(bar);
    // the approval sheet, over the page, for the action that was tapped
    if let Some(id) = s.sheet {
        return approval::sheet(s, id, t, accent);
    }
    page
}

/// What a phone mid-afternoon looks like, for headless renders and tests.
fn demo() -> Value {
    json!({
        "stage": "Active",
        "status": "Active, background work paused, battery 76%",
        "phone": "Jio lte signal 64%, mobile data on, wifi HomeNet",
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
        "models": ["qwen3.5-4b (small, port 8080): running pid 812, ready", "qwen3.5-9b (large, port 8081): stopped"],
        "prio": "mode Idle, model cores [4,5,6,7] of [0,1,2,3,4,5,6,7] (fast [4,5,6,7], reserved [0]), 4 threads, temp 41.0C",
        "functions": ["split_bill(total, people) v2: share of a bill", "add_gst(price, gst_percent) v1: price with GST"],
        "learning": "42 episodes, 17 usable for learning, 2 corrections",
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
            {"name": "Ravi", "number": "+91 98123 45678"}
        ],
        "connectors": [
            {"name": "telegram", "state": "connected", "tools": 2},
            {"name": "google", "state": "unavailable", "tools": 0}
        ],
        "store": {"installed": ["trip-recipe"]},
        "android": {
            "mode": "sim",
            "running": true,
            "apps": [
                {"name": "Notes", "package": "org.example.notes"},
                {"name": "WhatsApp", "package": "com.whatsapp"}
            ],
            "last_stop": {"reason": "only 500 MiB free", "at_ms": 5000}
        },
        "memory": {"available_mib": 1800, "psi": 1.2}
    })
}

fn main() {
    rune::run(AppSpec::<State> {
        scenes: &[
            ("demo", |s| {
                s.snap = demo();
                s.log.push((
                    "how many texts did Mum send me".into(),
                    "answer (attempt 1): Mum sent 4 texts".into(),
                ));
            }),
            ("setup", |s| {
                let mut d = demo();
                d["setup"] = json!({
                    "id": "wifi", "index": 2, "total": 13, "title": "Connect to Wi-Fi",
                    "body": "Home\nOffice", "options": ["skip"], "input": "text",
                    "can_skip": true, "error": null
                });
                s.snap = d;
            }),
            ("phone", |s| {
                s.snap = demo();
                s.tab = Tab::Phone;
            }),
            ("store", |s| {
                s.snap = demo();
                s.tab = Tab::Store;
                s.replies.push((
                    "do store_search {\"text\":\"agent\"}".into(),
                    "trip-planner: Trip planner — Plans a trip\ndaily-digest: Daily digest — A morning summary".into(),
                ));
            }),
            ("android", |s| {
                s.snap = demo();
                s.tab = Tab::Android;
            }),
            ("system", |s| {
                s.snap = demo();
                s.tab = Tab::System;
            }),
            ("sheet", |s| {
                s.snap = demo();
                s.sheet = Some(3);
            }),
            ("live", |s| {
                match ctl("state").and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
                {
                    Ok(v) => s.snap = v,
                    Err(e) => s.error = Some(e),
                }
                // for snapshots of a state: AGENTOS_TAB=home|phone|store|android|system,
                // AGENTOS_SEG=recents|messages|contacts|keypad, AGENTOS_SHEET=<pending id>
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
            }),
        ],
        setup: Some(setup),
        poll: Some(poll),
        min_size: Some((360.0, 600.0)),
        ..AppSpec::new("Agentic OS", (390.0, 844.0), State::new, view)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Runtime};

    fn rt() -> Runtime<State> {
        let mut s = State::new();
        s.snap = demo();
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
    fn tap(rt: &mut Runtime<State>, key: &str) {
        let (x, y) = rt.find(key).unwrap_or_else(|| panic!("no element {key}"));
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        for _ in 0..4 {
            rt.frame(1.0 / 60.0);
        }
    }

    #[test]
    fn approving_on_screen_presses_the_key_for_that_item() {
        let mut rt = rt();
        // no PIN set: Approve presses the key straight away
        rt.state.snap["pin_set"] = json!(false);
        rt.frame(0.0);
        tap(&mut rt, "approve-3");
        tap(&mut rt, "decline-4");
        assert_eq!(rt.state.sent, vec!["press 3", "decline 4"]);
    }
    #[test]
    fn everything_the_daemon_reports_is_on_screen() {
        let mut rt = rt();
        for key in [
            "header",
            "stage",
            "waiting",
            "pending-3",
            "pending-4",
            "recent",
            "recent-4",
            "chat",
        ] {
            assert!(rt.find(key).is_some(), "{key} missing");
        }
        tap(&mut rt, "tab-system");
        for key in ["model", "functions"] {
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
    fn nothing_waiting_means_no_waiting_card() {
        let mut s = State::new();
        let mut d = demo();
        d["pending"] = json!([]);
        s.snap = d;
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
        assert!(rt.find("waiting").is_none());
    }
    #[test]
    fn a_chat_answer_lands_next_to_its_question() {
        let mut s = State::new();
        s.log.push(("call mum".into(), String::new()));
        let (tx, rx) = channel();
        s.inbox = Some(rx);
        tx.send(Event::Reply {
            asked: "chat call mum".into(),
            answer: "not done: approval required (id 1)".into(),
        })
        .unwrap();
        let mut rt = Runtime::new(
            s,
            view,
            (390.0, 844.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.poll = Some(poll);
        rt.frame(0.0);
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.state.log[0].1, "not done: approval required (id 1)");
    }

    fn rt_with(mut snap: Value, setup: bool) -> Runtime<State> {
        let mut s = State::new();
        if setup {
            snap = demo();
            snap["setup"] = json!({"id": "wifi", "index": 2, "total": 13, "title": "Connect to Wi-Fi", "body": "Home", "options": ["skip"], "input": "text", "can_skip": true, "error": null});
        }
        s.snap = snap;
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

    #[test]
    fn setup_is_shown_over_everything_and_its_answers_are_sent() {
        let mut rt = rt_with(Value::Null, true);
        assert!(rt.find("setup-screen").is_some());
        assert!(
            rt.find("tab-home").is_none(),
            "the tabs are not shown during setup"
        );
        tap(&mut rt, "setup-option-0");
        assert_eq!(rt.state.sent.last().unwrap(), "setup answer skip");
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
        tap(&mut rt, "seg-keypad");
        tap(&mut rt, "answer-call");
        assert_eq!(rt.state.sent.last().unwrap(), "do answer_call {}");
    }

    #[test]
    fn the_store_searches_by_category_and_installs_from_the_results() {
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
    fn a_token_is_sent_masked_and_cleared_from_the_field() {
        let mut rt = rt();
        tap(&mut rt, "tab-system");
        assert!(rt.find("connector-telegram").is_some());
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
}
