//! Shared pieces: cards, lines, buttons and the colours the screens use.
use rune::tok::{Theme, radius, type_};
use rune::{Color, El, col, row, text};
use serde_json::Value;

pub const GOOD: Color = Color {
    r: 0.25,
    g: 0.75,
    b: 0.45,
    a: 1.0,
};
pub const BAD: Color = Color {
    r: 0.9,
    g: 0.35,
    b: 0.3,
    a: 1.0,
};

pub fn card(key: &str, title: &str, body: El, t: Theme) -> El {
    col()
        .key(key)
        .w_full()
        .padding(14.0)
        .gap(8.0)
        .radius(radius::SM)
        .fill(t.card)
        .child(
            text(title.to_uppercase())
                .font(type_::SECTION)
                .color(t.text_secondary),
        )
        .child(body)
}

pub fn line(s: impl Into<String>, t: Theme) -> El {
    text(s.into()).font(type_::BODY).color(t.text)
}

pub fn quiet(s: impl Into<String>, t: Theme) -> El {
    text(s.into()).font(type_::LABEL).color(t.text_secondary)
}

pub fn button(key: String, label: &str, fill: Color, ink: Color) -> El {
    row()
        .key(key)
        .h(32.0)
        .px(14.0)
        .center()
        .radius(radius::SM)
        .fill(fill)
        .role("button")
        .label(label)
        .focusable(true)
        .child(text(label.to_string()).font(type_::LABEL).color(ink))
}

pub fn str_of(v: &Value) -> String {
    v.as_str().map(String::from).unwrap_or_default()
}
