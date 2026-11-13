//! Remote MCP servers, end to end: a small fake server on a real TCP socket, offered to the
//! daemon as connectors. A server that is down must not stop the daemon from booting.
use agentd::config::{Config, McpServer};
use agentd::secrets;
use agentd::server::{ctl_send, start};
use agentos_hal::ScaledClock;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;

fn dir(tag: &str) -> PathBuf {
    let d = PathBuf::from("/tmp").join(format!("agentd-mcp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// A fake MCP server with one tool, `echo`, which repeats its `text` argument. When `token` is
/// set, a request without that bearer token is refused with 401.
fn fake_server(token: Option<&'static str>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut len = 0usize;
            let mut auth = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let l = line.trim_end();
                if l.is_empty() {
                    break;
                }
                let lower = l.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
                if lower.starts_with("authorization:") {
                    auth = l["authorization:".len()..].trim().to_string();
                }
            }
            let mut body = vec![0u8; len];
            let _ = reader.read_exact(&mut body);
            let mut out = stream;
            if let Some(t) = token {
                if auth != format!("Bearer {t}") {
                    let _ = out.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    continue;
                }
            }
            let req: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            let id = req.get("id").cloned();
            let reply = match req["method"].as_str().unwrap_or("") {
                "initialize" => {
                    serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"fake","version":"1"}}})
                }
                "tools/list" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{
                    "name":"echo","description":"repeats the text",
                    "inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}
                }]}}),
                "tools/call" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{
                    "content":[{"type":"text","text":format!("echo: {}", req["params"]["arguments"]["text"].as_str().unwrap_or(""))}],"isError":false}}),
                _ => serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
            };
            let text = if id.is_none() {
                String::new()
            } else {
                reply.to_string()
            };
            let _ = write!(
                out,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
        }
    });
    port
}

fn server(name: &str, port: u16, secret: Option<&str>) -> McpServer {
    McpServer {
        name: name.into(),
        url: format!("http://127.0.0.1:{port}/mcp"),
        secret: secret.map(str::to_string),
        tier: "high".into(),
        trust_hints: false,
        allow: vec![],
    }
}

fn boot(
    d: &std::path::Path,
    servers: Vec<McpServer>,
    token_for_demo: Option<&str>,
) -> agentd::server::Running {
    let c = Config {
        data_dir: d.to_path_buf(),
        listen: "127.0.0.1:0".into(),
        kdf_mib: 1,
        idle_secs: 3600,
        mcp_servers: servers,
        ..Config::default()
    };
    let mut s = secrets::open_or_create(d, "pw", 1).unwrap();
    if let Some(t) = token_for_demo {
        s.vault.put("conn.demo", t.as_bytes()).unwrap();
    }
    start(c, s, Arc::new(ScaledClock::new(1))).unwrap()
}

#[test]
fn a_remote_tool_asks_for_approval_at_the_default_tier() {
    let d = dir("echo");
    let port = fake_server(None);
    let r = boot(&d, vec![server("demo", port, None)], None);
    assert_eq!(
        ctl_send(&r.ctl_path, "connectors").unwrap(),
        "demo: connected, 1 tools"
    );
    let reply = ctl_send(&r.ctl_path, r#"do demo_echo {"text":"hi"}"#).unwrap();
    assert!(reply.contains("approval required"), "{reply}");
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_server_that_is_down_does_not_stop_the_daemon() {
    let d = dir("down");
    // a port nothing listens on
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let r = boot(&d, vec![server("demo", port, None)], None);
    assert!(
        ctl_send(&r.ctl_path, "status").is_ok(),
        "the daemon must still answer"
    );
    assert!(ctl_send(&r.ctl_path, "connectors")
        .unwrap()
        .starts_with("demo: unavailable"));
    let ledger = std::fs::read_to_string(d.join("ledger.log")).unwrap_or_default();
    assert!(ledger.contains("mcp demo unavailable"), "{ledger}");
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_bearer_token_comes_from_the_vault() {
    let d = dir("token");
    let port = fake_server(Some("sekret"));
    let r = boot(
        &d,
        vec![server("demo", port, Some("conn.demo"))],
        Some("sekret"),
    );
    assert_eq!(
        ctl_send(&r.ctl_path, "connectors").unwrap(),
        "demo: connected, 1 tools"
    );
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}
