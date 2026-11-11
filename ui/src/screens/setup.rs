//! First-boot setup (F3): one question per screen, over everything until setup is done.
use crate::State;
use crate::widgets::{BAD, button, quiet, str_of};
use rune::tok::{Theme, radius, type_};
use rune::{Color, El, Tx, col, el, row, text, text_input};
use serde_json::Value;

pub fn view(s: &State, st: &Value, t: Theme, accent: Color) -> El {
    let setup = &st["setup"];
    let index = setup["index"].as_u64().unwrap_or(1) as usize;
    let total = setup["total"].as_u64().unwrap_or(13) as usize;

    let mut dots = row().key("setup-progress").gap(6.0).center_y();
    for i in 1..=total {
        dots = dots.child(el().size(8.0, 8.0).radius(4.0).fill(if i <= index { accent } else { t.control_off }));
    }

    let mut panel = col()
        .key("setup-screen")
        .w_full()
        .grow(1.0)
        .gap(14.0)
        .padding(16.0)
        .child(quiet(format!("Step {index} of {total}"), t))
        .child(text(str_of(&setup["title"])).font(type_::SECTION).color(t.text))
        .child(text(str_of(&setup["body"])).font(type_::BODY).color(t.text).w_full().wrap());

    if let Some(e) = setup["error"].as_str() {
        panel = panel.child(text(e.to_string()).font(type_::LABEL).color(BAD).w_full().wrap());
    }

    // option buttons, one per answer
    let mut options = row().key("setup-options").gap(8.0).w_full();
    for (i, o) in setup["options"].as_array().cloned().unwrap_or_default().iter().enumerate() {
        let said = str_of(o);
        options = options.child(
            button(format!("setup-option-{i}"), &said, accent, t.text)
                .on_press(move |s: &mut State| s.send(format!("setup answer {said}"))),
        );
    }
    panel = panel.child(options);

    // a text field: a password is masked in what the UI keeps
    if setup["input"].is_string() {
        let secret = setup["input"].as_str() == Some("password");
        panel = panel.child(
            text_input(&s.setup_input, type_::BODY)
                .key("setup-input")
                .single_line()
                .placeholder(if secret { "PIN" } else { "Answer" }, t.text_secondary)
                .w_full()
                .h(40.0)
                .px(14.0)
                .radius(radius::SM)
                .fill(t.card)
                .on_edit(|s: &mut State| &mut s.setup_input)
                .on_submit(move |s: &mut State, _tx: &mut Tx| {
                    let said = s.setup_input.text.trim().to_string();
                    s.setup_input.clear();
                    if !said.is_empty() {
                        if secret {
                            s.send_shown(format!("setup answer {said}"), "setup answer ****".into());
                        } else {
                            s.send(format!("setup answer {said}"));
                        }
                    }
                }),
        );
    }

    let mut nav = row().key("setup-nav").gap(8.0).w_full();
    nav = nav.child(button("setup-back".into(), "Back", t.control_off, t.text).on_press(|s: &mut State| s.send("setup back".into())));
    if setup["can_skip"].as_bool() == Some(true) {
        nav = nav.child(button("setup-skip".into(), "Skip", t.control_off, t.text).on_press(|s: &mut State| s.send("setup skip".into())));
    }
    panel = panel.child(nav);

    col()
        .w_full()
        .h_full()
        .padding(16.0)
        .gap(12.0)
        .fill(t.bg)
        .child(text("Set up your phone").font(crate::H1).color(t.text))
        .child(dots)
        .child(panel)
}
