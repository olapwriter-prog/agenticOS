//! The System tab (F9), with the Services section (F6): connected services and their sign-in,
//! the model and the chip, the functions the agent wrote, what it has learned, and the ledger.
use crate::State;
use crate::widgets::{button, card, line, quiet, str_of};
use rune::tok::{Theme, radius, type_};
use rune::{Color, El, Tx, col, row, text_input};

pub fn view(s: &State, st: &serde_json::Value, t: Theme, accent: Color) -> El {
    let mut cards = col().key("cards").w_full().gap(12.0);

    cards = cards.child(services(s, st, t, accent));

    // the model and the chip
    let mut model = col().gap(4.0);
    for m in st["models"].as_array().cloned().unwrap_or_default() {
        model = model.child(line(str_of(&m), t));
    }
    model = model.child(quiet(str_of(&st["prio"]), t).w_full().wrap());
    model = model.child(
        row()
            .key("system-actions")
            .gap(8.0)
            .w_full()
            .child(
                button("models-reset".into(), "Reset models", t.control_off, t.text)
                    .on_press(|s: &mut State| s.send("models reset".into())),
            )
            .child(
                button(
                    "verify-ledger".into(),
                    "Check ledger",
                    t.control_off,
                    t.text,
                )
                .on_press(|s: &mut State| s.send("verify".into())),
            ),
    );
    cards = cards.child(card("model", "Model and chip", model, t));

    // functions the agent wrote
    let mut fns = col().gap(4.0);
    for f in st["functions"].as_array().cloned().unwrap_or_default() {
        fns = fns.child(line(str_of(&f), t));
    }
    cards = cards.child(card("functions", "Functions", fns, t));

    // learning
    let learning = col()
        .gap(8.0)
        .child(quiet(str_of(&st["learning"]), t).w_full().wrap())
        .child(
            button(
                "learn-status".into(),
                "Learning status",
                t.control_off,
                t.text,
            )
            .on_press(|s: &mut State| s.send("learn status".into())),
        );
    cards = cards.child(card("learning", "Learning", learning, t));

    cards = cards.child(
        button("restart".into(), "Restart agentd", t.control_off, t.text)
            .on_press(|s: &mut State| s.send("restart".into())),
    );
    cards
}

fn services(s: &State, st: &serde_json::Value, t: Theme, accent: Color) -> El {
    let mut list = col().gap(8.0);
    let connectors = st["connectors"].as_array().cloned().unwrap_or_default();
    if connectors.is_empty() {
        list = list.child(quiet("No services connected yet.", t));
    }
    for c in &connectors {
        let name = str_of(&c["name"]);
        let state = str_of(&c["state"]);
        let tools = c["tools"].as_u64().unwrap_or(0);
        let n = name.clone();
        list = list.child(
            row()
                .key(format!("connector-{name}"))
                .w_full()
                .center_y()
                .gap(8.0)
                .child(line(name.clone(), t).grow(1.0).shrink().truncate())
                .child(quiet(format!("{state}, {tools} tools"), t))
                .child(
                    button(format!("connect-{name}"), "Save token", accent, t.text).on_press(
                        move |s: &mut State| {
                            let token = s.token.text.trim().to_string();
                            s.token.clear();
                            if !token.is_empty() {
                                s.send_shown(
                                    format!("secret set conn.{n} {token}"),
                                    format!("secret set conn.{n} ****"),
                                );
                            }
                        },
                    ),
                ),
        );
    }
    // sign-in by pasted link: begin, open the link, paste the address back
    let signin = col().key("signin").w_full().gap(8.0).child(
        row()
            .w_full()
            .gap(8.0)
            .child(
                button(
                    "signin-google".into(),
                    "Sign in to Google",
                    t.control_off,
                    t.text,
                )
                .on_press(|s: &mut State| s.send("oauth begin google".into())),
            )
            .child(
                text_input(&s.paste, type_::BODY)
                    .key("signin-paste")
                    .single_line()
                    .placeholder("Pasted address", t.text_secondary)
                    .grow(1.0)
                    .h(40.0)
                    .px(12.0)
                    .radius(radius::SM)
                    .fill(t.card)
                    .on_edit(|s: &mut State| &mut s.paste)
                    .on_submit(|s: &mut State, _tx: &mut Tx| {
                        let p = s.paste.text.trim().to_string();
                        s.paste.clear();
                        if !p.is_empty() {
                            s.send_shown(
                                format!("oauth finish google {p}"),
                                "oauth finish google ****".into(),
                            );
                        }
                    }),
            ),
    );
    let token = row().key("token-row").w_full().gap(8.0).child(
        text_input(&s.token, type_::BODY)
            .key("token")
            .single_line()
            .placeholder("Bot or service token", t.text_secondary)
            .grow(1.0)
            .h(40.0)
            .px(12.0)
            .radius(radius::SM)
            .fill(t.card)
            .on_edit(|s: &mut State| &mut s.token),
    );
    card(
        "services",
        "Services",
        col()
            .gap(10.0)
            .child(list)
            .child(token)
            .child(signin)
            .child(
                quiet(
                    "A token is cleared from the field as soon as it is saved.",
                    t,
                )
                .w_full()
                .wrap(),
            ),
        t,
    )
}
