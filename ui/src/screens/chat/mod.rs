//! The assistant, full screen. One conversation, six ways to see it (plan v3, D.4): Poster,
//! Board, Steps, Thread, Voice and Reader. Every view draws the same turns: what was asked, what
//! came back, and the screen the backend generated for it. The switcher under the header picks
//! the view; with no pick, the newest answer's layout decides.
pub mod board;
pub mod poster;
pub mod reader;
pub mod steps;
pub mod thread;
pub mod voice;

use crate::icons;
use crate::theme::*;
use crate::{Mode, State};
use rune::{El, ScrollSpec, Tx, col, el, icon, row, text_input};
use serde_json::Value;

/// The screen the backend made for question `i`: the first one made after it was asked (and
/// before the next question was asked). Screens come newest first, so search from the oldest.
pub fn screen_for(st: &Value, s: &State, i: usize) -> Option<Value> {
    let after = *s.asked_after.get(i)?;
    let before = s.asked_after.get(i + 1).copied().unwrap_or(u64::MAX);
    st["screens"]
        .as_array()?
        .iter()
        .rev()
        .find(|sc| {
            let at = sc["at"].as_u64().unwrap_or(0);
            at > after && at <= before
        })
        .map(|sc| sc["ui"].clone())
}

/// An answer without its "done:" or "not done:" prefix, and whether it was done.
pub fn answer_text(a: &str) -> (String, bool) {
    if let Some(rest) = a.strip_prefix("done: ") {
        (rest.to_string(), true)
    } else if let Some(rest) = a.strip_prefix("not done: ") {
        (rest.to_string(), false)
    } else {
        (a.to_string(), true)
    }
}

/// One exchange, ready to draw.
pub struct Turn {
    pub i: usize,
    pub q: String,
    /// the answer in words ("" while it is being worked on)
    pub text: String,
    pub ok: bool,
    pub thinking: bool,
    pub doc: Option<Value>,
    /// an action that waits for the owner's approval
    pub approval: Option<u64>,
}

pub fn turns(s: &State, st: &Value) -> Vec<Turn> {
    s.log
        .iter()
        .enumerate()
        .map(|(i, (q, a))| {
            let (text, ok) = answer_text(a);
            Turn {
                i,
                q: q.clone(),
                approval: approval_id(a),
                text,
                ok,
                thinking: a.is_empty(),
                doc: screen_for(st, s, i),
            }
        })
        .collect()
}

/// "not done: approval required (id 3)" → 3.
pub fn approval_id(answer: &str) -> Option<u64> {
    let at = answer.find("(id ")?;
    answer[at + 4..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

/// The accent of an answer: its own, or its kind's.
pub fn accent_of(doc: &Value) -> String {
    let a = str_of(&doc["accent"]);
    if a.is_empty() {
        str_of(&doc["kind"])
    } else {
        a
    }
}

/// What a Field shows for an answer: a label, the big value, and one line.
pub fn headline(turn: &Turn) -> (String, String, String) {
    let Some(doc) = &turn.doc else {
        let line = if turn.thinking {
            "Working on it…".to_string()
        } else {
            turn.text.clone()
        };
        return ("Answer".into(), String::new(), line);
    };
    let cards = doc["cards"].as_array().cloned().unwrap_or_default();
    let label = str_of(&doc["title"]);
    let hero = cards
        .iter()
        .find_map(|c| c["metric"]["value"].as_str().map(String::from))
        .unwrap_or_default();
    let line = cards
        .iter()
        .find_map(|c| {
            let sub = str_of(&c["subtitle"]);
            if !sub.is_empty() {
                return Some(sub);
            }
            c["lines"].as_array().and_then(|l| l.first()).map(str_of)
        })
        .or_else(|| cards.first().map(|c| str_of(&c["title"])))
        .unwrap_or_else(|| turn.text.clone());
    (label, hero, line)
}

/// The first chart in an answer.
pub fn first_chart(doc: &Value) -> Option<Value> {
    doc["cards"]
        .as_array()?
        .iter()
        .find_map(|c| c.get("chart").filter(|v| v.is_object()).cloned())
}

/// The view to draw: the owner's pick, else the newest answer's layout, else the thread.
pub fn mode_now(s: &State, st: &Value) -> Mode {
    if let Some(m) = s.chat_mode {
        return m;
    }
    s.log
        .len()
        .checked_sub(1)
        .and_then(|i| screen_for(st, s, i))
        .and_then(|doc| Mode::from_layout(doc["layout"].as_str().unwrap_or("")))
        .unwrap_or(Mode::Thread)
}

/// Approve an action: with a PIN set the sheet asks for it, else the key is pressed at once.
pub fn approve(s: &mut State, id: u64) {
    if s.snap["pin_set"].as_bool() == Some(true) {
        s.sheet = Some(id);
        s.pin.clear();
    } else {
        s.send(format!("press {id}"));
    }
}

/// Before anything is asked: the mark, a line, and four things to try.
pub fn empty_state() -> El {
    let mut ideas = row().w_full().gap(8.0).flex_wrap().center();
    for (i, (label, say)) in [
        ("Weather", "weather"),
        ("Take a photo", "take a photo"),
        ("Android messages", "android inbox"),
        ("Files from Android", "received files"),
    ]
    .into_iter()
    .enumerate()
    {
        ideas = ideas.child(
            text_chip(&format!("start-{i}"), label, false)
                .on_press(move |s: &mut State| s.ask(say)),
        );
    }
    col()
        .key("chat-empty")
        .w_full()
        .items_center()
        .gap(14.0)
        .pt(48.0)
        .child(
            row()
                .size(64.0, 64.0)
                .center()
                .radius(32.0)
                .fill(ACCENT_FILL)
                .child(icon(&icons::SPARK, 30.0).color(rune::Color::WHITE)),
        )
        .child(t("Ask anything", TITLE, TEXT))
        .child(
            t(
                "Answers come as screens. Pick how to see them above.",
                BODY,
                TEXT2,
            )
            .wrap(),
        )
        .child(ideas)
}

fn mode_icon(m: Mode) -> &'static rune::Icon {
    match m {
        Mode::Poster => &icons::POSTER,
        Mode::Board => &icons::GRID,
        Mode::Steps => &icons::STEPS,
        Mode::Thread => &icons::MESSAGE,
        Mode::Voice => &icons::MIC,
        Mode::Reader => &icons::DOC,
    }
}

pub fn view(s: &State, st: &Value) -> El {
    let mode = mode_now(s, st);
    let stage = str_of(&st["stage"]);

    // the header
    let top = row()
        .key("chat-top")
        .w_full()
        .h(56.0)
        .px(14.0)
        .gap(12.0)
        .center_y()
        .child(
            round("chat-back", &icons::BACK, 40.0, SURFACE2, TEXT, "Back")
                .on_press(|s: &mut State| s.chat_open = false),
        )
        .child(
            col()
                .grow(1.0)
                .gap(1.0)
                .child(t("Ask", TITLE, TEXT))
                .child(t(
                    format!("{} · {} answers", mode.label(), s.log.len()),
                    CAPTION,
                    TEXT2,
                )),
        )
        .child(
            row()
                .h(26.0)
                .px(10.0)
                .gap(6.0)
                .center_y()
                .radius(13.0)
                .fill(SURFACE2)
                .child(el().size(7.0, 7.0).radius(3.5).fill(if stage == "Active" {
                    GOOD
                } else {
                    DIM
                }))
                .child(t("On this phone", CAPTION, TEXT2)),
        );

    // six ways to see an answer
    let mut switch = row().key("chat-modes").gap(6.0).px(14.0);
    for m in Mode::ALL {
        let on = m == mode;
        switch = switch.child(
            row()
                .key(format!("mode-{}", m.name()))
                .h(36.0)
                .px(12.0)
                .gap(6.0)
                .center_y()
                .radius(18.0)
                .fill(if on { ACCENT_FILL } else { SURFACE })
                .border(1.0, if on { ACCENT_FILL } else { HAIRLINE })
                .role("tab")
                .label(m.label())
                .focusable(true)
                .press_scale(0.96)
                .child(icon(mode_icon(m), 15.0).color(if on { rune::Color::WHITE } else { TEXT }))
                .child(t(
                    m.label(),
                    CALLOUT,
                    if on { rune::Color::WHITE } else { TEXT },
                ))
                .on_press(move |s: &mut State| {
                    s.chat_mode = Some(m);
                    if m == Mode::Poster {
                        s.poster_at = None;
                    }
                }),
        );
    }
    let switch = switch.scroll_x();

    let stage_el = match mode {
        Mode::Poster => poster::view(s, st),
        Mode::Board => board::view(s, st),
        Mode::Steps => steps::view(s, st),
        Mode::Thread => thread::view(s, st),
        Mode::Voice => voice::view(s, st),
        Mode::Reader => reader::view(s, st),
    };
    let stage_el = match mode {
        // the voice view and the poster fill the stage; the rest scroll
        Mode::Voice | Mode::Poster => stage_el.grow(1.0).min_h(0.0),
        _ => stage_el
            .scroll(ScrollSpec {
                stick_bottom: mode == Mode::Thread,
            })
            .grow(1.0)
            .min_h(0.0),
    };

    let bottom = if mode == Mode::Voice {
        // hands-free: one big microphone instead of the box
        col().w_full().items_center().pb(22.0).pt(8.0).child(
            row()
                .key("voice-mic")
                .size(72.0, 72.0)
                .center()
                .radius(36.0)
                .fill(if s.voice_on { CORAL } else { ACCENT_FILL })
                .role("button")
                .label(if s.voice_on {
                    "Stop listening"
                } else {
                    "Speak"
                })
                .press_scale(0.94)
                .child(icon(&icons::MIC, 30.0).color(rune::Color::WHITE))
                .on_press(|s: &mut State| s.voice_on = !s.voice_on),
        )
    } else {
        composer(s, st)
    };

    col()
        .w_full()
        .h_full()
        .fill(BG)
        .child(status_bar(st))
        .child(top)
        .child(switch)
        .child(el().h(8.0))
        .child(stage_el)
        .child(bottom)
}

/// Suggestions from the newest answer, and the box.
fn composer(s: &State, st: &Value) -> El {
    let mut ideas: Vec<(String, String)> = s
        .log
        .len()
        .checked_sub(1)
        .and_then(|i| screen_for(st, s, i))
        .map(|doc| {
            doc["actions"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|b| (str_of(&b["label"]), str_of(&b["say"])))
                .collect()
        })
        .unwrap_or_default();
    if ideas.is_empty() {
        ideas = [
            ("Take a photo", "take a photo"),
            ("Weather", "weather"),
            ("Android messages", "android inbox"),
            ("Read my messages", "read my messages"),
        ]
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    }
    let mut chips = row()
        .key("chat-ideas")
        .w_full()
        .gap(8.0)
        .px(16.0)
        .flex_wrap();
    for (i, (label, say)) in ideas.into_iter().take(4).enumerate() {
        chips = chips.child(
            text_chip(&format!("idea-{i}"), &label, false)
                .on_press(move |s: &mut State| s.ask(&say)),
        );
    }
    let bar = row()
        .key("chat-bar")
        .w_full()
        .h(56.0)
        .pl(6.0)
        .pr(6.0)
        .gap(8.0)
        .center_y()
        .radius(28.0)
        .fill(SURFACE)
        .border(1.0, VIOLET.alpha(0.4))
        .child(
            round(
                "chat-voice",
                &icons::MIC,
                40.0,
                VIOLET.alpha(0.16),
                VIOLET,
                "Speak",
            )
            .on_press(|s: &mut State| s.chat_mode = Some(Mode::Voice)),
        )
        .child(
            text_input(&s.chat, BODY)
                .key("chat")
                .single_line()
                .placeholder("Ask anything…", DIM)
                .grow(1.0)
                .h(22.0)
                .on_edit(|s: &mut State| &mut s.chat)
                .on_submit(|s: &mut State, _tx: &mut Tx| {
                    let said = s.chat.text.trim().to_string();
                    s.chat.clear();
                    s.ask(&said);
                }),
        )
        .child(
            round(
                "chat-send",
                &icons::SEND,
                44.0,
                ACCENT_FILL,
                rune::Color::WHITE,
                "Send",
            )
            .on_press(|s: &mut State| {
                let said = s.chat.text.trim().to_string();
                s.chat.clear();
                s.ask(&said);
            }),
        );
    col()
        .w_full()
        .gap(10.0)
        .pb(16.0)
        .child(chips)
        .child(col().w_full().px(14.0).child(bar))
}
