//! The design system: one dark palette with one accent, a type scale, and the few components
//! every screen is built from. Flat fills and hairline borders keep it light to draw.
use crate::State;
use crate::icons;
use rune::tok::TextStyle;
use rune::{Color, El, Icon, col, el, icon, row, text};
use serde_json::Value;

// ---- Signal tokens (plan v3, D.1). Rune colours are linear: these are the sRGB values converted.
// Ground: ink-0 page, ink-1 raised, ink-2 cards and fields, ink-3 pressed and tracks.
pub const INK0: Color = Color {
    r: 0.00212,
    g: 0.00243,
    b: 0.00368,
    a: 1.0,
}; // #07080C
pub const INK1: Color = Color {
    r: 0.00478,
    g: 0.00561,
    b: 0.00857,
    a: 1.0,
}; // #0F1117
pub const INK2: Color = Color {
    r: 0.00857,
    g: 0.01033,
    b: 0.01600,
    a: 1.0,
}; // #171A22
pub const INK3: Color = Color {
    r: 0.01600,
    g: 0.01938,
    b: 0.03310,
    a: 1.0,
}; // #222633
pub const TEXT1: Color = Color {
    r: 0.91310,
    g: 0.92158,
    b: 0.93869,
    a: 1.0,
}; // #F5F6F8
pub const TEXT2: Color = Color {
    r: 0.37124,
    g: 0.40724,
    b: 0.47932,
    a: 1.0,
}; // #A4ABB8
pub const TEXT3: Color = Color {
    r: 0.17144,
    g: 0.19807,
    b: 0.26225,
    a: 1.0,
}; // #737B8C, decoration
// Signal colours: a field (full block), and the colour of text on it.
pub const SKY: Color = Color {
    r: 0.11193,
    g: 0.57758,
    b: 1.00000,
    a: 1.0,
}; // #5EC8FF
pub const SKY_ON: Color = Color {
    r: 0.00121,
    g: 0.01444,
    b: 0.02843,
    a: 1.0,
}; // #04202F
pub const MINT: Color = Color {
    r: 0.07819,
    g: 0.76815,
    b: 0.35640,
    a: 1.0,
}; // #4FE3A1
pub const MINT_ON: Color = Color {
    r: 0.00121,
    g: 0.02416,
    b: 0.01096,
    a: 1.0,
}; // #042B1B
pub const SUN: Color = Color {
    r: 1.00000,
    g: 0.58408,
    b: 0.06848,
    a: 1.0,
}; // #FFC94A
pub const SUN_ON: Color = Color {
    r: 0.02732,
    g: 0.01521,
    b: 0.00000,
    a: 1.0,
}; // #2E2100
pub const CORAL: Color = Color {
    r: 1.00000,
    g: 0.19462,
    b: 0.13287,
    a: 1.0,
}; // #FF7A66
pub const CORAL_ON: Color = Color {
    r: 0.03310,
    g: 0.00335,
    b: 0.00152,
    a: 1.0,
}; // #330B05
pub const VIOLET: Color = Color {
    r: 0.33245,
    g: 0.26225,
    b: 1.00000,
    a: 1.0,
}; // #9C8CFF
pub const VIOLET_ON: Color = Color {
    r: 0.00750,
    g: 0.00335,
    b: 0.04667,
    a: 1.0,
}; // #150B3D
pub const LIME: Color = Color {
    r: 0.56471,
    g: 0.88792,
    b: 0.07619,
    a: 1.0,
}; // #C6F24E
pub const LIME_ON: Color = Color {
    r: 0.01298,
    g: 0.02315,
    b: 0.00000,
    a: 1.0,
}; // #1E2A00
/// filled buttons: white text on it reads at 4.9:1
pub const VIOLET_STRONG: Color = Color {
    r: 0.14703,
    g: 0.10702,
    b: 0.87137,
    a: 1.0,
}; // #6B5CF0
// Paper: the document viewer and Android apps.
pub const PAPER0: Color = Color {
    r: 0.92158,
    g: 0.93011,
    b: 0.95597,
    a: 1.0,
}; // #F6F7FA
pub const PAPER1: Color = Color {
    r: 1.00000,
    g: 1.00000,
    b: 1.00000,
    a: 1.0,
}; // #FFFFFF
pub const PAPER2: Color = Color {
    r: 0.83880,
    g: 0.85499,
    b: 0.89627,
    a: 1.0,
}; // #ECEEF3
pub const INK_TEXT: Color = Color {
    r: 0.00700,
    g: 0.00802,
    b: 0.01161,
    a: 1.0,
}; // #14161C
pub const INK_TEXT2: Color = Color {
    r: 0.06848,
    g: 0.08228,
    b: 0.11697,
    a: 1.0,
}; // #4A5160

// The names the screens use, mapped onto the tokens.
pub const BG: Color = INK0;
pub const SURFACE: Color = INK1;
pub const SURFACE2: Color = INK2;
pub const SURFACE3: Color = INK3;
pub const HAIRLINE: Color = Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 0.08,
};
pub const TEXT: Color = TEXT1;
pub const MUTED: Color = TEXT2;
pub const DIM: Color = TEXT3;
/// icons and text in the assistant's colour
pub const ACCENT: Color = VIOLET;
/// a filled button with white text
pub const ACCENT_FILL: Color = VIOLET_STRONG;
pub const ACCENT2: Color = SKY;
pub const GOOD: Color = MINT;
pub const WARN: Color = SUN;
pub const BAD: Color = CORAL;
pub const SCRIM: Color = Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.6,
};

/// the one big number or word of an answer
pub const HERO: TextStyle = TextStyle::new(64.0, 700.0);
pub const DISPLAY: TextStyle = TextStyle::new(34.0, 700.0);
pub const TITLE: TextStyle = TextStyle::new(22.0, 600.0);
pub const HEADLINE: TextStyle = TextStyle::new(17.0, 620.0);
pub const BODY: TextStyle = TextStyle::new(16.0, 400.0);
pub const CALLOUT: TextStyle = TextStyle::new(14.0, 520.0);
pub const CAPTION: TextStyle = TextStyle::new(12.5, 500.0);
pub const MICRO: TextStyle = TextStyle::new(11.5, 650.0);
pub const KEY: TextStyle = TextStyle::new(26.0, 450.0);

/// a small uppercase label above a group
pub const LABEL: TextStyle = TextStyle {
    tracking: 0.6,
    ..TextStyle::new(13.0, 600.0)
};

/// A kind of answer, or an accent name, to its signal colours: (field, text on it, tint).
pub fn signal(kind: &str) -> (Color, Color, Color) {
    let (field, on) = match kind {
        "sky" | "weather" | "air_travel" | "travel" => (SKY, SKY_ON),
        "mint" | "air" | "done" | "health" | "calls" | "battery" => (MINT, MINT_ON),
        "sun" | "money" | "waiting" | "alert" => (SUN, SUN_ON),
        "coral" | "inbox" | "thread" | "people" | "messages" | "high" => (CORAL, CORAL_ON),
        "lime" | "files" | "document" | "places" => (LIME, LIME_ON),
        _ => (VIOLET, VIOLET_ON),
    };
    (field, on, field.alpha(0.16))
}

pub fn t(s: impl Into<String>, style: TextStyle, c: Color) -> El {
    text(s.into()).font(style).color(c)
}

pub fn str_of(v: &Value) -> String {
    v.as_str().map(String::from).unwrap_or_default()
}

/// A colour for a name: the same name always gets the same calm hue.
pub fn hue_of(name: &str) -> Color {
    let h = name
        .bytes()
        .fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32))
        % 360;
    rune::color::oklch(0.68, 0.12, h as f32)
}

/// Initials for an avatar: "Mum" → "M", "Asha Rao" → "AR".
pub fn initials(name: &str) -> String {
    let mut s: String = name
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect();
    if s.is_empty() {
        s.push('?');
    }
    s.to_uppercase()
}

pub fn avatar(name: &str, size: f32) -> El {
    let c = hue_of(name);
    row()
        .size(size, size)
        .center()
        .radius(size / 2.0)
        .fill(c.alpha(0.22))
        .child(t(initials(name), TextStyle::new(size * 0.38, 650.0), c))
}

/// A rounded square with an icon in a tinted colour: the leading mark of a row or card.
pub fn tile(ic: &'static Icon, c: Color, size: f32) -> El {
    row()
        .size(size, size)
        .center()
        .radius(size * 0.3)
        .fill(c.alpha(0.16))
        .child(icon(ic, size * 0.52).color(c))
}

/// A surface with a hairline: the container for grouped content.
pub fn card(key: &str) -> El {
    col()
        .key(key)
        .w_full()
        .padding(16.0)
        .gap(12.0)
        .radius(22.0)
        .fill(SURFACE)
        .border(1.0, HAIRLINE)
}

/// A small uppercase label above a group.
pub fn section(label: &str) -> El {
    t(
        label.to_uppercase(),
        TextStyle {
            tracking: 0.8,
            ..MICRO
        },
        DIM,
    )
    .pl(4.0)
}

/// The main action: full width, filled with the accent.
pub fn primary(key: &str, label: &str) -> El {
    row()
        .key(key)
        .w_full()
        .h(52.0)
        .center()
        .radius(26.0)
        .fill(ACCENT_FILL)
        .role("button")
        .label(label)
        .focusable(true)
        .press_scale(0.97)
        .child(t(label, HEADLINE, Color::WHITE))
}

/// A quieter action next to the main one.
pub fn secondary(key: &str, label: &str) -> El {
    row()
        .key(key)
        .h(52.0)
        .px(20.0)
        .center()
        .radius(26.0)
        .fill(SURFACE2)
        .border(1.0, HAIRLINE)
        .role("button")
        .label(label)
        .focusable(true)
        .press_scale(0.97)
        .child(t(label, HEADLINE, TEXT))
}

/// A compact pill button.
pub fn pill(key: &str, label: &str, fill: Color, ink: Color) -> El {
    row()
        .key(key)
        .h(36.0)
        .px(16.0)
        .center()
        .radius(18.0)
        .fill(fill)
        .role("button")
        .label(label)
        .focusable(true)
        .press_scale(0.96)
        .child(t(label, CALLOUT, ink))
}

/// A round button with an icon.
pub fn round(key: &str, ic: &'static Icon, size: f32, fill: Color, ink: Color, label: &str) -> El {
    row()
        .key(key)
        .size(size, size)
        .center()
        .radius(size / 2.0)
        .fill(fill)
        .role("button")
        .label(label)
        .focusable(true)
        .press_scale(0.94)
        .child(icon(ic, size * 0.45).color(ink))
}

/// A chip with text only.
pub fn text_chip(key: &str, label: &str, active: bool) -> El {
    row()
        .key(key)
        .h(36.0)
        .px(14.0)
        .center()
        .radius(18.0)
        .fill(if active { ACCENT.alpha(0.22) } else { SURFACE2 })
        .border(1.0, if active { ACCENT.alpha(0.5) } else { HAIRLINE })
        .role("button")
        .label(label)
        .focusable(true)
        .press_scale(0.96)
        .child(t(label, CALLOUT, if active { TEXT } else { MUTED }))
}

/// A list row: leading mark, a title and a subtitle, and something on the right.
pub fn list_row(key: &str, lead: El, title: &str, sub: &str, trail: Option<El>) -> El {
    let mut r = row()
        .key(key)
        .w_full()
        .min_h(56.0)
        .py(8.0)
        .gap(14.0)
        .center_y()
        .child(lead)
        .child(
            col()
                .grow(1.0)
                .shrink()
                .min_w(0.0)
                .gap(2.0)
                .child(t(title, HEADLINE, TEXT).truncate())
                .child(t(sub, CAPTION, MUTED).truncate()),
        );
    if let Some(x) = trail {
        r = r.child(x);
    }
    r
}

pub fn divider() -> El {
    el().w_full().h(1.0).fill(HAIRLINE)
}

/// A text field with a placeholder: rounded, filled, one line.
pub fn field<'a>(edit: &'a rune::TextEdit, key: &str, hint: &str) -> rune::El {
    rune::text_input(edit, BODY)
        .key(key)
        .single_line()
        .placeholder(hint, DIM)
        .w_full()
        .h(50.0)
        .px(16.0)
        .pt(15.0)
        .radius(16.0)
        .fill(SURFACE2)
        .border(1.0, HAIRLINE)
}

/// A control-centre tile: an icon over a label, a quarter of the width.
pub fn quick_tile(key: &str, ic: &'static Icon, label: &str, c: Color) -> El {
    col()
        .key(key)
        .grow(1.0)
        .h(78.0)
        .center()
        .gap(7.0)
        .radius(20.0)
        .fill(SURFACE)
        .border(1.0, HAIRLINE)
        .role("button")
        .label(label)
        .focusable(true)
        .press_scale(0.95)
        .child(icon(ic, 22.0).color(c))
        .child(t(label, CAPTION, TEXT))
}

// ---- the phone chrome ---------------------------------------------------------------------

/// The status bar: the time on the left; signal, network, Wi-Fi and battery on the right.
pub fn status_bar(st: &Value) -> El {
    let d = &st["device"];
    let time = d["time"].as_str().unwrap_or("").to_string();
    let signal = d["signal"].as_u64().unwrap_or(0) as f32;
    let battery = d["battery"].as_u64().unwrap_or(0) as f32;
    let charging = d["charging"].as_bool() == Some(true);
    let wifi = d["wifi"].is_string();
    let tech = d["tech"].as_str().unwrap_or("").to_string();

    let mut bars = row().gap(2.0).items_end().h(12.0);
    for (i, h) in [4.0, 6.5, 9.0, 11.5].iter().enumerate() {
        let on = signal >= 25.0 * i as f32 + 1.0;
        bars = bars.child(el().w(3.0).h(*h).radius(1.0).fill(if on {
            TEXT
        } else {
            TEXT.alpha(0.25)
        }));
    }
    let level = (battery / 100.0).clamp(0.0, 1.0);
    let fill_c = if charging {
        GOOD
    } else if battery <= 20.0 {
        BAD
    } else {
        TEXT
    };
    let batt = row()
        .gap(1.5)
        .center_y()
        .child(
            row()
                .w(25.0)
                .h(12.5)
                .radius(3.5)
                .border(1.2, TEXT.alpha(0.45))
                .padding(1.8)
                .child(el().h_full().w(21.4 * level).radius(2.0).fill(fill_c)),
        )
        .child(el().w(1.6).h(4.5).radius(1.0).fill(TEXT.alpha(0.45)));

    let mut right = row().gap(6.0).center_y().child(bars);
    if !tech.is_empty() {
        right = right.child(t(tech, TextStyle::new(11.5, 650.0), TEXT));
    }
    if wifi {
        right = right.child(icon(&icons::WIFI, 15.0).color(TEXT));
    }
    right = right.child(batt);
    row()
        .key("header")
        .w_full()
        .h(44.0)
        .px(24.0)
        .center_y()
        .child(t(time, TextStyle::new(15.5, 650.0), TEXT).grow(1.0))
        .child(right)
}

/// The floating bar: Home, Inbox, a raised Ask button, Android and You. A dot on Home when
/// something waits for the human, and one on Inbox when messages are unread.
#[allow(dead_code)]
pub fn tab_bar(active: crate::Tab, waiting: usize) -> El {
    tab_bar_with(active, waiting, 0)
}

pub fn tab_bar_with(active: crate::Tab, waiting: usize, unread: usize) -> El {
    use crate::Tab;
    let clear = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
    let mut bar = row()
        .key("tabs")
        .h(68.0)
        .px(8.0)
        .radius(34.0)
        .fill(INK1.alpha(0.94))
        .border(1.0, HAIRLINE)
        .shadow(
            16.0,
            40.0,
            Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.5,
            },
        )
        .center_y();
    let items: [(Option<Tab>, &str, &str, &'static Icon); 5] = [
        (Some(Tab::Home), "Home", "tab-home", &icons::HOME),
        (Some(Tab::Inbox), "Inbox", "tab-inbox", &icons::INBOX),
        (None, "Ask", "tab-ask", &icons::SPARK),
        (
            Some(Tab::Android),
            "Android",
            "tab-android",
            &icons::ANDROID,
        ),
        (Some(Tab::System), "You", "tab-system", &icons::USER),
    ];
    for (tab, label, k, ic) in items {
        let Some(tab) = tab else {
            // the Ask button sits above the bar: the assistant is the centre of the phone
            bar = bar.child(
                col().grow(1.0).h(56.0).items_center().child(
                    row()
                        .key(k)
                        .size(58.0, 58.0)
                        .center()
                        .radius(29.0)
                        .fill(ACCENT_FILL)
                        .shadow(8.0, 22.0, ACCENT_FILL.alpha(0.45))
                        .offset(0.0, -14.0)
                        .role("button")
                        .label("Ask")
                        .focusable(true)
                        .press_scale(0.94)
                        .child(icon(ic, 25.0).color(Color::WHITE))
                        .on_press(|s: &mut State| {
                            s.chat_open = true;
                            s.confirm_restart = false;
                        }),
                ),
            );
            continue;
        };
        let on = active == tab;
        let c = if on { VIOLET } else { TEXT2 };
        let mut mark = row()
            .size(24.0, 24.0)
            .center()
            .child(icon(ic, 22.0).color(c));
        let dot = (tab == Tab::Home && waiting > 0) || (tab == Tab::Inbox && unread > 0);
        if dot {
            mark = mark.child(
                el().size(8.0, 8.0)
                    .radius(4.0)
                    .fill(if tab == Tab::Home { SUN } else { CORAL })
                    .absolute(17.0, -1.0),
            );
        }
        bar = bar.child(
            col()
                .key(k)
                .grow(1.0)
                .h(54.0)
                .center()
                .gap(3.0)
                .radius(27.0)
                .fill(if on { VIOLET.alpha(0.16) } else { clear })
                .role("tab")
                .label(label)
                .focusable(true)
                .press_scale(0.95)
                .child(mark)
                .child(t(
                    label,
                    TextStyle::new(11.0, 600.0),
                    if on { TEXT } else { TEXT2 },
                ))
                .on_press(move |s: &mut State| {
                    s.tab = tab;
                    s.confirm_restart = false;
                    s.inbox_open = None;
                }),
        );
    }
    bar
}

// ---- Signal components (plan v3, D.2) -----------------------------------------------------

/// A Field: a full-width block in a signal colour, the one thing an answer is about.
pub fn signal_field(key: &str, kind: &str, label: &str, hero: &str, line: &str) -> El {
    let (field, on, _) = signal(kind);
    let mut c = col()
        .key(key)
        .w_full()
        .min_h(200.0)
        .padding(22.0)
        .gap(10.0)
        .radius(32.0)
        .fill(field)
        .child(t(label.to_uppercase(), LABEL, on.alpha(0.85)));
    if !hero.is_empty() {
        c = c.child(t(hero, TextStyle::new(56.0, 700.0), on).key(format!("{key}-hero")));
    }
    if !line.is_empty() {
        c = c.child(t(line, BODY, on.alpha(0.92)).w_full().wrap());
    }
    c
}

/// A button: primary (strong violet, white) or secondary (ink-3).
pub fn action(key: &str, label: &str, primary: bool) -> El {
    row()
        .key(key)
        .h(48.0)
        .px(20.0)
        .center()
        .radius(24.0)
        .fill(if primary { ACCENT_FILL } else { INK3 })
        .role("button")
        .label(label)
        .focusable(true)
        .press_scale(0.97)
        .child(t(label, CALLOUT, if primary { Color::WHITE } else { TEXT }))
}

/// A button that sits on a Field: a dark wash, text in the field's own ink.
pub fn field_action(key: &str, label: &str, on: Color) -> El {
    row()
        .key(key)
        .h(38.0)
        .px(16.0)
        .center()
        .radius(19.0)
        .fill(Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.16,
        })
        .role("button")
        .label(label)
        .focusable(true)
        .press_scale(0.96)
        .child(t(label, CALLOUT, on))
}

/// Where something came from: SMS (mint), an Android app (coral, with the Android mark), a
/// connector (violet).
pub fn source_tag(source: &str) -> El {
    let (c, android) = match source {
        "" => (TEXT2, false),
        "SMS" => (MINT, false),
        "Connector" | "Agentic OS" => (VIOLET, false),
        _ => (CORAL, true),
    };
    let mut r = row()
        .h(20.0)
        .px(7.0)
        .gap(4.0)
        .center_y()
        .self_start()
        .radius(10.0)
        .fill(c.alpha(0.16));
    if android {
        r = r.child(icon(&icons::ANDROID, 11.0).color(c));
    }
    r.child(t(source, TextStyle::new(10.5, 650.0), c))
}

/// A tier: Low (mint), Medium (sun), High with the PIN (coral).
#[allow(dead_code)] // the plan names it; screens show tiers once the state carries them
pub fn tier_tag(tier: &str) -> El {
    let (label, c) = match tier.to_lowercase().as_str() {
        "low" => ("Low", MINT),
        "medium" => ("Medium", SUN),
        _ => ("High · PIN", CORAL),
    };
    row()
        .h(22.0)
        .px(9.0)
        .center()
        .radius(11.0)
        .fill(c.alpha(0.16))
        .child(t(label, TextStyle::new(11.0, 650.0), c))
}

/// One step of the agent's work: a status dot, what it did, the result, and its tier.
pub fn step_row(key: &str, label: &str, detail: &str, state: &str) -> El {
    let (c, word) = match state {
        "done" => (MINT, "Done"),
        "wait" => (SUN, "Waiting"),
        "fail" => (CORAL, "Failed"),
        _ => (VIOLET, "Running"),
    };
    row()
        .key(key)
        .w_full()
        .gap(12.0)
        .items_start()
        .child(
            el().size(14.0, 14.0)
                .radius(7.0)
                .fill(c)
                .border(4.0, c.alpha(0.25))
                .mt(3.0),
        )
        .child(
            col()
                .grow(1.0)
                .shrink()
                .min_w(0.0)
                .gap(2.0)
                .child(t(label, CALLOUT, TEXT).w_full().wrap())
                .child(t(detail, CAPTION, TEXT2).w_full().wrap()),
        )
        .child(
            row()
                .h(22.0)
                .px(9.0)
                .center()
                .radius(11.0)
                .fill(c.alpha(0.16))
                .child(t(word, TextStyle::new(11.0, 650.0), c)),
        )
}

/// A large title with an optional line under it: the top of every tab.
pub fn page_title(key: &str, title: &str, sub: Option<String>) -> El {
    let mut c = col()
        .w_full()
        .gap(4.0)
        .pt(4.0)
        .pb(4.0)
        .child(t(title, DISPLAY, TEXT).key(key));
    if let Some(s) = sub {
        c = c.child(t(s, BODY, MUTED).w_full().wrap());
    }
    c
}

/// A number pad: digits 1 to 9, then a blank, 0 and backspace. `on` sends each key.
pub fn keypad(prefix: &'static str, on: fn(&mut State, &str)) -> El {
    keypad_sized(prefix, on, 74.0)
}

pub fn keypad_sized(prefix: &'static str, on: fn(&mut State, &str), size: f32) -> El {
    let mut pad = col().gap(size * 0.18).w_full().items_center();
    for keys in [
        ["1", "2", "3"],
        ["4", "5", "6"],
        ["7", "8", "9"],
        ["", "0", "⌫"],
    ] {
        let mut r = row().gap(size * 0.3);
        for k in keys {
            if k.is_empty() {
                r = r.child(el().size(size, size));
                continue;
            }
            let key = if k == "⌫" {
                format!("{prefix}back")
            } else {
                format!("{prefix}{k}")
            };
            let label = k.to_string();
            let b = if k == "⌫" {
                row()
                    .key(&key)
                    .size(size, size)
                    .center()
                    .radius(size / 2.0)
                    .role("button")
                    .label("Delete")
                    .press_scale(0.92)
                    .child(icon(&icons::DELETE, 26.0).color(MUTED))
            } else {
                row()
                    .key(&key)
                    .size(size, size)
                    .center()
                    .radius(size / 2.0)
                    .fill(SURFACE2)
                    .border(1.0, HAIRLINE)
                    .role("button")
                    .label(k)
                    .press_scale(0.92)
                    .child(t(k, KEY, TEXT))
            };
            r = r.child(b.on_press(move |s: &mut State| on(s, &label)));
        }
        pad = pad.child(r);
    }
    pad
}

/// The dots that stand for a PIN: filled for each digit typed, never the digits.
pub fn pin_dots(n: usize, of: usize) -> El {
    let mut r = row().key("pin-dots").gap(14.0).center();
    for i in 0..of.max(n) {
        r = r.child(
            el().size(14.0, 14.0)
                .radius(7.0)
                .fill(if i < n {
                    TEXT
                } else {
                    Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.0,
                    }
                })
                .border(1.5, if i < n { TEXT } else { DIM }),
        );
    }
    r
}

/// What an action is, in words, with its icon: "call" → "Call", the phone.
pub fn describe(tool: &str, args: &Value) -> (&'static Icon, Color, String, String) {
    let a = |k: &str| args[k].as_str().unwrap_or("").to_string();
    let cap = |s: String| {
        let mut c = s.chars();
        c.next()
            .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
            .unwrap_or_default()
    };
    match tool {
        "call" => (
            &icons::PHONE,
            GOOD,
            format!("Call {}", cap(a("to"))),
            "Phone call".into(),
        ),
        "send_sms" => (
            &icons::MESSAGE,
            ACCENT2,
            format!("Text {}", cap(a("to"))),
            a("text"),
        ),
        "send_message" => (
            &icons::MESSAGE,
            ACCENT2,
            format!("Message {}", cap(a("to"))),
            a("text"),
        ),
        "telegram_send" => (
            &icons::MESSAGE,
            ACCENT2,
            "Telegram message".into(),
            a("text"),
        ),
        "gmail_send" => (
            &icons::MESSAGE,
            ACCENT2,
            format!("Email {}", a("to")),
            a("subject"),
        ),
        "store_install" => (
            &icons::DOWNLOAD,
            ACCENT,
            format!("Install {}", pretty_id(&a("id"))),
            "From the store".into(),
        ),
        "android_install" => (
            &icons::DOWNLOAD,
            ACCENT,
            "Install an Android app".into(),
            a("apk"),
        ),
        "add_contact" => (
            &icons::USER,
            ACCENT,
            format!("Save {}", cap(a("name"))),
            a("number"),
        ),
        "set_reminder" => (&icons::BELL, WARN, "Reminder".into(), a("text")),
        "add_event" | "gcal_add" => (
            &icons::CALENDAR,
            ACCENT2,
            format!("Add {}", a("title")),
            a("day"),
        ),
        "read_calendar" | "gcal_list" => (&icons::CALENDAR, ACCENT2, "Calendar".into(), a("day")),
        "flashlight" => (&icons::BOLT, WARN, "Torch".into(), String::new()),
        "wifi_scan" | "wifi_connect" => (&icons::WIFI, ACCENT2, "Wi-Fi".into(), a("ssid")),
        "phone_status" => (&icons::SIGNAL, GOOD, "Signal".into(), String::new()),
        "android_open" => (
            &icons::APPS,
            GOOD,
            format!("Open {}", a("app")),
            "Android app".into(),
        ),
        "" => (&icons::SPARK, MUTED, "Answered".into(), String::new()),
        other => (
            &icons::SPARK,
            ACCENT,
            cap(other.replace('_', " ")),
            String::new(),
        ),
    }
}

/// "daily-digest" → "Daily digest".
pub fn pretty_id(id: &str) -> String {
    let s = id.replace(['-', '_'], " ");
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}
