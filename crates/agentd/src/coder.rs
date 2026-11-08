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
- Rhai is NOT JavaScript. There is no Math object: use x ** y for powers, x.sqrt(), x.abs(), \
x.round(), x.floor(). There is no null: use () or \"\". There is no ?: operator: if is an \
expression, so write let y = if c { a } else { b };. Use parse_int/parse_float, not parseInt; \
.len(), not .length; fn, not function or =>. Strings have no reverse(): let c = s.to_chars(); \
c.reverse(); then build a string with for ch in c { out += ch; }. Booleans come back as \"true\"/\"false\".
- Rhai is NOT Rust either: no ? operator, no .rev(), no .collect(), no ::<Type>, no |x| closures, \
no match. Loop with for and build results by hand.
- phone(...) returns nothing: it only asks for an action after the function ends. send_sms and call \
take a contact name directly, so there is no need to look a number up first.
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

/// Hints for mistakes the model makes again and again (mostly JavaScript habits).
const HINTS: &[(&str, &str)] = &[
    ("Variable not found: Math", "Rhai has no Math object: use x ** y for powers, x.sqrt(), x.abs(), x.round(), x.floor()."),
    ("'null' is a reserved keyword", "Rhai has no null: use () for nothing, or \"\"."),
    ("Unknown operator: '?'", "Rhai has no ?: operator: if is an expression, so write let y = if c { a } else { b };"),
    ("Unknown operator: '==='", "Use == (there is no ===)."),
    ("Function not found: reverse (&str", "Strings have no reverse(): let c = s.to_chars(); c.reverse(); then let out = \"\"; for ch in c { out += ch; }"),
    ("Variable not found: parseInt", "Use parse_int(x)."),
    ("Variable not found: parseFloat", "Use parse_float(x)."),
    ("Variable not found: console", "There is no console; the last expression is the result."),
    ("Property not found: length", "Use .len() for the length of a string or array."),
    ("'function' is a reserved keyword", "Define functions with fn name(a) { ... }."),
    ("Expecting ';' to terminate", "Every statement ends with ;. This is often a Rust or JavaScript habit: no ?, .collect(), ::<T>, |x| closures or => arrows."),
    ("Function not found: rev ", "There is no .rev(): use let c = s.to_chars(); c.reverse();"),
    ("Function not found: collect", "There is no .collect(): build the result with a for loop."),
    ("Invalid character", "Strings use double quotes \"...\"; single quotes hold exactly one character, so '' is not allowed."),
];

/// What to tell the model after a failure: the report, the code lines it points at, and hints.
pub fn feedback(report: &str, answer: &str, phone_tools: &[&str]) -> String {
    let code = serde_json::from_str::<Value>(answer)
        .ok()
        .and_then(|v| v["code"].as_str().map(String::from))
        .unwrap_or_default();
    let lines: Vec<&str> = code.lines().collect();
    let mut out = format!("That did not work:\n{report}\n");
    let mut quoted = std::collections::BTreeSet::new();
    let mut rest = report;
    while let Some(i) = rest.find("(line ") {
        rest = &rest[i + 6..];
        let n: usize = rest
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .unwrap_or(0);
        let pos: usize = rest
            .find("position ")
            .map(|p| {
                rest[p + 9..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
            })
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        if n >= 1 && n <= lines.len() && quoted.insert(n) {
            out.push_str(&format!("line {n}: {}\n", lines[n - 1]));
            if pos >= 1 {
                out.push_str(&format!(
                    "{}^ here\n",
                    " ".repeat(pos - 1 + format!("line {n}: ").len())
                ));
            }
        }
    }
    let mut hints: Vec<String> = HINTS
        .iter()
        .filter(|(pat, _)| report.contains(pat))
        .map(|(_, h)| h.to_string())
        .collect();
    // calling a phone tool as if it were a function
    for t in phone_tools {
        if report.contains(&format!("Function not found: {t} ")) {
            hints.push(format!(
                "{t} is a phone tool, not a function: use phone(\"{t}\", #{{...}}). Its result is \
                 not available to the code. send_sms and call accept a contact name directly."
            ));
        }
    }
    if !hints.is_empty() {
        out.push_str("Hint: ");
        out.push_str(&hints.join(" "));
        out.push('\n');
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
    fn feedback_quotes_the_line_and_gives_a_hint() {
        let answer = r#"{"code":"let a = 2;\nlet b = Math.pow(a, 3);\nb"}"#;
        let f = feedback(
            "example 1: error: Variable not found: Math (line 2, position 9)",
            answer,
            &[],
        );
        assert!(f.contains("line 2: let b = Math.pow(a, 3);"), "{f}");
        assert!(f.contains("^ here") && f.contains("x ** y"), "{f}");
        assert!(f.starts_with("That did not work"));
        let plain = feedback("example 1: expected \"3\", got \"4\"", answer, &[]);
        let tool = feedback(
            "error: Function not found: find_contact (&str) (line 1, position 3)",
            answer,
            &["find_contact"],
        );
        assert!(tool.contains("phone(\"find_contact\""), "{tool}");
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
