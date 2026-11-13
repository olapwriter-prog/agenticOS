//! Store: search, categories, results as cards with Get, the details of one item, and what is
//! installed with Remove. Installing is High: it waits for the approval sheet.
use crate::State;
use crate::icons;
use crate::theme::*;
use rune::{El, Tx, col, icon, row, text_input};
use serde_json::{Value, json};

/// One result line, "id: Name — summary", as its parts.
fn parse(line: &str) -> Option<(String, String, String)> {
    let (id, rest) = line.split_once(':')?;
    let id = id.trim().to_string();
    if id.is_empty() || id.contains(' ') {
        return None;
    }
    let (name, summary) = rest
        .split_once('—')
        .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
        .unwrap_or((rest.trim().to_string(), String::new()));
    Some((id, name, summary))
}

fn search_cmd(q: &str) -> String {
    format!("do store_search {}", json!({"text": q}))
}

pub fn view(s: &State, st: &Value) -> El {
    let mut page = col().key("store").w_full().gap(18.0).child(page_title(
        "title-store",
        "Store",
        Some("Agents, connectors and apps. Installs wait for your approval.".into()),
    ));

    // search
    page = page.child(
        row()
            .key("store-search-row")
            .w_full()
            .h(50.0)
            .px(16.0)
            .gap(10.0)
            .center_y()
            .radius(25.0)
            .fill(SURFACE2)
            .border(1.0, HAIRLINE)
            .child(icon(&icons::SEARCH, 18.0).color(MUTED))
            .child(
                text_input(&s.store_q, BODY)
                    .key("store-search")
                    .single_line()
                    .placeholder("Search the store", DIM)
                    .grow(1.0)
                    .h(22.0)
                    .on_edit(|s: &mut State| &mut s.store_q)
                    .on_submit(|s: &mut State, _tx: &mut Tx| {
                        let q = s.store_q.text.trim().to_string();
                        if !q.is_empty() {
                            s.send(search_cmd(&q));
                        }
                    }),
            ),
    );

    // categories
    let last_q = s
        .replies
        .iter()
        .rev()
        .find(|(a, _)| a.starts_with("do store_search"))
        .map(|(a, _)| a.clone())
        .unwrap_or_default();
    let mut chips = row().key("store-chips").w_full().gap(8.0);
    for (key, _ic, label, q) in [
        ("chip-agents", &icons::SPARK, "Agents", "agent"),
        ("chip-skills", &icons::LEDGER, "Skills", "skill"),
        ("chip-connectors", &icons::LINK, "Connectors", "connector"),
        ("chip-android", &icons::APPS, "Android", "android"),
    ] {
        let on = last_q.contains(&format!("\"{q}\""));
        chips = chips
            .child(text_chip(key, label, on).on_press(move |s: &mut State| s.send(search_cmd(q))));
    }
    page = page.child(chips);

    // one item's details, if asked
    if let Some((_, detail)) = s
        .replies
        .iter()
        .rev()
        .find(|(a, _)| a.starts_with("do store_show"))
    {
        let mut lines = detail.lines();
        let head = lines.next().unwrap_or("").to_string();
        let mut d = card("store-detail").child(t(head, HEADLINE, TEXT));
        for l in lines {
            d = d.child(t(l.to_string(), CALLOUT, MUTED).w_full().wrap());
        }
        page = page.child(d);
    }

    // results
    let mut results = col().key("store-results").w_full().gap(10.0);
    match s
        .replies
        .iter()
        .rev()
        .find(|(a, _)| a.starts_with("do store_search"))
    {
        Some((_, answer)) => {
            let items: Vec<_> = answer.lines().filter_map(parse).collect();
            if items.is_empty() {
                results = results.child(card("store-none").py(24.0).items_center().child(t(
                    "Nothing in the store for that",
                    BODY,
                    MUTED,
                )));
            }
            for (i, (id, name, summary)) in items.into_iter().enumerate() {
                let (show_id, install_id) = (id.clone(), id.clone());
                results = results.child(
                    row()
                        .key(format!("store-result-{i}"))
                        .w_full()
                        .padding(14.0)
                        .gap(14.0)
                        .center_y()
                        .radius(22.0)
                        .fill(SURFACE)
                        .border(1.0, HAIRLINE)
                        .child(
                            row()
                                .size(52.0, 52.0)
                                .center()
                                .radius(15.0)
                                .linear_gradient(
                                    135.0,
                                    [(0.0, hue_of(&id)), (1.0, hue_of(&id).mix(ACCENT, 0.5))],
                                )
                                .child(t(
                                    initials(&name),
                                    TextStyle::new(20.0, 700.0),
                                    rune::Color::WHITE,
                                )),
                        )
                        .child(
                            col()
                                .grow(1.0)
                                .shrink()
                                .min_w(0.0)
                                .gap(3.0)
                                .child(t(name, HEADLINE, TEXT).truncate())
                                .child(t(summary, CAPTION, MUTED).w_full().wrap().max_lines(2)),
                        )
                        .child(
                            col()
                                .gap(6.0)
                                .items_center()
                                .child(
                                    pill(
                                        &format!("store-install-{i}"),
                                        "Get",
                                        ACCENT.alpha(0.2),
                                        ACCENT,
                                    )
                                    .on_press(
                                        move |s: &mut State| {
                                            s.send(format!(
                                                "do store_install {}",
                                                json!({"id": install_id})
                                            ))
                                        },
                                    ),
                                )
                                .child(
                                    row()
                                        .key(format!("store-show-{i}"))
                                        .h(24.0)
                                        .px(6.0)
                                        .center()
                                        .role("button")
                                        .label("Details")
                                        .child(t("Details", CAPTION, MUTED))
                                        .on_press(move |s: &mut State| {
                                            s.send(format!(
                                                "do store_show {}",
                                                json!({"id": show_id})
                                            ))
                                        }),
                                ),
                        ),
                );
            }
        }
        None => {
            results = results.child(
                card("store-start")
                    .gap(10.0)
                    .child(tile(&icons::BAG, ACCENT, 44.0))
                    .child(t("Find something to add", HEADLINE, TEXT))
                    .child(t("Pick a category above or search. Agents work with the tools they list; Android apps open in the sandbox.", CALLOUT, MUTED).w_full().wrap()),
            );
        }
    }
    page = page.child(results);

    // installed
    let installed = st["store"]["installed"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut inst = card("store-installed").gap(0.0).py(6.0);
    if installed.is_empty() {
        inst = inst
            .py(18.0)
            .child(t("Nothing installed from the store yet", BODY, MUTED));
    }
    for (i, v) in installed.iter().enumerate() {
        let id = str_of(v);
        let rid = id.clone();
        if i > 0 {
            inst = inst.child(divider());
        }
        inst = inst.child(list_row(
            &format!("installed-{i}"),
            tile(&icons::CHECK, GOOD, 36.0),
            &pretty_id(&id),
            "Installed",
            Some(
                pill(&format!("store-remove-{i}"), "Remove", SURFACE2, BAD).on_press(
                    move |s: &mut State| s.send(format!("do store_remove {}", json!({"id": rid}))),
                ),
            ),
        ));
    }
    page.child(
        col()
            .w_full()
            .gap(10.0)
            .child(section("Installed"))
            .child(inst),
    )
}

use rune::tok::TextStyle;
