//! Files: documents on this phone, and the ones Android apps sent through the bridge. Tapping a
//! file asks the agent to read it; the answer opens in the Reader view.
use crate::State;
use crate::icons;
use crate::screens::chat::reader::file_kind;
use crate::theme::*;
use rune::{El, col, row};
use serde_json::Value;

fn size_text(bytes: u64) -> String {
    let kb = bytes / 1024;
    if kb >= 1024 {
        format!("{:.1} MB", kb as f64 / 1024.0)
    } else {
        format!("{} KB", kb.max(1))
    }
}

fn kind_colour(badge: &str) -> rune::Color {
    match badge {
        "PDF" => CORAL,
        "Sheet" => MINT,
        "Slides" => SUN,
        _ => SKY,
    }
}

fn file_row(key: &str, name: &str, sub: &str, source: Option<&str>) -> El {
    let (badge, ic) = file_kind(name);
    let c = kind_colour(badge);
    let mut meta = row().gap(8.0).center_y();
    if let Some(src) = source {
        meta = meta.child(source_tag(src));
    }
    meta = meta.child(t(sub.to_string(), CAPTION, TEXT2).truncate());
    let n = name.to_string();
    row()
        .key(key)
        .w_full()
        .min_h(64.0)
        .py(8.0)
        .gap(14.0)
        .center_y()
        .role("button")
        .label(format!("Open {name}"))
        .press_scale(0.98)
        .child(tile(ic, c, 44.0))
        .child(
            col()
                .grow(1.0)
                .shrink()
                .min_w(0.0)
                .gap(4.0)
                .child(t(name.to_string(), HEADLINE, TEXT).truncate())
                .child(meta),
        )
        .child(rune::icon(&icons::NEXT, 18.0).color(TEXT3))
        .on_press(move |s: &mut State| s.ask(&format!("read document {n}")))
}

pub fn view(_s: &State, st: &Value) -> El {
    let mut page =
        col()
            .key("files")
            .w_full()
            .gap(16.0)
            .child(page_title("title-files", "Files", None));

    let local: Vec<Value> = st["documents"].as_array().cloned().unwrap_or_default();
    let mut on_phone = card("files-local").gap(0.0).py(4.0);
    if local.is_empty() {
        on_phone = on_phone
            .py(16.0)
            .child(t("No documents on this phone yet.", BODY, TEXT2));
    }
    for (i, d) in local.iter().enumerate() {
        if i > 0 {
            on_phone = on_phone.child(divider());
        }
        on_phone = on_phone.child(file_row(
            &format!("file-local-{i}"),
            &str_of(&d["name"]),
            &size_text(d["size"].as_u64().unwrap_or(0)),
            None,
        ));
    }
    page = page.child(section("On this phone")).child(on_phone);

    let received: Vec<Value> = st["bridge"]["received"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut android = card("files-android").gap(0.0).py(4.0);
    if received.is_empty() {
        android = android.py(16.0).child(
            t(
                "Files from apps appear here once you allow their files in You › Android bridge.",
                BODY,
                TEXT2,
            )
            .w_full()
            .wrap(),
        );
    }
    for (i, r) in received.iter().enumerate() {
        if i > 0 {
            android = android.child(divider());
        }
        let app = str_of(&r["from_app"]);
        let conv = str_of(&r["conversation"]);
        let sub = if conv.is_empty() {
            size_text(r["size"].as_u64().unwrap_or(0))
        } else {
            format!("{conv} · {}", size_text(r["size"].as_u64().unwrap_or(0)))
        };
        android = android.child(file_row(
            &format!("file-android-{i}"),
            &str_of(&r["name"]),
            &sub,
            Some(&app),
        ));
    }
    page.child(section("From Android")).child(android)
}
