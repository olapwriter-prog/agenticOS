//! A phone that runs a business (examples/cafe), over the real sockets: orders come in on the
//! control channel as a channel adapter would deliver them; the owner talks; a Claude client
//! reads over MCP. Tiers, the approval key and the privacy rules hold end to end.
use agentd::config::Config;
use agentd::secrets;
use agentd::server::{ctl_send, start, Running};
use agentos_hal::ScaledClock;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;

fn profile() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/cafe/profile.toml")
}
fn boot(tag: &str, business: bool) -> (PathBuf, Running, String) {
    let d = PathBuf::from("/tmp").join(format!("agentd-biz-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let c = Config {
        data_dir: d.clone(),
        listen: "127.0.0.1:0".into(),
        kdf_mib: 1,
        idle_secs: 3600,
        business_profile: business.then(profile),
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
const SECRETS: [&str; 5] = ["Lakshmi", "Prasanna", "9000012345", "90000 12345", "Sea Breeze"];
fn order(r: &Running, id: &str, at: &str, pay: &str) -> String {
    ctl(r, &format!(r#"biz event {{"t":"order","id":"{id}","channel":"whatsapp","at":"{at}","items":[{{"code":"R3","qty":1}},{{"code":"C3","qty":1}}],"customer":{{"name":"Lakshmi Prasanna","phone":"+91 90000 12345","address":"Flat 204, Sea Breeze Residency, MVP Colony"}},"locality":"MVP Colony","km":4.5,"payment":"{pay}"}}"#))
}
fn deliver(r: &Running, id: &str, rider: &str, at: &str) {
    for ev in [
        format!(r#"{{"t":"assign","id":"{id}","rider":"{rider}","at":"{at}"}}"#),
        format!(r#"{{"t":"status","id":"{id}","status":"out","at":"{at}"}}"#),
        format!(r#"{{"t":"status","id":"{id}","status":"delivered","at":"{at}"}}"#),
    ] {
        let said = ctl(r, &format!("biz event {ev}"));
        assert!(!said.starts_with("refused"), "{said}");
    }
}

#[test]
fn a_phone_without_a_business_has_no_business_tools() {
    let (d, r, tok) = boot("none", false);
    let list = rpc(&r.http_addr, &tok, json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).to_string();
    assert!(list.contains("send_sms") && !list.contains("biz_"));
    assert!(ctl(&r, "biz state").contains("no business on this phone"));
    assert!(ctl(&r, "chat dashboard").contains("could not turn that into"));
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn an_order_is_billed_tracked_and_settled_and_money_waits_for_the_key() {
    let (d, r, tok) = boot("day", true);
    // a WhatsApp order arrives: it is booked and the customer gets their bill, with no one asked
    let said = order(&r, "W-1", "2026-11-13T13:05", "cod");
    assert!(said.contains("W-1 whatsapp ₹389") && said.contains("bill VWC/26-27/00001 (₹389) sent to La*** ••••••45"), "{said}");
    // a Swiggy order: no bill from us, no customer kept
    let said = ctl(&r, r#"biz event {"t":"order","id":"SW-1","channel":"swiggy","at":"2026-11-13T13:06","items":[{"code":"N3","qty":2}],"locality":"Gajuwaka","km":14}"#);
    assert!(said.contains("swiggy customer"), "{said}");
    deliver(&r, "W-1", "R1", "2026-11-13T13:30");
    assert_eq!(ctl(&r, "biz sent"), r#"{"document":1,"location":1,"text":1}"#);

    // the owner asks, in fixed phrases (no model on this phone)
    let dash = ctl(&r, "chat dashboard");
    assert!(dash.starts_with("done: Vizag Wok & Crunch — Friday 2026-11-13"), "{dash}");
    assert!(dash.contains("Orders 2: delivered 1") && dash.contains("whatsapp"), "{dash}");
    let riders = ctl(&r, "chat rider settlement");
    assert!(riders.contains("Ravi (R1): 1 deliveries, 4.5 km → earnings ₹52") && riders.contains("to hand in ₹389"), "{riders}");

    // a Claude client reads the same things, and never sees the customer
    let list = rpc(&r.http_addr, &tok, json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).to_string();
    assert!(list.contains("biz_dashboard") && list.contains("biz_pay_rider"));
    for (tool, args) in [
        ("biz_dashboard", json!({})), ("biz_orders", json!({})), ("biz_plan", json!({})), ("biz_riders", json!({})),
        ("biz_payouts", json!({})), ("biz_privacy", json!({})), ("biz_issues", json!({})), ("biz_gst", json!({"period":"2026-11"})),
    ] {
        let (ok, out) = call(&r.http_addr, &tok, tool, args);
        assert!(ok, "{tool}: {out}");
        for s in SECRETS {
            assert!(!out.contains(s), "{tool} shows {s} to an MCP client: {out}");
        }
    }
    let (_, orders) = call(&r.http_addr, &tok, "biz_orders", json!({}));
    assert!(orders.contains("La*** ••••••45 · MVP Colony"), "{orders}");
    // the control channel's dashboard data is masked too
    let state = ctl(&r, "biz state");
    for s in SECRETS {
        assert!(!state.contains(s));
    }

    // money: the client cannot pay the rider; the owner's key can, once, and only the sheet's amount
    let pay = json!({"rider":"R1","amount":"52"});
    let (ok, out) = call(&r.http_addr, &tok, "biz_pay_rider", pay.clone());
    assert!(!ok && out.contains("approval required"), "{out}");
    assert!(ctl(&r, "pending").contains("biz_pay_rider amount=52 rider=R1"));
    assert!(ctl(&r, "press 1").contains("approved"));
    let (ok, out) = call(&r.http_addr, &tok, "biz_pay_rider", pay.clone());
    assert!(ok && out.contains("₹52 recorded as handed to Ravi"), "{out}");
    let (ok, _) = call(&r.http_addr, &tok, "biz_pay_rider", pay);
    assert!(!ok, "one approval, one payment");
    // an approved wrong amount still does not pay
    let (_, out) = call(&r.http_addr, &tok, "biz_pay_rider", json!({"rider":"R2","amount":"5000"}));
    let id = out.split("id ").nth(1).and_then(|s| s.split(')').next()).unwrap().to_string();
    ctl(&r, &format!("press {id}"));
    let (ok, out) = call(&r.http_addr, &tok, "biz_pay_rider", json!({"rider":"R2","amount":"5000"}));
    assert!(!ok && out.contains("no deliveries today") || out.contains("nothing due"), "{out}");
    // erasing customers and lodging a return are High as well
    for (tool, args) in [("biz_forget", json!({"days":"0"})), ("biz_file_gst", json!({"period":"2026-10"})), ("biz_claim", json!({"platform":"swiggy","amount":"10"}))] {
        let (ok, out) = call(&r.http_addr, &tok, tool, args);
        assert!(!ok && out.contains("approval required"), "{tool}: {out}");
    }

    // nothing about the customer is on disk in the clear, anywhere under the data directory
    fn walk(p: &std::path::Path, hits: &mut Vec<String>) {
        for e in std::fs::read_dir(p).unwrap().flatten() {
            let path = e.path();
            if path.is_dir() {
                walk(&path, hits);
            } else if let Ok(bytes) = std::fs::read(&path) {
                let text = String::from_utf8_lossy(&bytes);
                for s in SECRETS {
                    if text.contains(s) {
                        hits.push(format!("{} in {}", s, path.display()));
                    }
                }
            }
        }
    }
    let mut hits = Vec::new();
    walk(&d, &mut hits);
    assert!(hits.is_empty(), "customer data on disk in the clear: {hits:?}");
    // the operator can still take a copy of the bill, on purpose
    let pdf = d.join("bill.pdf");
    assert!(ctl(&r, &format!("biz bill W-1 {}", pdf.display())).contains("wrote"));
    assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF"));

    // the book survives a restart, sealed
    r.stop();
    let c = Config { data_dir: d.clone(), listen: "127.0.0.1:0".into(), kdf_mib: 1, idle_secs: 3600, business_profile: Some(profile()), ..Config::default() };
    let s = secrets::open_or_create(&d, "pw", 1).unwrap();
    let r = start(c, s, Arc::new(ScaledClock::new(1))).unwrap();
    assert!(ctl(&r, "chat today's orders").contains("W-1"));
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_typed_order_is_read_or_the_customer_is_asked() {
    let (d, r, _tok) = boot("typed", true);
    ctl(&r, "biz clock 2026-11-13T19:30");
    let said = ctl(&r, "biz wa +91 90000 12345|Lakshmi Prasanna|Flat 204, Sea Breeze Residency|MVP Colony|upi|2 chicken fried rice and wings");
    assert!(said.contains("W-1001 whatsapp") && said.contains("(3 items)") && said.contains("bill VWC/26-27/00001"), "{said}");
    let unclear = ctl(&r, "biz wa +91 90000 12345|Lakshmi Prasanna|Flat 204|MVP Colony|upi|2 fried rice");
    assert!(unclear.contains("refused") && unclear.contains("unclear"), "{unclear}");
    let far = ctl(&r, "biz wa +91 90000 12345|Lakshmi Prasanna|Plot 9|Gajuwaka|upi|1 bucket");
    assert!(far.contains("we deliver up to 8 km"), "{far}");
    let unknown = ctl(&r, "biz wa +91 90000 12345|Lakshmi Prasanna|Plot 9|Anakapalle|upi|1 bucket");
    assert!(unknown.contains("we do not know Anakapalle"), "{unknown}");
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_owners_own_words_are_understood_by_code_and_learned() {
    let (d, r, _tok) = boot("words", true);
    order(&r, "W-1", "2026-11-13T13:05", "upi");
    deliver(&r, "W-1", "R1", "2026-11-13T13:30");
    ctl(&r, "metrics reset");
    // not a fixed phrase: the classifier knows what kind of request it is, rules fill the rest
    let a = ctl(&r, "chat what do i owe the delivery boys today");
    assert!(a.contains("Ravi (R1): 1 deliveries"), "{a}");
    // "settle ravi": the amount comes from the sheet and is shown on the approval
    let a = ctl(&r, "chat settle ravi for today");
    assert!(a.contains("approval required"), "{a}");
    assert!(ctl(&r, "pending").contains("biz_pay_rider amount=52 rider=R1"), "{}", ctl(&r, "pending"));
    let id = a.split("id ").nth(1).and_then(|s| s.split(')').next()).unwrap().to_string();
    assert!(ctl(&r, &format!("press {id}")).contains("approved"));
    assert!(ctl(&r, "chat settle ravi for today").contains("₹52 recorded as handed to Ravi"));
    // the approved wording is now a shortcut: tomorrow it goes straight to the same action
    let m: Value = serde_json::from_str(&ctl(&r, "metrics")).unwrap();
    assert!(m["chats"]["classifier"]["n"].as_u64().unwrap() >= 2, "{m}");
    assert!(m["chats"]["shortcut"]["n"].as_u64().unwrap() >= 1, "{m}");
    assert!(m["chats"].get("model").is_none() && m["chats"].get("gave-up").is_none(), "{m}");
    // a wording nobody taught it, with no model on the phone: it says so instead of guessing
    let a = ctl(&r, "chat zxqv blorp");
    assert!(a.contains("could not turn that into"), "{a}");
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn answers_the_owner_usually_asks_for_are_ready_before_they_ask() {
    let (d, r, _tok) = boot("ready", true);
    // three evenings of the same closing routine, with an order each day
    for (i, day) in ["2026-11-10", "2026-11-11", "2026-11-12", "2026-11-13"].iter().enumerate() {
        order(&r, &format!("W-{i}"), &format!("{day}T20:05"), "upi");
        deliver(&r, &format!("W-{i}"), "R1", &format!("{day}T20:40"));
        ctl(&r, &format!("biz clock {day}T22:10"));
        if i == 3 {
            ctl(&r, "metrics reset");
        }
        let a = ctl(&r, "chat dashboard");
        assert!(a.contains(day), "{a}");
        assert!(ctl(&r, "chat rider settlement").contains("Ravi"));
        assert!(ctl(&r, "chat plan for tomorrow").contains("Plan for"));
    }
    // on the fourth evening the riders' sheet and the plan were prepared while the owner read
    // the dashboard
    let m: Value = serde_json::from_str(&ctl(&r, "metrics")).unwrap();
    assert!(m["ready"]["ahead_hits"].as_u64().unwrap() >= 2, "{m}");
    assert!(m["chats"]["intent+ready"]["n"].as_u64().unwrap() >= 2, "{m}");
    // a prepared answer is never stale: a new order changes the book, and the next dashboard is new
    let before = ctl(&r, "chat dashboard");
    order(&r, "W-9", "2026-11-13T22:12", "upi");
    let after = ctl(&r, "chat dashboard");
    assert!(before.contains("Orders 1:") && after.contains("Orders 2:"), "{before}\n{after}");
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_learned_request_pays_todays_amount_not_the_day_it_was_learned() {
    let (d, r, _tok) = boot("fresh", true);
    // day one: one drop; "settle ravi" is approved and becomes a shortcut
    order(&r, "W-1", "2026-11-12T13:05", "upi");
    deliver(&r, "W-1", "R1", "2026-11-12T13:30");
    let a = ctl(&r, "chat settle ravi");
    assert!(ctl(&r, "pending").contains("amount=52 rider=R1"));
    ctl(&r, &format!("press {}", a.split("id ").nth(1).and_then(|s| s.split(')').next()).unwrap()));
    assert!(ctl(&r, "chat settle ravi").contains("₹52 recorded"));
    // day two: two drops. The same words must ask for today's ₹104, and say so on the approval
    for id in ["W-2", "W-3"] {
        order(&r, id, "2026-11-13T13:05", "upi");
        deliver(&r, id, "R1", "2026-11-13T13:30");
    }
    let a = ctl(&r, "chat settle ravi");
    assert!(a.contains("approval required"), "{a}");
    assert!(ctl(&r, "pending").contains("amount=104 rider=R1"), "{}", ctl(&r, "pending"));
    ctl(&r, &format!("press {}", a.split("id ").nth(1).and_then(|s| s.split(')').next()).unwrap()));
    assert!(ctl(&r, "chat settle ravi").contains("₹104 recorded as handed to Ravi for 2 deliveries"));
    let m: Value = serde_json::from_str(&ctl(&r, "metrics")).unwrap();
    assert!(m["chats"]["shortcut"]["n"].as_u64().unwrap() >= 2, "{m}");
    // a number the owner says is used as said (and refused if it is not what is due)
    order(&r, "W-4", "2026-11-13T14:05", "upi");
    deliver(&r, "W-4", "R1", "2026-11-13T14:30");
    let a = ctl(&r, "chat pay 500 to rider ravi");
    assert!(ctl(&r, "pending").contains("amount=500"));
    ctl(&r, &format!("press {}", a.split("id ").nth(1).and_then(|s| s.split(')').next()).unwrap()));
    assert!(ctl(&r, "chat pay 500 to rider ravi").contains("is due ₹52 today, not ₹500"));
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_word_is_never_taken_for_an_amount_on_the_approval_screen() {
    let (d, r, _tok) = boot("it", true);
    // a week of Zomato orders, paid ₹480 short
    for i in 0..3 {
        ctl(&r, &format!(r#"biz event {{"t":"order","id":"ZO-{i}","channel":"zomato","at":"2026-11-0{}T13:00","items":[{{"code":"C2","qty":2}}],"locality":"Gajuwaka","km":14,"status":"delivered"}}"#, i + 2));
    }
    ctl(&r, "biz clock 2026-11-11T10:00");
    assert!(ctl(&r, "biz payout zomato 2026-11-02 2026-11-08 480").contains("zomato paid"));
    // "claim it from zomato" fits the phrase "claim {amount} from {platform}" with amount = "it"
    let a = ctl(&r, "chat claim it from zomato");
    assert!(a.contains("approval required"), "{a}");
    let pending = ctl(&r, "pending");
    assert!(pending.contains("biz_claim amount=480 platform=zomato"), "the owner approves a figure, not a word: {pending}");
    ctl(&r, &format!("press {}", a.split("id ").nth(1).and_then(|s| s.split(')').next()).unwrap()));
    assert!(ctl(&r, "chat claim it from zomato").contains("claim CLM-ZOMATO-1 for ₹480 filed"));
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_question_the_phone_knows_is_not_sent_to_write_analysis_code() {
    let (d, r, _tok) = boot("question", true);
    order(&r, "W-1", "2026-11-13T13:05", "upi");
    // "did ..." and "which ..." open like analysis questions (which a model answers by writing
    // code, or not at all on a phone with no model); the classifier knows these are reports
    let a = ctl(&r, "chat did swiggy pay correctly");
    assert!(a.starts_with("done: Swiggy (commission"), "{a}");
    let a = ctl(&r, "chat which orders came in");
    assert!(a.contains("W-1") && a.contains("La***"), "{a}");
    let m: Value = serde_json::from_str(&ctl(&r, "metrics")).unwrap();
    assert_eq!(m["chats"]["classifier"]["n"], 2, "{m}");
    r.stop();
    let _ = std::fs::remove_dir_all(d);
}
