//! Steps: how the agent worked. Each question, then the steps that hang off a line: the steps
//! the answer carries, or one step for the tool that answered. An action that waits for the owner
//! shows Approve and Decline right on its step. The result closes each run.
use super::{Turn, accent_of, approve, headline, turns};
use crate::State;
use crate::theme::*;
use rune::tok::TextStyle;
use rune::{El, col, el, row};
use serde_json::Value;

pub fn view(s: &State, st: &Value) -> El {
    let all = turns(s, st);
    let mut page = col()
        .key("mode-steps-view")
        .w_full()
        .px(16.0)
        .gap(16.0)
        .pb(16.0)
        .child(t("HOW THE AGENT WORKED", LABEL, TEXT2));
    if all.is_empty() {
        return page.child(super::empty_state());
    }
    let pending: Vec<u64> = st["pending"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["id"].as_u64())
        .collect();
    for turn in &all {
        page = page.child(run(turn, &pending));
    }
    page
}

fn run(turn: &Turn, pending: &[u64]) -> El {
    let i = turn.i;
    let mut steps = col()
        .w_full()
        .gap(10.0)
        .pl(10.0)
        .border_sides([0.0, 0.0, 0.0, 2.0], HAIRLINE);
    let mut drawn = 0;
    if let Some(doc) = &turn.doc {
        for c in doc["cards"].as_array().into_iter().flatten() {
            for st in c["steps"].as_array().into_iter().flatten() {
                steps = steps.child(step_card(
                    &format!("run-{i}-step-{drawn}"),
                    &str_of(&st["label"]),
                    &str_of(&st["detail"]),
                    &str_of(&st["state"]),
                ));
                drawn += 1;
            }
        }
    }
    if drawn == 0 {
        // one step for the answer itself: waiting, failed, working or done
        let (state, detail) = if turn.thinking {
            ("run", "Working on it".to_string())
        } else if turn.approval.is_some_and(|id| pending.contains(&id)) {
            ("wait", "Waits for your approval".to_string())
        } else if !turn.ok {
            ("fail", turn.text.clone())
        } else {
            ("done", turn.text.clone())
        };
        let label = turn
            .doc
            .as_ref()
            .map(|d| str_of(&d["title"]))
            .filter(|x| !x.is_empty())
            .unwrap_or_else(|| "Answer".into());
        let mut card = step_card(&format!("run-{i}-step-0"), &label, &detail, state);
        if let (Some(id), "wait") = (turn.approval, state) {
            card = card.child(
                row()
                    .w_full()
                    .gap(8.0)
                    .pl(26.0)
                    .child(
                        action(&format!("steps-decline-{id}"), "Decline", false)
                            .grow(1.0)
                            .on_press(move |s: &mut State| s.send(format!("decline {id}"))),
                    )
                    .child(
                        action(&format!("steps-approve-{id}"), "Approve", true)
                            .grow(1.0)
                            .on_press(move |s: &mut State| approve(s, id)),
                    ),
            );
        }
        steps = steps.child(card);
    }
    // the result
    let mut out = col()
        .key(format!("run-{i}"))
        .w_full()
        .gap(10.0)
        .child(
            col()
                .w_full()
                .padding(14.0)
                .gap(3.0)
                .radius(22.0)
                .fill(SURFACE)
                .border(1.0, HAIRLINE)
                .child(t("You asked", CAPTION, TEXT2))
                .child(t(turn.q.clone(), CALLOUT, TEXT).w_full().wrap()),
        )
        .child(steps);
    if let Some(doc) = &turn.doc {
        let (field, on, _) = signal(&accent_of(doc));
        let (label, hero, line) = headline(turn);
        out = out.child(
            col()
                .key(format!("run-{i}-result"))
                .w_full()
                .padding(16.0)
                .gap(4.0)
                .radius(22.0)
                .fill(field)
                .child(t(label.to_uppercase(), LABEL, on.alpha(0.85)))
                .child(
                    t(
                        if hero.is_empty() { line.clone() } else { hero },
                        TextStyle::new(26.0, 700.0),
                        on,
                    )
                    .w_full()
                    .wrap(),
                )
                .child(t(line, CAPTION, on.alpha(0.9)).w_full().wrap()),
        );
    }
    out
}

fn step_card(key: &str, label: &str, detail: &str, state: &str) -> El {
    let (ring, fill) = match state {
        "wait" => (WARN.alpha(0.4), SURFACE2),
        "fail" => (BAD.alpha(0.4), SURFACE),
        "run" => (VIOLET.alpha(0.35), SURFACE),
        _ => (MINT.alpha(0.25), SURFACE),
    };
    col()
        .w_full()
        .padding(12.0)
        .gap(8.0)
        .radius(18.0)
        .fill(fill)
        .border(1.0, ring)
        .child(step_row(key, label, detail, state))
        .child(el().h(0.0))
}
