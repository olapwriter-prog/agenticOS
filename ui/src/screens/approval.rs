//! The approval sheet: a sheet from the bottom over a dimmed screen. The action in words, its
//! details, and when a PIN is set, the PIN pad. The PIN is masked in everything the UI keeps.
use crate::State;
use crate::icons;
use crate::theme::*;
use rune::{El, col, el, icon, row};

fn type_pin(s: &mut State, k: &str) {
    if k == "⌫" {
        s.pin.pop();
    } else if s.pin.len() < 8 {
        s.pin.push_str(k);
    }
}

pub fn sheet(s: &State, id: u64) -> El {
    let pending = s.snap["pending"].as_array().cloned().unwrap_or_default();
    let item = pending
        .iter()
        .find(|p| p["id"].as_u64() == Some(id))
        .cloned()
        .unwrap_or_default();
    let (ic, c, title, sub) = describe(&str_of(&item["tool"]), &item["args"]);
    let pin_set = s.snap["pin_set"].as_bool() == Some(true);

    let mut details = col().w_full().gap(8.0);
    if let Some(m) = item["args"].as_object() {
        for (k, v) in m {
            details = details.child(
                row()
                    .w_full()
                    .child(t(crate::theme::pretty_id(k), CALLOUT, MUTED).grow(1.0))
                    .child(t(str_of(v), CALLOUT, TEXT).truncate()),
            );
        }
    }

    let mut body = col()
        .key("sheet")
        .w_full()
        .pt(10.0)
        .pb(30.0)
        .px(22.0)
        .gap(18.0)
        .items_center()
        .radius(30.0)
        .fill(SURFACE)
        .border(1.0, HAIRLINE)
        .child(el().w(40.0).h(5.0).radius(2.5).fill(SURFACE3))
        .child(tile(ic, c, 60.0))
        .child(
            col()
                .items_center()
                .gap(4.0)
                .child(t(format!("{title}?"), TITLE, TEXT))
                .child(t(
                    if sub.is_empty() {
                        "This needs your approval".into()
                    } else {
                        sub
                    },
                    BODY,
                    MUTED,
                )),
        )
        .child(
            col()
                .w_full()
                .padding(14.0)
                .radius(16.0)
                .fill(SURFACE2)
                .child(details),
        );

    if pin_set {
        body = body
            .child(
                row()
                    .gap(8.0)
                    .center_y()
                    .child(icon(&icons::LOCK, 15.0).color(MUTED))
                    .child(t("Enter your PIN", CALLOUT, MUTED)),
            )
            .child(pin_dots(s.pin.len(), 4))
            .child(keypad("pin-", type_pin));
    }

    let ready = !pin_set || s.pin.len() >= 4;
    body = body
        .child(
            primary("sheet-approve", "Approve")
                .opacity(if ready { 1.0 } else { 0.45 })
                .on_press(move |s: &mut State| {
                    let pin_set = s.snap["pin_set"].as_bool() == Some(true);
                    if pin_set && s.pin.len() < 4 {
                        return;
                    }
                    let pin = std::mem::take(&mut s.pin);
                    s.sheet = None;
                    if pin_set {
                        s.send_shown(format!("press {id} {pin}"), format!("press {id} ****"));
                    } else {
                        s.send(format!("press {id}"));
                    }
                }),
        )
        .child(
            row()
                .key("sheet-decline")
                .h(40.0)
                .px(20.0)
                .center()
                .role("button")
                .label("Decline")
                .press_scale(0.96)
                .child(t("Decline", HEADLINE, BAD))
                .on_press(move |s: &mut State| {
                    s.sheet = None;
                    s.pin.clear();
                    s.send(format!("decline {id}"));
                }),
        );

    // the sheet sits at the bottom, over a dimmed page
    col()
        .w_full()
        .h_full()
        .fill(SCRIM)
        .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
        .child(
            el().grow(1.0)
                .w_full()
                .key("scrim")
                .on_press(|s: &mut State| {
                    s.sheet = None;
                    s.pin.clear();
                }),
        )
        .child(body)
}
