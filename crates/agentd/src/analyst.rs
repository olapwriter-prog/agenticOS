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
    ("all_messages", "every text message, oldest first, one per line. `from NAME (NUMBER): TEXT` is a text NAME sent to you (incoming); `to NAME (NUMBER): TEXT` is one you sent (outgoing). A sender that is not in your contacts has no name: `from NUMBER: TEXT` (businesses, offers and promotions, strangers). `no messages` if there are none. Invented example lines:\n    from Asha (+919000011111): see you at 6?\n    to Asha (+919000011111): yes, on my way\n    from +918022233344: Flat 40% off today only, shop now"),
    ("call_log", "every call, one per line. `from NAME (NUMBER) STATE` is a call NAME made to you (incoming); `to NAME (NUMBER) STATE` is a call you made (outgoing); no name when the number is not a contact. STATE is ringing, dialing, active, held or ended (a call that has finished, answered or not). `no calls` if there are none. Invented example lines:\n    from Asha (+919000011111) ended\n    to +918022233344 ended\n    from Dev (+919000022222) ringing"),
    ("list_contacts", "the address book, one per line: `NAME: NUMBER`; `no contacts` if none. Invented example line:\n    Asha: +919000011111"),
    ("read_calendar", "takes args {\"day\": DAY}; that day's events joined by `; `, or `nothing`. Invented example: `dentist; call with Dev`"),
    ("wifi_scan", "nearby Wi-Fi networks, one per line: `SSID (NN%)`, strongest first; `no networks found` if none. Invented example line:\n    HomeNet (80%)"),
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
carefully: lines may not all look the same. Match lines by their structure (`from `, `to `, a \
name followed by ` (`), not by words from the question: \"calls Ravi made to me\" are the \
incoming lines `from Ravi (`. A question about strangers, offers or promotions means senders \
with no name.

Prefer these parsers to string matching: messages(args.get(\"all_messages\")?) gives a \
Vec<Message> with fields incoming (bool: they sent it to you), name (Option<String>: None for a \
number not in your contacts), number and text; calls(args.get(\"call_log\")?) gives a Vec<Call> \
with incoming (bool: they called you), name, number and state. Example: people in your contacts \
who texted you: messages(..).iter().filter(|m| m.incoming).filter_map(|m| m.name.clone()) \
collected into a BTreeSet.

Sources you may load, and their formats:
{}

Example: for \"how many texts did Mum send me?\":
{{\"sources\":[{{\"tool\":\"all_messages\",\"args\":{{}}}}],\"code\":\"fn run(args: &Args) -> Result<String, String> {{\\n    let n = messages(args.get(\\\"all_messages\\\")?)\\n        .iter()\\n        .filter(|m| m.incoming && m.name.as_deref() == Some(\\\"Mum\\\"))\\n        .count();\\n    Ok(format!(\\\"Mum sent {{}} texts\\\", n))\\n}}\"}}
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
        // openers no phrase command uses, so a command is never taken for a question
        "did ",
        "has ",
        "have i ",
        "when did ",
        "which ",
        "who has ",
        "who have ",
    ] {
        if lower.starts_with(p) {
            return Some(t.to_string());
        }
    }
    None
}

/// The sources from a model's answer or a client's request: a JSON list of {tool, args}, or
/// tool names separated by commas.
/// A source to load: the tool and its arguments.
pub type Source = (String, Vec<(String, String)>);

pub fn parse_sources(v: &Value) -> Result<Vec<Source>, String> {
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
        if out.iter().any(|(t, _): &Source| *t == tool) {
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

/// Does this answer say "none"? Code that matches nothing usually has a matching bug, so such an
/// answer over data that is not empty is checked once more.
pub fn says_zero(answer: &str) -> bool {
    answer
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| matches!(w, "0" | "zero" | "none" | "nobody" | "nothing" | "no"))
}

/// Lines of real data loaded (empty markers such as `no messages` do not count).
pub fn data_lines(data: &BTreeMap<String, String>) -> usize {
    data.values()
        .flat_map(|t| t.lines())
        .filter(|l| !l.trim().is_empty() && !l.starts_with("no ") && *l != "nothing")
        .count()
}

/// What the model is told when its code found nothing in data that has lines.
pub fn zero_feedback(answer: &str, lines: usize) -> String {
    format!(
        "Your code answered {answer:?}, but the data it loaded has {lines} lines. Code that \
matches nothing usually has a matching bug: check that it matches lines exactly as they look in \
the formats and example lines (incoming lines start with `from `, outgoing with `to `; names are \
followed by ` (`; strangers have no name). If the answer really is none, return the same code \
again."
    )
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
    fn zero_answers_and_line_counts() {
        assert!(says_zero("Ravi made 0 calls to you") && says_zero("You received no promotional texts"));
        assert!(!says_zero("Mum sent 4 texts") && !says_zero("10 offers"));
        let mut d = BTreeMap::new();
        d.insert("call_log".to_string(), "from Ravi (+91) ended\nto Mum (+92) ended".to_string());
        d.insert("all_messages".to_string(), "no messages".to_string());
        assert_eq!(data_lines(&d), 2);
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
        assert_eq!(question_of("did Mum call me").as_deref(), Some("did Mum call me"));
        assert!(question_of("what's on friday").is_none(), "commands stay commands");
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
