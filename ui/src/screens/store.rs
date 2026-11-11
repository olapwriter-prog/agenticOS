//! The Store tab (F7): search the catalogue, pick a category, read an item and install it. Installs
//! are High, so they wait for the approval sheet like everything else that installs.
use crate::State;
use crate::widgets::{button, card, line, quiet};
use rune::tok::{Theme, radius, type_};
use rune::{Color, El, Tx, col, row, text_input};
use serde_json::json;

pub fn view(s: &State, _st: &serde_json::Value, t: Theme, accent: Color) -> El {
    let search = row()
        .key("store-search-row")
        .w_full()
        .gap(8.0)
        .child(
            text_input(&s.store_q, type_::BODY)
                .key("store-search")
                .single_line()
                .placeholder("Search the store", t.text_secondary)
                .grow(1.0)
                .h(40.0)
                .px(14.0)
                .radius(radius::SM)
                .fill(t.card)
                .on_edit(|s: &mut State| &mut s.store_q)
                .on_submit(|s: &mut State, _tx: &mut Tx| {
                    let q = s.store_q.text.trim().to_string();
                    if !q.is_empty() {
                        s.send(format!("do store_search {}", json!({"text": q})));
                    }
                }),
        );

    // category chips: each is a search on what its items are
    let mut chips = row().key("store-chips").w_full().gap(6.0);
    for (key, label, q) in [
        ("chip-agents", "Agents", "agent"),
        ("chip-skills", "Skills", "skill"),
        ("chip-connectors", "Connectors", "connector"),
        ("chip-android", "Android apps", "android"),
    ] {
        chips = chips.child(
            button(key.into(), label, t.control_off, t.text)
                .grow(1.0)
                .on_press(move |s: &mut State| s.send(format!("do store_search {}", json!({"text": q})))),
        );
    }

    // the last answer from the store, with each result's show and install buttons
    let mut results = col().gap(8.0);
    if let Some((asked, answer)) = s.replies.iter().rev().find(|(a, _)| a.starts_with("do store_")) {
        results = results.child(quiet(asked.clone(), t).w_full().wrap());
        for (i, l) in answer.lines().enumerate() {
            let id = l.split(':').next().unwrap_or("").trim().to_string();
            let mut r = row().key(format!("store-result-{i}")).w_full().center_y().gap(8.0);
            r = r.child(line(l.to_string(), t).grow(1.0).shrink().wrap());
            if !id.is_empty() && !id.contains(' ') {
                let show_id = id.clone();
                let install_id = id.clone();
                r = r.child(
                    button(format!("store-show-{i}"), "Show", t.control_off, t.text)
                        .on_press(move |s: &mut State| s.send(format!("do store_show {}", json!({"id": show_id})))),
                );
                r = r.child(
                    button(format!("store-install-{i}"), "Install", accent, t.text)
                        .on_press(move |s: &mut State| s.send(format!("do store_install {}", json!({"id": install_id})))),
                );
            }
            results = results.child(r);
        }
    } else {
        results = results.child(quiet("Search for an agent, a connector or an Android app.", t));
    }

    let installed = _st["store"]["installed"].as_array().cloned().unwrap_or_default();
    let installed_text = if installed.is_empty() {
        "Nothing installed from the store yet.".to_string()
    } else {
        installed.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", ")
    };

    col()
        .key("store")
        .w_full()
        .gap(12.0)
        .child(search)
        .child(chips)
        .child(card("store-results", "Results", results, t))
        .child(card("store-installed", "Installed", quiet(installed_text, t).w_full().wrap(), t))
}
