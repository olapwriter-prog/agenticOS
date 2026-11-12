//! Settings: grouped lists, as on any phone. Services and their sign-in, the assistant model,
//! safety (PIN and ledger), learning, functions, and restart with a confirmation.
use crate::State;
use crate::icons;
use crate::theme::*;
use rune::{El, Tx, col, icon, row};
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
            .child(page_title("title-system", "You", None));

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
                    .child(pill(&format!("connect-{name}"), "Save token", ACCENT_FILL, rune::Color::WHITE).on_press(move |s: &mut State| {
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

    // the Android bridge: what each app may share with Agentic OS
    page = page.child(bridge_group(st));

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

/// A switch: a track and a knob.
fn switch(key: &str, label: &str, on: bool) -> El {
    let clear = rune::Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
    row()
        .key(key)
        .w(48.0)
        .h(28.0)
        .px(2.0)
        .center_y()
        .radius(14.0)
        .fill(if on { GOOD } else { SURFACE3 })
        .role("switch")
        .label(label)
        .focusable(true)
        .press_scale(0.95)
        .child(
            rune::el()
                .grow(if on { 1.0 } else { 0.0 })
                .h(1.0)
                .fill(clear),
        )
        .child(
            rune::el()
                .size(24.0, 24.0)
                .radius(12.0)
                .fill(rune::Color::WHITE),
        )
}

/// Settings › Android bridge: whether it is up, and three switches per app (read, reply, files).
/// Each change is a control command, and the daemon writes it to the ledger.
fn bridge_group(st: &Value) -> El {
    let b = &st["bridge"];
    let up = b["up"].as_bool() == Some(true);
    let apps = b["apps"].as_array().cloned().unwrap_or_default();
    let allowed = apps
        .iter()
        .filter(|a| {
            ["read", "reply", "files"]
                .iter()
                .any(|k| a[*k].as_bool() == Some(true))
        })
        .count();
    let mut rows = vec![list_row(
        "bridge-status",
        tile(&icons::LINK, if up { GOOD } else { WARN }, 36.0),
        if up { "Connected" } else { "Not connected" },
        &format!(
            "{allowed} {} allowed · logged in your ledger",
            if allowed == 1 { "app" } else { "apps" }
        ),
        None,
    )];
    if apps.is_empty() {
        rows.push(
            t(
                "Apps appear here the first time they post a notification in Android. Each starts with everything off.",
                CAPTION,
                MUTED,
            )
            .w_full()
            .wrap(),
        );
    }
    for a in apps {
        let pkg = str_of(&a["package"]);
        let name = str_of(&a["app"]);
        let mut c = col()
            .key(format!("bridge-app-{pkg}"))
            .w_full()
            .gap(4.0)
            .py(6.0)
            .child(
                row()
                    .gap(8.0)
                    .center_y()
                    .child(t(name.clone(), HEADLINE, TEXT))
                    .child(source_tag(&name)),
            );
        for (what, label) in [
            ("read", "Read messages"),
            ("reply", "Reply for me"),
            ("files", "Receive files"),
        ] {
            let on = a[what].as_bool() == Some(true);
            let line = format!(
                "bridge allow {pkg} {what} {}",
                if on { "off" } else { "on" }
            );
            c = c.child(
                row()
                    .w_full()
                    .min_h(44.0)
                    .center_y()
                    .child(t(label, BODY, TEXT).grow(1.0))
                    .child(
                        switch(
                            &format!("bridge-{pkg}-{what}"),
                            &format!("{label} from {name}"),
                            on,
                        )
                        .on_press(move |s: &mut State| s.send(line.clone())),
                    ),
            );
        }
        rows.push(c);
    }
    group("bridge", "Android bridge", rows)
}
