#![allow(dead_code)] // the whole set is kept, so screens can use any of it
//! The icon set: line icons on a 24-unit grid, drawn in the element's colour. Our own simple
//! shapes, so the UI carries no third-party artwork.
use rune::Icon;

macro_rules! line_icon {
    ($name:ident, $id:literal, $body:literal) => {
        pub static $name: Icon = Icon {
            name: $id,
            view: 24.0,
            body: concat!(
                r#"<g fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">"#,
                $body,
                "</g>"
            ),
        };
    };
}
macro_rules! solid_icon {
    ($name:ident, $id:literal, $body:literal) => {
        pub static $name: Icon = Icon {
            name: $id,
            view: 24.0,
            body: concat!(r#"<g fill="currentColor" stroke="none">"#, $body, "</g>"),
        };
    };
}

line_icon!(
    HOME,
    "home",
    r#"<path d="M3.5 10.5 12 3.5l8.5 7V19a1.5 1.5 0 0 1-1.5 1.5h-4.5v-6h-5v6H5A1.5 1.5 0 0 1 3.5 19z"/>"#
);
line_icon!(
    PHONE,
    "phone",
    r#"<path d="M5.5 3.5h3.2l1.8 4.6-2.3 1.5a11.5 11.5 0 0 0 6.2 6.2l1.5-2.3 4.6 1.8v3.2a2 2 0 0 1-2 2A15.5 15.5 0 0 1 3.5 5.5a2 2 0 0 1 2-2z"/>"#
);
line_icon!(
    BAG,
    "bag",
    r#"<path d="M5.5 8h13l-1 12.5h-11z"/><path d="M9 8V7a3 3 0 0 1 6 0v1"/>"#
);
line_icon!(
    APPS,
    "apps",
    r#"<rect x="4" y="4" width="6.5" height="6.5" rx="2"/><rect x="13.5" y="4" width="6.5" height="6.5" rx="2"/><rect x="4" y="13.5" width="6.5" height="6.5" rx="2"/><rect x="13.5" y="13.5" width="6.5" height="6.5" rx="2"/>"#
);
line_icon!(
    SLIDERS,
    "sliders",
    r#"<path d="M4 7h9M17 7h3M4 17h3M11 17h9"/><circle cx="15" cy="7" r="2"/><circle cx="9" cy="17" r="2"/>"#
);
solid_icon!(
    SPARK,
    "spark",
    r#"<path d="M12 2.5l1.9 5.6L19.5 10l-5.6 1.9L12 17.5l-1.9-5.6L4.5 10l5.6-1.9z"/><path d="M18.5 15.5l.8 2.2 2.2.8-2.2.8-.8 2.2-.8-2.2-2.2-.8 2.2-.8z"/>"#
);
line_icon!(SEND, "send", r#"<path d="M12 19V5M6 11l6-6 6 6"/>"#);
line_icon!(CHECK, "check", r#"<path d="M5 12.5l4.5 4.5L19 7.5"/>"#);
line_icon!(
    CLOSE,
    "close",
    r#"<path d="M6.5 6.5l11 11M17.5 6.5l-11 11"/>"#
);
line_icon!(
    LOCK,
    "lock",
    r#"<rect x="5" y="10.5" width="14" height="10" rx="2.5"/><path d="M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5"/>"#
);
line_icon!(
    SHIELD,
    "shield",
    r#"<path d="M12 3.5 5 6v5.5c0 4.5 3 7.7 7 9 4-1.3 7-4.5 7-9V6z"/><path d="M9 12l2.2 2.2L15.5 10"/>"#
);
line_icon!(
    WIFI,
    "wifi",
    r#"<path d="M3 9.5a13 13 0 0 1 18 0M6 13a8.5 8.5 0 0 1 12 0M9 16.5a4 4 0 0 1 6 0"/><path d="M12 20h.01"/>"#
);
line_icon!(
    SIM,
    "sim",
    r#"<path d="M7 3.5h7l4 4V20a.5.5 0 0 1-.5.5h-10A.5.5 0 0 1 7 20z"/><rect x="9.5" y="11" width="6" height="6" rx="1.2"/>"#
);
line_icon!(
    PIN,
    "pin",
    r#"<path d="M12 20.5s-6.5-5.8-6.5-11a6.5 6.5 0 0 1 13 0c0 5.2-6.5 11-6.5 11z"/><circle cx="12" cy="9.5" r="2.3"/>"#
);
line_icon!(
    USER,
    "user",
    r#"<circle cx="12" cy="8" r="3.8"/><path d="M4.5 20.5a7.5 7.5 0 0 1 15 0"/>"#
);
line_icon!(
    USERS,
    "users",
    r#"<circle cx="9" cy="8.5" r="3.3"/><path d="M3 20a6 6 0 0 1 12 0"/><path d="M15.5 5.2a3.3 3.3 0 0 1 0 6.6M17.5 14.3A6 6 0 0 1 21 20"/>"#
);
line_icon!(
    CHIP,
    "chip",
    r#"<rect x="6.5" y="6.5" width="11" height="11" rx="2.2"/><path d="M9.5 2.5v4M14.5 2.5v4M9.5 17.5v4M14.5 17.5v4M2.5 9.5h4M2.5 14.5h4M17.5 9.5h4M17.5 14.5h4"/>"#
);
line_icon!(
    LINK,
    "link",
    r#"<path d="M10 13.5a3.8 3.8 0 0 0 5.4.2l3-3a3.8 3.8 0 0 0-5.4-5.4l-1.5 1.5"/><path d="M14 10.5a3.8 3.8 0 0 0-5.4-.2l-3 3a3.8 3.8 0 0 0 5.4 5.4l1.5-1.5"/>"#
);
line_icon!(
    MESSAGE,
    "message",
    r#"<path d="M4.5 5.5h15a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1H9.5l-5 4z"/>"#
);
line_icon!(
    CALENDAR,
    "calendar",
    r#"<rect x="3.5" y="5" width="17" height="15.5" rx="2.5"/><path d="M3.5 10h17M8 3v4M16 3v4"/>"#
);
line_icon!(
    BELL,
    "bell",
    r#"<path d="M6.5 16v-5a5.5 5.5 0 0 1 11 0v5l1.5 2h-14z"/><path d="M10 21h4"/>"#
);
line_icon!(
    BOLT,
    "bolt",
    r#"<path d="M13 2.5 5 13.5h6.5l-1 8 8-11H12z"/>"#
);
line_icon!(
    SEARCH,
    "search",
    r#"<circle cx="11" cy="11" r="6.5"/><path d="M20 20l-4.2-4.2"/>"#
);
line_icon!(BACK, "back", r#"<path d="M15 5l-7 7 7 7"/>"#);
line_icon!(NEXT, "next", r#"<path d="M9 5l7 7-7 7"/>"#);
line_icon!(
    DELETE,
    "delete",
    r#"<path d="M9 5.5h11a.5.5 0 0 1 .5.5v12a.5.5 0 0 1-.5.5H9L3.5 12z"/><path d="M12.5 9.5l5 5M17.5 9.5l-5 5"/>"#
);
line_icon!(PLUS, "plus", r#"<path d="M12 5v14M5 12h14"/>"#);
line_icon!(
    INFO,
    "info",
    r#"<circle cx="12" cy="12" r="8.5"/><path d="M12 11v5.5M12 7.8v.2"/>"#
);
line_icon!(
    ALERT,
    "alert",
    r#"<path d="M12 3.5 2.5 20h19z"/><path d="M12 10v4.5M12 17.3v.2"/>"#
);
line_icon!(
    RESTART,
    "restart",
    r#"<path d="M19.5 12a7.5 7.5 0 1 1-2.2-5.3"/><path d="M19.5 4v4.5H15"/>"#
);
line_icon!(
    LEDGER,
    "ledger",
    r#"<path d="M6 3.5h10.5A2.5 2.5 0 0 1 19 6v14.5H8.5A2.5 2.5 0 0 1 6 18z"/><path d="M6 18a2.5 2.5 0 0 1 2.5-2.5H19M9.5 8h6M9.5 11h4"/>"#
);
line_icon!(
    DOWNLOAD,
    "download",
    r#"<path d="M12 4v11M7 10.5l5 5 5-5M5 20h14"/>"#
);
line_icon!(
    SIGNAL,
    "signal",
    r#"<path d="M5 19v-3M9.5 19v-6M14 19v-9M18.5 19V5"/>"#
);
line_icon!(
    INCOMING,
    "incoming",
    r#"<path d="M17 7 7 17M7 9.5V17h7.5"/>"#
);
line_icon!(
    MISSED,
    "missed",
    r#"<path d="M4 8l6 6 4-4 6 6"/><path d="M20 11.5V16h-4.5"/>"#
);
line_icon!(
    OUTGOING,
    "outgoing",
    r#"<path d="M7 17 17 7M9.5 7H17v7.5"/>"#
);
line_icon!(
    GLOBE,
    "globe",
    r#"<circle cx="12" cy="12" r="8.5"/><path d="M3.5 12h17M12 3.5c2.5 2.6 3.5 5.4 3.5 8.5s-1 5.9-3.5 8.5c-2.5-2.6-3.5-5.4-3.5-8.5s1-5.9 3.5-8.5z"/>"#
);
line_icon!(
    TRASH,
    "trash",
    r#"<path d="M4.5 7h15M9.5 7V4.5h5V7M6.5 7l1 13h9l1-13"/>"#
);
line_icon!(
    CAMERA,
    "camera",
    r#"<path d="M4 8.5a2 2 0 0 1 2-2h2l1.5-2h5l1.5 2h2a2 2 0 0 1 2 2V18a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2z"/><circle cx="12" cy="13" r="3.5"/>"#
);
line_icon!(
    WAND,
    "wand",
    r#"<path d="M4 20 15 9M14 4v3M18.5 5.5l-2 2M20 10h-3"/><path d="M13.5 10.5l-3-3"/>"#
);
