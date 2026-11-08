use agentos_action::{Registry, Tier, Tool};

fn main() {
    let r = Registry::new(vec![
        Tool {
            name: "set_reminder",
            args: &["text", "when"],
            tier: Tier::Medium,
        },
        Tool {
            name: "pay",
            args: &["to", "amount"],
            tier: Tier::High,
        },
    ]);
    print!("{}", r.gbnf());
}
