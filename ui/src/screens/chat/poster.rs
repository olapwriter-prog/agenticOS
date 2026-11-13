//! Poster: one answer fills the stage as a field in its signal colour, like a magazine cover:
//! the question small at the top, the label, the big value, one line, a chart in the field's own
//! ink, and the answer's actions. Previous and next move between answers.
use super::{Turn, accent_of, first_chart, headline, turns};
use crate::State;
use crate::icons;
use crate::theme::*;
use rune::tok::TextStyle;
use rune::{Color, El, col, el, row};
use serde_json::Value;

pub fn view(s: &State, st: &Value) -> El {
    let all = turns(s, st);
    let mut page = col().key("mode-poster-view").w_full().px(20.0).gap(12.0);
    if all.is_empty() {
        return page.child(super::empty_state());
    }
    let n = all.len();
    let idx = s.poster_at.filter(|i| *i < n).unwrap_or(n - 1);
    let turn: &Turn = &all[idx];
    let kind = turn.doc.as_ref().map(accent_of).unwrap_or_default();
    let (field, on, _) = signal(&kind);
    let (label, hero, line) = headline(turn);

    page = page.child(t(turn.q.clone(), CAPTION, TEXT2).w_full().truncate());

    let mut f = col()
        .key("poster-field")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .padding(22.0)
        .gap(10.0)
        .radius(32.0)
        .fill(field)
        .child(t(label.to_uppercase(), LABEL, on.alpha(0.85)));
    if !hero.is_empty() {
        f = f.child(t(hero, HERO, on).key("poster-hero"));
        if !line.is_empty() {
            f = f.child(t(line, BODY, on.alpha(0.92)).w_full().wrap());
        }
    } else if !line.is_empty() {
        // no number: the line itself is the headline
        f = f.child(
            t(line, TextStyle::new(32.0, 700.0), on)
                .key("poster-hero")
                .w_full()
                .wrap(),
        );
    }
    // the chart, drawn in the field's ink: the newest value solid, the rest washed
    if let Some(ch) = turn.doc.as_ref().and_then(first_chart) {
        let values: Vec<f64> = ch["values"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_f64)
            .collect();
        let labels: Vec<String> = ch["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .map(str_of)
            .collect();
        let unit = str_of(&ch["unit"]);
        let (lo, hi) = values
            .iter()
            .fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
        let mut bars = row()
            .key("poster-chart")
            .w_full()
            .h(150.0)
            .gap(10.0)
            .items_end();
        for (i, v) in values.iter().enumerate() {
            let h = 18.0 + (((v - lo) / (hi - lo).max(1e-9)) as f32) * 92.0;
            let last = i + 1 == values.len();
            bars = bars.child(
                col()
                    .key(format!("poster-bar-{i}"))
                    .grow(1.0)
                    .h_full()
                    .justify_end()
                    .items_center()
                    .gap(5.0)
                    .child(t(format!("{v:.0}{unit}"), TextStyle::new(12.0, 650.0), on))
                    .child(el().w_full().h(h).radius(12.0).fill(if last {
                        on
                    } else {
                        Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 0.2,
                        }
                    }))
                    .child(t(
                        labels.get(i).cloned().unwrap_or_default(),
                        CAPTION,
                        on.alpha(0.8),
                    )),
            );
        }
        f = f.child(el().grow(1.0)).child(bars);
    } else {
        // no chart: the answer's own lines, so the field is never an empty block
        let lines: Vec<String> = turn
            .doc
            .as_ref()
            .map(|d| {
                d["cards"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|c| c["lines"].as_array().cloned().unwrap_or_default())
                    .map(|l| str_of(&l))
                    .collect()
            })
            .unwrap_or_default();
        let mut list = col().key("poster-lines").w_full().gap(10.0).pt(6.0);
        for l in lines.iter().skip(1).take(4) {
            list = list.child(
                row()
                    .w_full()
                    .gap(10.0)
                    .items_start()
                    .child(el().size(7.0, 7.0).radius(3.5).fill(on).mt(8.0))
                    .child(
                        t(l.clone(), TextStyle::new(17.0, 500.0), on)
                            .grow(1.0)
                            .shrink()
                            .min_w(0.0)
                            .wrap(),
                    ),
            );
        }
        f = f.child(list).child(el().grow(1.0));
    }
    // the answer's actions, on the field
    let mut acts = row().w_full().gap(8.0).flex_wrap();
    let mut any = false;
    if let Some(doc) = &turn.doc {
        let mut buttons: Vec<(String, String)> = doc["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|b| (str_of(&b["label"]), str_of(&b["say"])))
            .collect();
        for c in doc["cards"].as_array().into_iter().flatten() {
            for b in c["actions"].as_array().into_iter().flatten() {
                buttons.push((str_of(&b["label"]), str_of(&b["say"])));
            }
        }
        for (i, (label, say)) in buttons.into_iter().take(4).enumerate() {
            any = true;
            acts = acts.child(
                field_action(&format!("poster-act-{i}"), &label, on)
                    .on_press(move |s: &mut State| s.ask(&say)),
            );
        }
    }
    if any {
        f = f.child(acts);
    }
    page = page.child(f);

    // previous, the dots, next
    let mut dots = row().gap(6.0).center();
    for i in 0..n {
        dots = dots.child(
            el().w(if i == idx { 18.0 } else { 6.0 })
                .h(6.0)
                .radius(3.0)
                .fill(if i == idx { field } else { TEXT3 }),
        );
    }
    page.child(
        row()
            .w_full()
            .center_y()
            .child(
                round(
                    "poster-prev",
                    &icons::BACK,
                    44.0,
                    SURFACE2,
                    TEXT,
                    "Previous answer",
                )
                .on_press(move |s: &mut State| s.poster_at = Some(idx.saturating_sub(1))),
            )
            .child(dots.grow(1.0))
            .child(
                round(
                    "poster-next",
                    &icons::NEXT,
                    44.0,
                    SURFACE2,
                    TEXT,
                    "Next answer",
                )
                .on_press(move |s: &mut State| s.poster_at = Some((idx + 1).min(n - 1))),
            ),
    )
}
