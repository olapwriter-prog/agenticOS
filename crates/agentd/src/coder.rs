//! The coder: the local model writes a function, the sandbox tests it, failures go back to the
//! model, up to a few attempts. Generation runs on its own thread (it can take minutes on a
//! phone) so the daemon keeps answering calls meanwhile; testing happens on the core thread.
use serde_json::{json, Value};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

/// What the model is told before it writes a function.
pub fn system_prompt(phone_tools: &[agentos_action::Tool]) -> String {
    let mut s = String::from(
        "You write small functions for a phone, in the Rhai scripting language. Answer with one \
JSON object: name (lowercase_with_underscores), description (one line), args (list of argument \
names), code (the Rhai script), examples (3 to 5 objects with args and the exact expected result).

Rules for the code:
- The arguments arrive in the map `args`, and every value is text: use parse_int(args.x) or \
parse_float(args.x) for numbers.
- The value of the last expression is the result. Return text: `${x}` turns a value into text.
- Rhai basics: let x = 1; if a > b { } else { }; for item in list { }; for i in 0..n { }; \
let list = []; list.push(x); list.len(); let m = #{key: 1}; m.key; s.split(\" \"); s.to_upper(); \
s.trim(); s.contains(\"x\"); x.round(); x.floor(); fn helper(a) { a * 2 }.
- There is no file, network or clock access.
- To make the phone do something, call phone(\"tool\", #{arg: value, ...}). It is carried out \
after the function ends, and calls and texts still need the human's approval key.
- Each example's args must use exactly the declared argument names, and expect must be exactly \
what the code returns for them.

Phone tools that phone() may use:
",
    );
    for t in phone_tools {
        s.push_str(&format!("- {}({})\n", t.name, t.args.join(", ")));
    }
    s.push_str(
        r#"
Example answer, for "split a bill between people, with a tip percent":
{"name":"split_bill","description":"each person's share of a bill, tip included","args":["total","people","tip"],"code":"let total = parse_float(args.total) * (1.0 + parse_float(args.tip) / 100.0);\nlet each = (total * 100.0 / parse_int(args.people)).round() / 100.0;\n`${each}`","examples":[{"args":{"total":"1000","people":"4","tip":"10"},"expect":"275.0"},{"args":{"total":"90","people":"3","tip":"0"},"expect":"30.0"},{"args":{"total":"100","people":"3","tip":"0"},"expect":"33.33"}]}
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

/// A function being written for someone waiting on the control socket.
pub struct Job {
    pub reply: Sender<String>,
    pub url: String,
    pub messages: Vec<(String, String)>,
    pub attempt: u32,
    pub rx: Receiver<Result<String, String>>,
    pub timeout: Duration,
}

/// Ask the model on another thread; the answer arrives on the returned channel.
pub fn generate(
    url: &str,
    messages: &[(String, String)],
    timeout: Duration,
) -> Receiver<Result<String, String>> {
    let (tx, rx) = channel();
    let (url, messages) = (url.to_string(), messages.to_vec());
    std::thread::spawn(move || {
        let _ = tx.send(agentos_host::chat_json(
            &url,
            &messages,
            &schema(),
            1500,
            timeout,
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
    fn the_answer_becomes_write_function_args() {
        let a = write_args(r#"{"name":"add","description":"d","args":["a","b"],"code":"1","examples":[{"args":{"a":"1","b":"2"},"expect":"3"}]}"#).unwrap();
        assert_eq!(a[2], ("args".into(), "a,b".into()));
        assert!(a[4].1.starts_with('[') && a[4].1.contains("expect"));
        assert!(write_args("not json").unwrap_err().contains("not JSON"));
    }
    #[test]
    fn the_prompt_lists_phone_tools_and_the_schema_requires_examples() {
        use agentos_action::{Tier, Tool};
        let p = system_prompt(&[Tool {
            name: "send_sms",
            args: &["to", "text"],
            tier: Tier::High,
        }]);
        assert!(p.contains("- send_sms(to, text)") && p.contains("phone(\"tool\""));
        assert_eq!(schema()["properties"]["examples"]["minItems"], 3);
    }
}
