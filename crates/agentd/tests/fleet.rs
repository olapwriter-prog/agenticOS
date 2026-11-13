//! A phone that keeps a taxi fleet's book (examples/fleet), over the real sockets: trips and
//! shifts arrive on the control channel as a platform feed and the drivers' app would deliver
//! them, the owner asks in fixed phrases, a Claude client reads over MCP, and money waits for
//! the owner's key.
use agentd::config::Config;
use agentd::secrets;
use agentd::server::{ctl_send, start, Running};
use agentos_hal::ScaledClock;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;

fn boot(tag: &str, fleet: bool) -> (PathBuf, Running, String) {
    let d = PathBuf::from("/tmp").join(format!("agentd-fleet-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let c = Config {
        data_dir: d.clone(),
        listen: "127.0.0.1:0".into(),
        kdf_mib: 1,
        idle_secs: 3600,
        fleet_profile: fleet.then(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/fleet/profile.toml")),
        ..Config::default()
    };
    let s = secrets::open_or_create(&d, "pw", 1).unwrap();
    let token = s.gateway_token.clone();
    (d, start(c, s, Arc::new(ScaledClock::new(1))).unwrap(), token)
}
fn rpc(addr: &str, tok: &str, body: Value) -> Value {
    let b = body.to_string();
    let mut s = TcpStream::connect(addr).unwrap();
    write!(s, "POST /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {tok}\r\nContent-Length: {}\r\n\r\n{b}", b.len()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    serde_json::from_str(out.split("\r\n\r\n").nth(1).unwrap_or("")).unwrap_or(Value::Null)
}
fn call(addr: &str, tok: &str, name: &str, args: Value) -> (bool, String) {
    let v = rpc(addr, tok, json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}));
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
fn a_phone_without_a_fleet_has_no_fleet_tools() {
    let (d, r, tok) = boot("none", false);
    let list = rpc(&r.http_addr, &tok, json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).to_string();
    assert!(list.contains("send_sms") && !list.contains("fleet_"));
    assert!(ctl(&r, "fleet state").contains("no fleet on this phone"));
    assert!(ctl(&r, "chat fleet dashboard").contains("could not turn that into"));
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn trips_come_in_the_owner_and_a_client_read_and_money_waits_for_the_key() {
    let (d, r, tok) = boot("day", true);
    // a platform's trips, the driver's shift and the cash they hand in
    for ev in [
        r#"{"t":"trip","id":"T1","car":"C01","driver":"D01","platform":"Uber","at":"2026-10-14T09:00","mins":25,"km":8,"dead_km":2,"fare":20000,"pay":"online","from":"Hebbal","to":"Majestic"}"#,
        r#"{"t":"trip","id":"T2","car":"C01","driver":"D01","platform":"Namma Yatri","at":"2026-10-14T10:00","mins":30,"km":10,"dead_km":1,"fare":30000,"tip":2000,"pay":"cash","from":"Majestic","to":"Jayanagar"}"#,
        r#"{"t":"shift","car":"C01","driver":"D01","start":"2026-10-14T07:00","end":"2026-10-14T17:00","odo_start":41200,"odo_end":41221}"#,
        r#"{"t":"fill","car":"C01","driver":"D01","at":"2026-10-14T16:00","kind":"cng","qty":4,"amount":35600,"odometer":41215}"#,
    ] {
        let said = ctl(&r, &format!("fleet event {ev}"));
        assert!(!said.starts_with("refused"), "{said}");
    }
    assert!(ctl(&r, r#"fleet event {"t":"trip","id":"T1","car":"C01","driver":"D01","platform":"Uber","at":"2026-10-14T09:00","fare":1}"#).contains("already in the book"));
    assert!(ctl(&r, r#"fleet event {"t":"cash","driver":"D01","at":"2026-10-14T17:10","amount":900}"#).contains("holds ₹320"));

    // the owner, in fixed phrases (no model on this phone)
    let dash = ctl(&r, "chat fleet dashboard");
    assert!(dash.starts_with("done: Namma Wheels Fleet — Wednesday 2026-10-14"), "{dash}");
    assert!(dash.contains("1 of 15 cars out: 2 trips") && dash.contains("still holds ₹320"), "{dash}");
    let sheet = ctl(&r, "chat driver sheet");
    assert!(sheet.contains("Manjunath (D01) on C01: 2 trips in 10.0 h → earnings ₹174.28"), "{sheet}");
    assert!(ctl(&r, "chat driver cash 320 from manjunath").contains("still holds ₹0"));
    assert!(ctl(&r, "chat fleet month 2026-10").contains("Fleet profit for the period"));
    assert!(ctl(&r, "chat fleet plan").contains("Plan for Thursday 2026-10-15"));

    // a Claude client reads the same over MCP; it can record nothing about money without the key
    let list = rpc(&r.http_addr, &tok, json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).to_string();
    assert!(list.contains("fleet_dashboard") && list.contains("fleet_pay_driver"));
    for tool in ["fleet_dashboard", "fleet_cars", "fleet_drivers", "fleet_issues", "fleet_payouts", "fleet_plan"] {
        let (ok, out) = call(&r.http_addr, &tok, tool, json!({}));
        assert!(ok && !out.contains("90000"), "{tool}: {out}");
    }
    let pay = json!({"driver": "D01", "amount": "174.28"});
    let (ok, out) = call(&r.http_addr, &tok, "fleet_pay_driver", pay.clone());
    assert!(!ok && out.contains("approval required"), "{out}");
    let id = out.split("id ").nth(1).and_then(|s| s.split(')').next()).unwrap().to_string();
    assert!(ctl(&r, &format!("decline {id}")).contains("declined"));
    let (ok, out) = call(&r.http_addr, &tok, "fleet_claim", json!({"platform": "uber", "amount": "10"}));
    assert!(!ok && out.contains("approval required"), "{out}");

    // the owner pays with the key: once, and only the sheet's amount
    let ask = ctl(&r, "chat pay 200 to driver manjunath");
    key(&r, &ask);
    assert!(ctl(&r, "chat pay 200 to driver manjunath").contains("is due ₹174.28 today, not ₹200"));
    let ask = ctl(&r, "chat pay 174.28 to driver manjunath");
    key(&r, &ask);
    assert!(ctl(&r, "chat pay 174.28 to driver manjunath").contains("₹174.28 recorded as handed to Manjunath"));

    // the platform pays ₹15 short for the week; the exact shortfall is claimed with the key
    assert!(ctl(&r, "fleet payout uber 2026-10-14 2026-10-20 15").contains("uber paid ₹125.80"));
    assert!(ctl(&r, "chat fleet issues").contains("Uber paid ₹15 less"));
    let ask = ctl(&r, "chat fleet claim 15 from uber");
    key(&r, &ask);
    assert!(ctl(&r, "chat fleet claim 15 from uber").contains("CLM-UBER-1"));

    // nothing readable on disk; the book comes back after a restart
    fn walk(p: &std::path::Path, hits: &mut Vec<String>) {
        for e in std::fs::read_dir(p).unwrap().flatten() {
            if e.path().is_dir() {
                walk(&e.path(), hits);
            } else if e.path().to_string_lossy().contains("/fleet/") && String::from_utf8_lossy(&std::fs::read(e.path()).unwrap_or_default()).contains("Majestic") {
                hits.push(e.path().display().to_string());
            }
        }
    }
    let mut hits = Vec::new();
    walk(&d, &mut hits);
    assert!(hits.is_empty(), "the fleet's book is readable: {hits:?}");
    r.stop();
    let c = Config { data_dir: d.clone(), listen: "127.0.0.1:0".into(), kdf_mib: 1, idle_secs: 3600,
        fleet_profile: Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/fleet/profile.toml")), ..Config::default() };
    let r = start(c, secrets::open_or_create(&d, "pw", 1).unwrap(), Arc::new(ScaledClock::new(1))).unwrap();
    assert!(ctl(&r, "chat driver sheet").contains("paid ₹174.28, due ₹0"));
    // the time set on the control channel is part of the book: it is still there after a restart
    assert!(ctl(&r, "fleet clock 2026-10-16T08:00").contains("2026-10-16T08:00"));
    r.stop();
    let c = Config { data_dir: d.clone(), listen: "127.0.0.1:0".into(), kdf_mib: 1, idle_secs: 3600,
        fleet_profile: Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/fleet/profile.toml")), ..Config::default() };
    let r = start(c, secrets::open_or_create(&d, "pw", 1).unwrap(), Arc::new(ScaledClock::new(1))).unwrap();
    assert!(ctl(&r, "chat fleet dashboard").contains("Friday 2026-10-16"), "{}", ctl(&r, "chat fleet dashboard"));
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_simulator_scores_the_checks_against_what_it_planted() {
    let (d, r, _tok) = boot("sim", true);
    let v: Value = serde_json::from_str(&ctl(&r, "fleet sim 2026-10-14 2026-10-28 7")).unwrap();
    assert!(v["events"].as_u64().unwrap() > 3_000 && v["refused"].as_array().unwrap().is_empty(), "{v}");
    assert_eq!(v["planted"], v["found"], "{v}");
    assert!(v["missed"].as_array().unwrap().is_empty() && v["extra"].as_array().unwrap().is_empty(), "{v}");
    assert!(v["planted"].as_u64().unwrap() >= 10, "{v}");
    assert!(ctl(&r, "chat cars today").contains("C15"));
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}
