//! The coder: the local model writes a function in Rust, rustc compiles it, the sandbox tests
//! it, and the compiler's errors or failed examples go back to the model, up to a few attempts.
//! Generation runs on its own thread (it can take minutes on a phone) so the daemon keeps
//! answering calls meanwhile; compiling and testing happen on the core thread.
use serde_json::{json, Value};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

/// What the model is told before it writes a function.
pub fn system_prompt(phone_tools: &[agentos_action::Tool]) -> String {
    let mut s = String::from(
        "You write small functions for a phone, in Rust. Answer with one JSON object: name \
(lowercase_with_underscores), description (one line), args (list of argument names), code (the \
Rust source), examples (3 to 5 objects with args and the exact expected result).

The code must define exactly this function, plus any helper functions you need:
    fn run(args: &Args) -> Result<String, String>
Rules:
- Use only the standard library. No crates, no unsafe, no files, network, clock or environment \
(they are not available), no include_str!, env!, extern, mod name;, or #![...].
- args.get(\"x\")? gives an argument's text, args.num(\"x\")? an f64, args.int(\"x\")? an i64. \
Use ? on them; never unwrap() user input: return Err(\"...\".to_string()) instead of panicking.
- Return Ok(text). Format numbers with format!, e.g. format!(\"{:.2}\", x) for two decimals.
- To make the phone do something, call phone(\"tool\", &[(\"arg\", value), ...])?; with &str \
values. It is carried out after the function ends (calls and texts still need the human's \
approval key) and returns nothing. send_sms and call accept a contact name directly.
- Each example's args must use exactly the declared argument names, and expect must be exactly \
what run returns for them. Work the expected values out carefully.

Phone tools that phone() may use:
",
    );
    for t in phone_tools {
        s.push_str(&format!("- {}({})\n", t.name, t.args.join(", ")));
    }
    s.push_str(
        r#"
Example answer, for "split a bill between people, with a tip percent":
{"name":"split_bill","description":"each person's share of a bill, tip included","args":["total","people","tip"],"code":"fn run(args: &Args) -> Result<String, String> {\n    let people = args.num(\"people\")?;\n    if people <= 0.0 {\n        return Err(\"people must be more than 0\".to_string());\n    }\n    let total = args.num(\"total\")? * (1.0 + args.num(\"tip\")? / 100.0);\n    Ok(format!(\"{:.2}\", total / people))\n}","examples":[{"args":{"total":"1000","people":"4","tip":"10"},"expect":"275.00"},{"args":{"total":"90","people":"3","tip":"0"},"expect":"30.00"},{"args":{"total":"100","people":"3","tip":"0"},"expect":"33.33"}]}
"#,
    );
    s
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {"type": "string", "pattern": "^[a-z][a-z0-9_]{0,39}$"},
            "description": {"type": "string"},
            "args": {"type": "array", "items": {"type": "string"}, "maxItems": 8},
            "code": {"type": "string"},
            "examples": {
                "type": "array", "minItems": 3, "maxItems": 5,
                "items": {
                    "type": "object",
                    "properties": {
                        "args": {"type": "object", "additionalProperties": {"type": "string"}},
                        "expect": {"type": "string"}
                    },
                    "required": ["args", "expect"]
                }
            }
        },
        "required": ["name", "description", "args", "code", "examples"]
    })
}

/// Is this chat line a request to write a function? Returns what to write.
pub fn request_of(text: &str) -> Option<String> {
    let t = text.trim();
    let lower = t.to_lowercase();
    for p in [
        "make a function",
        "write a function",
        "build a function",
        "create a function",
        "make a tool",
        "write a tool",
    ] {
        if lower.starts_with(p) && lower.len() > p.len() {
            return Some(t[p.len()..].trim_start_matches([':', ' ']).to_string());
        }
    }
    None
}

/// The arguments for write_function, from the model's JSON answer.
pub fn write_args(answer: &str) -> Result<Vec<(String, String)>, String> {
    let v: Value = serde_json::from_str(answer.trim())
        .map_err(|e| format!("your answer was not JSON: {e}"))?;
    let text = |k: &str| {
        v[k].as_str()
            .map(String::from)
            .ok_or_else(|| format!("your answer has no {k}"))
    };
    let args: Vec<String> = v["args"]
        .as_array()
        .ok_or("your answer has no args list")?
        .iter()
        .filter_map(|a| a.as_str().map(String::from))
        .collect();
    let examples = v.get("examples").ok_or("your answer has no examples")?;
    Ok(vec![
        ("name".into(), text("name")?),
        ("description".into(), text("description")?),
        ("args".into(), args.join(",")),
        ("code".into(), text("code")?),
        ("examples".into(), examples.to_string()),
    ])
}

/// What to tell the model after a failure: the report, and each code line rustc points at.
pub fn feedback(report: &str, answer: &str, phone_tools: &[&str]) -> String {
    let code = serde_json::from_str::<Value>(answer)
        .ok()
        .and_then(|v| v["code"].as_str().map(String::from))
        .unwrap_or_default();
    let lines: Vec<&str> = code.lines().collect();
    let mut out = format!("That did not work:\n{report}\n");
    let mut quoted = std::collections::BTreeSet::new();
    // rustc's short format: f.rs:LINE:COL: error...
    let mut rest = report;
    while let Some(i) = rest.find("f.rs:") {
        rest = &rest[i + 5..];
        let mut nums = rest.split(':');
        let line: usize = nums.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let col: usize = nums.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        if line >= 1 && line <= lines.len() && quoted.insert(line) {
            let prefix = format!("line {line}: ");
            out.push_str(&format!("{prefix}{}\n", lines[line - 1]));
            if col >= 1 {
                out.push_str(&format!("{}^ here\n", " ".repeat(prefix.len() + col - 1)));
            }
        }
    }
    // calling a phone tool as if it were a Rust function
    for t in phone_tools {
        if report.contains(&format!("cannot find function `{t}`")) {
            out.push_str(&format!(
                "Hint: {t} is a phone tool, not a function: phone(\"{t}\", &[(\"arg\", value)])?; \
                 it returns nothing. send_sms and call accept a contact name directly.\n"
            ));
        }
    }
    out.push_str(
        "Fix the code (or an example that was wrong) and answer again with the whole function.",
    );
    out
}

/// A function being written for someone waiting on the control socket.
pub struct Job {
    pub reply: Sender<String>,
    pub url: String,
    pub messages: Vec<(String, String)>,
    pub attempt: u32,
    pub rx: Receiver<Result<String, String>>,
    pub timeout: Duration,
    /// The JSON schema the answer must match.
    pub schema: Value,
    /// Some(question) for analysis code (crate::analyst); None for a function to keep.
    pub question: Option<String>,
}

/// Ask the model on another thread; the answer arrives on the returned channel.
pub fn generate(
    url: &str,
    messages: &[(String, String)],
    schema: &Value,
    timeout: Duration,
) -> Receiver<Result<String, String>> {
    let (tx, rx) = channel();
    let (url, messages, schema) = (url.to_string(), messages.to_vec(), schema.clone());
    std::thread::spawn(move || {
        let _ = tx.send(agentos_host::chat_json(
            &url, &messages, &schema, 1500, timeout,
        ));
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requests_are_recognised() {
        assert_eq!(
            request_of("Make a function that adds two numbers").as_deref(),
            Some("that adds two numbers")
        );
        assert_eq!(
            request_of("write a tool: convert km to miles").as_deref(),
            Some("convert km to miles")
        );
        assert!(request_of("make a function").is_none());
        assert!(request_of("call mum").is_none());
    }
    #[test]
    fn feedback_quotes_the_lines_rustc_points_at() {
        let answer = r#"{"code":"fn run(args: &Args) -> Result<String, String> {\n    let x: i64 = \"a\";\n    Ok(x.to_string())\n}"}"#;
        let f = feedback(
            "does not compile:\nf.rs:2:18: error[E0308]: mismatched types",
            answer,
            &[],
        );
        assert!(f.contains("line 2:     let x: i64 = \"a\";"), "{f}");
        assert!(
            f.contains("^ here") && f.starts_with("That did not work"),
            "{f}"
        );
        let tool = feedback(
            "f.rs:2:5: error[E0425]: cannot find function `find_contact` in this scope",
            answer,
            &["find_contact"],
        );
        assert!(tool.contains("phone(\"find_contact\""), "{tool}");
        let plain = feedback("example 1: expected \"3\", got \"4\"", answer, &[]);
        assert!(!plain.contains("Hint") && plain.contains("Fix the code"));
    }
    #[test]
    fn the_answer_becomes_write_function_args() {
        let a = write_args(r#"{"name":"add","description":"d","args":["a","b"],"code":"1","examples":[{"args":{"a":"1","b":"2"},"expect":"3"}]}"#).unwrap();
        assert_eq!(a[2], ("args".into(), "a,b".into()));
        assert!(a[4].1.starts_with('[') && a[4].1.contains("expect"));
        assert!(write_args("not json").unwrap_err().contains("not JSON"));
    }
    #[test]
    fn the_prompt_asks_for_rust_and_lists_phone_tools() {
        use agentos_action::{Tier, Tool};
        let p = system_prompt(&[Tool {
            name: "send_sms",
            args: &["to", "text"],
            tier: Tier::High,
        }]);
        assert!(
            p.contains("in Rust") && p.contains("fn run(args: &Args) -> Result<String, String>")
        );
        assert!(p.contains("- send_sms(to, text)"));
        assert_eq!(schema()["properties"]["examples"]["minItems"], 3);
    }
}
