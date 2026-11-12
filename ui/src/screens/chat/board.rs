//! Board: every answer becomes a tile that stays, two to a row, newest first. Answers with a
//! chart, steps or a comparison get a wide tile with a small chart. Tapping a tile opens it as a
//! poster.
use super::{accent_of, first_chart, headline, turns};
use crate::theme::*;
use crate::{Mode, State};
use rune::tok::TextStyle;
use rune::{El, col, el, row};
use serde_json::Value;

pub fn view(s: &State, st: &Value) -> El {
    let all = turns(s, st);
    let mut page = col()
        .key("mode-board-view")
        .w_full()
        .px(16.0)
        .gap(12.0)
        .pb(16.0)
        .child(
            row()
                .w_full()
                .child(t("BOARD · LIVE ANSWERS", LABEL, TEXT2).grow(1.0))
                .child(t(format!("{} pinned", all.len()), CAPTION, TEXT2)),
        );
    if all.is_empty() {
        return page.child(super::empty_state());
    }
    // newest first; wide tiles take a row of their own
    let mut pending_small: Option<El> = None;
    for turn in all.iter().rev() {
        let kind = turn.doc.as_ref().map(accent_of).unwrap_or_default();
        let (field, _, tint) = signal(&kind);
        let (label, hero, line) = headline(turn);
        let chart = turn.doc.as_ref().and_then(first_chart);
        let wide = chart.is_some()
            || turn.doc.as_ref().is_some_and(|d| {
                d["cards"].as_array().into_iter().flatten().any(|c| {
                    c["steps"].as_array().is_some_and(|x| !x.is_empty())
                        || c["compare"].as_array().is_some_and(|x| !x.is_empty())
                })
            });
        let value = if hero.is_empty() { label.clone() } else { hero };
        let i = turn.i;
        let mut tile = col()
            .key(format!("tile-{i}"))
            .min_h(150.0)
            .padding(16.0)
            .gap(6.0)
            .radius(24.0)
            .fill(tint)
            .role("button")
            .label(format!("Open {}", turn.q))
            .press_scale(0.98)
            .child(
                t(turn.q.clone(), TextStyle::new(12.5, 600.0), field)
                    .w_full()
                    .truncate(),
            )
            .child(
                t(value, TextStyle::new(30.0, 700.0), field)
                    .w_full()
                    .truncate(),
            )
            .child(t(line, CAPTION, TEXT2).w_full().max_lines(2).wrap())
            .on_press(move |s: &mut State| {
                s.chat_mode = Some(Mode::Poster);
                s.poster_at = Some(i);
            });
        if let Some(ch) = chart {
            let values: Vec<f64> = ch["values"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_f64)
                .collect();
            let (lo, hi) = values
                .iter()
                .fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
            let mut bars = row().w_full().h(56.0).gap(6.0).items_end();
            for (j, v) in values.iter().rev().take(8).rev().enumerate() {
                let h = 8.0 + (((v - lo) / (hi - lo).max(1e-9)) as f32) * 46.0;
                bars = bars.child(
                    el().key(format!("tile-{i}-bar-{j}"))
                        .grow(1.0)
                        .h(h)
                        .radius(6.0)
                        .fill(field.alpha(if j + 1 == values.len().min(8) {
                            1.0
                        } else {
                            0.5
                        })),
                );
            }
            tile = tile.child(bars);
        }
        if wide {
            if let Some(small) = pending_small.take() {
                page = page.child(
                    row()
                        .w_full()
                        .gap(12.0)
                        .child(small.grow(1.0).w_frac(0.5))
                        .child(el().grow(1.0).w_frac(0.5)),
                );
            }
            page = page.child(tile.w_full());
        } else if let Some(small) = pending_small.take() {
            page = page.child(
                row()
                    .w_full()
                    .gap(12.0)
                    .items_stretch()
                    .child(small.grow(1.0).w_frac(0.5).min_w(0.0))
                    .child(tile.grow(1.0).w_frac(0.5).min_w(0.0)),
            );
        } else {
            pending_small = Some(tile);
        }
    }
    if let Some(small) = pending_small {
        page = page.child(
            row()
                .w_full()
                .gap(12.0)
                .child(small.grow(1.0).w_frac(0.5).min_w(0.0))
                .child(el().grow(1.0).w_frac(0.5)),
        );
    }
    page
}
