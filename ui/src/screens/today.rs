//! Today: the day as a flow (what you asked, what the agent did, how it ended), and the numbers
//! behind it: actions, success, approvals, what ran without the model, hours, domains, the week.
use crate::State;
use crate::theme::*;
use rune::tok::TextStyle;
use rune::{El, col, el, row};
use serde_json::Value;

fn outcome_colour(o: &str) -> rune::Color {
    match o {
        "done" | "approved" => MINT,
        "declined" => SUN,
        "failed" => CORAL,
        _ => TEXT3,
    }
}

/// Bars with labels underneath; the largest is solid.
fn bars(key: &str, values: &[u32], labels: &[String], c: rune::Color, h: f32) -> El {
    let max = values.iter().copied().max().unwrap_or(0).max(1) as f32;
    let mut r = row().key(key).w_full().h(h + 34.0).gap(4.0).items_end();
    for (i, v) in values.iter().enumerate() {
        let bh = 4.0 + (*v as f32 / max) * h;
        r = r.child(
            col()
                .key(format!("{key}-{i}"))
                .grow(1.0)
                .h_full()
                .justify_end()
                .items_center()
                .gap(4.0)
                .child(t(
                    if *v > 0 { v.to_string() } else { String::new() },
                    TextStyle::new(10.5, 600.0),
                    TEXT2,
                ))
                .child(
                    el().w_full()
                        .h(bh)
                        .radius(5.0)
                        .fill(if *v as f32 == max && *v > 0 {
                            c
                        } else {
                            c.alpha(0.35)
                        }),
                )
                .child(t(
                    labels.get(i).cloned().unwrap_or_default(),
                    TextStyle::new(10.0, 500.0),
                    TEXT3,
                )),
        );
    }
    r
}

fn metric(key: &str, label: &str, value: String, c: rune::Color) -> El {
    col()
        .key(key)
        .grow(1.0)
        .w_frac(0.5)
        .min_w(0.0)
        .padding(14.0)
        .gap(4.0)
        .radius(22.0)
        .fill(c.alpha(0.14))
        .child(t(label, CAPTION, c))
        .child(t(value, TextStyle::new(28.0, 700.0), TEXT))
}

pub fn view(_s: &State, st: &Value) -> El {
    let d = &st["today"];
    let flow = d["flow"].as_array().cloned().unwrap_or_default();
    let n = flow.len();
    let done = d["done"].as_u64().unwrap_or(0) + d["approved"].as_u64().unwrap_or(0);
    let success = d["success"].as_f64().unwrap_or(0.0);
    let mut page = col().key("today").w_full().gap(16.0).child(page_title(
        "title-today",
        "Today",
        Some(match d["busiest_hour"].as_u64() {
            Some(h) => format!("{n} actions · busiest around {h:02}:00"),
            None => "Nothing yet today. What you ask shows up here.".to_string(),
        }),
    ));

    // the numbers
    page = page
        .child(
            row()
                .w_full()
                .gap(10.0)
                .child(metric("today-actions", "Actions", n.to_string(), VIOLET))
                .child(metric(
                    "today-success",
                    "Went well",
                    format!("{:.0}%", success * 100.0),
                    MINT,
                )),
        )
        .child(
            row()
                .w_full()
                .gap(10.0)
                .child(metric(
                    "today-approved",
                    "You approved",
                    d["approved"].as_u64().unwrap_or(0).to_string(),
                    SUN,
                ))
                .child(metric(
                    "today-local",
                    "No model needed",
                    d["without_model"].as_u64().unwrap_or(0).to_string(),
                    SKY,
                )),
        );
    let _ = done;

    // the flow
    let mut list = card("today-flow").gap(0.0).py(6.0);
    if flow.is_empty() {
        list = list.py(16.0).child(
            t(
                "Ask anything, approve something, and the day starts here.",
                BODY,
                TEXT2,
            )
            .w_full()
            .wrap(),
        );
    }
    for (i, s) in flow.iter().rev().take(12).enumerate() {
        let o = str_of(&s["outcome"]);
        let c = outcome_colour(&o);
        list = list.child(
            row()
                .key(format!("flow-{i}"))
                .w_full()
                .gap(12.0)
                .py(9.0)
                .items_start()
                .child(t(str_of(&s["at"]), CAPTION, TEXT2).w(42.0))
                .child(el().size(10.0, 10.0).radius(5.0).fill(c).mt(4.0))
                .child(
                    col()
                        .grow(1.0)
                        .shrink()
                        .min_w(0.0)
                        .gap(2.0)
                        .child(t(str_of(&s["prompt"]), CALLOUT, TEXT).w_full().truncate())
                        .child(
                            t(
                                format!(
                                    "{} · {} · {}",
                                    if str_of(&s["tool"]).is_empty() {
                                        "answered".to_string()
                                    } else {
                                        str_of(&s["tool"]).replace('_', " ")
                                    },
                                    str_of(&s["domain"]),
                                    o
                                ),
                                CAPTION,
                                TEXT2,
                            )
                            .truncate(),
                        ),
                ),
        );
    }
    page = page.child(section("The flow, newest first")).child(list);

    // when: hours from 6 to 23
    let hours: Vec<u32> = d["by_hour"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_u64())
        .map(|v| v as u32)
        .collect();
    if hours.len() == 24 {
        let vals: Vec<u32> = hours[6..24].to_vec();
        let labels: Vec<String> = (6..24)
            .map(|h| {
                if h % 3 == 0 {
                    format!("{h}")
                } else {
                    String::new()
                }
            })
            .collect();
        page = page
            .child(section("By hour"))
            .child(card("today-hours").child(bars("hour", &vals, &labels, VIOLET, 70.0)));
    }

    // what: domains
    let doms: Vec<(String, u32)> = d["by_domain"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| (str_of(&p[0]), p[1].as_u64().unwrap_or(0) as u32))
        .collect();
    if !doms.is_empty() {
        let total: u32 = doms.iter().map(|x| x.1).sum::<u32>().max(1);
        let mut c = card("today-domains").gap(10.0);
        for (i, (k, v)) in doms.iter().take(6).enumerate() {
            let (field, _, _) = signal(match k.as_str() {
                "phone" | "device" => "mint",
                "messages" => "coral",
                "money" => "sun",
                "info" => "sky",
                "documents" => "lime",
                _ => "violet",
            });
            c = c.child(
                col()
                    .key(format!("domain-{i}"))
                    .w_full()
                    .gap(5.0)
                    .child(
                        row()
                            .w_full()
                            .child(t(k.clone(), CALLOUT, TEXT).grow(1.0))
                            .child(t(v.to_string(), CALLOUT, TEXT2)),
                    )
                    .child(
                        row().w_full().h(8.0).radius(4.0).fill(SURFACE3).child(
                            el().h_full()
                                .w_frac(*v as f32 / total as f32)
                                .radius(4.0)
                                .fill(field),
                        ),
                    ),
            );
        }
        page = page.child(section("By kind")).child(c);
    }

    // the week
    let week: Vec<u32> = d["week"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_u64())
        .map(|v| v as u32)
        .collect();
    if week.len() == 7 {
        let labels: Vec<String> = (0..7)
            .map(|i| {
                if i == 6 {
                    "today".into()
                } else {
                    format!("-{}", 6 - i)
                }
            })
            .collect();
        page = page
            .child(section("Last 7 days"))
            .child(card("today-week").child(bars("week", &week, &labels, SKY, 60.0)));
    }
    page
}
