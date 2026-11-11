//! The Android tab (F8): the sandbox apps as a grid, a Close all button, a memory bar, and a banner
//! when the OS closed the sandbox and why.
use crate::State;
use crate::widgets::{BAD, button, card, line, quiet, str_of};
use rune::tok::{Theme, radius, type_};
use rune::{Color, El, col, el, row, text};
use serde_json::json;

pub fn view(_s: &State, st: &serde_json::Value, t: Theme, accent: Color) -> El {
    let android = &st["android"];
    let running = android["running"].as_bool() == Some(true);
    let mode = str_of(&android["mode"]);

    let mut page = col().key("android").w_full().gap(12.0);

    // the banner: the OS closed the sandbox, and why
    if let Some(stop) = android["last_stop"].as_object() {
        let reason = stop
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        page = page.child(
            col()
                .key("android-banner")
                .w_full()
                .padding(12.0)
                .radius(radius::SM)
                .fill(BAD)
                .child(
                    text(format!("The OS closed the Android sandbox: {reason}"))
                        .font(type_::LABEL)
                        .color(t.text)
                        .w_full()
                        .wrap(),
                ),
        );
    }

    // memory: how much is free, against 4 GiB, and the pressure
    let mem = &st["memory"];
    let mib = mem["available_mib"].as_u64().unwrap_or(0);
    let frac = (mib as f32 / 4096.0).clamp(0.0, 1.0);
    let bar = row()
        .key("memory-bar")
        .w_full()
        .h(10.0)
        .radius(5.0)
        .fill(t.control_off)
        .child(
            el().key("memory-used")
                .h(10.0)
                .w(frac * 340.0)
                .radius(5.0)
                .fill(accent),
        );
    page = page.child(card(
        "memory",
        "Memory",
        col()
            .gap(6.0)
            .child(line(format!("{mib} MiB free"), t))
            .child(bar)
            .child(quiet(
                format!("pressure {:.1}%", mem["psi"].as_f64().unwrap_or(0.0)),
                t,
            )),
        t,
    ));

    // the apps, as a grid of cards
    let apps = android["apps"].as_array().cloned().unwrap_or_default();
    let status = if mode == "none" {
        "The Android sandbox is not set up on this phone.".to_string()
    } else if running {
        format!("Running: {} apps open or installed", apps.len())
    } else {
        "Not running. Opening an app starts it.".to_string()
    };
    let mut grid = col().gap(8.0);
    let mut current = row().gap(8.0).w_full();
    for (i, a) in apps.iter().enumerate() {
        let package = str_of(&a["package"]);
        let name = str_of(&a["name"]);
        let p2 = package.clone();
        current = current.child(
            button(format!("app-{package}"), &name, t.card, t.text)
                .grow(1.0)
                .on_press(move |s: &mut State| {
                    s.send(format!("do android_open {}", json!({"app": p2})))
                }),
        );
        if i % 2 == 1 {
            grid = grid.child(current);
            current = row().gap(8.0).w_full();
        }
    }
    if apps.len() % 2 == 1 {
        grid = grid.child(current);
    }
    page = page.child(card(
        "android-apps",
        "Apps",
        col()
            .gap(8.0)
            .child(quiet(status, t).w_full().wrap())
            .child(grid),
        t,
    ));

    page = page.child(
        button("close-all".into(), "Close all", t.control_off, t.text)
            .on_press(|s: &mut State| s.send("do android_close_all {}".into())),
    );
    page
}
