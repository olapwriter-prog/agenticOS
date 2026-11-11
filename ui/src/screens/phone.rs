//! The Phone tab (F5): recent calls, messages, contacts and a keypad. Every action is a tool call,
//! so the same tiers and approval steps apply as in chat.
use crate::widgets::{button, card, line, quiet, str_of};
use crate::{Seg, State};
use rune::tok::{Theme, radius, type_};
use rune::{Color, El, col, row, text_input};
use serde_json::json;

pub fn view(s: &State, st: &serde_json::Value, t: Theme, accent: Color) -> El {
    let mut segs = row().key("phone-segments").gap(6.0).w_full();
    for (seg, label, key) in [
        (Seg::Recents, "Recents", "seg-recents"),
        (Seg::Messages, "Messages", "seg-messages"),
        (Seg::Contacts, "Contacts", "seg-contacts"),
        (Seg::Keypad, "Keypad", "seg-keypad"),
    ] {
        let fill = if s.seg == seg { accent } else { t.control_off };
        segs = segs.child(
            button(key.into(), label, fill, t.text)
                .grow(1.0)
                .on_press(move |s: &mut State| s.seg = seg),
        );
    }

    let body = match s.seg {
        Seg::Recents => recents(st, t),
        Seg::Messages => messages(st, t),
        Seg::Contacts => contacts(s, st, t),
        Seg::Keypad => keypad(s, t, accent),
    };

    let header = row()
        .key("phone-status")
        .w_full()
        .child(quiet(str_of(&st["phone"]), t).grow(1.0).shrink().truncate());

    col()
        .key("phone")
        .w_full()
        .gap(12.0)
        .child(header)
        .child(segs)
        .child(body)
}

fn recents(st: &serde_json::Value, t: Theme) -> El {
    let calls = st["calls"].as_array().cloned().unwrap_or_default();
    let mut list = col().gap(8.0);
    if calls.is_empty() {
        list = list.child(quiet("No calls yet.", t));
    }
    for (i, c) in calls.iter().rev().enumerate() {
        let who = str_of(&c["who"]);
        list = list.child(
            row()
                .key(format!("call-row-{i}"))
                .w_full()
                .center_y()
                .gap(8.0)
                .child(line(who.clone(), t).grow(1.0).shrink().truncate())
                .child(quiet(str_of(&c["state"]), t))
                .child(
                    button(format!("callback-{i}"), "Call", t.control_off, t.text).on_press(
                        move |s: &mut State| s.send(json!({"to": who}).to_string().pipe_do("call")),
                    ),
                ),
        );
    }
    card("calls", "Recent calls", list, t)
}

fn messages(st: &serde_json::Value, t: Theme) -> El {
    let msgs = st["messages"].as_array().cloned().unwrap_or_default();
    let mut list = col().gap(8.0);
    if msgs.is_empty() {
        list = list.child(quiet("No messages yet.", t));
    }
    for (i, m) in msgs.iter().rev().enumerate() {
        list = list.child(
            col()
                .key(format!("message-{i}"))
                .w_full()
                .gap(2.0)
                .child(line(str_of(&m["who"]), t))
                .child(quiet(str_of(&m["text"]), t).w_full().wrap()),
        );
    }
    card("messages", "Messages", list, t)
}

fn contacts(s: &State, st: &serde_json::Value, t: Theme) -> El {
    let mut list = col().gap(8.0);
    for (i, c) in st["contacts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let name = str_of(&c["name"]);
        list = list.child(
            row()
                .key(format!("contact-{i}"))
                .w_full()
                .center_y()
                .gap(8.0)
                .child(line(name.clone(), t).grow(1.0).shrink().truncate())
                .child(quiet(str_of(&c["number"]), t))
                .child(
                    button(format!("call-{i}"), "Call", t.control_off, t.text).on_press(
                        move |s: &mut State| {
                            s.send(json!({"to": name}).to_string().pipe_do("call"))
                        },
                    ),
                ),
        );
    }
    let form = row()
        .key("add-contact")
        .w_full()
        .gap(8.0)
        .child(
            text_input(&s.new_name, type_::BODY)
                .key("new-name")
                .single_line()
                .placeholder("Name", t.text_secondary)
                .grow(1.0)
                .h(40.0)
                .px(12.0)
                .radius(radius::SM)
                .fill(t.card)
                .on_edit(|s: &mut State| &mut s.new_name),
        )
        .child(
            text_input(&s.new_number, type_::BODY)
                .key("new-number")
                .single_line()
                .placeholder("Number", t.text_secondary)
                .grow(1.0)
                .h(40.0)
                .px(12.0)
                .radius(radius::SM)
                .fill(t.card)
                .on_edit(|s: &mut State| &mut s.new_number),
        )
        .child(
            button("save-contact".into(), "Save", t.control_off, t.text).on_press(
                |s: &mut State| {
                    let (name, number) = (
                        s.new_name.text.trim().to_string(),
                        s.new_number.text.trim().to_string(),
                    );
                    s.new_name.clear();
                    s.new_number.clear();
                    if !name.is_empty() && !number.is_empty() {
                        s.send(
                            json!({"name": name, "number": number})
                                .to_string()
                                .pipe_do("add_contact"),
                        );
                    }
                },
            ),
        );
    col()
        .w_full()
        .gap(10.0)
        .child(card("contacts", "Contacts", list, t))
        .child(form)
}

fn keypad(s: &State, t: Theme, accent: Color) -> El {
    let row_call = row()
        .key("keypad-row")
        .w_full()
        .gap(8.0)
        .child(
            text_input(&s.keypad, type_::BODY)
                .key("keypad")
                .single_line()
                .placeholder("Number or name", t.text_secondary)
                .grow(1.0)
                .h(44.0)
                .px(14.0)
                .radius(radius::SM)
                .fill(t.card)
                .on_edit(|s: &mut State| &mut s.keypad),
        )
        .child(
            button("keypad-call".into(), "Call", accent, t.text).on_press(|s: &mut State| {
                let to = s.keypad.text.trim().to_string();
                s.keypad.clear();
                if !to.is_empty() {
                    s.send(json!({"to": to}).to_string().pipe_do("call"));
                }
            }),
        );
    let calls = row()
        .key("call-controls")
        .w_full()
        .gap(8.0)
        .child(
            button("answer-call".into(), "Answer", t.control_off, t.text)
                .grow(1.0)
                .on_press(|s: &mut State| s.send("do answer_call {}".into())),
        )
        .child(
            button("hangup".into(), "Hang up", t.control_off, t.text)
                .grow(1.0)
                .on_press(|s: &mut State| s.send("do hangup {}".into())),
        );
    col().w_full().gap(10.0).child(row_call).child(calls)
}

/// `do <tool> <json>` for a tool the Phone tab calls.
trait PipeDo {
    fn pipe_do(self, tool: &str) -> String;
}
impl PipeDo for String {
    fn pipe_do(self, tool: &str) -> String {
        format!("do {tool} {self}")
    }
}
