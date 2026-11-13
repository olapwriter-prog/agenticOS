//! The learning lab: the phone teaching its own small model. What it has learned from you, a
//! random test suite run on demand (your requests and reworded versions of them), how the score
//! moves over time and per domain, and the experts: one small adapter per domain with a router
//! that picks between them, the practical mixture of experts for an 8B-or-smaller model on a phone.
use crate::State;
use crate::icons;
use crate::theme::*;
use rune::tok::TextStyle;
use rune::{El, col, el, icon, row};
use serde_json::Value;

fn pct(v: &Value) -> String {
    format!("{:.0}%", v.as_f64().unwrap_or(0.0) * 100.0)
}

pub fn view(s: &State, st: &Value) -> El {
    let lab = &st["lab"];
    let runs = lab["runs"].as_array().cloned().unwrap_or_default();
    let last = runs.last().cloned();
    let mut page = col().key("lab").w_full().gap(16.0).child(page_title(
        "title-lab",
        "Learning lab",
        Some("Your phone teaches its own small model from what you do, and tests that it got better.".into()),
    ));

    // the model and what it has learned
    let has_model = lab["has_model"].as_bool() == Some(true);
    page = page.child(
        col()
            .key("lab-model")
            .w_full()
            .padding(18.0)
            .gap(8.0)
            .radius(28.0)
            .fill(VIOLET)
            .child(t("ON-DEVICE MODEL", LABEL, VIOLET_ON.alpha(0.85)))
            .child(
                t(
                    if has_model {
                        str_of(&lab["model"])
                    } else {
                        "No model loaded: phrases and shortcuts answer".to_string()
                    },
                    TextStyle::new(20.0, 700.0),
                    VIOLET_ON,
                )
                .w_full()
                .wrap(),
            )
            .child(
                row()
                    .w_full()
                    .gap(16.0)
                    .child(stat(&lab["episodes"], "requests seen"))
                    .child(stat(&lab["labelled"], "confirmed by you"))
                    .child(stat(&lab["corrections"], "corrections")),
            )
            .child(
                t(
                    format!(
                        "Adapter: {} · {}",
                        lab["adapter"]
                            .as_str()
                            .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
                            .unwrap_or_else(|| "none yet".into()),
                        str_of(&lab["training"])
                    ),
                    CAPTION,
                    VIOLET_ON.alpha(0.85),
                )
                .w_full()
                .wrap(),
            ),
    );

    // what to do
    page = page.child(
        row()
            .key("lab-actions")
            .w_full()
            .gap(8.0)
            .flex_wrap()
            .child(
                action("lab-run", "Run a random test", true)
                    .on_press(|s: &mut State| s.send("lab run".into())),
            )
            .child(
                action("lab-train", "Train tonight", false)
                    .on_press(|s: &mut State| s.send("learn train".into())),
            )
            .child(
                action("lab-experts", "Export experts", false)
                    .on_press(|s: &mut State| s.send("lab experts".into())),
            )
            .child(
                action("lab-rollback", "Roll back", false)
                    .on_press(|s: &mut State| s.send("learn rollback".into())),
            ),
    );
    if let Some((_, reply)) = s
        .replies
        .iter()
        .rev()
        .find(|(q, _)| q.starts_with("lab") || q.starts_with("learn"))
    {
        page = page.child(
            row()
                .key("lab-reply")
                .w_full()
                .padding(12.0)
                .gap(10.0)
                .radius(18.0)
                .fill(SURFACE2)
                .child(icon(&icons::SPARK, 16.0).color(VIOLET))
                .child(
                    t(reply.clone(), CAPTION, TEXT)
                        .grow(1.0)
                        .shrink()
                        .min_w(0.0)
                        .wrap(),
                ),
        );
    }

    // the score over time
    if let Some(r) = &last {
        let delta = lab["delta"].as_f64();
        let mut head = row().w_full().items_end().gap(10.0).child(t(
            pct(&r["score"]),
            TextStyle::new(44.0, 700.0),
            TEXT,
        ));
        if let Some(d) = delta {
            let c = if d >= 0.0 { MINT } else { CORAL };
            head = head.child(
                row()
                    .h(24.0)
                    .px(9.0)
                    .center()
                    .radius(12.0)
                    .fill(c.alpha(0.16))
                    .mb(8.0)
                    .child(t(format!("{d:+.0} pts"), MICRO, c)),
            );
        }
        let mut chart = row()
            .key("lab-history")
            .w_full()
            .h(110.0)
            .gap(6.0)
            .items_end();
        for (i, run) in runs.iter().enumerate() {
            let v = run["score"].as_f64().unwrap_or(0.0) as f32;
            chart = chart.child(
                col()
                    .key(format!("lab-run-{i}"))
                    .grow(1.0)
                    .h_full()
                    .justify_end()
                    .items_center()
                    .gap(4.0)
                    .child(t(
                        format!("{:.0}", v * 100.0),
                        TextStyle::new(10.0, 600.0),
                        TEXT2,
                    ))
                    .child(el().w_full().h(4.0 + v * 80.0).radius(6.0).fill(
                        if i + 1 == runs.len() {
                            VIOLET
                        } else {
                            VIOLET.alpha(0.4)
                        },
                    )),
            );
        }
        page = page.child(section("Score on your own requests")).child(
            card("lab-score")
                .child(head)
                .child(
                    t(
                        format!(
                            "{} · {} requests · {} as you said them · {} reworded · router {}",
                            str_of(&r["label"]),
                            r["n"].as_u64().unwrap_or(0),
                            pct(&r["held_out"]),
                            pct(&r["variants"]),
                            pct(&r["router"])
                        ),
                        CAPTION,
                        TEXT2,
                    )
                    .w_full()
                    .wrap(),
                )
                .child(chart),
        );
        // per domain
        let mut doms = card("lab-domains").gap(10.0);
        for (i, dm) in r["by_domain"].as_array().into_iter().flatten().enumerate() {
            let v = dm["score"].as_f64().unwrap_or(0.0) as f32;
            let c = if v >= 0.8 {
                MINT
            } else if v >= 0.5 {
                SUN
            } else {
                CORAL
            };
            doms = doms.child(
                col()
                    .key(format!("lab-domain-{i}"))
                    .w_full()
                    .gap(5.0)
                    .child(
                        row()
                            .w_full()
                            .child(t(str_of(&dm["domain"]), CALLOUT, TEXT).grow(1.0))
                            .child(t(
                                format!(
                                    "{:.0}% · {} tests",
                                    v * 100.0,
                                    dm["n"].as_u64().unwrap_or(0)
                                ),
                                CAPTION,
                                TEXT2,
                            )),
                    )
                    .child(
                        row()
                            .w_full()
                            .h(8.0)
                            .radius(4.0)
                            .fill(SURFACE3)
                            .child(el().h_full().w_frac(v.max(0.02)).radius(4.0).fill(c)),
                    ),
            );
        }
        page = page.child(section("By domain")).child(doms);
    } else {
        page = page.child(
            card("lab-empty")
                .child(t("No test run yet", HEADLINE, TEXT))
                .child(t(
                    "A run draws a random set of requests you confirmed, plus reworded versions (a typo, a word dropped, “please” added), and scores the answers. Run one now, and again after training, to see if it improved.",
                    CAPTION,
                    TEXT2,
                ).w_full().wrap()),
        );
    }

    // the experts
    let min = lab["min_expert"].as_u64().unwrap_or(8);
    let mut ex = card("lab-experts-list").gap(10.0).child(
        t(
            format!(
                "One small adapter per domain, and a router that picks one per request ({} domains learned). A dense model cannot become a true mixture of experts on a phone; adapters with a router give the same effect at this size. A domain needs {min} confirmed requests.",
                lab["router_domains"].as_u64().unwrap_or(0)
            ),
            CAPTION,
            TEXT2,
        )
        .w_full()
        .wrap(),
    );
    let experts = lab["experts"].as_array().cloned().unwrap_or_default();
    if experts.is_empty() {
        ex = ex.child(t(
            "No experts yet: confirm a few answers first.",
            BODY,
            TEXT2,
        ));
    }
    for (i, e) in experts.iter().enumerate() {
        let ready = e["ready"].as_bool() == Some(true);
        let n = e["examples"].as_u64().unwrap_or(0);
        ex = ex.child(
            row()
                .key(format!("expert-{i}"))
                .w_full()
                .gap(10.0)
                .center_y()
                .child(tile(&icons::CHIP, if ready { MINT } else { TEXT3 }, 34.0))
                .child(
                    col()
                        .grow(1.0)
                        .shrink()
                        .min_w(0.0)
                        .gap(4.0)
                        .child(t(str_of(&e["domain"]), CALLOUT, TEXT))
                        .child(
                            row().w_full().h(6.0).radius(3.0).fill(SURFACE3).child(
                                el().h_full()
                                    .w_frac((n as f32 / min as f32).min(1.0))
                                    .radius(3.0)
                                    .fill(if ready { MINT } else { SUN }),
                            ),
                        ),
                )
                .child(t(
                    if ready {
                        "ready".to_string()
                    } else {
                        format!("{n}/{min}")
                    },
                    MICRO,
                    if ready { MINT } else { SUN },
                )),
        );
    }
    page.child(section("Experts"))
        .child(ex)
        .child(section("Predictive cache"))
        .child(cache_card(st))
}

fn mb(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0) >> 20
}

/// The predictive cache: what the model server keeps in memory, why, and whether it pays off.
fn cache_card(st: &Value) -> El {
    let c = &st["cache"];
    let on = c["enabled"].as_bool() == Some(true);
    let server = &c["server"];
    let (used, budget) = (mb(&server["used_bytes"]), mb(&server["budget_bytes"]));
    let mut card = card("lab-cache").gap(12.0).child(
        row()
            .w_full()
            .gap(10.0)
            .center_y()
            .child(tile(&icons::CHIP, if on { SKY } else { TEXT3 }, 34.0))
            .child(
                col()
                    .grow(1.0)
                    .shrink()
                    .min_w(0.0)
                    .gap(2.0)
                    .child(t(
                        if on { "On: the agent keeps what you'll need next" } else { "Off" },
                        CALLOUT,
                        TEXT,
                    ))
                    .child(t(
                        "Reads your system prompt and likely experts ahead, drops what you stopped using, shrinks on low battery or memory.",
                        CAPTION,
                        TEXT2,
                    ).w_full().wrap()),
            )
            .child(
                action("cache-toggle", if on { "Turn off" } else { "Turn on" }, false).on_press(
                    move |s: &mut State| s.send(if on { "cache off" } else { "cache on" }.into()),
                ),
            ),
    );
    // the numbers
    let stat = |key: &str, label: &str, value: String, col_: rune::Color| {
        col()
            .key(key)
            .grow(1.0)
            .w_frac(0.25)
            .min_w(0.0)
            .gap(2.0)
            .child(t(value, TextStyle::new(20.0, 700.0), col_))
            .child(t(label, MICRO, TEXT2))
    };
    card = card.child(
        row()
            .w_full()
            .gap(8.0)
            .child(stat("cache-hit", "hit rate", pct(&c["hit_rate"]), MINT))
            .child(stat(
                "cache-saved",
                "saved",
                format!("{:.0} s", c["saved_s"].as_f64().unwrap_or(0.0)),
                SKY,
            ))
            .child(stat(
                "cache-mem",
                "memory",
                format!("{used}/{budget} MB"),
                VIOLET,
            ))
            .child(stat(
                "cache-warm",
                "warm-ups used",
                format!("{}/{}", c["warm_used"].as_u64().unwrap_or(0), c["warmed"].as_u64().unwrap_or(0)),
                SUN,
            )),
    );
    // the memory bar: one segment per kept state
    let entries = server["entries"].as_array().cloned().unwrap_or_default();
    let cap = budget.max(1) as f32;
    let mut bar = row().key("cache-bar").w_full().h(10.0).radius(5.0).fill(SURFACE3).gap(2.0);
    for e in &entries {
        let c_ = if e["adapter"].as_str().unwrap_or("").is_empty() { SKY } else { VIOLET };
        bar = bar.child(el().h_full().w_frac((mb(&e["bytes"]) as f32 / cap).min(1.0)).radius(5.0).fill(c_));
    }
    card = card.child(bar);
    for (i, e) in entries.iter().enumerate() {
        let p = e["priority"].as_f64().unwrap_or(0.0);
        card = card.child(
            row()
                .key(format!("cache-entry-{i}"))
                .w_full()
                .gap(10.0)
                .center_y()
                .child(el().size(8.0, 8.0).radius(4.0).fill(if p >= 0.5 { MINT } else if p >= 0.2 { SUN } else { CORAL }))
                .child(
                    t(
                        format!(
                            "{} · {} tokens · {} MB",
                            str_of(&e["label"]),
                            e["tokens"].as_u64().unwrap_or(0),
                            mb(&e["bytes"])
                        ),
                        CAPTION,
                        TEXT,
                    )
                    .grow(1.0)
                    .shrink()
                    .min_w(0.0)
                    .truncate(),
                )
                .child(t(
                    format!("{} hits · keep {:.0}%", e["hits"].as_u64().unwrap_or(0), p * 100.0),
                    MICRO,
                    TEXT2,
                )),
        );
    }
    if entries.is_empty() {
        card = card.child(t(
            c["error"].as_str().unwrap_or("Nothing kept right now."),
            CAPTION,
            TEXT2,
        ).w_full().wrap());
    }
    // why: the manager's reasons, then what it did lately
    let mut why = col().key("cache-why").w_full().gap(6.0);
    let mut lines: Vec<String> = c["plan"]["reasons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    lines.extend(c["log"].as_array().into_iter().flatten().take(3).filter_map(|v| v.as_str().map(String::from)));
    for (i, l) in lines.iter().take(6).enumerate() {
        why = why.child(
            row()
                .key(format!("cache-why-{i}"))
                .w_full()
                .gap(8.0)
                .items_start()
                .child(icon(&icons::SPARK, 12.0).color(VIOLET).mt(3.0))
                .child(t(l.clone(), CAPTION, TEXT2).grow(1.0).shrink().min_w(0.0).wrap()),
        );
    }
    if let Some(next) = c["plan"]["next"].as_array().filter(|n| !n.is_empty()) {
        let s = next
            .iter()
            .take(3)
            .map(|p| format!("{} {:.0}%", str_of(&p[0]), p[1].as_f64().unwrap_or(0.0) * 100.0))
            .collect::<Vec<_>>()
            .join(" · ");
        why = why.child(t(format!("Next request: {s}"), CAPTION, TEXT).w_full().wrap());
    }
    card.child(why).child(
        row()
            .w_full()
            .gap(8.0)
            .child(action("cache-now", "Decide now", false).on_press(|s: &mut State| s.send("cache now".into())))
            .child(action("cache-clear", "Free the memory", false).on_press(|s: &mut State| s.send("cache clear".into()))),
    )
}

fn stat(v: &Value, label: &str) -> El {
    col()
        .gap(1.0)
        .child(t(
            v.as_u64().unwrap_or(0).to_string(),
            TextStyle::new(22.0, 700.0),
            VIOLET_ON,
        ))
        .child(t(label, CAPTION, VIOLET_ON.alpha(0.8)))
}
