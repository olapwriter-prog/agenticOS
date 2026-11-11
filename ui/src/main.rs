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
use screens::{home, placeholder, system};
use widgets::{button, str_of};
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

pub struct State {
    pub tab: Tab,
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
        if let Some(l) = &self.link {
            let _ = l.send(line.clone());
        }
        self.sent.push(line);
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
                .fill(if stage == "Active" { accent } else { t.control_off })
                .child(
                    text(if stage.is_empty() { "…".to_string() } else { stage.clone() })
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
    let title_el = text(title).key(format!("title-{key}")).font(H1).color(t.text);

    let body = match s.tab {
        Tab::Home => home::cards(st, t, accent),
        Tab::System => system::view(st, t),
        Tab::Phone | Tab::Store | Tab::Android => placeholder::view(&format!("screen-{key}"), title, t),
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
            button(k.to_string(), label, if active { accent } else { t.control_off }, t.text)
                .grow(1.0)
                .on_press(move |s: &mut State| s.tab = tab),
        );
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
    page.child(bar)
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
        "learning": "42 episodes, 17 usable for learning, 2 corrections"
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
            ("live", |s| {
                match ctl("state").and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
                {
                    Ok(v) => s.snap = v,
                    Err(e) => s.error = Some(e),
                }
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
        tap(&mut rt, "approve-3");
        tap(&mut rt, "decline-4");
        assert_eq!(rt.state.sent, vec!["press 3", "decline 4"]);
    }
    #[test]
    fn everything_the_daemon_reports_is_on_screen() {
        let mut rt = rt();
        for key in ["header", "stage", "waiting", "pending-3", "pending-4", "recent", "recent-4", "chat"] {
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
}
