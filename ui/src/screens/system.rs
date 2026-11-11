//! Settings: grouped lists, as on any phone. Services and their sign-in, the assistant model,
//! safety (PIN and ledger), learning, functions, and restart with a confirmation.
use crate::State;
use crate::icons;
use crate::theme::*;
use rune::{El, Tx, col, icon};
use serde_json::Value;

fn group(key: &str, label: &str, rows: Vec<El>) -> El {
    let mut c = card(key).gap(0.0).py(4.0);
    for (i, r) in rows.into_iter().enumerate() {
        if i > 0 {
            c = c.child(divider());
        }
        c = c.child(r);
    }
    col().w_full().gap(10.0).child(section(label)).child(c)
}

fn chevron() -> El {
    icon(&icons::NEXT, 16.0).color(DIM)
}

pub fn view(s: &State, st: &Value) -> El {
    let mut page =
        col()
            .key("cards")
            .w_full()
            .gap(22.0)
            .child(page_title("title-system", "Settings", None));

    // services: each row opens its sign-in
    let mut services: Vec<(String, String)> = st["connectors"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|c| {
            (
                str_of(&c["name"]),
                format!(
                    "{} · {} tools",
                    pretty_id(&str_of(&c["state"])),
                    c["tools"].as_u64().unwrap_or(0)
                ),
            )
        })
        .collect();
    for known in ["telegram", "google"] {
        if !services.iter().any(|(n, _)| n == known) {
            services.push((known.to_string(), "Not connected".into()));
        }
    }
    let mut rows = Vec::new();
    for (name, state) in services {
        let open = s.expanded.as_deref() == Some(name.as_str());
        let n = name.clone();
        let mut r = col().w_full().child(
            list_row(
                &format!("connector-{name}"),
                tile(
                    if name == "google" {
                        &icons::GLOBE
                    } else {
                        &icons::MESSAGE
                    },
                    ACCENT2,
                    36.0,
                ),
                &pretty_id(&name),
                &state,
                Some(chevron()),
            )
            .role("button")
            .on_press(move |s: &mut State| {
                s.expanded = if s.expanded.as_deref() == Some(n.as_str()) {
                    None
                } else {
                    Some(n.clone())
                }
            }),
        );
        if open {
            let n2 = name.clone();
            let mut inner = col().w_full().gap(10.0).pb(12.0);
            if name == "google" {
                inner = inner
                    .child(t("Sign in opens a link. Approve it, then paste the address the browser lands on.", CAPTION, MUTED).w_full().wrap())
                    .child(pill("signin-google", "Get the sign-in link", ACCENT.alpha(0.2), ACCENT).on_press(|s: &mut State| s.send("oauth begin google".into())))
                    .child(field(&s.paste, "signin-paste", "Paste the address here").on_edit(|s: &mut State| &mut s.paste).on_submit(|s: &mut State, _tx: &mut Tx| {
                        let p = s.paste.text.trim().to_string();
                        s.paste.clear();
                        if !p.is_empty() {
                            s.send_shown(format!("oauth finish google {p}"), "oauth finish google ****".into());
                        }
                    }));
            } else {
                inner = inner
                    .child(t(format!("Paste the {} token. It is stored in the vault and cleared from this box.", pretty_id(&name)), CAPTION, MUTED).w_full().wrap())
                    .child(field(&s.token, "token", "Token").on_edit(|s: &mut State| &mut s.token))
                    .child(pill(&format!("connect-{name}"), "Save token", ACCENT, rune::Color::WHITE).on_press(move |s: &mut State| {
                        let token = s.token.text.trim().to_string();
                        s.token.clear();
                        if !token.is_empty() {
                            s.send_shown(format!("secret set conn.{n2} {token}"), format!("secret set conn.{n2} ****"));
                        }
                    }));
            }
            r = r.child(inner);
        }
        rows.push(r);
    }
    page = page.child(group("services", "Services", rows));

    // the assistant
    let models: Vec<String> = st["models"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(str_of)
        .collect();
    let model_line = models.first().cloned().unwrap_or_else(|| "No model".into());
    page = page.child(group(
        "model",
        "Assistant",
        vec![
            list_row(
                "model-row",
                tile(&icons::SPARK, ACCENT, 36.0),
                "On-device model",
                &model_line,
                Some(
                    pill("models-reset", "Reset", SURFACE2, TEXT)
                        .on_press(|s: &mut State| s.send("models reset".into())),
                ),
            ),
            list_row(
                "chip-row",
                tile(&icons::CHIP, ACCENT2, 36.0),
                "Chip",
                &str_of(&st["prio"]),
                None,
            ),
        ],
    ));

    // safety
    let pin = if st["pin_set"].as_bool() == Some(true) {
        "On"
    } else {
        "Off"
    };
    page = page.child(group(
        "safety",
        "Safety",
        vec![
            list_row(
                "pin-row",
                tile(&icons::LOCK, GOOD, 36.0),
                "Approval PIN",
                pin,
                None,
            ),
            list_row(
                "ledger-row",
                tile(&icons::LEDGER, GOOD, 36.0),
                "Ledger",
                "Every action, hash-chained",
                Some(
                    pill("verify-ledger", "Check", SURFACE2, TEXT)
                        .on_press(|s: &mut State| s.send("verify".into())),
                ),
            ),
        ],
    ));

    // learning and functions
    let fns: Vec<String> = st["functions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(str_of)
        .collect();
    page = page.child(group(
        "functions",
        "Learning",
        vec![
            list_row(
                "learning",
                tile(&icons::SPARK, WARN, 36.0),
                "What it learned",
                &str_of(&st["learning"]),
                Some(
                    pill("learn-status", "Details", SURFACE2, TEXT)
                        .on_press(|s: &mut State| s.send("learn status".into())),
                ),
            ),
            list_row(
                "functions-row",
                tile(&icons::CHIP, WARN, 36.0),
                "Functions",
                &fns.join(", "),
                None,
            ),
        ],
    ));

    // restart, with a confirmation
    let confirm = s.confirm_restart;
    page.child(group(
        "device",
        "Device",
        vec![list_row(
            "restart-row",
            tile(&icons::RESTART, BAD, 36.0),
            "Restart agentd",
            if confirm {
                "Tap again to restart"
            } else {
                "Restarts the assistant service"
            },
            Some(
                pill(
                    if confirm {
                        "restart-confirm"
                    } else {
                        "restart"
                    },
                    if confirm { "Restart now" } else { "Restart" },
                    if confirm { BAD } else { SURFACE2 },
                    if confirm { rune::Color::WHITE } else { BAD },
                )
                .on_press(|s: &mut State| {
                    if s.confirm_restart {
                        s.confirm_restart = false;
                        s.send("restart".into());
                    } else {
                        s.confirm_restart = true;
                    }
                }),
            ),
        )],
    ))
}
