//! Home: what waits for the human, what the agent did lately, and chat.
use crate::State;
use crate::widgets::{BAD, GOOD, button, card, line, quiet, str_of};
use rune::tok::{Theme, type_};
use rune::{Color, El, Tx, col, el, row, text_input};

/// The Home cards, scrollable; the chat box is placed by the caller, below them.
pub fn cards(st: &serde_json::Value, t: Theme, accent: Color) -> El {
    let mut cards = col().key("cards").w_full().gap(12.0);

    // what is waiting for the human: the approval key, on screen
    let pending = st["pending"].as_array().cloned().unwrap_or_default();
    if !pending.is_empty() {
        let mut list = col().gap(10.0);
        for p in &pending {
            let id = p["id"].as_u64().unwrap_or(0);
            let args: Vec<String> = p["args"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| format!("{k}: {}", str_of(v)))
                        .collect()
                })
                .unwrap_or_default();
            list = list.child(
                row()
                    .key(format!("pending-{id}"))
                    .w_full()
                    .center_y()
                    .gap(8.0)
                    // the details give way to the buttons, never the other way round
                    .child(
                        col()
                            .grow(1.0)
                            .shrink()
                            .min_w(0.0)
                            .gap(2.0)
                            .child(line(str_of(&p["tool"]).replace('_', " "), t).truncate())
                            .child(quiet(args.join("  ·  "), t).truncate()),
                    )
                    .child(
                        button(format!("decline-{id}"), "Decline", t.control_off, t.text)
                            .on_press(move |s: &mut State| s.send(format!("decline {id}"))),
                    )
                    .child(
                        button(format!("approve-{id}"), "Approve", accent, t.text).on_press(
                            move |s: &mut State| {
                                // with a PIN set, the sheet asks for it first
                                if s.snap["pin_set"].as_bool() == Some(true) {
                                    s.sheet = Some(id);
                                } else {
                                    s.send(format!("press {id}"));
                                }
                            },
                        ),
                    ),
            );
        }
        cards = cards.child(card("waiting", "Waiting for you", list, t));
    }

    // what the agent did lately, and how it went
    let recent = st["recent"].as_array().cloned().unwrap_or_default();
    let mut acts = col().gap(6.0);
    if recent.is_empty() {
        acts = acts.child(quiet("Nothing yet. Ask something below.", t));
    }
    for (i, e) in recent.iter().enumerate() {
        let r = e["reward"].as_f64().unwrap_or(0.0);
        let dot = el().size(8.0, 8.0).radius(4.0).fill(if r > 0.0 {
            GOOD
        } else if r < 0.0 {
            BAD
        } else {
            t.control_off
        });
        let tool = str_of(&e["tool"]);
        acts = acts.child(
            row()
                .key(format!("recent-{i}"))
                .w_full()
                .center_y()
                .gap(8.0)
                .child(dot)
                .child(line(str_of(&e["prompt"]), t).grow(1.0).shrink().truncate())
                .child(quiet(
                    if tool.is_empty() {
                        "no action".into()
                    } else {
                        tool.replace('_', " ")
                    },
                    t,
                )),
        );
    }
    cards = cards.child(card("recent", "Recent", acts, t));
    cards
}

/// Chat: the last three exchanges and the input box.
pub fn chat(s: &State, t: Theme, accent: Color) -> El {
    let mut log = col().key("chat-log").w_full().gap(6.0);
    for (i, (q, a)) in s.log.iter().rev().take(3).rev().enumerate() {
        log = log
            .child(
                row()
                    .key(format!("you-{i}"))
                    .w_full()
                    .child(el().grow(1.0))
                    .child(
                        row()
                            .px(12.0)
                            .h(30.0)
                            .center()
                            .radius(15.0)
                            .fill(accent)
                            .child(line(q.clone(), t)),
                    ),
            )
            .child(
                quiet(
                    if a.is_empty() {
                        "…".into()
                    } else {
                        a.clone()
                    },
                    t,
                )
                .key(format!("agent-{i}"))
                .w_full()
                .wrap(),
            );
    }
    let input = text_input(&s.chat, type_::BODY)
        .key("chat")
        .single_line()
        .placeholder("Ask the agent…", t.text_secondary)
        .w_full()
        .h(40.0)
        .px(14.0)
        .radius(20.0)
        .fill(t.card)
        .on_edit(|s: &mut State| &mut s.chat)
        .on_submit(|s: &mut State, _tx: &mut Tx| {
            let said = s.chat.text.trim().to_string();
            if !said.is_empty() {
                s.log.push((said.clone(), String::new()));
                s.send(format!("chat {said}"));
            }
            s.chat.clear();
        });
    col().w_full().gap(8.0).child(log).child(input)
}
