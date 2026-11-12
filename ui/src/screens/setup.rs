//! First-boot setup: one question per screen, with a progress bar, a large title, and one main
//! button. The PIN step uses the masked pad, never a text box.
use crate::State;
use crate::icons;
use crate::theme::*;
use rune::{El, Icon, Tx, col, el, icon, row};
use serde_json::Value;

fn hero(id: &str) -> (&'static Icon, rune::Color) {
    match id {
        "welcome" => (&icons::SPARK, ACCENT),
        "wifi" => (&icons::WIFI, ACCENT2),
        "sim" => (&icons::SIM, GOOD),
        "location" => (&icons::PIN, WARN),
        "owner" => (&icons::USER, ACCENT),
        "import" => (&icons::USERS, ACCENT2),
        "pin" => (&icons::LOCK, ACCENT),
        "privacy" => (&icons::SHIELD, GOOD),
        "models" => (&icons::CHIP, ACCENT),
        "services" => (&icons::LINK, ACCENT2),
        "apps" => (&icons::APPS, WARN),
        "android" => (&icons::APPS, GOOD),
        _ => (&icons::CHECK, GOOD),
    }
}

/// What to type, for each step that takes text.
fn hint(id: &str) -> &'static str {
    match id {
        "wifi" => "Network name|password",
        "owner" => "Your name|+91 phone number",
        "import" => "Path to a .vcf contacts file",
        "services" => "telegram <bot token>",
        "apps" => "app=mode, e.g. zomato=headless",
        _ => "Your answer",
    }
}

fn type_pin(s: &mut State, k: &str) {
    if k == "⌫" {
        s.setup_pin.pop();
    } else if s.setup_pin.len() < 8 {
        s.setup_pin.push_str(k);
    }
}

pub fn view(s: &State, st: &Value) -> El {
    let setup = &st["setup"];
    let id = str_of(&setup["id"]);
    let index = setup["index"].as_u64().unwrap_or(1) as f32;
    let total = setup["total"].as_u64().unwrap_or(13) as f32;
    let can_skip = setup["can_skip"].as_bool() == Some(true);
    let input = setup["input"].as_str().map(str::to_string);
    let options: Vec<String> = setup["options"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(str_of)
        .collect();
    let (ic, c) = hero(&id);

    // top: back (not on the first step), progress, skip
    let mut top = row().w_full().h(44.0).gap(14.0).center_y();
    top = if index > 1.0 {
        top.child(
            round("setup-back", &icons::BACK, 40.0, SURFACE2, TEXT, "Back")
                .on_press(|s: &mut State| s.send("setup back".into())),
        )
    } else {
        top.child(el().size(40.0, 40.0))
    };
    top = top.child(
        row()
            .key("setup-progress")
            .grow(1.0)
            .h(6.0)
            .radius(3.0)
            .fill(SURFACE2)
            .child(
                el().h_full()
                    .w_frac(index / total)
                    .radius(3.0)
                    .fill(ACCENT_FILL),
            ),
    );
    top = if can_skip && !options.iter().any(|o| o == "skip") {
        top.child(
            row()
                .key("setup-skip")
                .h(40.0)
                .px(6.0)
                .center()
                .role("button")
                .label("Skip")
                .child(t("Skip", CALLOUT, MUTED))
                .on_press(|s: &mut State| s.send("setup skip".into())),
        )
    } else {
        top.child(el().size(40.0, 40.0))
    };

    let mut body = col()
        .key("setup-screen")
        .w_full()
        .grow(1.0)
        .gap(18.0)
        .pt(28.0)
        .child(tile(ic, c, 64.0))
        .child(t(
            format!("Step {} of {}", index as u32, total as u32),
            CAPTION,
            DIM,
        ))
        .child(t(str_of(&setup["title"]), DISPLAY, TEXT).w_full().wrap());

    let body_text = str_of(&setup["body"]);
    let shown = if id == "wifi" && body_text.trim().is_empty() {
        "No networks found nearby. Type one below, or skip.".to_string()
    } else if id == "apps" && body_text.trim().is_empty() {
        "Say how each app you use should run: headless, sandbox, built in or drop. Or skip."
            .to_string()
    } else {
        body_text
    };
    body = body.child(t(shown, BODY, MUTED).w_full().wrap());
    if let Some(e) = setup["error"].as_str() {
        body = body.child(
            row()
                .w_full()
                .gap(8.0)
                .padding(12.0)
                .radius(14.0)
                .fill(BAD.alpha(0.12))
                .child(icon(&icons::ALERT, 16.0).color(BAD))
                .child(t(e.to_string(), CALLOUT, BAD).grow(1.0).shrink().wrap()),
        );
    }

    let mut bottom = col().w_full().gap(12.0);
    match input.as_deref() {
        Some("password") => {
            body = body
                .child(pin_dots(s.setup_pin.len(), 4))
                .child(keypad("setup-pin-", type_pin));
            let ok = s.setup_pin.len() >= 4;
            bottom = bottom.child(
                primary("setup-continue", "Set PIN")
                    .opacity(if ok { 1.0 } else { 0.45 })
                    .on_press(|s: &mut State| {
                        if s.setup_pin.len() < 4 {
                            return;
                        }
                        let pin = std::mem::take(&mut s.setup_pin);
                        s.send_shown(format!("setup answer {pin}"), "setup answer ****".into());
                    }),
            );
        }
        Some(_) => {
            body = body.child(
                field(&s.setup_input, "setup-input", hint(&id))
                    .on_edit(|s: &mut State| &mut s.setup_input)
                    .on_submit(|s: &mut State, _tx: &mut Tx| s.setup_submit()),
            );
            bottom = bottom.child(
                primary("setup-continue", "Continue").on_press(|s: &mut State| s.setup_submit()),
            );
        }
        None => {}
    }
    // the answers this step offers, as large buttons; the first is the main one
    for (i, o) in options.iter().enumerate() {
        let said = o.clone();
        let label = crate::theme::pretty_id(o);
        let b = if i == 0 && input.is_none() {
            primary(&format!("setup-option-{i}"), &label)
        } else {
            secondary(&format!("setup-option-{i}"), &label).w_full()
        };
        bottom =
            bottom.child(b.on_press(move |s: &mut State| s.send(format!("setup answer {said}"))));
    }
    if id == "done" {
        bottom = bottom
            .child(primary("setup-done", "Start").on_press(|s: &mut State| s.send("state".into())));
    }

    col()
        .w_full()
        .h_full()
        .fill(BG)
        .child(status_bar(st))
        .child(
            col()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .px(24.0)
                .pb(28.0)
                .child(top)
                .child(
                    body.scroll(rune::ScrollSpec {
                        stick_bottom: false,
                    })
                    .grow(1.0)
                    .min_h(0.0),
                )
                .child(bottom),
        )
}
