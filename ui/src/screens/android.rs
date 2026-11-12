//! Apps: the Android sandbox. Its apps as a grid of icons, memory as a bar, and a banner when the
//! OS closed the sandbox, with the reason.
use crate::State;
use crate::icons;
use crate::theme::*;
use rune::tok::TextStyle;
use rune::{El, col, el, icon, row};
use serde_json::{Value, json};

pub fn view(_s: &State, st: &Value) -> El {
    let android = &st["android"];
    let mode = str_of(&android["mode"]);
    let running = android["running"].as_bool() == Some(true);
    let apps = android["apps"].as_array().cloned().unwrap_or_default();
    let status = if mode == "none" || mode.is_empty() {
        "Not set up"
    } else if running {
        "Running"
    } else {
        "Stopped"
    };
    let mut page = col().key("android").w_full().gap(18.0).child(
        row()
            .w_full()
            .center_y()
            .child(page_title("title-android", "Android", None).grow(1.0))
            .child(
                row()
                    .h(28.0)
                    .px(12.0)
                    .gap(6.0)
                    .center_y()
                    .radius(14.0)
                    .fill(SURFACE2)
                    .child(
                        el().size(7.0, 7.0)
                            .radius(3.5)
                            .fill(if running { GOOD } else { DIM }),
                    )
                    .child(t(status, CAPTION, MUTED)),
            ),
    );

    page = page.child(play_card(st));

    if let Some(stop) = android["last_stop"].as_object() {
        let reason = stop
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        page = page.child(
            row()
                .key("android-banner")
                .w_full()
                .padding(14.0)
                .gap(12.0)
                .center_y()
                .radius(18.0)
                .fill(WARN.alpha(0.12))
                .border(1.0, WARN.alpha(0.35))
                .child(icon(&icons::ALERT, 20.0).color(WARN))
                .child(
                    col()
                        .grow(1.0)
                        .shrink()
                        .min_w(0.0)
                        .gap(2.0)
                        .child(t("Closed to free memory", HEADLINE, TEXT))
                        .child(
                            t(
                                format!("The OS closed the sandbox: {reason}"),
                                CAPTION,
                                MUTED,
                            )
                            .w_full()
                            .wrap(),
                        ),
                ),
        );
    }

    // memory
    let mem = &st["memory"];
    let mib = mem["available_mib"].as_u64().unwrap_or(0);
    let frac = (mib as f32 / 4096.0).clamp(0.0, 1.0);
    let gb = mib as f32 / 1024.0;
    let low = mib < 1024;
    page = page.child(
        card("memory")
            .child(
                row()
                    .w_full()
                    .center_y()
                    .child(t("Memory", HEADLINE, TEXT).grow(1.0))
                    .child(t(
                        format!("{gb:.1} GB free"),
                        CALLOUT,
                        if low { WARN } else { MUTED },
                    )),
            )
            .child(
                row()
                    .key("memory-bar")
                    .w_full()
                    .h(8.0)
                    .radius(4.0)
                    .fill(SURFACE3)
                    .child(el().h_full().w_frac(frac).radius(4.0).fill(if low {
                        WARN
                    } else {
                        GOOD
                    })),
            )
            .child(t(
                format!("Pressure {:.1}%", mem["psi"].as_f64().unwrap_or(0.0)),
                CAPTION,
                DIM,
            )),
    );

    // the apps, four to a row
    if mode == "none" || mode.is_empty() {
        page = page.child(
            card("android-apps")
                .py(24.0)
                .items_center()
                .gap(8.0)
                .child(tile(&icons::APPS, DIM, 44.0))
                .child(t(
                    "The Android sandbox is not set up on this phone",
                    BODY,
                    MUTED,
                )),
        );
        return page;
    }
    let mut grid = col().key("android-apps").w_full().gap(18.0);
    if apps.is_empty() {
        grid = grid.child(card("android-empty").py(24.0).items_center().child(t(
            if running {
                "No apps in the sandbox yet"
            } else {
                "Open an app to start the sandbox"
            },
            BODY,
            MUTED,
        )));
    }
    for chunk in apps.chunks(4) {
        let mut r = row().w_full().gap(10.0);
        for a in chunk {
            let package = str_of(&a["package"]);
            let name = str_of(&a["name"]);
            let short = name.rsplit('.').next().unwrap_or(&name).to_string();
            let label = pretty_id(&short);
            let p = package.clone();
            r = r.child(
                col()
                    .key(format!("app-{package}"))
                    .w(76.0)
                    .gap(8.0)
                    .items_center()
                    .role("button")
                    .label(&label)
                    .press_scale(0.93)
                    .child(
                        row()
                            .size(62.0, 62.0)
                            .center()
                            .radius(18.0)
                            .linear_gradient(
                                135.0,
                                [
                                    (0.0, hue_of(&package)),
                                    (1.0, hue_of(&package).mix(ACCENT2, 0.4)),
                                ],
                            )
                            .child(t(
                                initials(&label),
                                TextStyle::new(22.0, 700.0),
                                rune::Color::WHITE,
                            )),
                    )
                    .child(t(label, CAPTION, TEXT).truncate())
                    .on_press(move |s: &mut State| {
                        s.send(format!("do android_open {}", json!({"app": p})))
                    }),
            );
        }
        grid = grid.child(r);
    }
    page = page.child(grid);
    if running {
        page = page.child(
            secondary("close-all", "Close all apps")
                .w_full()
                .on_press(|s: &mut State| s.send("do android_close_all {}".into())),
        );
    }
    page
}

/// Google Play and the bridge: set up the store (High tier: it waits for the PIN), ask its
/// status, and see whether Android is sharing with Agentic OS.
fn play_card(st: &Value) -> El {
    let b = &st["bridge"];
    let up = b["up"].as_bool() == Some(true);
    let unread = b["unread"].as_u64().unwrap_or(0);
    let apps = b["apps"].as_array().map_or(0, |a| a.len());
    col()
        .key("play-card")
        .w_full()
        .padding(16.0)
        .gap(12.0)
        .radius(24.0)
        .fill(SURFACE)
        .border(1.0, HAIRLINE)
        .child(
            row()
                .w_full()
                .gap(12.0)
                .center_y()
                .child(tile(&icons::BAG, MINT, 44.0))
                .child(
                    col()
                        .grow(1.0)
                        .shrink()
                        .min_w(0.0)
                        .gap(2.0)
                        .child(t("Google Play", HEADLINE, TEXT))
                        .child(
                            t(
                                "Install apps in the sandbox. Sign-in happens inside Android.",
                                CAPTION,
                                MUTED,
                            )
                            .w_full()
                            .wrap(),
                        ),
                ),
        )
        .child(
            row()
                .w_full()
                .gap(8.0)
                .child(
                    action("play-setup", "Set up Play Store", true)
                        .grow(1.0)
                        .on_press(|s: &mut State| s.ask("set up play store")),
                )
                .child(
                    action("play-status", "Status", false)
                        .on_press(|s: &mut State| s.ask("play store status")),
                ),
        )
        .child(
            row()
                .key("bridge-line")
                .w_full()
                .gap(8.0)
                .center_y()
                .child(
                    el().size(8.0, 8.0)
                        .radius(4.0)
                        .fill(if up { GOOD } else { WARN }),
                )
                .child(
                    t(
                        if up {
                            format!("Bridge connected · {apps} apps known · {unread} unread")
                        } else {
                            "Bridge not connected: install Agentic Bridge in Android".to_string()
                        },
                        CAPTION,
                        MUTED,
                    )
                    .grow(1.0)
                    .wrap(),
                ),
        )
}
