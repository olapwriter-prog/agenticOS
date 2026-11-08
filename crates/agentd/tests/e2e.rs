//! The whole system, over real sockets, with a real vault, ledger, stores and gateway.
use agentd::config::Config;
use agentd::core::{BootError, Core};
use agentd::server::{ctl_send, start, Running};
use agentd::{anchors, secrets};
use agentos_hal::{FakeClock, ScaledClock};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;

fn dir(tag: &str) -> PathBuf {
    // Unix socket paths are limited to about 100 bytes, and macOS's temp_dir is long.
    let d = PathBuf::from("/tmp").join(format!("agentd-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}
fn cfg(d: &std::path::Path) -> Config {
    Config {
        data_dir: d.to_path_buf(),
        listen: "127.0.0.1:0".into(),
        kdf_mib: 1,
        idle_secs: 3600,
        ..Config::default()
    }
}
fn boot(tag: &str) -> (PathBuf, Running, String) {
    let d = dir(tag);
    let c = cfg(&d);
    let s = secrets::open_or_create(&d, "pw", 1).unwrap();
    let token = s.gateway_token.clone();
    let r = start(c, s, Arc::new(ScaledClock::new(1))).unwrap();
    (d, r, token)
}
fn post(addr: &str, token: Option<&str>, body: &Value) -> (u16, Value) {
    let b = body.to_string();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let mut s = TcpStream::connect(addr).unwrap();
    write!(
        s,
        "POST /mcp HTTP/1.1\r\nHost: x\r\n{auth}Content-Length: {}\r\n\r\n{b}",
        b.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let code: u16 = out.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = out.split("\r\n\r\n").nth(1).unwrap_or("");
    (code, serde_json::from_str(body).unwrap_or(Value::Null))
}
fn call(addr: &str, tok: &str, name: &str, args: Value) -> Value {
    let (code, v) = post(
        addr,
        Some(tok),
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}),
    );
    assert_eq!(code, 200);
    v
}
fn text(v: &Value) -> String {
    v["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[test]
fn the_door_needs_the_token() {
    let (d, r, tok) = boot("door");
    let ping = json!({"jsonrpc":"2.0","id":1,"method":"ping"});
    assert_eq!(post(&r.http_addr, None, &ping).0, 401);
    assert_eq!(post(&r.http_addr, Some("wrong"), &ping).0, 401);
    assert_eq!(post(&r.http_addr, Some(&tok), &ping).0, 200);
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn claude_lists_tools_and_runs_a_medium_one() {
    let (d, r, tok) = boot("tools");
    let (_, v) = post(
        &r.http_addr,
        Some(&tok),
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    );
    let names: Vec<&str> = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for n in ["read_calendar", "add_event", "set_reminder", "send_message"] {
        assert!(names.contains(&n), "{n} missing in {names:?}");
    }
    let v = call(
        &r.http_addr,
        &tok,
        "set_reminder",
        json!({"text":"call mum","when":"18:00"}),
    );
    assert_eq!(v["result"]["isError"], false);
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_high_action_needs_a_human_key_press_on_the_control_channel() {
    let (d, r, tok) = boot("high");
    let args = json!({"to":"Asha","text":"running late"});
    // beat 1: asked, nothing sent
    let v = call(&r.http_addr, &tok, "send_message", args.clone());
    assert_eq!(v["result"]["isError"], true);
    assert!(text(&v).contains("approval required"));
    // calling again without a key press still sends nothing
    assert_eq!(
        call(&r.http_addr, &tok, "send_message", args.clone())["result"]["isError"],
        true
    );
    let pending = ctl_send(&r.ctl_path, "pending").unwrap();
    assert!(
        pending.contains("send_message") && pending.contains("Asha"),
        "{pending}"
    );
    assert!(ctl_send(&r.ctl_path, "status")
        .unwrap()
        .contains("1 approval waiting"));
    // beat 2: the human presses the key
    assert!(ctl_send(&r.ctl_path, "press 1")
        .unwrap()
        .contains("approved"));
    // beat 3: the same call now runs, exactly once
    let v = call(&r.http_addr, &tok, "send_message", args.clone());
    assert_eq!(v["result"]["isError"], false, "{v}");
    assert!(text(&v).contains("sent to Asha"));
    assert_eq!(
        call(&r.http_addr, &tok, "send_message", args)["result"]["isError"],
        true,
        "one approval, one send"
    );
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn an_approval_for_one_message_does_not_cover_another() {
    let (d, r, tok) = boot("exact");
    call(
        &r.http_addr,
        &tok,
        "send_message",
        json!({"to":"Asha","text":"hi"}),
    );
    ctl_send(&r.ctl_path, "press 1").unwrap();
    let v = call(
        &r.http_addr,
        &tok,
        "send_message",
        json!({"to":"Asha","text":"send me 50000"}),
    );
    assert_eq!(v["result"]["isError"], true);
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn chat_uses_intents_with_no_model_and_still_respects_tiers() {
    let (d, r, _tok) = boot("chat");
    let a = ctl_send(&r.ctl_path, "chat Remind me to call mum at 6pm").unwrap();
    assert!(a.starts_with("done:"), "{a}");
    let a = ctl_send(&r.ctl_path, "chat message Asha running late").unwrap();
    assert!(a.contains("approval required"), "{a}");
    assert!(ctl_send(&r.ctl_path, "pending")
        .unwrap()
        .contains("send_message"));
    ctl_send(&r.ctl_path, "press 1").unwrap();
    let a = ctl_send(&r.ctl_path, "chat message Asha running late").unwrap();
    assert!(a.starts_with("done:") && a.contains("sent to Asha"), "{a}");
    let a = ctl_send(&r.ctl_path, "chat write me a sonnet about rain").unwrap();
    assert!(a.contains("could not"), "{a}");
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn chat_wakes_the_phone_and_the_ledger_records_it() {
    let (d, r, _t) = boot("stage");
    assert_eq!(ctl_send(&r.ctl_path, "stage").unwrap(), "Asleep");
    ctl_send(&r.ctl_path, "chat what's on friday").unwrap();
    assert_eq!(ctl_send(&r.ctl_path, "stage").unwrap(), "Active");
    let ledger = std::fs::read_to_string(d.join("ledger.log")).unwrap();
    assert!(
        ledger.contains("stage Asleep -> Active on ChatArrived"),
        "{ledger}"
    );
    assert!(ctl_send(&r.ctl_path, "verify")
        .unwrap()
        .contains("ledger ok"));
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn plugging_in_enters_deep_and_chat_still_wins() {
    let (d, r, _t) = boot("deep");
    ctl_send(&r.ctl_path, "power 80 charger").unwrap();
    assert_eq!(ctl_send(&r.ctl_path, "stage").unwrap(), "Deep");
    assert!(ctl_send(&r.ctl_path, "status")
        .unwrap()
        .contains("charging"));
    ctl_send(&r.ctl_path, "chat what's on friday").unwrap();
    assert_eq!(ctl_send(&r.ctl_path, "stage").unwrap(), "Active");
    // the simulated cgroup shows background work frozen while the human is here
    let f = std::fs::read_to_string(
        d.join("sim/cgroup/agentos.slice/agentos-agents.slice/cgroup.freeze"),
    )
    .unwrap();
    assert_eq!(f.trim(), "1");
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn secrets_are_not_on_disk_in_the_clear() {
    let (d, r, tok) = boot("clear");
    call(
        &r.http_addr,
        &tok,
        "set_reminder",
        json!({"text":"x","when":"1"}),
    );
    r.stop();
    fn walk(p: &std::path::Path, tok: &str, hits: &mut Vec<String>) {
        for e in std::fs::read_dir(p).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, tok, hits);
            } else if let Ok(b) = std::fs::read(&p) {
                if b.windows(tok.len()).any(|w| w == tok.as_bytes()) {
                    hits.push(p.display().to_string());
                }
            }
        }
    }
    let mut hits = vec![];
    walk(&d, &tok, &mut hits);
    assert!(hits.is_empty(), "token found in {hits:?}");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_rewritten_ledger_stops_the_next_boot() {
    let d = dir("tamper");
    let s = || secrets::open_or_create(&d, "pw", 1).unwrap();
    {
        let mut core = Core::new(cfg(&d), s(), Arc::new(FakeClock::new())).unwrap();
        core.chat("what's on friday"); // writes ledger entries
        core.ctl("anchor");
    }
    // an attacker deletes the ledger and writes a tidy replacement that verifies on its own
    std::fs::remove_file(d.join("ledger.log")).unwrap();
    {
        let mut l = agentos_ledger::Ledger::open(&d.join("ledger.log")).unwrap();
        for i in 0..3 {
            l.append(&format!("nothing to see {i}")).unwrap();
        }
    }
    match Core::new(cfg(&d), s(), Arc::new(FakeClock::new())) {
        Err(BootError::LedgerAnchor(_)) => {}
        other => panic!("expected a refusal, got {:?}", other.err()),
    }
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn an_honest_restart_boots_fine() {
    let d = dir("restart");
    let s = || secrets::open_or_create(&d, "pw", 1).unwrap();
    {
        let mut core = Core::new(cfg(&d), s(), Arc::new(FakeClock::new())).unwrap();
        core.chat("what's on friday");
    }
    assert!(Core::new(cfg(&d), s(), Arc::new(FakeClock::new())).is_ok());
    assert!(anchors::load(&d).is_some());
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn quiet_hours_hold_ordinary_notices_but_not_urgent_ones() {
    let d = dir("quiet");
    let s = secrets::open_or_create(&d, "pw", 1).unwrap();
    let mut core = Core::new(cfg(&d), s, Arc::new(FakeClock::new())).unwrap();
    core.fixed_minute_of_day = Some(23 * 60); // 11pm
    assert!(core
        .ctl("notify mail|boss|normal|lunch?")
        .starts_with("held"));
    assert_eq!(
        core.ctl("notify bank|bank|urgent|fraud alert"),
        "deliver now"
    );
    assert!(core.ctl("digest").contains("lunch?"));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn idle_puts_an_active_phone_back_to_sleep() {
    let d = dir("idle");
    let s = secrets::open_or_create(&d, "pw", 1).unwrap();
    let clock = FakeClock::new();
    let mut c = cfg(&d);
    c.idle_secs = 60;
    let mut core = Core::new(c, s, Arc::new(clock.clone())).unwrap();
    core.chat("what's on friday");
    assert_eq!(core.ctl("stage"), "Active");
    clock.advance_ms(59_000);
    core.tick();
    assert_eq!(core.ctl("stage"), "Active");
    clock.advance_ms(2_000);
    core.tick();
    assert_eq!(core.ctl("stage"), "Asleep");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_phone_works_headless_and_calls_need_the_key() {
    let (d, r, tok) = boot("phone");
    let c = |line: &str| ctl_send(&r.ctl_path, line).unwrap();

    // contacts are Medium: saved straight away, and kept on disk
    let a = c("chat save Mum as +91 98765 43210");
    assert!(a.starts_with("done:") && a.contains("+919876543210"), "{a}");
    assert!(std::fs::read_to_string(d.join("contacts.json"))
        .unwrap()
        .contains("Mum"));

    // calling is High: refused until the human presses the key, then runs once
    let a = c("chat call mum");
    assert!(a.contains("approval required"), "{a}");
    assert!(c("pending").contains("call to=mum"));
    c("press 1");
    let a = c("chat call mum");
    assert!(a.contains("calling Mum (+919876543210)"), "{a}");
    assert!(
        c("chat call mum").contains("approval required"),
        "one press, one call"
    );
    assert!(c("chat hang up").contains("ended call with Mum"));

    // the network rings us: logged, alerted, answerable by voice
    let a = c("sim-call +919876543210");
    assert!(
        a.contains("incoming call from Mum") && a.contains("alerted"),
        "{a}"
    );
    assert!(c("chat answer").contains("answered Mum"));
    c("chat hang up");
    assert!(
        !c("sim-sms +919876543210|x").contains("missed"),
        "an answered call is not missed"
    );

    // a text arrives; reading it is Low and needs no key
    assert!(c("sim-sms +919876543210|dinner at 8?").contains("dinner at 8?"));
    assert!(c("chat read my messages").contains("from Mum (+919876543210): dinner at 8?"));

    // switches
    assert!(c("chat flashlight on").contains("flashlight on"));
    assert!(c("chat airplane mode on").contains("airplane mode on"));
    assert!(c("phone").contains("airplane mode"));
    c("chat airplane mode off");
    assert!(c("chat check balance *123#").contains("balance"));

    // a Claude client sees the same tools over MCP, with the same tiers
    let v = call(
        &r.http_addr,
        &tok,
        "send_sms",
        json!({"to":"Mum","text":"on my way"}),
    );
    assert_eq!(v["result"]["isError"], true);
    let v = call(&r.http_addr, &tok, "phone_status", json!({}));
    assert!(
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("SimNet"),
        "{v}"
    );

    let ledger = std::fs::read_to_string(d.join("ledger.log")).unwrap();
    assert!(ledger.contains("phone: incoming call") && ledger.contains("phone: sms received"));
    assert!(c("verify").contains("ledger ok"));
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

/// A stand-in for llama-server: answers /health, and turns every chat request into
/// read_calendar(friday). It saves the last request so the test can see the system prompt.
fn fake_llama(dir: &std::path::Path) -> PathBuf {
    let p = dir.join("llama-server");
    let script = r#"#!/bin/sh
while [ $# -gt 0 ]; do [ "$1" = "--port" ] && PORT=$2; shift; done
exec python3 - "$PORT" "$(dirname "$0")/last-request.json" <<'PY'
import sys, json
from http.server import BaseHTTPRequestHandler, HTTPServer
class H(BaseHTTPRequestHandler):
    def do_GET(s):
        s.send_response(200); s.send_header("Content-Length", "15"); s.end_headers()
        s.wfile.write(b'{"status":"ok"}')
    def do_POST(s):
        req = s.rfile.read(int(s.headers.get("Content-Length", 0)))
        open(sys.argv[2], "wb").write(req)
        act = json.dumps({"tool": "read_calendar", "args": {"day": "friday"}})
        body = json.dumps({"choices": [{"message": {"role": "assistant", "content": act}}]}).encode()
        s.send_response(200); s.send_header("Content-Type", "application/json")
        s.send_header("Content-Length", str(len(body))); s.end_headers(); s.wfile.write(body)
    def log_message(s, *a): pass
HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
PY
"#;
    std::fs::write(&p, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[test]
fn the_local_model_is_supervised_prioritised_and_answers_what_intents_cannot() {
    if std::process::Command::new("python3")
        .arg("-V")
        .output()
        .is_err()
    {
        eprintln!("skipped: needs python3 for the fake llama-server");
        return;
    }
    let d = dir("llm");
    std::fs::create_dir_all(d.join("models")).unwrap();
    std::fs::write(d.join("models/small.gguf"), vec![0u8; 4096]).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut c = cfg(&d);
    c.llama_server = fake_llama(&d);
    c.models = vec![toml::from_str(&format!(
        "name = \"small\"\nrole = \"small\"\nfile = \"small.gguf\"\nport = {port}\nctx = 512"
    ))
    .unwrap()];
    let s = secrets::open_or_create(&d, "pw", 1).unwrap();
    let r = start(c, s, Arc::new(ScaledClock::new(1))).unwrap();
    let ctl = |l: &str| ctl_send(&r.ctl_path, l).unwrap();

    // kept loaded while asleep, so the first question is fast
    assert!(
        ctl("models").contains("small (small, port"),
        "{}",
        ctl("models")
    );

    // no intent matches this; the model answers, and the action runs through the executor
    let a = ctl("chat is anything happening at the end of the week");
    assert!(a.starts_with("done:"), "{a}");
    let req = std::fs::read_to_string(d.join("last-request.json")).unwrap();
    assert!(
        req.contains("You are the agent that runs this phone"),
        "{req}"
    );
    assert!(
        req.contains("call(to) [approval]") && req.contains("grammar"),
        "{req}"
    );
    assert!(ctl("models").contains("ready"));

    // the server lives in the model's cgroup, with the model's cores; back to idle afterwards
    let procs =
        std::fs::read_to_string(d.join("sim/cgroup/agentos.slice/agentos-llm.slice/cgroup.procs"))
            .unwrap();
    assert!(!procs.trim().is_empty());
    let p = ctl("prio");
    assert!(
        p.starts_with("mode Idle") && p.contains("model cores"),
        "{p}"
    );
    let min =
        std::fs::read_to_string(d.join("sim/cgroup/agentos.slice/agentos-llm.slice/memory.min"))
            .unwrap();
    assert_eq!(
        min.trim(),
        "4096",
        "the loaded weights are protected from reclaim"
    );
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}
