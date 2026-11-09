//! A stand-in for llama.cpp's llama-server, for agentd's end-to-end tests.
//! Started by the model supervisor with the same arguments as the real one. What it does is
//! written in its "model file" (-m): `tools` answers every chat with read_calendar(friday);
//! `coder` writes a Rust function that does not compile, then fixes it once shown the errors;
//! `analyst` does the same with analysis code that counts Mum's texts.
//! Every request is saved next to the model file, as <model>.last.json, and each code request
//! adds a line to <model>.coder-requests.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

const BROKEN: &str = "fn run(args: &Args) -> Result<String, String> {\n    let each: f64 = args.get(\"total\")? / args.num(\"people\")?;\n    Ok(format!(\"{:.2}\", each))\n}";
const FIXED: &str = "fn run(args: &Args) -> Result<String, String> {\n    let each = args.num(\"total\")? / args.num(\"people\")?;\n    Ok(format!(\"{:.2}\", each))\n}";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let after = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_else(|| panic!("missing {flag}"))
    };
    let (port, model) = (after("--port"), after("-m"));
    let mode = std::fs::read_to_string(&model)
        .unwrap_or_default()
        .trim()
        .to_string();
    // If the test that started us dies (a panicking test never stops its daemon), go too.
    // On Linux the supervisor's PR_SET_PDEATHSIG does this; macOS has no such thing.
    let parent = std::os::unix::process::parent_id();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if std::os::unix::process::parent_id() != parent {
            std::process::exit(0);
        }
    });
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind");
    for conn in listener.incoming().flatten() {
        let mut r = BufReader::new(conn.try_clone().unwrap());
        let mut first = String::new();
        if r.read_line(&mut first).is_err() {
            continue;
        }
        let mut len = 0;
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                break;
            }
            if let Some(v) = h.to_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; len];
        let _ = r.read_exact(&mut body);
        let reply = if first.starts_with("GET /health") {
            json!({"status": "ok"})
        } else {
            let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            // agentd's warm-up (one token, at start) is not a request the tests look at
            if req["max_tokens"] != 1 {
                std::fs::write(format!("{model}.last.json"), &body).ok();
            }
            let content = answer(&mode, &model, &req);
            json!({"choices": [{"message": {"role": "assistant", "content": content}}]})
        };
        let b = reply.to_string();
        let mut w = conn;
        let _ = write!(
            w,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
            b.len()
        );
    }
}

const COUNT_BROKEN: &str = "fn run(args: &Args) -> Result<String, String> {\n    let n = args.get(\"all_messages\")?.lines().filter(|l| l.starts_with(\"from Mum \")).count();\n    Ok(n)\n}";
const COUNT_FIXED: &str = "fn run(args: &Args) -> Result<String, String> {\n    let n = args.get(\"all_messages\")?.lines().filter(|l| l.starts_with(\"from Mum \")).count();\n    Ok(format!(\"Mum sent {} texts\", n))\n}";

const COUNT_ZERO: &str = "fn run(args: &Args) -> Result<String, String> {\n    let n = args.get(\"all_messages\")?.lines().filter(|l| l.starts_with(\"from mum \")).count();\n    Ok(format!(\"Mum sent {} texts\", n))\n}";

fn answer(mode: &str, model: &str, req: &Value) -> String {
    if mode == "analyst-zero" && req.get("json_schema").is_some() {
        // matches lowercase "mum" (finds nothing) until told its code matched nothing
        let told = req["messages"].as_array().is_some_and(|m| {
            m.iter().any(|m| m["content"].as_str().unwrap_or("").contains("matches nothing"))
        });
        return json!({
            "sources": [{"tool": "all_messages", "args": {}}],
            "code": if told { COUNT_FIXED } else { COUNT_ZERO }
        })
        .to_string();
    }
    if mode == "analyst" && req.get("json_schema").is_some() {
        let shown_errors = req["messages"].as_array().is_some_and(|m| {
            m.iter()
                .any(|m| m["content"].as_str().unwrap_or("").contains("did not work"))
        });
        return json!({
            "sources": [{"tool": "all_messages", "args": {}}],
            "code": if shown_errors { COUNT_FIXED } else { COUNT_BROKEN }
        })
        .to_string();
    }
    if mode == "coder" && req.get("json_schema").is_some() {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("{model}.coder-requests"))
            .unwrap();
        writeln!(f, "request").unwrap();
        let shown_errors = req["messages"].as_array().is_some_and(|m| {
            m.iter()
                .any(|m| m["content"].as_str().unwrap_or("").contains("did not work"))
        });
        return json!({
            "name": "split_bill",
            "description": "share of a bill",
            "args": ["total", "people"],
            "code": if shown_errors { FIXED } else { BROKEN },
            "examples": [
                {"args": {"total": "900", "people": "3"}, "expect": "300.00"},
                {"args": {"total": "100", "people": "4"}, "expect": "25.00"},
                {"args": {"total": "10", "people": "3"}, "expect": "3.33"}
            ]
        })
        .to_string();
    }
    if mode == "coder" {
        return json!({"tool": "list_functions", "args": {}}).to_string();
    }
    json!({"tool": "read_calendar", "args": {"day": "friday"}}).to_string()
}
