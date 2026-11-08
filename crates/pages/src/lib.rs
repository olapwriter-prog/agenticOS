//! Pages: the screen the phone does not have. A page is plain data (title and blocks).
//! It renders to HTML for Claude's visual surface and to plain text for the terminal.
//! Rendering never emits a script, and every piece of text is escaped.
use agentos_action::{Registry, Tier};

#[derive(Debug, Clone)]
pub enum Block {
    Text(String),
    List(Vec<String>),
    /// Horizontal bars: (label, value). Values must be zero or more.
    Bars {
        title: String,
        items: Vec<(String, f64)>,
    },
    /// A suggested action. Clicking it only asks the phone; the executor still decides.
    Button {
        label: String,
        tool: String,
        args: Vec<(String, String)>,
    },
}

#[derive(Debug, Clone)]
pub struct Page {
    pub title: String,
    pub blocks: Vec<Block>,
}

pub fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            _ => o.push(c),
        }
    }
    o
}

fn bars_svg(title: &str, items: &[(String, f64)]) -> String {
    let max = items.iter().map(|(_, v)| *v).fold(0.0_f64, f64::max);
    let row_h = 26;
    let label_w = 120;
    let bar_w = 240.0;
    let h = items.len() as i32 * row_h + 28;
    let mut s = format!(
        "<svg role=\"img\" aria-label=\"{t}\" viewBox=\"0 0 420 {h}\" width=\"100%\"><title>{t}</title>",
        t = escape(title)
    );
    s.push_str(&format!(
        "<text x=\"0\" y=\"14\" font-size=\"13\" font-weight=\"600\">{}</text>",
        escape(title)
    ));
    for (i, (label, v)) in items.iter().enumerate() {
        let y = 28 + i as i32 * row_h;
        let w = if max > 0.0 {
            (v.max(0.0) / max * bar_w).round()
        } else {
            0.0
        };
        s.push_str(&format!(
            "<text x=\"0\" y=\"{}\" font-size=\"12\">{}</text>",
            y + 14,
            escape(label)
        ));
        s.push_str(&format!("<rect x=\"{label_w}\" y=\"{}\" width=\"{w}\" height=\"16\" rx=\"3\" fill=\"#2B5CAB\"/>", y + 2));
        s.push_str(&format!(
            "<text x=\"{}\" y=\"{}\" font-size=\"12\">{}</text>",
            label_w as f64 + w + 6.0,
            y + 14,
            v
        ));
    }
    s.push_str("</svg>");
    s
}

pub fn render_html(page: &Page, reg: &Registry) -> String {
    let mut h = format!("<article><h1>{}</h1>", escape(&page.title));
    for b in &page.blocks {
        match b {
            Block::Text(t) => h.push_str(&format!("<p>{}</p>", escape(t))),
            Block::List(items) => {
                h.push_str("<ul>");
                for i in items {
                    h.push_str(&format!("<li>{}</li>", escape(i)));
                }
                h.push_str("</ul>");
            }
            Block::Bars { title, items } => h.push_str(&bars_svg(title, items)),
            Block::Button { label, tool, args } => {
                let tier = reg.tier_of(tool);
                let args_txt: Vec<String> = args.iter().map(|(k, v)| format!("{k}={v}")).collect();
                let data = escape(&format!("{tool}({})", args_txt.join(", ")));
                match tier {
                    Some(Tier::High) => h.push_str(&format!(
                        "<button disabled data-action=\"{data}\" title=\"Needs the approval key on the phone\">{} (needs approval key)</button>",
                        escape(label)
                    )),
                    Some(_) => h.push_str(&format!("<button data-action=\"{data}\">{}</button>", escape(label))),
                    None => h.push_str(&format!("<button disabled title=\"Unknown tool\">{} (unavailable)</button>", escape(label))),
                }
            }
        }
    }
    h.push_str("</article>");
    h
}

pub fn render_text(page: &Page, reg: &Registry) -> String {
    let mut out = format!(
        "{}\n{}\n",
        page.title,
        "=".repeat(page.title.chars().count())
    );
    let mut n = 0;
    for b in &page.blocks {
        match b {
            Block::Text(t) => out.push_str(&format!("\n{t}\n")),
            Block::List(items) => {
                out.push('\n');
                for i in items {
                    out.push_str(&format!("  - {i}\n"));
                }
            }
            Block::Bars { title, items } => {
                out.push_str(&format!("\n{title}\n"));
                let max = items.iter().map(|(_, v)| *v).fold(0.0_f64, f64::max);
                let lw = items
                    .iter()
                    .map(|(l, _)| l.chars().count())
                    .max()
                    .unwrap_or(0);
                for (l, v) in items {
                    let w = if max > 0.0 {
                        (v.max(0.0) / max * 20.0).round() as usize
                    } else {
                        0
                    };
                    out.push_str(&format!("  {:<lw$}  {} {}\n", l, "#".repeat(w), v, lw = lw));
                }
            }
            Block::Button { label, tool, .. } => {
                n += 1;
                let note = match reg.tier_of(tool) {
                    Some(Tier::High) => " (needs the approval key)",
                    Some(_) => "",
                    None => " (unavailable)",
                };
                out.push_str(&format!("\n  [{n}] {label}{note}\n"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_action::Tool;

    fn reg() -> Registry {
        Registry::new(vec![
            Tool {
                name: "set_reminder",
                args: &["text", "when"],
                tier: Tier::Medium,
            },
            Tool {
                name: "send_message",
                args: &["to", "text"],
                tier: Tier::High,
            },
        ])
    }
    fn page(blocks: Vec<Block>) -> Page {
        Page {
            title: "Today".into(),
            blocks,
        }
    }
    fn btn(label: &str, tool: &str) -> Block {
        Block::Button {
            label: label.into(),
            tool: tool.into(),
            args: vec![("text".into(), "x".into())],
        }
    }

    #[test]
    fn hostile_text_is_escaped_everywhere() {
        let evil = "<script>alert(1)</script>\"'&";
        let p = Page {
            title: evil.into(),
            blocks: vec![
                Block::Text(evil.into()),
                Block::List(vec![evil.into()]),
                Block::Bars {
                    title: evil.into(),
                    items: vec![(evil.into(), 1.0)],
                },
                btn(evil, "set_reminder"),
            ],
        };
        let h = render_html(&p, &reg());
        assert!(!h.contains("<script"), "{h}");
        assert!(h.contains("&lt;script&gt;"));
    }
    #[test]
    fn no_page_ever_contains_a_script_tag_or_event_handler() {
        let p = page(vec![
            Block::Text("hi".into()),
            btn("Go", "set_reminder"),
            btn("Send", "send_message"),
        ]);
        let h = render_html(&p, &reg());
        assert!(!h.contains("<script") && !h.contains("onclick") && !h.contains("javascript:"));
    }
    #[test]
    fn a_medium_button_is_enabled() {
        let h = render_html(&page(vec![btn("Remind me", "set_reminder")]), &reg());
        assert!(h.contains("<button data-action=\"set_reminder(text=x)\">Remind me</button>"));
    }
    #[test]
    fn a_high_button_is_disabled_and_says_why() {
        let h = render_html(&page(vec![btn("Send", "send_message")]), &reg());
        assert!(h.contains("<button disabled"));
        assert!(h.contains("needs approval key"));
    }
    #[test]
    fn an_unknown_tool_button_is_disabled() {
        let h = render_html(&page(vec![btn("Wipe", "format_disk")]), &reg());
        assert!(h.contains("disabled") && h.contains("unavailable"));
    }
    #[test]
    fn bars_scale_to_the_largest_value() {
        let h = render_html(
            &page(vec![Block::Bars {
                title: "Hours".into(),
                items: vec![("a".into(), 5.0), ("b".into(), 10.0)],
            }]),
            &reg(),
        );
        assert!(h.contains("width=\"120\""), "{h}"); // half of 240
        assert!(h.contains("width=\"240\""));
    }
    #[test]
    fn all_zero_or_empty_bars_do_not_divide_by_zero() {
        let h = render_html(
            &page(vec![
                Block::Bars {
                    title: "Zero".into(),
                    items: vec![("a".into(), 0.0)],
                },
                Block::Bars {
                    title: "None".into(),
                    items: vec![],
                },
            ]),
            &reg(),
        );
        assert!(h.contains("width=\"0\""));
    }
    #[test]
    fn text_rendering_numbers_the_buttons_and_marks_high_ones() {
        let t = render_text(
            &page(vec![
                btn("Remind me", "set_reminder"),
                btn("Send", "send_message"),
            ]),
            &reg(),
        );
        assert!(t.contains("[1] Remind me\n"));
        assert!(t.contains("[2] Send (needs the approval key)"));
    }
    #[test]
    fn text_bars_line_up() {
        let t = render_text(
            &page(vec![Block::Bars {
                title: "Hours".into(),
                items: vec![("work".into(), 10.0), ("rest".into(), 5.0)],
            }]),
            &reg(),
        );
        assert!(t.contains("work  ####################"));
        assert!(t.contains("rest  ##########"));
    }
    #[test]
    fn the_title_is_underlined_to_its_length() {
        assert!(render_text(&page(vec![]), &reg()).starts_with("Today\n=====\n"));
    }
}
