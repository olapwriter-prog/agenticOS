//! Thread: the familiar conversation. Your words on the right; each answer drawn as the screen
//! the backend generated for it, or as text when it has none.
use super::{answer_text, screen_for};
use crate::State;
use crate::icons;
use crate::theme::*;
use rune::{El, col, el, icon, row};
use serde_json::Value;

pub fn view(s: &State, st: &Value) -> El {
    let mut thread = col()
        .key("chat-thread")
        .w_full()
        .gap(14.0)
        .px(16.0)
        .pt(8.0)
        .pb(16.0);
    if s.log.is_empty() {
        thread = thread.child(super::empty_state());
    }
    for (i, (q, a)) in s.log.iter().enumerate() {
        thread = thread.child(
            row().w_full().child(el().grow(1.0)).child(
                row()
                    .key(format!("q-{i}"))
                    .px(16.0)
                    .py(11.0)
                    .radius(22.0)
                    .fill(ACCENT_FILL)
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
                    .child(t("Thinking…", BODY, TEXT2)),
            );
            continue;
        }
        let (text, ok) = answer_text(a);
        match screen_for(st, s, i) {
            Some(doc) => {
                thread = thread.child(
                    row()
                        .w_full()
                        .gap(10.0)
                        .items_start()
                        .child(tile(&icons::SPARK, ACCENT, 28.0))
                        .child(
                            col()
                                .key(format!("a-{i}"))
                                .grow(1.0)
                                .shrink()
                                .min_w(0.0)
                                // the latest answer's own buttons show as suggestions above the box
                                .child(crate::genui::document(
                                    &doc,
                                    &format!("answer-{i}"),
                                    s,
                                    i + 1 != s.log.len(),
                                )),
                        ),
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
                            28.0,
                        ))
                        .child(
                            col()
                                .grow(1.0)
                                .shrink()
                                .min_w(0.0)
                                .px(14.0)
                                .py(11.0)
                                .radius(20.0)
                                .fill(if ok { SURFACE2 } else { WARN.alpha(0.10) })
                                .border(1.0, if ok { HAIRLINE } else { WARN.alpha(0.3) })
                                .child(t(text, BODY, TEXT).w_full().wrap()),
                        ),
                );
            }
        }
    }
    let _ = icon;
    thread
}
