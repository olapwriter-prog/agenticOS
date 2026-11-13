//! Phone: recents, messages, contacts and the dial pad. Every action is a tool call, with the
//! same tiers and approval steps as asking in words.
use crate::icons;
use crate::theme::*;
use crate::{Seg, State};
use rune::{El, col, icon, row};
use serde_json::{Value, json};

fn call_cmd(to: &str) -> String {
    format!("do call {}", json!({"to": to}))
}

pub fn view(s: &State, st: &Value) -> El {
    let calls = st["calls"].as_array().cloned().unwrap_or_default();
    let ringing = calls
        .last()
        .filter(|c| c["state"].as_str() == Some("incoming"))
        .map(|c| str_of(&c["who"]));
    let mut page =
        col()
            .key("phone")
            .w_full()
            .gap(18.0)
            .child(page_title("title-phone", "Phone", None));

    // an incoming call comes first, with Answer and Decline
    if let Some(who) = &ringing {
        page = page.child(
            row()
                .key("incoming")
                .w_full()
                .padding(14.0)
                .gap(12.0)
                .center_y()
                .radius(22.0)
                .fill(GOOD.alpha(0.12))
                .border(1.0, GOOD.alpha(0.35))
                .child(avatar(who, 44.0))
                .child(
                    col()
                        .grow(1.0)
                        .shrink()
                        .min_w(0.0)
                        .gap(2.0)
                        .child(t(who.clone(), HEADLINE, TEXT))
                        .child(t("Incoming call", CAPTION, GOOD)),
                )
                .child(
                    round(
                        "hangup",
                        &icons::CLOSE,
                        46.0,
                        BAD,
                        rune::Color::WHITE,
                        "Decline",
                    )
                    .on_press(|s: &mut State| s.send("do hangup {}".into())),
                )
                .child(
                    round(
                        "answer-call",
                        &icons::PHONE,
                        46.0,
                        GOOD,
                        rune::Color::WHITE,
                        "Answer",
                    )
                    .on_press(|s: &mut State| s.send("do answer_call {}".into())),
                ),
        );
    }

    // the segmented control
    let mut seg = row()
        .key("phone-segments")
        .w_full()
        .h(42.0)
        .padding(4.0)
        .radius(21.0)
        .fill(SURFACE2);
    for (which, label, key) in [
        (Seg::Recents, "Recents", "seg-recents"),
        (Seg::Messages, "Messages", "seg-messages"),
        (Seg::Contacts, "Contacts", "seg-contacts"),
        (Seg::Keypad, "Keypad", "seg-keypad"),
    ] {
        let on = s.seg == which;
        seg = seg.child(
            row()
                .key(key)
                .grow(1.0)
                .h_full()
                .center()
                .radius(17.0)
                .fill(if on {
                    SURFACE3
                } else {
                    rune::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.0,
                    }
                })
                .role("tab")
                .label(label)
                .press_scale(0.97)
                .child(t(label, CALLOUT, if on { TEXT } else { MUTED }))
                .on_press(move |s: &mut State| s.seg = which),
        );
    }
    page = page.child(seg);

    page.child(match s.seg {
        Seg::Recents => recents(&calls),
        Seg::Messages => messages(st),
        Seg::Contacts => contacts(s, st),
        Seg::Keypad => keypad_view(s),
    })
}

fn empty(msg: &str) -> El {
    card("empty")
        .py(28.0)
        .items_center()
        .child(t(msg, BODY, MUTED))
}

fn recents(calls: &[Value]) -> El {
    if calls.is_empty() {
        return empty("No calls yet");
    }
    let mut c = card("calls").gap(0.0).py(6.0);
    for (i, call) in calls.iter().rev().enumerate() {
        let who = str_of(&call["who"]);
        let state = str_of(&call["state"]);
        let (ic, col_, label) = match state.as_str() {
            "missed" => (&icons::MISSED, BAD, "Missed"),
            "incoming" => (&icons::INCOMING, GOOD, "Incoming"),
            _ => (&icons::OUTGOING, MUTED, "Outgoing"),
        };
        if i > 0 {
            c = c.child(divider());
        }
        let w = who.clone();
        c = c.child(
            row()
                .key(format!("call-row-{i}"))
                .w_full()
                .min_h(60.0)
                .gap(14.0)
                .center_y()
                .child(avatar(&who, 40.0))
                .child(
                    col()
                        .grow(1.0)
                        .shrink()
                        .min_w(0.0)
                        .gap(3.0)
                        .child(t(
                            who.clone(),
                            HEADLINE,
                            if state == "missed" { BAD } else { TEXT },
                        ))
                        .child(
                            row()
                                .gap(5.0)
                                .center_y()
                                .child(icon(ic, 13.0).color(col_))
                                .child(t(label, CAPTION, MUTED)),
                        ),
                )
                .child(
                    round(
                        &format!("callback-{i}"),
                        &icons::PHONE,
                        40.0,
                        GOOD.alpha(0.16),
                        GOOD,
                        "Call back",
                    )
                    .on_press(move |s: &mut State| s.send(call_cmd(&w))),
                ),
        );
    }
    c
}

fn messages(st: &Value) -> El {
    let msgs = st["messages"].as_array().cloned().unwrap_or_default();
    if msgs.is_empty() {
        return empty("No messages yet");
    }
    let mut c = card("messages").gap(0.0).py(6.0);
    for (i, m) in msgs.iter().rev().enumerate() {
        let who = str_of(&m["who"]);
        if i > 0 {
            c = c.child(divider());
        }
        c = c.child(list_row(
            &format!("message-{i}"),
            avatar(&who, 40.0),
            &who,
            &str_of(&m["text"]),
            Some(icon(&icons::NEXT, 16.0).color(DIM)),
        ));
    }
    c
}

fn contacts(s: &State, st: &Value) -> El {
    let list = st["contacts"].as_array().cloned().unwrap_or_default();
    let mut c = card("contacts").gap(0.0).py(6.0);
    if list.is_empty() {
        c = c.py(20.0).child(t("No contacts yet", BODY, MUTED));
    }
    for (i, ct) in list.iter().enumerate() {
        let name = str_of(&ct["name"]);
        let number = str_of(&ct["number"]);
        let me = name == "Me";
        let shown = if me { "You".to_string() } else { name.clone() };
        if i > 0 {
            c = c.child(divider());
        }
        let n = name.clone();
        let lead = avatar(if me { "You" } else { &name }, 40.0);
        let trail = if me {
            row()
                .h(24.0)
                .px(10.0)
                .center()
                .radius(12.0)
                .fill(ACCENT.alpha(0.16))
                .child(t("Owner", MICRO, ACCENT))
        } else {
            round(
                &format!("call-{i}"),
                &icons::PHONE,
                40.0,
                GOOD.alpha(0.16),
                GOOD,
                "Call",
            )
            .on_press(move |s: &mut State| s.send(call_cmd(&n)))
        };
        c = c.child(list_row(
            &format!("contact-{i}"),
            lead,
            &shown,
            &number,
            Some(trail),
        ));
    }
    let form = card("add-contact")
        .child(section("New contact"))
        .child(field(&s.new_name, "new-name", "Name").on_edit(|s: &mut State| &mut s.new_name))
        .child(
            field(&s.new_number, "new-number", "Phone number")
                .on_edit(|s: &mut State| &mut s.new_number),
        )
        .child(
            primary("save-contact", "Save contact").on_press(|s: &mut State| {
                let (name, number) = (
                    s.new_name.text.trim().to_string(),
                    s.new_number.text.trim().to_string(),
                );
                s.new_name.clear();
                s.new_number.clear();
                if !name.is_empty() && !number.is_empty() {
                    s.send(format!(
                        "do add_contact {}",
                        json!({"name": name, "number": number})
                    ));
                }
            }),
        );
    col().w_full().gap(16.0).child(c).child(form)
}

fn dial(s: &mut State, k: &str) {
    let mut v = s.keypad.text.clone();
    if k == "⌫" {
        v.pop();
    } else {
        v.push_str(k);
    }
    s.keypad = rune::TextEdit::new(&v);
}

fn keypad_view(s: &State) -> El {
    let number = s.keypad.text.clone();
    col()
        .key("keypad-view")
        .w_full()
        .gap(22.0)
        .items_center()
        .pt(6.0)
        .child(row().key("keypad").w_full().h(56.0).center().child(t(
            if number.is_empty() {
                "Enter a number".to_string()
            } else {
                number
            },
            TextStyle::new(30.0, 500.0),
            if s.keypad.text.is_empty() { DIM } else { TEXT },
        )))
        .child(keypad_sized("kp-", dial, 64.0))
        .child(
            round(
                "keypad-call",
                &icons::PHONE,
                72.0,
                GOOD,
                rune::Color::WHITE,
                "Call",
            )
            .on_press(|s: &mut State| {
                let to = s.keypad.text.trim().to_string();
                s.keypad.clear();
                if !to.is_empty() {
                    s.send(call_cmd(&to));
                }
            }),
        )
}

use rune::tok::TextStyle;
