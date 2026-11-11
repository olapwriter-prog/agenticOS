//! The design system: one dark palette with one accent, a type scale, and the few components
//! every screen is built from. Flat fills and hairline borders keep it light to draw.
use crate::State;
use crate::icons;
use rune::tok::TextStyle;
use rune::{Color, El, Icon, col, el, icon, row, text};
use serde_json::Value;

pub const BG: Color = Color {
    r: 0.00304,
    g: 0.00335,
    b: 0.00478,
    a: 1.0,
};
pub const SURFACE: Color = Color {
    r: 0.00700,
    g: 0.00802,
    b: 0.01161,
    a: 1.0,
};
pub const SURFACE2: Color = Color {
    r: 0.01161,
    g: 0.01370,
    b: 0.02029,
    a: 1.0,
};
pub const SURFACE3: Color = Color {
    r: 0.02122,
    g: 0.02519,
    b: 0.03820,
    a: 1.0,
};
pub const HAIRLINE: Color = Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 0.07,
};
pub const TEXT: Color = Color {
    r: 0.89627,
    g: 0.90466,
    b: 0.92158,
    a: 1.0,
};
pub const MUTED: Color = Color {
    r: 0.33245,
    g: 0.36625,
    b: 0.42869,
    a: 1.0,
};
pub const DIM: Color = Color {
    r: 0.14703,
    g: 0.16827,
    b: 0.21586,
    a: 1.0,
};
pub const ACCENT: Color = Color {
    r: 0.20156,
    g: 0.22697,
    b: 1.00000,
    a: 1.0,
};
pub const ACCENT2: Color = Color {
    r: 0.01600,
    g: 0.65141,
    b: 0.85499,
    a: 1.0,
};
pub const GOOD: Color = Color {
    r: 0.03434,
    g: 0.65141,
    b: 0.31855,
    a: 1.0,
};
pub const WARN: Color = Color {
    r: 0.96469,
    g: 0.52100,
    b: 0.01764,
    a: 1.0,
};
pub const BAD: Color = Color {
    r: 0.93869,
    g: 0.16513,
    b: 0.16513,
    a: 1.0,
};
pub const SCRIM: Color = Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.6,
};

pub const DISPLAY: TextStyle = TextStyle::new(32.0, 700.0);
pub const TITLE: TextStyle = TextStyle::new(22.0, 680.0);
pub const HEADLINE: TextStyle = TextStyle::new(17.0, 620.0);
pub const BODY: TextStyle = TextStyle::new(15.0, 450.0);
pub const CALLOUT: TextStyle = TextStyle::new(14.0, 520.0);
pub const CAPTION: TextStyle = TextStyle::new(12.5, 500.0);
pub const MICRO: TextStyle = TextStyle::new(11.5, 650.0);
pub const KEY: TextStyle = TextStyle::new(26.0, 450.0);

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
        .linear_gradient(90.0, [(0.0, ACCENT), (1.0, ACCENT.mix(ACCENT2, 0.35))])
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

/// The tab bar: five icons with labels, floating above the bottom edge. A dot on Home when
/// something waits for the human.
pub fn tab_bar(active: crate::Tab, waiting: usize) -> El {
    use crate::Tab;
    let mut bar = row()
        .key("tabs")
        .h(66.0)
        .px(8.0)
        .radius(33.0)
        .fill(SURFACE2)
        .border(1.0, HAIRLINE)
        .shadow(
            10.0,
            30.0,
            Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.45,
            },
        )
        .center_y();
    for (tab, label, k, ic) in [
        (Tab::Home, "Home", "tab-home", &icons::HOME),
        (Tab::Phone, "Phone", "tab-phone", &icons::PHONE),
        (Tab::Store, "Store", "tab-store", &icons::BAG),
        (Tab::Android, "Apps", "tab-android", &icons::APPS),
        (Tab::System, "Settings", "tab-system", &icons::SLIDERS),
    ] {
        let on = active == tab;
        let c = if on { ACCENT } else { DIM };
        let mut mark = row()
            .size(24.0, 24.0)
            .center()
            .child(icon(ic, 22.0).color(c));
        if tab == Tab::Home && waiting > 0 {
            mark = mark.child(
                el().size(8.0, 8.0)
                    .radius(4.0)
                    .fill(BAD)
                    .absolute(17.0, -1.0),
            );
        }
        bar = bar.child(
            col()
                .key(k)
                .grow(1.0)
                .h(56.0)
                .center()
                .gap(3.0)
                .radius(24.0)
                .fill(if on {
                    ACCENT.alpha(0.12)
                } else {
                    Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.0,
                    }
                })
                .role("tab")
                .label(label)
                .focusable(true)
                .press_scale(0.95)
                .child(mark)
                .child(t(
                    label,
                    TextStyle::new(11.0, 600.0),
                    if on { TEXT } else { DIM },
                ))
                .on_press(move |s: &mut State| {
                    s.tab = tab;
                    s.confirm_restart = false;
                }),
        );
    }
    bar
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
