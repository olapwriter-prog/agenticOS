//! The local model's system prompt and answer grammar, as agentd builds them with the simulated
//! phone. For building training and eval sets outside the daemon.
//!   cargo run -p agentd --example prompt                      JSON {"system", "grammar"} on stdout
//!   cargo run -p agentd --example prompt -- REQUESTS.tsv DIR  DIR/sft.jsonl and DIR/grammar.gbnf,
//!     the files agentd exports, from "request<TAB>tool call" lines (# starts a comment)
use agentos_action::Registry;
use agentos_connectors::{Calendar, Messenger, Reminders};
use agentos_executor::Connector;
use agentos_functions::{Compiler, FunctionStore, FunctionTools};
use agentos_phone::{Contacts, Phone, PhoneTools, Sim};

fn main() {
    let tmp = std::env::temp_dir().join(format!("agentd-prompt-{}", std::process::id()));
    let phone = Phone::new(
        Box::new(Sim::new()),
        Contacts::load(tmp.join("contacts.json")).unwrap(),
    )
    .shared();
    let mut connectors: Vec<Box<dyn Connector>> = vec![
        Box::new(Calendar::default()),
        Box::new(Reminders::default()),
        Box::new(Messenger::default()),
        Box::new(PhoneTools(phone)),
    ];
    let phone_tools: Vec<agentos_action::Tool> =
        connectors.iter().flat_map(|c| c.tools()).collect();
    let reserved: Vec<&str> = phone_tools.iter().map(|t| t.name).collect();
    let compiler = Compiler::new("rustc".into(), tmp.join("compile"));
    let functions = FunctionStore::in_memory(&reserved, compiler).shared();
    connectors.push(Box::new(FunctionTools(functions)));
    let tools: Vec<agentos_action::Tool> = connectors.iter().flat_map(|c| c.tools()).collect();
    let system = agentd::model::system_prompt(&tools);
    let grammar = Registry::new(tools.clone()).gbnf();
    let _ = std::fs::remove_dir_all(&tmp);
    let a: Vec<String> = std::env::args().collect();
    let [_, requests, dir] = &a[..] else {
        println!(
            "{}",
            serde_json::json!({"system": system, "grammar": grammar})
        );
        return;
    };
    let mut sft = String::new();
    for line in std::fs::read_to_string(requests).unwrap().lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let (user, call) = line.split_once('\t').expect("request<TAB>tool call");
        serde_json::from_str::<serde_json::Value>(call).expect("the tool call is JSON");
        let chat = serde_json::json!({"messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
            {"role": "assistant", "content": call},
        ]});
        sft.push_str(&format!("{chat}\n"));
    }
    let dir = std::path::Path::new(dir);
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("sft.jsonl"), sft).unwrap();
    std::fs::write(dir.join("grammar.gbnf"), grammar).unwrap();
}
