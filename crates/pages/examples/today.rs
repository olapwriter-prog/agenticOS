use agentos_action::{Registry, Tier, Tool};
use agentos_pages::{render_html, render_text, Block, Page};

fn main() {
    let reg = Registry::new(vec![
        Tool { name: "set_reminder", args: &["text", "when"], tier: Tier::Medium },
        Tool { name: "send_message", args: &["to", "text"], tier: Tier::High },
    ]);
    let page = Page {
        title: "Today".into(),
        blocks: vec![
            Block::Text("Phone is asleep, battery 80%. Two things need you.".into()),
            Block::List(vec!["10:00 Dentist".into(), "18:00 Call mum".into()]),
            Block::Bars { title: "Messages held for the digest".into(), items: vec![("chat".into(), 7.0), ("mail".into(), 3.0), ("news".into(), 12.0)] },
            Block::Button { label: "Remind me about the dentist".into(), tool: "set_reminder".into(), args: vec![("text".into(), "Dentist".into()), ("when".into(), "09:30".into())] },
            Block::Button { label: "Tell Asha I'm running late".into(), tool: "send_message".into(), args: vec![("to".into(), "Asha".into()), ("text".into(), "Running late".into())] },
        ],
    };
    println!("{}", render_text(&page, &reg));
    println!("---");
    println!("{}", render_html(&page, &reg));
}
