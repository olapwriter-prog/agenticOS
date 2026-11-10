//! The partner services against the simulated world: every tool through the same connector agentd
//! uses, and the world's side checked (what WhatsApp received, what was paid, what was filed).
use agentos_action::Action;
use agentos_executor::Connector;
use agentos_partner::{inr, Config, Partner, Ui};
use serde_json::Value;
use std::process::{Child, Command};

struct World(Child, String);
impl Drop for World {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}
fn world() -> World {
    let port = 19_000 + (std::process::id() % 900) as u16;
    let child = Command::new(env!("CARGO_BIN_EXE_agentos-phonesim")).args(["world", &port.to_string()]).spawn().unwrap();
    let url = format!("http://127.0.0.1:{port}");
    for _ in 0..100 {
        if ureq::get(&format!("{url}/health")).call().is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    World(child, url)
}
fn state(w: &World) -> Value {
    ureq::get(&format!("{}/sim/state", w.1)).call().unwrap().into_json().unwrap()
}
fn sim(w: &World, path: &str, body: Value) {
    ureq::post(&format!("{}{path}", w.1)).send_json(body).unwrap();
}

/// Call a tool; the reply must be a screen. Returns its text form.
fn call(p: &mut Partner, tool: &str, args: &[(&str, &str)]) -> Result<String, String> {
    let a = Action { tool: tool.into(), args: args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() };
    let r = p.call(&a)?;
    let (ui, text) = Ui::from_reply(&r).unwrap_or_else(|| panic!("{tool}: not a screen: {r}"));
    assert!(!ui.title.is_empty());
    Ok(text)
}
fn ok(p: &mut Partner, tool: &str, args: &[(&str, &str)]) -> String {
    call(p, tool, args).unwrap_or_else(|e| panic!("{tool} {args:?}: {e}"))
}

#[test]
fn the_partner_runs_a_business_and_a_life_against_the_world() {
    let w = world();
    let cfg = Config { world_url: w.1.clone(), business_name: "Lakshmi Widgets".into(), interests: vec!["music".into(), "trekking".into()], ..Config::default() };
    let mut p = Partner::in_memory(cfg);

    // ---- channels: one inbox, a reply drafted per thread ----
    let t = ok(&mut p, "inbox", &[]);
    assert!(t.contains("5 waiting"), "{t}");
    ok(&mut p, "add_item", &[("name", "Widget"), ("price", "500"), ("gst", "18")]);
    ok(&mut p, "add_item", &[("name", "Repair"), ("price", "1500"), ("gst", "18%")]);
    assert!(call(&mut p, "add_item", &[("name", "X"), ("price", "10"), ("gst", "17")]).unwrap_err().contains("not a GST rate"));
    ok(&mut p, "add_customer", &[("name", "Sharma Traders"), ("gstin", "29AABCT1332L1ZA"), ("phone", "98111 11111")]);
    ok(&mut p, "add_customer", &[("name", "Patil Stores"), ("gstin", "27aapfu0939f1zv"), ("phone", "+91 98222 22222")]);
    assert!(call(&mut p, "add_customer", &[("name", "Typo Ltd"), ("gstin", "27AAPFU0939F1ZX"), ("phone", "9800000000")]).unwrap_err().contains("check character"));
    let t = ok(&mut p, "read_thread", &[("who", "Sharma")]);
    assert!(t.contains("Widget is ₹500.00 + 18% GST (₹590.00 in all)"), "{t}");
    let t = ok(&mut p, "read_thread", &[("who", "Patil Stores")]);
    assert!(t.contains("10 x Widget, 2 x Repair comes to ₹9,440.00"), "both items of the order: {t}");
    assert!(t.contains("invoice Patil Stores for 10 x Widget, 2 x Repair"), "{t}");
    let t = ok(&mut p, "read_thread", &[("who", "Kiran Rao")]);
    assert!(t.contains("add customer Kiran Rao phone 919844444444"), "an unknown sender can become a customer: {t}");
    let t = ok(&mut p, "read_thread", &[("who", "Meenakshi Caterers")]);
    assert!(t.contains("Widget is ₹500.00 + 18% GST (₹590.00 in all). For 40 x Widget that is ₹23,600.00"), "a price question with a quantity after the item gets a quote: {t}");
    assert!(t.contains("add customer Meenakshi Caterers phone orders@meenakshi.example"), "{t}");
    ok(&mut p, "reply", &[("to", "Sharma Traders"), ("text", "Hi Sharma, Widget is ₹500.00 + 18% GST. How many would you like?")]);
    let s = state(&w);
    let sharma = s["whatsapp"]["conversations"].as_array().unwrap().iter().find(|c| c["wa_id"] == "919811111111").unwrap().clone();
    assert_eq!(sharma["messages"].as_array().unwrap().last().unwrap()["direction"], "out");
    assert!(ok(&mut p, "inbox", &[]).contains("4 waiting"));
    ok(&mut p, "reply", &[("to", "Meenakshi Caterers"), ("text", "Price list attached")]);
    assert_eq!(state(&w)["mail"]["sent"][0]["to"], "orders@meenakshi.example");

    // ---- an invoice across states: IGST; sent on WhatsApp with a UPI link; paid; matched ----
    let t = ok(&mut p, "invoice", &[("customer", "Patil Stores"), ("items", "10 x Widget, 2 x Repair")]);
    let num = p.data.invoices[0].number.clone();
    assert!(t.contains("IGST ₹1,440.00") && t.contains("₹9,440.00"), "{t}");
    ok(&mut p, "send_invoice", &[("number", &num)]);
    let s = state(&w);
    let patil = s["whatsapp"]["conversations"].as_array().unwrap().iter().find(|c| c["wa_id"] == "919822222222").unwrap().clone();
    let sent = patil["messages"].as_array().unwrap().last().unwrap()["text"].as_str().unwrap().to_string();
    assert!(sent.contains("upi://pay?pa=mybusiness@upi") && sent.contains("am=9440.00") && sent.contains(&num.replace('/', "%2F")), "{sent}");
    assert!(ok(&mut p, "invoices", &[]).contains("1 open, ₹9,440.00 owed"));
    sim(&w, "/sim/pay/collect", serde_json::json!({"from_vpa": "patil@okhdfc", "amount_paise": 944_000, "note": format!("{num} payment")}));
    let t = ok(&mut p, "invoices", &[]);
    assert!(t.contains("0 open") && t.contains("payment received by UPI and matched"), "{t}");
    // a consumer within the state: CGST + SGST
    ok(&mut p, "add_customer", &[("name", "Asha Menon"), ("gstin", "none"), ("phone", "9833333333")]);
    let t = ok(&mut p, "invoice", &[("customer", "Asha"), ("items", "1 x Repair")]);
    assert!(t.contains("CGST ₹135.00 + SGST ₹135.00"), "{t}");

    // ---- bills by email, paid from the wallet; their GST becomes input credit ----
    let t = ok(&mut p, "bills", &[]);
    assert!(t.contains("3 bills") && t.contains("FiberNet Broadband") && t.contains("pay bill m-103"), "{t}");
    let before = s["pay"]["balance_paise"].as_i64().unwrap() + 944_000;
    ok(&mut p, "pay_bill", &[("id", "m-103")]);
    let s = state(&w);
    assert_eq!(s["pay"]["balance_paise"].as_i64().unwrap(), before - 117_900);
    assert_eq!(s["pay"]["payouts"][0]["vpa"], "fibernet@upi");
    assert!(call(&mut p, "pay_bill", &[("id", "m-103")]).is_err(), "a bill is paid once");
    assert!(ok(&mut p, "bills", &[]).contains("2 bills"));
    assert_eq!(p.data.expenses[0].igst, 17_984, "a Tamil Nadu supplier's GST is IGST credit");

    // ---- GST: the month's figures, filed once through the GSP ----
    let t = ok(&mut p, "gst_summary", &[("period", "this month")]);
    assert!(t.contains("Sales (outward supplies) [₹9,500.00]"), "{t}");
    assert!(t.contains("IGST ₹1,440.00 · CGST ₹135.00 · SGST ₹135.00"), "{t}");
    assert!(t.contains("Input tax credit (bills paid) [₹179.84]"), "{t}");
    assert!(t.contains("file gstr-3b for"), "{t}");
    let t = ok(&mut p, "file_gst", &[("form", "GSTR-3B"), ("period", "this month")]);
    assert!(t.contains("ARN AA29"), "{t}");
    assert!(call(&mut p, "file_gst", &[("form", "gstr3b"), ("period", "this month")]).unwrap_err().contains("filed already"));
    ok(&mut p, "file_gst", &[("form", "gstr-1"), ("period", "this month")]);
    let f = &state(&w)["gsp"]["filings"];
    assert_eq!(f.as_array().unwrap().len(), 2);
    assert_eq!(f[0]["data"]["tx_pmt"]["cash"]["iamt"].as_f64().unwrap(), 1260.16, "IGST 1440 less 179.84 credit");
    assert_eq!(f[1]["data"]["b2b"][0]["ctin"], "27AAPFU0939F1ZV");
    assert!(ok(&mut p, "gst_summary", &[("period", "this month")]).contains("filed, ARN"));

    // ---- wallet ----
    let t = ok(&mut p, "pay", &[("to", "ravi@okaxis"), ("amount", "500"), ("note", "lunch")]);
    assert!(t.contains("Paid ₹500.00"), "{t}");
    assert!(call(&mut p, "pay", &[("to", "not an address"), ("amount", "5"), ("note", "x")]).unwrap_err().contains("not a UPI address"));
    assert!(call(&mut p, "pay", &[("to", "big@upi"), ("amount", "10000000"), ("note", "x")]).unwrap_err().contains("is needed"));
    assert!(ok(&mut p, "wallet", &[]).contains("lunch"));

    // ---- shopping from the order history ----
    let t = ok(&mut p, "orders", &[]);
    assert!(t.contains("Golden Harvest Whole Wheat Atta 10 kg") && t.contains("bought 3 times"), "{t}");
    let t = ok(&mut p, "best_price", &[("item", "atta")]);
    assert!(t.contains("on quickmart") && t.contains("[₹439.00]"), "the cheapest well-rated offer of the usual atta: {t}");
    assert!(t.contains("you usually buy this; last paid ₹469.00 on shopkart (₹30.00 cheaper now)"), "{t}");
    let bal = state(&w)["pay"]["balance_paise"].as_i64().unwrap();
    let t = ok(&mut p, "buy", &[("option", "best")]);
    assert!(t.contains("quickmart order"), "{t}");
    let s = state(&w);
    assert_eq!(s["pay"]["balance_paise"].as_i64().unwrap(), bal - 43_900);
    assert_eq!(s["shop"]["quickmart"]["orders"].as_array().unwrap().len(), 2);
    // a badly rated seller is not "best", however cheap
    let t = ok(&mut p, "best_price", &[("item", "earbuds")]);
    assert!(t.contains("on quickmart") && !t.contains("Sonic X200 Bluetooth Earbuds [₹1,399.00]"), "{t}");
    // the price changes between showing and buying: no purchase
    ok(&mut p, "best_price", &[("item", "green tea")]);
    assert_eq!(p.data.last_options[0].platform, "bazaarnow");
    sim(&w, "/sim/price", serde_json::json!({"platform": "bazaarnow", "id": "BN-204", "price_paise": 40_000}));
    assert!(call(&mut p, "buy", &[("option", "best")]).unwrap_err().contains("price went up"));
    assert!(call(&mut p, "buy", &[("option", "atta")]).unwrap_err().contains("compare first"));

    // ---- life: news and suggestions by interests, learned; booking ----
    let t = ok(&mut p, "news", &[("topic", "any")]);
    let first = t.lines().nth(1).unwrap().to_string();
    assert!(first.contains("Indie bands") || first.contains("Jazz") || first.contains("treks"), "music or trekking first: {t}");
    ok(&mut p, "news", &[("topic", "gst")]);
    assert!(p.data.interests["gst"] > 0.0, "asking about a topic is learned");
    let t = ok(&mut p, "suggest", &[("kind", "adventures")]);
    assert!(t.lines().nth(1).unwrap().contains("Skandagiri"), "trekking interest puts the trek first: {t}");
    let before = p.data.interests["trekking"];
    let t = ok(&mut p, "book", &[("event", "EV-102"), ("qty", "2")]);
    assert!(t.contains("₹2,998.00") && t.contains("-T2"), "{t}");
    assert!(p.data.interests["trekking"] > before, "booking is learned");
    assert_eq!(state(&w)["events"]["list"][1]["seats_left"], 10);
    assert!(call(&mut p, "book", &[("event", "EV-103"), ("qty", "9")]).unwrap_err().contains("only 8 seats left"));
    assert!(call(&mut p, "book", &[("event", "EV-102"), ("qty", "11")]).unwrap_err().contains("1 to 10 tickets"));
    ok(&mut p, "like", &[("topic", "jazz")]);
    let t = ok(&mut p, "suggest", &[("kind", "music")]);
    assert!(t.lines().nth(1).unwrap().contains("Jazz on the Roof"), "a stated like moves jazz up: {t}");
    let t = ok(&mut p, "interests", &[]);
    assert!(t.contains("jazz") && t.contains("trekking"), "{t}");

    // the money that left, all accounted for
    let spent: i64 = p.data.payments.iter().map(|x| x.amount).sum();
    assert_eq!(spent, 117_900 + 50_000 + 43_900 + 299_800, "{}", inr(spent));
}
