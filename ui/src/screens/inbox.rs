//! Inbox: SMS and the messages Android apps shared through the bridge, in one list, grouped by
//! conversation, each with where it came from. A conversation opens as a thread; its files open
//! as documents; a reply goes back by SMS or through the app's own reply action, once the owner
//! has allowed replies for that app.
use crate::State;
use crate::icons;
use crate::theme::*;
use rune::{El, Tx, col, el, icon, row, text_input};
use serde_json::Value;

/// One message, from either side.
#[derive(Clone)]
pub struct Msg {
    pub conv: String,
    pub source: String,
    pub package: String,
    pub text: String,
    pub at: u64,
    pub files: Vec<String>,
    pub unread: bool,
}

/// Everything in the inbox, oldest first.
pub fn messages(st: &Value) -> Vec<Msg> {
    let mut v: Vec<Msg> = st["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| Msg {
            conv: str_of(&m["who"]),
            source: "SMS".into(),
            package: String::new(),
            text: str_of(&m["text"]),
            at: m["at"].as_u64().unwrap_or(0),
            files: vec![],
            unread: false,
        })
        .collect();
    for m in st["inbox"].as_array().into_iter().flatten() {
        v.push(Msg {
            conv: str_of(&m["conversation"]),
            source: str_of(&m["app"]),
            package: str_of(&m["package"]),
            text: str_of(&m["text"]),
            at: m["time_ms"].as_u64().unwrap_or(0),
            files: m["files"]
                .as_array()
                .into_iter()
                .flatten()
                .map(str_of)
                .collect(),
            unread: m["read"].as_bool() == Some(false),
        });
    }
    v.sort_by_key(|m| m.at);
    v
}

/// Unread messages from Android apps (the bar's dot).
pub fn unread(st: &Value) -> usize {
    st["bridge"]["unread"].as_u64().unwrap_or(0) as usize
}

pub fn view(s: &State, st: &Value) -> El {
    let all = messages(st);
    // a conversation is a person on one app: "Mum|WhatsApp" and "Mum|SMS" are two threads
    if let Some(key) = &s.inbox_open {
        let (conv, source) = key.split_once('|').unwrap_or((key.as_str(), "SMS"));
        return thread(s, st, &all, conv, source);
    }
    let mut page = col().key("inbox").w_full().gap(16.0).child(page_title(
        "title-inbox",
        "Inbox",
        Some(format!("{} unread from Android apps", unread(st))),
    ));

    // filters from what is there: All, SMS, then each app
    let mut sources: Vec<String> = vec!["All".into(), "SMS".into()];
    for m in &all {
        if !sources.contains(&m.source) {
            sources.push(m.source.clone());
        }
    }
    let mut chips = row().key("inbox-filters").gap(8.0);
    for src in sources {
        let on = s.inbox_filter == src;
        let k = format!("inbox-filter-{}", src.to_lowercase());
        let pick = src.clone();
        chips = chips.child(
            text_chip(&k, &src, on).on_press(move |s: &mut State| s.inbox_filter = pick.clone()),
        );
    }
    page = page.child(chips.scroll_x());

    // one row per conversation, newest first
    let mut convs: Vec<(String, Msg, usize)> = vec![];
    for m in all.iter().rev() {
        if s.inbox_filter != "All" && m.source != s.inbox_filter {
            continue;
        }
        let key = format!("{}|{}", m.conv, m.source);
        if let Some(c) = convs.iter_mut().find(|(c, _, _)| *c == key) {
            if m.unread {
                c.2 += 1;
            }
            continue;
        }
        convs.push((key, m.clone(), usize::from(m.unread)));
    }
    let mut list = card("inbox-list").gap(0.0).py(4.0);
    if convs.is_empty() {
        list = list.py(18.0).child(t(
            "Nothing yet. Messages from apps you allow appear here.",
            BODY,
            TEXT2,
        ));
    }
    for (i, (key, last, n)) in convs.iter().enumerate() {
        if i > 0 {
            list = list.child(divider());
        }
        let c = key.clone();
        let conv = &last.conv;
        let mut right = col().gap(6.0).items_end();
        if *n > 0 {
            right = right.child(
                row()
                    .min_w(20.0)
                    .h(20.0)
                    .px(6.0)
                    .center()
                    .radius(10.0)
                    .fill(CORAL)
                    .child(t(n.to_string(), MICRO, CORAL_ON)),
            );
        }
        if !last.files.is_empty() {
            right = right.child(icon(&icons::PDF, 16.0).color(TEXT2));
        }
        list = list.child(
            row()
                .key(format!("inbox-row-{i}"))
                .w_full()
                .min_h(68.0)
                .py(10.0)
                .gap(14.0)
                .center_y()
                .role("button")
                .label(format!("Open {conv}"))
                .press_scale(0.98)
                .child(avatar(conv, 46.0))
                .child(
                    col()
                        .grow(1.0)
                        .shrink()
                        .min_w(0.0)
                        .gap(4.0)
                        .child(
                            row()
                                .gap(8.0)
                                .center_y()
                                .child(t(conv.clone(), HEADLINE, TEXT).shrink().truncate())
                                .child(source_tag(&last.source)),
                        )
                        .child(t(last.text.clone(), CAPTION, TEXT2).truncate()),
                )
                .child(right)
                .on_press(move |s: &mut State| {
                    s.inbox_open = Some(c.clone());
                    s.reply.clear();
                }),
        );
    }
    page.child(list)
}

fn thread(s: &State, st: &Value, all: &[Msg], conv: &str, source: &str) -> El {
    let msgs: Vec<&Msg> = all
        .iter()
        .filter(|m| m.conv == conv && m.source == source)
        .collect();
    let source = source.to_string();
    let package = msgs
        .iter()
        .rev()
        .find(|m| !m.package.is_empty())
        .map(|m| m.package.clone())
        .unwrap_or_default();
    let rule = st["bridge"]["apps"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| str_of(&a["package"]) == package)
        .cloned();
    let replies_on = source == "SMS"
        || rule
            .as_ref()
            .is_some_and(|r| r["reply"].as_bool() == Some(true));

    let mut page = col().key("inbox-thread").w_full().gap(12.0).child(
        row()
            .w_full()
            .gap(12.0)
            .center_y()
            .pt(6.0)
            .child(
                round(
                    "inbox-back",
                    &icons::BACK,
                    40.0,
                    SURFACE2,
                    TEXT,
                    "Back to the inbox",
                )
                .on_press(|s: &mut State| s.inbox_open = None),
            )
            .child(avatar(conv, 40.0))
            .child(
                col()
                    .grow(1.0)
                    .gap(4.0)
                    .child(t(conv.to_string(), HEADLINE, TEXT))
                    .child(source_tag(&source)),
            ),
    );
    for (i, m) in msgs.iter().enumerate() {
        let mut bubble = col()
            .key(format!("inbox-msg-{i}"))
            .max_w(300.0)
            .px(14.0)
            .py(10.0)
            .gap(8.0)
            .radius(18.0)
            .fill(SURFACE2)
            .child(t(m.text.clone(), BODY, TEXT).wrap());
        for (j, f) in m.files.iter().enumerate() {
            let name = f.clone();
            let (badge, ic) = crate::screens::chat::reader::file_kind(f);
            bubble = bubble.child(
                row()
                    .key(format!("inbox-file-{i}-{j}"))
                    .w_full()
                    .gap(10.0)
                    .center_y()
                    .padding(10.0)
                    .radius(14.0)
                    .fill(BG.alpha(0.6))
                    .role("button")
                    .label(format!("Open {f}"))
                    .press_scale(0.98)
                    .child(tile(ic, CORAL, 34.0))
                    .child(
                        col()
                            .grow(1.0)
                            .shrink()
                            .min_w(0.0)
                            .child(t(f.clone(), CALLOUT, TEXT).truncate())
                            .child(t(format!("{badge} · open in Agentic OS"), CAPTION, TEXT2)),
                    )
                    .on_press(move |s: &mut State| s.ask(&format!("read document {name}"))),
            );
        }
        page = page.child(row().w_full().child(bubble).child(el().grow(1.0)));
    }
    if replies_on {
        let to = conv.to_string();
        let src = source.clone();
        let send = move |s: &mut State| {
            let text = s.reply.text.trim().to_string();
            if text.is_empty() {
                return;
            }
            s.reply.clear();
            let line = if src == "SMS" {
                format!("chat text {to} {text}")
            } else {
                format!("chat reply on android to {to} saying {text}")
            };
            s.send(line);
        };
        let send2 = send.clone();
        page = page.child(
            row()
                .key("inbox-reply")
                .w_full()
                .h(54.0)
                .pl(16.0)
                .pr(6.0)
                .gap(8.0)
                .center_y()
                .radius(27.0)
                .fill(SURFACE)
                .border(1.0, VIOLET.alpha(0.4))
                .child(
                    text_input(&s.reply, BODY)
                        .key("reply")
                        .single_line()
                        .placeholder(&format!("Reply on {source}"), DIM)
                        .grow(1.0)
                        .h(22.0)
                        .on_edit(|s: &mut State| &mut s.reply)
                        .on_submit(move |s: &mut State, _tx: &mut Tx| send2(s)),
                )
                .child(
                    round(
                        "inbox-send",
                        &icons::SEND,
                        42.0,
                        ACCENT_FILL,
                        rune::Color::WHITE,
                        "Send reply",
                    )
                    .on_press(move |s: &mut State| send(s)),
                ),
        );
    } else {
        let line = format!("bridge allow {package} reply on");
        page = page.child(
            col()
                .key("inbox-replies-off")
                .w_full()
                .padding(16.0)
                .gap(10.0)
                .radius(24.0)
                .fill(SURFACE)
                .border(1.0, WARN.alpha(0.35))
                .child(t(format!("Replies to {source} are off"), HEADLINE, TEXT))
                .child(t(
                    "Agentic OS answers only through the notification's reply action. Every reply is written to your ledger.",
                    CAPTION,
                    TEXT2,
                ).w_full().wrap())
                .child(
                    action("inbox-turn-on", &format!("Turn on replies for {source}"), true)
                        .on_press(move |s: &mut State| s.send(line.clone())),
                ),
        );
    }
    page
}
