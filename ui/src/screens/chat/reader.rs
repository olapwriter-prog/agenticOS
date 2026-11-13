//! Reader: documents and long answers. The document on light paper at the top (its type, where
//! it came from, its parts), the agent's notes on a dark sheet below, with what to do next: ask
//! about it, share it to an Android app, or reply to whoever sent it.
use super::turns;
use crate::State;
use crate::icons;
use crate::theme::*;
use rune::tok::TextStyle;
use rune::{El, TextEdit, col, el, icon, row};
use serde_json::Value;

/// The kind of file from its name: (badge, icon).
pub fn file_kind(name: &str) -> (&'static str, &'static rune::Icon) {
    let low = name.to_lowercase();
    if low.ends_with(".pdf") {
        ("PDF", &icons::PDF)
    } else if low.ends_with(".xlsx") || low.ends_with(".csv") {
        ("Sheet", &icons::SHEET)
    } else if low.ends_with(".pptx") {
        ("Slides", &icons::SLIDES)
    } else if low.ends_with(".docx") {
        ("Word", &icons::DOC)
    } else {
        ("Doc", &icons::DOC)
    }
}

pub fn view(s: &State, st: &Value) -> El {
    let all = turns(s, st);
    let doc = all.iter().rev().find_map(|t| {
        t.doc
            .as_ref()
            .filter(|d| str_of(&d["layout"]) == "reader" || str_of(&d["kind"]) == "document")
            .cloned()
    });
    let page = col().key("mode-reader-view").w_full().gap(0.0);
    let Some(doc) = doc else {
        return page.child(
            col()
                .w_full()
                .px(20.0)
                .pt(40.0)
                .gap(10.0)
                .items_center()
                .child(icon(&icons::DOC, 40.0).color(TEXT2))
                .child(t("No document open", TITLE, TEXT))
                .child(
                    t(
                        "Open one from Files or the inbox, or ask to read a document.",
                        BODY,
                        TEXT2,
                    )
                    .wrap(),
                ),
        );
    };
    let name = str_of(&doc["title"]);
    let (badge, ic) = file_kind(&name);
    // where it came from: a file the bridge received names its app and conversation
    let received = st["bridge"]["received"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| str_of(&r["name"]) == name)
        .cloned();
    let source = received
        .as_ref()
        .map(|r| str_of(&r["from_app"]))
        .unwrap_or_default();
    let from = received
        .as_ref()
        .map(|r| str_of(&r["conversation"]))
        .unwrap_or_default();
    let cards = doc["cards"].as_array().cloned().unwrap_or_default();

    // the paper
    let mut sheet = col()
        .key("reader-page")
        .w_full()
        .padding(18.0)
        .gap(10.0)
        .radius(16.0)
        .fill(PAPER1)
        .border(1.0, PAPER2)
        .child(
            t(name.clone(), TextStyle::new(20.0, 700.0), INK_TEXT)
                .w_full()
                .wrap(),
        )
        .child(
            t(str_of(&doc["subtitle"]), CAPTION, INK_TEXT2)
                .w_full()
                .wrap(),
        );
    for c in cards.iter().take(3) {
        sheet = sheet.child(t(str_of(&c["title"]), CALLOUT, INK_TEXT).w_full().wrap());
        for l in c["lines"].as_array().into_iter().flatten().take(3) {
            sheet = sheet.child(
                t(str_of(l), CAPTION, INK_TEXT2)
                    .w_full()
                    .max_lines(3)
                    .wrap(),
            );
        }
    }
    let mut tags = row().w_full().gap(8.0).center_y().child(
        row()
            .h(22.0)
            .px(9.0)
            .gap(5.0)
            .center_y()
            .radius(11.0)
            .fill(LIME)
            .child(icon(ic, 12.0).color(LIME_ON))
            .child(t(badge, TextStyle::new(11.5, 700.0), LIME_ON)),
    );
    tags = tags.child(source_tag(if source.is_empty() {
        "Agentic OS"
    } else {
        &source
    }));
    let paper = col()
        .w_full()
        .padding(16.0)
        .gap(10.0)
        .fill(PAPER0)
        .radius(24.0)
        .child(tags)
        .child(sheet);

    // the notes and what to do next
    let mut summary = col()
        .key("reader-summary")
        .w_full()
        .padding(18.0)
        .gap(10.0)
        .radius(28.0)
        .fill(SURFACE)
        .child(t("SUMMARY", LABEL, VIOLET));
    let mut points = 0;
    for c in &cards {
        for l in c["lines"].as_array().into_iter().flatten() {
            if points < 3 {
                summary = summary.child(
                    row()
                        .w_full()
                        .gap(10.0)
                        .items_start()
                        .child(el().size(6.0, 6.0).radius(3.0).fill(VIOLET).mt(8.0))
                        .child(
                            t(str_of(l), BODY, TEXT)
                                .grow(1.0)
                                .shrink()
                                .min_w(0.0)
                                .max_lines(2)
                                .wrap(),
                        ),
                );
                points += 1;
            }
        }
    }
    let n1 = name.clone();
    let n2 = name.clone();
    let mut acts =
        row()
            .w_full()
            .gap(8.0)
            .flex_wrap()
            .child(
                action("reader-ask", "Ask about this", true).on_press(move |s: &mut State| {
                    s.chat = TextEdit::new(&format!("read document {n1} "));
                }),
            )
            .child(action("reader-share", "Share to WhatsApp", false).on_press(
                move |s: &mut State| s.ask(&format!("share {n2} to android app WhatsApp")),
            ));
    if !from.is_empty() {
        let who = from.clone();
        acts = acts.child(
            action("reader-reply", &format!("Reply to {from}"), false).on_press(
                move |s: &mut State| {
                    s.chat = TextEdit::new(&format!("reply on android to {who} saying "));
                },
            ),
        );
    }
    summary = summary.child(acts);
    page.child(
        col()
            .w_full()
            .px(16.0)
            .gap(12.0)
            .pb(16.0)
            .child(paper)
            .child(summary),
    )
}
