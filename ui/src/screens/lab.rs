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
    page.child(section("Experts")).child(ex)
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
