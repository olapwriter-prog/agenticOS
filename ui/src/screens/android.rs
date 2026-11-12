//! Apps: the Android sandbox. Its apps as a grid of icons, memory as a bar, and a banner when the
//! OS closed the sandbox, with the reason.
use crate::State;
use crate::icons;
use crate::theme::*;
use rune::tok::TextStyle;
use rune::{El, col, el, icon, row};
use serde_json::Value;

pub fn view(_s: &State, st: &Value) -> El {
    let android = &st["android"];
    let mode = str_of(&android["mode"]);
    let running = android["running"].as_bool() == Some(true);
    let paused = android["paused"].as_bool() == Some(true);
    let showing = android["showing"].as_bool() == Some(true);
    let after = android["pause_after_secs"].as_u64().unwrap_or(60);
    let set_up = !(mode == "none" || mode.is_empty());
    let (status, dot) = if !set_up {
        ("Not set up", DIM)
    } else if paused {
        ("Paused", SUN)
    } else if running {
        ("Running", GOOD)
    } else {
        ("Stopped", DIM)
    };
    let mut page = col().key("android").w_full().gap(18.0).child(
        row()
            .w_full()
            .center_y()
            .child(page_title("title-android", "Android", None).grow(1.0))
            .child(
                row()
                    .key("android-status")
                    .h(28.0)
                    .px(12.0)
                    .gap(6.0)
                    .center_y()
                    .radius(14.0)
                    .fill(SURFACE2)
                    .child(el().size(7.0, 7.0).radius(3.5).fill(dot))
                    .child(t(status, CAPTION, MUTED)),
            ),
    );

    if !set_up {
        return page.child(
            card("android-none")
                .py(24.0)
                .items_center()
                .gap(8.0)
                .child(tile(&icons::ANDROID, DIM, 44.0))
                .child(
                    t(
                        "The Android sandbox is not set up on this phone",
                        BODY,
                        MUTED,
                    )
                    .wrap(),
                ),
        );
    }

    // Android is not redrawn here: this opens Android's own screen, as Android draws it
    let line = if showing {
        "Android is on screen. Come back here any time; it keeps your apps as they were."
            .to_string()
    } else if paused {
        "Paused: nothing needs Android, so it uses no CPU. Your apps keep their state and wake when you open it or the agent needs it.".to_string()
    } else if running {
        format!("Running in the background. It pauses after {after} s if nothing needs it.")
    } else {
        "Starts the sandbox and shows Android's own launcher, apps and Play Store.".to_string()
    };
    let mut launch = col()
        .key("android-launch")
        .w_full()
        .padding(20.0)
        .gap(14.0)
        .radius(32.0)
        .fill(MINT)
        .child(t("ANDROID 15 · SANDBOX", LABEL, MINT_ON.alpha(0.85)))
        .child(t(
            if showing { "On screen" } else { "Open Android" },
            TextStyle::new(34.0, 700.0),
            MINT_ON,
        ))
        .child(t(line, BODY, MINT_ON.alpha(0.9)).w_full().wrap())
        .child(
            row()
                .key("android-open")
                .w_full()
                .h(52.0)
                .center()
                .radius(26.0)
                .fill(MINT_ON)
                .role("button")
                .label("Open Android")
                .press_scale(0.97)
                .child(t(
                    if paused {
                        "Resume and open"
                    } else {
                        "Open Android"
                    },
                    HEADLINE,
                    MINT,
                ))
                .on_press(|s: &mut State| s.send("do android_show {}".into())),
        );
    if running {
        let mut actions = row().w_full().gap(10.0).flex_wrap();
        if showing {
            actions = actions.child(
                field_action("android-back", "Back to Agentic OS", MINT_ON)
                    .on_press(|s: &mut State| s.send("do android_hide {}".into())),
            );
        }
        actions = actions.child(if paused {
            field_action("android-resume", "Resume", MINT_ON)
                .on_press(|s: &mut State| s.send("do android_resume {}".into()))
        } else {
            field_action("android-pause", "Pause now", MINT_ON)
                .on_press(|s: &mut State| s.send("do android_pause {}".into()))
        });
        actions = actions.child(
            field_action("close-all", "Stop", MINT_ON)
                .on_press(|s: &mut State| s.send("do android_close_all {}".into())),
        );
        launch = launch.child(actions);
    }
    page = page.child(launch);

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
    page.child(
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
    )
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
