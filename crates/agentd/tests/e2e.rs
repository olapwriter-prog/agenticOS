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
    let p = ctl_send(&r.ctl_path, "prio").unwrap();
    assert!(
        p.starts_with("mode Training"),
        "the boost follows the stage at once: {p}"
    );
    let w =
        std::fs::read_to_string(d.join("sim/cgroup/agentos.slice/agentos-llm.slice/cpu.weight"))
            .unwrap();
    assert_eq!(w.trim(), "10000");
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

    // one JSON snapshot for screens (the Rune UI)
    ctl_send(&r.ctl_path, "chat call mum").unwrap();
    let st: Value = serde_json::from_str(&c("state")).unwrap();
    assert_eq!(st["pending"][0]["tool"], "call", "{st}");
    assert_eq!(st["pending"][0]["args"]["to"], "mum");
    assert!(st["phone"].as_str().unwrap().contains("SimNet"));
    assert!(st["recent"].as_array().unwrap().len() > 3);
    let ledger = std::fs::read_to_string(d.join("ledger.log")).unwrap();
    assert!(ledger.contains("phone: incoming call") && ledger.contains("phone: sms received"));
    assert!(c("verify").contains("ledger ok"));
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

/// The fake llama-server (tests/support/fake_llama.rs), set up to behave as `mode`.
/// Returns the config's models entry and the path of the model file it reads.
fn fake_model(d: &std::path::Path, mode: &str) -> (Vec<agentos_models::ModelSpec>, PathBuf) {
    std::fs::create_dir_all(d.join("models")).unwrap();
    let model = d.join("models/small.gguf");
    std::fs::write(&model, mode).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let spec = toml::from_str(&format!(
        "name = \"small\"\nrole = \"small\"\nfile = \"small.gguf\"\nport = {port}\nctx = 512"
    ))
    .unwrap();
    (vec![spec], model)
}
fn fake_llama_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agentd-fake-llama"))
}

#[test]
fn the_local_model_is_supervised_prioritised_and_answers_what_intents_cannot() {
    let d = dir("llm");
    let (models, model) = fake_model(&d, "tools");
    let mut c = cfg(&d);
    c.llama_server = fake_llama_bin();
    c.models = models;
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
    let a = ctl("chat zorble the quantum flux");
    assert!(a.starts_with("done:"), "{a}");
    let req = std::fs::read_to_string(format!("{}.last.json", model.display())).unwrap();
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
        "5",
        "the loaded weights are protected from reclaim"
    );
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_agent_writes_compiles_repairs_and_runs_its_own_rust_functions() {
    let d = dir("forge");
    let (models, model) = fake_model(&d, "coder");
    let mut c = cfg(&d);
    c.llama_server = fake_llama_bin();
    c.models = models;
    let s = secrets::open_or_create(&d, "pw", 1).unwrap();
    let tok = s.gateway_token.clone();
    let r = start(c, s, Arc::new(ScaledClock::new(1))).unwrap();
    let ctl = |l: &str| ctl_send(&r.ctl_path, l).unwrap();
    assert!(
        ctl("functions").contains("compiler: rustc"),
        "{}",
        ctl("functions")
    );

    // written; rustc refuses the first try; the model sees the error and fixes it
    let a = ctl("chat make a function that splits a bill between people");
    assert!(
        a.starts_with("done (attempt 2)") && a.contains("split_bill v1 passed all 3"),
        "{a}"
    );
    let asked = std::fs::read_to_string(format!("{}.coder-requests", model.display())).unwrap();
    assert_eq!(asked.lines().count(), 2);
    let last = std::fs::read_to_string(format!("{}.last.json", model.display())).unwrap();
    // the model was shown the compiler's verdict on its line 2 (rustc 1.88 crashes on it;
    // newer compilers report the type error), with the line quoted
    assert!(
        last.contains("does not compile") && last.contains("line 2:"),
        "{last}"
    );
    assert!(!last.contains("HirId"), "no compiler internals: {last}");
    assert!(ctl("show split_bill").contains("args.num(\"total\")?"));
    assert!(ctl("chat run split_bill with total=900; people=3").contains("not installed"));

    // installing needs the key, for that exact version
    assert!(ctl("chat install function split_bill version 1").contains("approval required"));
    assert!(ctl("pending").contains("install_function name=split_bill version=1"));
    ctl("press 1");
    assert!(ctl("chat install function split_bill version 1").contains("split_bill v1 installed"));
    assert_eq!(
        ctl("chat run split_bill with total=1250; people=4"),
        "done: 312.50"
    );

    // a Claude client writes one that asks the phone to text: the text still needs the key
    let code = r#"fn run(args: &Args) -> Result<String, String> {
    let who = args.get("who")?;
    phone("send_sms", &[("to", who), ("text", &format!("Running {} min late", args.get("mins")?))])?;
    Ok(format!("told {who}"))
}"#;
    let examples = json!([
        {"args": {"who": "Mum", "mins": "10"}, "expect": "told Mum"},
        {"args": {"who": "Ravi", "mins": "5"}, "expect": "told Ravi"},
        {"args": {"who": "Asha", "mins": "1"}, "expect": "told Asha"}
    ])
    .to_string();
    let v = call(
        &r.http_addr,
        &tok,
        "write_function",
        json!({"name": "tell_late", "description": "say I am late",
        "args": "who,mins", "code": code, "examples": examples}),
    );
    assert!(
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("tell_late v1 passed"),
        "{v}"
    );
    // and one that tries to read a file while compiling is refused
    let v = call(
        &r.http_addr,
        &tok,
        "write_function",
        json!({"name": "peek", "description": "x", "args": "", "examples": "[]",
        "code": "fn run(a: &Args) -> Result<String, String> { Ok(include_str!(\"/etc/hosts\").into()) }"}),
    );
    assert_eq!(v["result"]["isError"], true);
    let install = json!({"name": "tell_late", "version": "1"});
    assert_eq!(
        call(&r.http_addr, &tok, "install_function", install.clone())["result"]["isError"],
        true
    );
    ctl("press 2");
    assert_eq!(
        call(&r.http_addr, &tok, "install_function", install)["result"]["isError"],
        false
    );
    ctl("chat save Mum as +919876543210");
    let a = ctl("chat run tell_late with who=Mum; mins=10");
    assert!(
        a.starts_with("done: told Mum") && a.contains("send_sms: not done: approval required"),
        "{a}"
    );

    assert!(ctl("functions").contains("split_bill(total, people) v1"));
    let ledger = std::fs::read_to_string(d.join("ledger.log")).unwrap();
    assert!(ledger.contains("function written: split_bill"));
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn it_learns_from_corrections_and_approvals() {
    let d = dir("learn");
    let (models, model) = fake_model(&d, "tools"); // answers everything with read_calendar(friday)
    let mut c = cfg(&d);
    c.llama_server = fake_llama_bin();
    c.models = models;
    let s = secrets::open_or_create(&d, "pw", 1).unwrap();
    let r = start(c, s, Arc::new(ScaledClock::new(1))).unwrap();
    let ctl = |l: &str| ctl_send(&r.ctl_path, l).unwrap();

    // the model guesses friday; the human says it was wrong and does it the right way
    assert!(ctl("chat zorble the weekend").starts_with("done:"));
    assert!(ctl("chat that was wrong").contains("was wrong"));
    assert!(ctl("chat what's on saturday").starts_with("done:"));

    // the same request now goes straight to the corrected action, without the model
    let before = std::fs::metadata(format!("{}.last.json", model.display()))
        .unwrap()
        .modified()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(ctl("chat Zorble the weekend!").starts_with("done:"));
    let after = std::fs::metadata(format!("{}.last.json", model.display()))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(before, after, "the model was not asked");

    // a similar request: the classifier learned the correction, so it needs no model at all
    assert!(ctl("chat zorble the weekend kids").starts_with("done:"));
    let after2 = std::fs::metadata(format!("{}.last.json", model.display()))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        before, after2,
        "learned from the correction: the model was not asked"
    );
    let log = std::fs::read_to_string(d.join("experience.jsonl")).unwrap();
    let last = log
        .lines()
        .filter(|l| l.contains(r#""e":"ep""#))
        .last()
        .unwrap();
    assert!(
        last.contains("classifier") && last.contains("read_calendar"),
        "{last}"
    );

    // an approved call is rewarded; a declined one is not repeated as a shortcut
    assert!(ctl("chat call mum").contains("approval required"));
    ctl("press 1");
    let stats = ctl("learn");
    assert!(
        stats.contains("1 corrections") && stats.contains("shortcut 1/1"),
        "{stats}"
    );
    assert!(stats.contains("1 DPO pairs"), "{stats}");
    let dpo = std::fs::read_to_string(d.join("learn/dpo.jsonl")).unwrap();
    assert!(dpo.contains("saturday") && dpo.contains("friday"), "{dpo}");

    // the eval gate: the fake model still says friday, so the right tool with the wrong day
    let e = ctl("learn eval");
    assert!(e.contains("scores 50%"), "{e}");
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn questions_are_answered_by_code_in_the_sandbox_and_the_model_never_sees_the_data() {
    let d = dir("analyst");
    let (models, model) = fake_model(&d, "analyst");
    let mut c = cfg(&d);
    c.llama_server = fake_llama_bin();
    c.models = models;
    let s = secrets::open_or_create(&d, "pw", 1).unwrap();
    let tok = s.gateway_token.clone();
    let r = start(c, s, Arc::new(ScaledClock::new(1))).unwrap();
    let ctl = |l: &str| ctl_send(&r.ctl_path, l).unwrap();
    ctl("chat save Mum as +919876543210");
    for text in ["dinner at 8", "bring milk", "call me back"] {
        ctl(&format!("sim-sms +919876543210|{text}"));
    }
    for i in 0..20 {
        ctl(&format!("sim-sms +91800000{i:04}|offer number {i}"));
    }

    // the model writes code (fixing a compile error on the way); the sandbox counts
    // regular questions are answered by code, without the model
    let a = ctl("chat how many texts did Mum send me");
    assert_eq!(a, "answer: Mum sent you 3 texts.", "{a}");
    assert!(!std::path::Path::new(&format!("{}.coder-requests", model.display())).exists());
    // one the classifier has no evidence for goes to the model, which writes the code
    let a = ctl("chat analyse zorbly the texts");
    assert_eq!(a, "answer (attempt 2): Mum sent 3 texts", "{a}");
    let sent = std::fs::read_to_string(format!("{}.last.json", model.display())).unwrap();
    assert!(
        sent.contains("from NAME (NUMBER): TEXT"),
        "the model saw the format"
    );
    for private in ["dinner at 8", "bring milk", "offer number", "+919876543210"] {
        assert!(
            !sent.contains(private),
            "the model must not see {private:?}"
        );
    }
    assert!(std::fs::read_to_string(d.join("ledger.log"))
        .unwrap()
        .contains("analysis over all_messages"));

    // a Claude client does the same over MCP and gets only the answer
    let (_, list) = post(
        &r.http_addr,
        Some(&tok),
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    );
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"analyze_data") && names.contains(&"data_formats"),
        "{names:?}"
    );
    let code = r#"fn run(args: &Args) -> Result<String, String> {
    let offers = args.get("all_messages")?.lines().filter(|l| l.contains("offer")).count();
    Ok(format!("{offers} offers"))
}"#;
    let v = call(
        &r.http_addr,
        &tok,
        "analyze_data",
        json!({"sources": "all_messages", "code": code}),
    );
    assert_eq!(v["result"]["content"][0]["text"], "20 offers", "{v}");
    // loading anything that is not read-only is refused, as is code that tries to act
    let v = call(
        &r.http_addr,
        &tok,
        "analyze_data",
        json!({"sources": "send_sms", "code": code}),
    );
    assert_eq!(v["result"]["isError"], true);
    let acts = r#"fn run(args: &Args) -> Result<String, String> { phone("call", &[("to", "Mum")])?; Ok(String::new()) }"#;
    let v = call(
        &r.http_addr,
        &tok,
        "analyze_data",
        json!({"sources": "call_log", "code": acts}),
    );
    assert!(
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cannot ask the phone"),
        "{v}"
    );
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn an_answer_of_none_over_real_data_gets_a_second_look() {
    let d = dir("zero");
    let (models, model) = fake_model(&d, "analyst-zero");
    let mut c = cfg(&d);
    c.llama_server = fake_llama_bin();
    c.models = models;
    let s = secrets::open_or_create(&d, "pw", 1).unwrap();
    let r = start(c, s, Arc::new(ScaledClock::new(1))).unwrap();
    let ctl = |l: &str| ctl_send(&r.ctl_path, l).unwrap();
    ctl("chat save Mum as +919876543210");
    for text in ["dinner at 8", "bring milk"] {
        ctl(&format!("sim-sms +919876543210|{text}"));
    }
    let a = ctl("chat analyse zorbly the texts");
    assert_eq!(
        a, "answer (attempt 1): Mum sent 2 texts",
        "the recheck is not an attempt: {a}"
    );
    let sent = std::fs::read_to_string(format!("{}.last.json", model.display())).unwrap();
    assert!(
        sent.contains("has 2 lines") && sent.contains("Mum sent 0 texts"),
        "{sent}"
    );
    assert!(
        !sent.contains("dinner at 8"),
        "still no data shown to the model"
    );
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn regular_requests_are_handled_by_code_and_the_model_is_not_asked() {
    let d = dir("nollm");
    let (models, model) = fake_model(&d, "tools");
    let mut c = cfg(&d);
    c.llama_server = fake_llama_bin();
    c.models = models;
    let s = secrets::open_or_create(&d, "pw", 1).unwrap();
    let r = start(c, s, Arc::new(ScaledClock::new(1))).unwrap();
    let ctl = |l: &str| ctl_send(&r.ctl_path, l).unwrap();
    ctl("chat save Mum as +919876543210");
    ctl("chat save Ravi as +919812345678");
    ctl("sim-sms +919876543210|dinner at 8?");
    ctl("sim-sms +918000000001|big sale");
    ctl("sim-call +919812345678");
    ctl("chat hang up");
    std::thread::sleep(std::time::Duration::from_millis(300)); // let the warm-up request land
    let last = format!("{}.last.json", model.display());
    let before = std::fs::metadata(&last)
        .ok()
        .and_then(|m| m.modified().ok());
    // commands: classifier + rules
    assert!(ctl("chat could you ring my mother").contains("approval required"));
    assert!(ctl("pending").contains("call to=Mum"));
    assert_eq!(ctl("chat switch the torch on"), "done: flashlight on");
    assert_eq!(ctl("chat make it louder, 80"), "done: volume 80%");
    // questions: classifier + exact code over every record
    assert_eq!(
        ctl("chat did Ravi call me"),
        "answer: Yes, Ravi called you (1 time)."
    );
    assert_eq!(
        ctl("chat how many texts came from numbers not in my contacts"),
        "answer: 1 text came from numbers not in your contacts."
    );
    assert_eq!(
        ctl("chat who texted me the most"),
        "answer: Mum and +918000000001 texted you the most (1 text)."
    );
    let after = std::fs::metadata(&last)
        .ok()
        .and_then(|m| m.modified().ok());
    assert_eq!(before, after, "the model was never asked");
    assert!(ctl("learn").contains("classifier"));
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn on_the_charger_the_model_learns_the_confirmed_requests_and_can_be_rolled_back() {
    let d = dir("train");
    let (models, model) = fake_model(&d, "tools");
    // the server must be agentos-llm: only it reads our adapters
    let bin = d.join("agentos-llm");
    std::fs::copy(fake_llama_bin(), &bin).unwrap();
    let mut c = cfg(&d);
    c.llama_server = bin;
    c.models = models;
    c.train_min_new = 1;
    c.idle_secs = 1; // the human goes quiet: Active -> Asleep -> Deep on the charger
    let s = secrets::open_or_create(&d, "pw", 1).unwrap();
    let r = start(c, s, Arc::new(ScaledClock::new(1))).unwrap();
    let ctl = |l: &str| ctl_send(&r.ctl_path, l).unwrap();
    let args = || std::fs::read_to_string(format!("{}.args", model.display())).unwrap_or_default();

    assert!(ctl("chat save Mum as +919876543210").starts_with("done"));
    assert!(
        ctl("learn status").contains("not on the charger"),
        "{}",
        ctl("learn status")
    );
    ctl("power 90 charger");
    let ledger = || std::fs::read_to_string(d.join("ledger.log")).unwrap_or_default();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !ledger().contains("training kept") && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let l = ledger();
    assert!(l.contains("training small on 1 confirmed requests"), "{l}");
    assert!(
        l.contains("training kept") && l.contains("50% -> 80%"),
        "{l}"
    );
    assert!(d.join("learn/adapter.lora").exists());
    assert!(d.join("learn/sft.jsonl").exists() && d.join("learn/grammar.gbnf").exists());
    // the model server comes back with the adapter
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !args().contains("--lora") && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(
        args().contains("--lora") && args().contains("adapter.lora"),
        "{}",
        args()
    );
    assert!(ctl("learn status").contains("adapter in use:"));
    // nothing new confirmed since: no second run
    assert!(
        ctl("learn status").contains("0 new confirmed requests"),
        "{}",
        ctl("learn status")
    );

    // and back
    assert!(ctl("learn rollback").contains("no adapter"));
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while args().contains("--lora") && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(!args().contains("--lora"), "{}", args());
    assert!(ledger().contains("adapter rolled back"));
    assert!(ctl("verify").contains("ledger ok"));
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

/// One `struct input_event`, as the kernel writes it.
fn key_event(ms: u64, code: u16, value: i32) -> Vec<u8> {
    let mut e: libc::input_event = unsafe { std::mem::zeroed() };
    e.time.tv_sec = (ms / 1000) as _;
    e.time.tv_usec = ((ms % 1000) * 1000) as _;
    e.type_ = 1; // EV_KEY
    e.code = code;
    e.value = value;
    let p = &e as *const libc::input_event as *const u8;
    unsafe { std::slice::from_raw_parts(p, std::mem::size_of::<libc::input_event>()) }.to_vec()
}

#[test]
fn holding_the_hardware_key_approves_the_one_waiting_action() {
    let d = dir("hwkey");
    std::fs::create_dir_all(&d).unwrap();
    // a named pipe stands in for /dev/input/eventN
    let dev = d.join("event0");
    let cpath = std::ffi::CString::new(dev.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    let mut c = cfg(&d);
    c.approval_key = Some(dev.clone());
    let s = secrets::open_or_create(&d, "pw", 1).unwrap();
    let tok = s.gateway_token.clone();
    let r = start(c, s, Arc::new(ScaledClock::new(1))).unwrap();
    let mut key = std::fs::OpenOptions::new().write(true).open(&dev).unwrap();
    let mut t = 100_000u64;
    let mut hold = |ms: u64| {
        key.write_all(&key_event(t, 114, 1)).unwrap();
        key.write_all(&key_event(t + ms, 114, 0)).unwrap();
        key.flush().unwrap();
        t += ms + 1000;
    };
    let ledger_has = |what: &str| {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < until {
            let l = std::fs::read_to_string(d.join("ledger.log")).unwrap_or_default();
            if l.contains(what) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    };
    let asha = json!({"to":"Asha","text":"running late"});
    let ravi = json!({"to":"Ravi","text":"on my way"});
    // two waiting: a hold approves neither
    call(&r.http_addr, &tok, "send_message", asha.clone());
    call(&r.http_addr, &tok, "send_message", ravi.clone());
    hold(2500);
    assert!(ledger_has("2 actions waiting; none approved"));
    assert_eq!(
        call(&r.http_addr, &tok, "send_message", asha.clone())["result"]["isError"],
        true
    );
    ctl_send(&r.ctl_path, "decline 2").unwrap();
    // one waiting: a short press is the volume's; a two-second hold approves it
    hold(300);
    hold(2100);
    assert!(
        ledger_has("with the approval key"),
        "{}",
        std::fs::read_to_string(d.join("ledger.log")).unwrap_or_default()
    );
    let v = call(&r.http_addr, &tok, "send_message", asha);
    assert_eq!(v["result"]["isError"], false, "{v}");
    assert!(text(&v).contains("sent to Asha"));
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn service_secrets_survive_a_restart_and_only_conn_names_are_accepted() {
    let (d, r, _tok) = boot("secrets-ctl");
    assert!(ctl_send(&r.ctl_path, "secret set conn.test abc123")
        .unwrap()
        .contains("stored conn.test"));
    assert!(ctl_send(&r.ctl_path, "secret set gate_secret x")
        .unwrap()
        .contains("refused"));
    assert!(ctl_send(&r.ctl_path, "secret set gateway_token x")
        .unwrap()
        .contains("refused"));
    r.stop();
    // the same data directory, a second boot: the secret is still there, and the value is not
    let c = cfg(&d);
    let s = secrets::open_or_create(&d, "pw", 1).unwrap();
    let r = start(c, s, Arc::new(ScaledClock::new(1))).unwrap();
    let list = ctl_send(&r.ctl_path, "secret list").unwrap();
    assert!(list.contains("conn.test"), "{list}");
    assert!(!list.contains("abc123"));
    let ledger = std::fs::read_to_string(d.join("ledger.log")).unwrap_or_default();
    assert!(ledger.contains("secret set: conn.test"));
    assert!(!ledger.contains("abc123"), "the value reached the ledger");
    assert!(ctl_send(&r.ctl_path, "secret rm conn.test")
        .unwrap()
        .contains("removed"));
    assert!(ctl_send(&r.ctl_path, "secret list")
        .unwrap()
        .contains("no service secrets"));
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}
