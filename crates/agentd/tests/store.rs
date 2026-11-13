//! A phone that keeps a retail store's books (examples/retail), over the real sockets: the
//! tills deliver a day of bills, a Claude client reads the audit and proposes fixes over MCP,
//! and only the owner's key applies them or lodges a return.
use agentd::config::Config;
use agentd::secrets;
use agentd::server::{ctl_send, start, Running};
use agentos_hal::ScaledClock;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;

fn boot(tag: &str) -> (PathBuf, Running, String) {
    let d = PathBuf::from("/tmp").join(format!("agentd-store-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let c = Config {
        data_dir: d.clone(),
        listen: "127.0.0.1:0".into(),
        kdf_mib: 1,
        idle_secs: 3600,
        store_profile: Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/retail/profile.toml")),
        ..Config::default()
    };
    let s = secrets::open_or_create(&d, "pw", 1).unwrap();
    let token = s.gateway_token.clone();
    (d, start(c, s, Arc::new(ScaledClock::new(1))).unwrap(), token)
}
fn call(addr: &str, tok: &str, name: &str, args: Value) -> (bool, String) {
    let b = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}).to_string();
    let mut s = TcpStream::connect(addr).unwrap();
    write!(s, "POST /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {tok}\r\nContent-Length: {}\r\n\r\n{b}", b.len()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let v: Value = serde_json::from_str(out.split("\r\n\r\n").nth(1).unwrap_or("")).unwrap_or(Value::Null);
    (v["result"]["isError"] != json!(true), v["result"]["content"][0]["text"].as_str().unwrap_or("").to_string())
}
fn ctl(r: &Running, line: &str) -> String {
    ctl_send(&r.ctl_path, line).unwrap()
}
fn key(r: &Running, said: &str) {
    let id = said.split("id ").nth(1).and_then(|s| s.split(')').next()).unwrap_or_else(|| panic!("no approval asked: {said}"));
    assert!(ctl(r, &format!("press {id}")).contains("approved"));
}

#[test]
fn a_day_of_bills_is_audited_a_client_proposes_and_only_the_key_applies_or_files() {
    let (d, r, tok) = boot("day");
    // the tills' export for one June day, with faults in it, comes in as a file
    let file = d.join("tills-2025-06-03.jsonl");
    let made = ctl(&r, &format!("shop gen 2025-06-03 11 {}", file.display()));
    assert!(made.contains("bills for 2025-06-03"), "{made}");
    assert!(std::fs::read_to_string(&file).unwrap().contains("+91 90000"), "the tills' file carries loyalty numbers");
    let said = ctl(&r, &format!("shop import {}", file.display()));
    assert!(said.contains("2025-06-03:") && said.contains("exception(s)") && said.contains("slab_on_tag"), "{said}");
    std::fs::remove_file(&file).unwrap();
    assert!(ctl(&r, &format!("shop import {}", file.display())).starts_with("refused"));

    // the owner, in a fixed phrase
    let audit = ctl(&r, "chat audit 2025-06-03");
    assert!(audit.starts_with("done: Audit of 2025-06-03:") && audit.contains("open"), "{audit}");
    // a Claude client reads the same and sees invoice numbers, never a shopper
    for (tool, args) in [
        ("shop_audit", json!({"period": "2025-06"})),
        ("shop_exceptions", json!({"period": "2025-06", "kind": "slab_on_tag"})),
        ("shop_gst", json!({"period": "2025-06"})),
        ("shop_year", json!({})),
    ] {
        let (ok, out) = call(&r.http_addr, &tok, tool, args);
        assert!(ok && !out.contains("90000"), "{tool}: {out}");
    }
    let (_, list) = call(&r.http_addr, &tok, "shop_exceptions", json!({"period": "2025-06", "kind": "slab_on_tag"}));
    assert!(list.contains("VVF/25-26/") && list.contains("charged 12%, the rate is 5%"), "{list}");
    // it may propose a fix (nothing changes), not a fix that does not fit, and may not apply or file
    let (ok, out) = call(&r.http_addr, &tok, "shop_propose_fix", json!({"period": "2025-06", "kind": "slab_on_tag", "fix": "delete_bills"}));
    assert!(!ok && out.contains("is not a fix"), "{out}");
    let (_, before) = call(&r.http_addr, &tok, "shop_gst", json!({"period": "2025-06"}));
    let (ok, out) = call(&r.http_addr, &tok, "shop_propose_fix", json!({"period": "2025-06", "kind": "slab_on_tag", "fix": "credit_note"}));
    assert!(ok && out.contains("nothing changes until the owner approves"), "{out}");
    assert_eq!(call(&r.http_addr, &tok, "shop_gst", json!({"period": "2025-06"})).1, before);
    for (tool, args) in [("shop_apply_fixes", json!({"period": "2025-06"})), ("shop_file_gst", json!({"period": "2025-06"})), ("shop_file_income_tax", json!({}))] {
        let (ok, out) = call(&r.http_addr, &tok, tool, args.clone());
        assert!(!ok && out.contains("approval required"), "{tool}: {out}");
        let id = out.split("id ").nth(1).and_then(|s| s.split(')').next()).unwrap().to_string();
        assert!(ctl(&r, &format!("decline {id}")).contains("declined"));
    }
    // the owner applies with the key: the over-charged tax goes back by credit note
    let ask = ctl(&r, "chat apply fixes for 2025-06");
    key(&r, &ask);
    let done = ctl(&r, "chat apply fixes for 2025-06");
    assert!(done.contains("× credit_note"), "{done}");
    let (_, after) = call(&r.http_addr, &tok, "shop_gst", json!({"period": "2025-06"}));
    assert!(after != before && !after.contains("credit notes ₹0 "), "{after}");
    // filing waits for the month to end, and for the key
    let ask = ctl(&r, "chat file store gst for 2025-06");
    key(&r, &ask);
    assert!(ctl(&r, "chat file store gst for 2025-06").contains("not over"));
    ctl(&r, "shop clock 2025-07-11T10:00");
    let ask = ctl(&r, "chat file store gst for 2025-06");
    key(&r, &ask);
    assert!(ctl(&r, "chat file store gst for 2025-06").contains("lodged with the simulated portal"));

    // nothing of a shopper is on disk, and the books come back after a restart
    fn walk(p: &std::path::Path, hits: &mut Vec<String>) {
        for e in std::fs::read_dir(p).unwrap().flatten() {
            if e.path().is_dir() {
                walk(&e.path(), hits);
            } else if String::from_utf8_lossy(&std::fs::read(e.path()).unwrap_or_default()).contains("90000") {
                hits.push(e.path().display().to_string());
            }
        }
    }
    let mut hits = Vec::new();
    walk(&d, &mut hits);
    assert!(hits.is_empty(), "loyalty numbers on disk: {hits:?}");
    r.stop();
    let c = Config { data_dir: d.clone(), listen: "127.0.0.1:0".into(), kdf_mib: 1, idle_secs: 3600,
        store_profile: Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/retail/profile.toml")), ..Config::default() };
    let r = start(c, secrets::open_or_create(&d, "pw", 1).unwrap(), Arc::new(ScaledClock::new(1))).unwrap();
    assert!(ctl(&r, "chat store gst for 2025-06").contains("Filed on 2025-07-11"));
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_simulator_scores_the_audit_against_what_it_planted() {
    let (d, r, _tok) = boot("sim");
    let v: Value = serde_json::from_str(&ctl(&r, "shop sim 2025-09-20 2025-09-24 7")).unwrap();
    assert!(v["bills"].as_u64().unwrap() > 20_000, "{v}");
    assert_eq!(v["planted"], v["found"], "{v}");
    assert!(v["missed"].as_array().unwrap().is_empty() && v["extra"].as_array().unwrap().is_empty(), "{v}");
    assert!(v["by_family"]["rate"]["planted"].as_u64().unwrap() > 100, "the rate change on the 22nd: {v}");
    let audit = ctl(&r, "chat audit 2025-09");
    assert!(audit.contains("old_rate"), "{audit}");
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}
