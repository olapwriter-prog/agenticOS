//! The assistant: a full-screen conversation. Each answer is drawn as the screen the backend
//! generated for it (a chart, a photo with its edits, a list with buttons), or as text when it
//! has none. Suggestions above the box come from the latest answer.
use crate::State;
use crate::icons;
use crate::theme::*;
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

fn answer_text(a: &str) -> (String, bool) {
    if let Some(rest) = a.strip_prefix("done: ") {
        (rest.to_string(), true)
    } else if let Some(rest) = a.strip_prefix("not done: ") {
        (rest.to_string(), false)
    } else {
        (a.to_string(), true)
    }
}

pub fn view(s: &State, st: &Value) -> El {
    // the top bar
    let stage = str_of(&st["stage"]);
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
            row()
                .size(36.0, 36.0)
                .center()
                .radius(18.0)
                .linear_gradient(135.0, [(0.0, ACCENT), (1.0, ACCENT2)])
                .child(icon(&icons::SPARK, 18.0).color(rune::Color::WHITE)),
        )
        .child(
            col()
                .grow(1.0)
                .gap(1.0)
                .child(t("Assistant", HEADLINE, TEXT))
                .child(
                    row()
                        .gap(6.0)
                        .center_y()
                        .child(el().size(7.0, 7.0).radius(3.5).fill(if stage == "Active" {
                            GOOD
                        } else {
                            DIM
                        }))
                        .child(t("On this phone · private", CAPTION, MUTED)),
                ),
        );

    // the thread
    let mut thread = col()
        .key("chat-thread")
        .w_full()
        .gap(14.0)
        .px(16.0)
        .pt(8.0)
        .pb(16.0);
    if s.log.is_empty() {
        thread = thread.child(
            col()
                .w_full()
                .items_center()
                .gap(12.0)
                .pt(60.0)
                .child(
                    row()
                        .size(64.0, 64.0)
                        .center()
                        .radius(32.0)
                        .linear_gradient(135.0, [(0.0, ACCENT), (1.0, ACCENT2)])
                        .child(icon(&icons::SPARK, 30.0).color(rune::Color::WHITE)),
                )
                .child(t("What can I do for you?", TITLE, TEXT))
                .child(t(
                    "Ask in your own words. Answers come as screens you can tap.",
                    BODY,
                    MUTED,
                )),
        );
    }
    for (i, (q, a)) in s.log.iter().enumerate() {
        thread = thread.child(
            row().w_full().child(el().grow(1.0)).child(
                row()
                    .key(format!("q-{i}"))
                    .px(16.0)
                    .py(11.0)
                    .radius(22.0)
                    .linear_gradient(90.0, [(0.0, ACCENT), (1.0, ACCENT.mix(ACCENT2, 0.3))])
                    .child(t(q.clone(), BODY, rune::Color::WHITE).wrap()),
            ),
        );
        if a.is_empty() {
            thread = thread.child(
                row()
                    .key(format!("a-{i}"))
                    .gap(10.0)
                    .center_y()
                    .child(tile(&icons::SPARK, ACCENT, 30.0))
                    .child(t("Thinking…", BODY, DIM)),
            );
            continue;
        }
        let (text, ok) = answer_text(a);
        match screen_for(st, s, i) {
            Some(doc) => {
                thread = thread.child(
                    col()
                        .key(format!("a-{i}"))
                        .w_full()
                        .padding(14.0)
                        .radius(24.0)
                        .fill(SURFACE2)
                        .border(1.0, ACCENT.alpha(0.25))
                        // the latest answer's own buttons show as suggestions above the box instead
                        .child(crate::genui::document(
                            &doc,
                            &format!("answer-{i}"),
                            s,
                            i + 1 != s.log.len(),
                        )),
                );
            }
            None => {
                thread = thread.child(
                    row()
                        .key(format!("a-{i}"))
                        .w_full()
                        .gap(10.0)
                        .items_start()
                        .child(tile(
                            if ok { &icons::SPARK } else { &icons::ALERT },
                            if ok { ACCENT } else { WARN },
                            30.0,
                        ))
                        .child(
                            col()
                                .grow(1.0)
                                .shrink()
                                .min_w(0.0)
                                .px(14.0)
                                .py(11.0)
                                .radius(20.0)
                                .fill(if ok { SURFACE } else { WARN.alpha(0.10) })
                                .border(1.0, if ok { HAIRLINE } else { WARN.alpha(0.3) })
                                .child(t(text, BODY, TEXT).w_full().wrap()),
                        ),
                );
            }
        }
    }

    // suggestions from the latest answer, or a few to start with
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
            ("What's on today?", "what's on today"),
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

    // the box
    let bar = row()
        .key("chat-bar")
        .w_full()
        .mx(0.0)
        .h(56.0)
        .pl(18.0)
        .pr(6.0)
        .gap(10.0)
        .center_y()
        .radius(28.0)
        .fill(SURFACE)
        .border(1.0, ACCENT.alpha(0.35))
        .child(
            text_input(&s.chat, BODY)
                .key("chat")
                .single_line()
                .placeholder("Message the assistant…", DIM)
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
                ACCENT,
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
        .h_full()
        .fill(BG)
        .child(status_bar(st))
        .child(top)
        .child(
            thread
                .scroll(ScrollSpec { stick_bottom: true })
                .grow(1.0)
                .min_h(0.0),
        )
        .child(
            col()
                .w_full()
                .gap(10.0)
                .pb(16.0)
                .child(chips)
                .child(col().w_full().px(14.0).child(bar)),
        )
}
