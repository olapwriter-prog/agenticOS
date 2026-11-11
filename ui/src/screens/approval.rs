//! The approval sheet (F4): the action in plain words, its arguments, a PIN pad when a PIN is set,
//! and Approve and Decline. The PIN is kept only while it is typed; what is sent and kept is masked.
use crate::State;
use crate::widgets::{button, line, quiet, str_of};
use rune::tok::{Theme, radius, type_};
use rune::{Color, El, col, row, text};

pub fn sheet(s: &State, id: u64, t: Theme, accent: Color) -> El {
    let pending = s.snap["pending"].as_array().cloned().unwrap_or_default();
    let item = pending.iter().find(|p| p["id"].as_u64() == Some(id)).cloned().unwrap_or_default();
    let args: Vec<String> = item["args"]
        .as_object()
        .map(|m| m.iter().map(|(k, v)| format!("{k}: {}", str_of(v))).collect())
        .unwrap_or_default();
    let pin_set = s.snap["pin_set"].as_bool() == Some(true);

    let mut card = col()
        .key("sheet")
        .w_full()
        .padding(16.0)
        .gap(12.0)
        .radius(radius::SM)
        .fill(t.card)
        .child(text("Approve this?").font(type_::SECTION).color(t.text))
        .child(line(str_of(&item["tool"]).replace('_', " "), t))
        .child(quiet(args.join("  ·  "), t).w_full().wrap());

    if pin_set {
        let dots: String = (0..s.pin.len()).map(|_| '•').collect();
        card = card.child(text(if dots.is_empty() { "Enter your PIN".into() } else { dots }).key("pin-dots").font(type_::BODY).color(t.text));
        for row_keys in [["1", "2", "3"], ["4", "5", "6"], ["7", "8", "9"]] {
            let mut r = row().gap(8.0).w_full();
            for k in row_keys {
                r = r.child(pad_key(k, t));
            }
            card = card.child(r);
        }
        let mut last = row().gap(8.0).w_full();
        last = last.child(button("pin-clear".into(), "Clear", t.control_off, t.text).grow(1.0).on_press(|s: &mut State| s.pin.clear()));
        last = last.child(pad_key("0", t));
        last = last.child(button("pin-back".into(), "⌫", t.control_off, t.text).grow(1.0).on_press(|s: &mut State| {
            s.pin.pop();
        }));
        card = card.child(last);
    }

    let mut buttons = row().key("sheet-buttons").gap(8.0).w_full();
    buttons = buttons.child(
        button("sheet-decline".into(), "Decline", t.control_off, t.text)
            .grow(1.0)
            .on_press(move |s: &mut State| {
                s.sheet = None;
                s.pin.clear();
                s.send(format!("decline {id}"));
            }),
    );
    buttons = buttons.child(
        button("sheet-approve".into(), "Approve", accent, t.text).grow(1.0).on_press(move |s: &mut State| {
            let pin_set = s.snap["pin_set"].as_bool() == Some(true);
            let pin = std::mem::take(&mut s.pin);
            s.sheet = None;
            if pin_set {
                s.send_shown(format!("press {id} {pin}"), format!("press {id} ****"));
            } else {
                s.send(format!("press {id}"));
            }
        }),
    );
    card = card.child(buttons);

    col().w_full().h_full().padding(16.0).gap(12.0).fill(t.bg).child(card)
}

fn pad_key(k: &'static str, t: Theme) -> El {
    button(format!("pin-{k}"), k, t.card, t.text)
        .grow(1.0)
        .on_press(move |s: &mut State| {
            if s.pin.len() < 8 {
                s.pin.push_str(k);
            }
        })
}
