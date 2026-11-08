//! Analysis in the sandbox: the model answers questions about phone data it never sees.
//!
//! Loading every message, call and contact into the model's context is slow on a phone, fills
//! the context window, and shows private data to the model. Instead the model is told only what
//! each source looks like (its format, never its content) and writes Rust that reads the sources
//! and returns just the answer. agentd fetches the sources through the executor (read-only, Low
//! tier tools only), compiles the code with rustc, and runs it in the sandbox over all the data.
//! Compiler errors and failures go back to the model, as when it writes functions.
use agentos_functions::{Compiler, Outcome};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// The sources analysis code may load, and the format of each. Only read-only (Low tier)
/// tools appear here; the caller also checks the tier at run time.
pub const SOURCES: &[(&str, &str)] = &[
    ("all_messages", "every text message, one per line: `from NAME (NUMBER): TEXT` or `to NAME (NUMBER): TEXT`; just `from NUMBER: TEXT` when the sender is not a contact; `no messages` if none"),
    ("call_log", "every call, one per line: `from NAME (NUMBER) STATE` or `to NAME (NUMBER) STATE`, STATE one of ringing, dialing, active, held, ended; `no calls` if none"),
    ("list_contacts", "the address book, one per line: `NAME: NUMBER`; `no contacts` if none"),
    ("read_calendar", "takes args {\"day\": DAY}; that day's events joined by `; `, or `nothing`"),
    ("wifi_scan", "nearby Wi-Fi networks, one per line: `SSID (NN%)`, strongest first; `no networks found` if none"),
    ("phone_status", "one line, e.g. `Jio lte signal 64%, mobile data on, wifi HomeNet`"),
    ("list_functions", "the agent's functions, one per line: `name(args) vN: description`"),
];

pub fn format_of(tool: &str) -> Option<&'static str> {
    SOURCES.iter().find(|(t, _)| *t == tool).map(|(_, f)| *f)
}

/// The formats, for a Claude client (`data_formats`) or the local model.
pub fn formats() -> String {
    SOURCES
        .iter()
        .map(|(t, f)| format!("- {t}: {f}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn system_prompt() -> String {
    format!(
        "You answer questions about the data on a phone by writing Rust that processes it. You \
never see the data itself, only its format. Answer with one JSON object: sources (the data to load, \
each {{\"tool\": NAME, \"args\": {{...}}}}) and code (the Rust source).

The code must define fn run(args: &Args) -> Result<String, String>. Each source's text arrives as \
args.get(\"NAME\")? (NAME is the tool name), and the question as args.get(\"question\")?. Use only the \
standard library; no unsafe, files, network, include_str!, env!, extern or mod name;. Never unwrap \
on the data: return Err(...) instead. Return only the short answer, not the data. Parse \
carefully: lines may not all look the same.

Sources you may load, and their formats:
{}

Example: for \"how many texts did Mum send me?\":
{{\"sources\":[{{\"tool\":\"all_messages\",\"args\":{{}}}}],\"code\":\"fn run(args: &Args) -> Result<String, String> {{\\n    let n = args.get(\\\"all_messages\\\")?\\n        .lines()\\n        .filter(|l| l.starts_with(\\\"from Mum \\\"))\\n        .count();\\n    Ok(format!(\\\"Mum sent {{}} texts\\\", n))\\n}}\"}}
",
        formats()
    )
}

pub fn schema() -> Value {
    let names: Vec<&str> = SOURCES.iter().map(|(t, _)| *t).collect();
    json!({
        "type": "object",
        "properties": {
            "sources": {
                "type": "array", "minItems": 1, "maxItems": 6,
                "items": {
                    "type": "object",
                    "properties": {
                        "tool": {"type": "string", "enum": names},
                        "args": {"type": "object", "additionalProperties": {"type": "string"}}
                    },
                    "required": ["tool", "args"]
                }
            },
            "code": {"type": "string"}
        },
        "required": ["sources", "code"]
    })
}

/// Is this chat line a question for analysis? Returns the question.
pub fn question_of(text: &str) -> Option<String> {
    let t = text.trim();
    let lower = t.to_lowercase();
    for p in [
        "analyse ",
        "analyze ",
        "look through ",
        "go through ",
        "find out ",
        "work out ",
    ] {
        if lower.starts_with(p) && lower.len() > p.len() + 2 {
            return Some(t[p.len()..].trim().to_string());
        }
    }
    for p in [
        "how many ",
        "how often ",
        "who texted ",
        "who called ",
        "who sent ",
        "summarise my ",
        "summarize my ",
    ] {
        if lower.starts_with(p) {
            return Some(t.to_string());
        }
    }
    None
}

/// The sources from a model's answer or a client's request: a JSON list of {tool, args}, or
/// tool names separated by commas.
pub fn parse_sources(v: &Value) -> Result<Vec<(String, Vec<(String, String)>)>, String> {
    let list: Vec<Value> = match v {
        Value::Array(a) => a.clone(),
        Value::String(s) if s.trim_start().starts_with('[') => {
            serde_json::from_str(s).map_err(|e| format!("sources: {e}"))?
        }
        Value::String(s) => s
            .split(',')
            .map(|t| json!({"tool": t.trim(), "args": {}}))
            .filter(|v| v["tool"] != "")
            .collect(),
        _ => return Err("sources must be a list".into()),
    };
    let mut out = Vec::new();
    for item in list {
        let tool = item["tool"]
            .as_str()
            .ok_or("each source needs a tool")?
            .to_string();
        if format_of(&tool).is_none() {
            return Err(format!(
                "{tool} cannot be loaded for analysis; sources are: {}",
                SOURCES
                    .iter()
                    .map(|(t, _)| *t)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if out.iter().any(|(t, _): &(String, _)| *t == tool) {
            return Err(format!("load {tool} only once"));
        }
        let args = item["args"]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect()
            })
            .unwrap_or_default();
        out.push((tool, args));
    }
    if out.is_empty() {
        return Err("load at least one source".into());
    }
    Ok(out)
}

/// Compile the code and run it in the sandbox over the loaded data. Analysis only reads: code
/// that asks the phone to act is refused.
pub fn run(
    compiler: &Compiler,
    code: &str,
    question: &str,
    data: BTreeMap<String, String>,
) -> Result<String, String> {
    let wasm = compiler.compile(code)?;
    let mut args = data;
    args.insert("question".into(), question.to_string());
    let Outcome { result, calls } = agentos_functions::compile::run(&wasm, &args)
        .map_err(|e| format!("the code failed when it ran: {e}"))?;
    if !calls.is_empty() {
        return Err("analysis only reads data: it cannot ask the phone to act".into());
    }
    Ok(result)
}

/// The answer must be short: it goes to the model or the human, not the data itself.
pub const MAX_ANSWER: usize = 2000;

#[cfg(test)]
mod tests {
    use super::*;

    fn compiler() -> Compiler {
        Compiler::new(
            "rustc".into(),
            std::env::temp_dir().join(format!("analyst-{}", std::process::id())),
        )
    }
    #[test]
    fn questions_are_recognised() {
        assert_eq!(
            question_of("How many texts did Mum send?").as_deref(),
            Some("How many texts did Mum send?")
        );
        assert_eq!(
            question_of("look through my calls for missed ones").as_deref(),
            Some("my calls for missed ones")
        );
        assert!(question_of("call mum").is_none());
    }
    #[test]
    fn only_read_only_sources_with_known_formats_load() {
        let ok = parse_sources(&json!([{"tool": "all_messages", "args": {}}, {"tool": "read_calendar", "args": {"day": "monday"}}])).unwrap();
        assert_eq!(ok[1].1, vec![("day".to_string(), "monday".to_string())]);
        assert_eq!(
            parse_sources(&json!("all_messages, call_log"))
                .unwrap()
                .len(),
            2
        );
        assert!(parse_sources(&json!([{"tool": "send_sms", "args": {}}]))
            .unwrap_err()
            .contains("cannot be loaded"));
        assert!(parse_sources(&json!("call_log,call_log"))
            .unwrap_err()
            .contains("only once"));
        assert!(parse_sources(&json!([])).is_err());
    }
    #[test]
    fn code_runs_over_all_the_data_and_returns_only_the_answer() {
        let mut data = BTreeMap::new();
        let msgs: String = (0..500)
            .map(|i| {
                if i % 50 == 0 {
                    format!("from Mum (+91999): call me {i}\n")
                } else {
                    format!("from +9100{i}: offer {i}\n")
                }
            })
            .collect();
        data.insert("all_messages".to_string(), msgs);
        let code = r#"fn run(args: &Args) -> Result<String, String> {
    let n = args.get("all_messages")?.lines().filter(|l| l.starts_with("from Mum ")).count();
    Ok(format!("Mum sent {} of them", n))
}"#;
        assert_eq!(
            run(&compiler(), code, "how many from mum", data).unwrap(),
            "Mum sent 10 of them"
        );
    }
    #[test]
    fn analysis_cannot_act_and_reports_compiler_errors() {
        let acts = r#"fn run(args: &Args) -> Result<String, String> { phone("send_sms", &[("to", "x"), ("text", "y")])?; Ok(String::new()) }"#;
        assert!(run(&compiler(), acts, "q", BTreeMap::new())
            .unwrap_err()
            .contains("cannot ask the phone"));
        let bad = "fn run(args: &Args) -> Result<String, String> { Ok(args.get(\"q\")?.len()) }";
        assert!(run(&compiler(), bad, "q", BTreeMap::new())
            .unwrap_err()
            .contains("does not compile"));
    }
    #[test]
    fn the_prompt_shows_formats_not_data_and_the_schema_limits_sources() {
        let p = system_prompt();
        assert!(
            p.contains("never see the data") && p.contains("- all_messages: every text message")
        );
        assert!(
            schema()["properties"]["sources"]["items"]["properties"]["tool"]["enum"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| format_of(t.as_str().unwrap()).is_some())
        );
    }
}
