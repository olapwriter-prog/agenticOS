//! The cafe from examples/cafe/profile.toml, end to end inside the crate: orders in, bills out,
//! riders and platforms settled, GST, the plan, and the privacy rules.
use agentos_action::{Action, Tier};
use agentos_business::dataset::{day_events, import, payout_event, play};
use agentos_business::profile::Profile;
use agentos_business::tools::BusinessTools;
use agentos_business::util::*;
use agentos_business::wa::{parse_order, Msg, SimWhatsapp};
use agentos_business::Business;
use agentos_executor::Connector;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

fn profile() -> Profile {
    Profile::load(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/cafe/profile.toml")).unwrap()
}
fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("biz-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}
fn shop(tag: &str) -> Business {
    let mut b = Business::open(profile(), dir(tag), &[9u8; 32], Box::new(SimWhatsapp::default())).unwrap();
    b.tick("2026-11-13T12:00");
    b
}
const LAKSHMI: &str = r#""customer":{"name":"Lakshmi Prasanna","phone":"+91 90000 12345","address":"Flat 204, Sea Breeze Residency, MVP Colony"}"#;
fn wa_order(b: &mut Business, id: &str, at: &str, items: &str, pay: &str) -> String {
    play(b, &format!(r#"{{"t":"order","id":"{id}","channel":"whatsapp","at":"{at}","items":{items},{LAKSHMI},"locality":"MVP Colony","km":4.5,"payment":"{pay}"}}"#)).unwrap()
}
fn act(tool: &str, args: &[(&str, &str)]) -> Action {
    Action { tool: tool.into(), args: args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }
}

#[test]
fn a_direct_order_is_billed_with_gst_and_the_pdf_goes_to_its_customer() {
    let mut b = shop("bill");
    let said = wa_order(&mut b, "W-1", "2026-11-13T13:05", r#"[{"code":"R3","qty":1},{"name":"hot wings","qty":1}]"#, "upi");
    assert!(said.contains("bill VWC/26-27/00001 (₹389) sent to La*** ••••••45"), "{said}");
    let o = b.order("W-1").unwrap().clone();
    let v = b.values(&o);
    // 160 + 180 + 30 delivery = 370; 2.5% + 2.5% = 18.50; 388.50 rounds to 389
    assert_eq!((v.items, v.delivery, v.taxable), (34_000, 3_000, 37_000));
    assert_eq!((v.cgst, v.sgst, v.round_off, v.total), (925, 925, 50, 38_900));
    let sent = b.wa.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "+91 90000 12345");
    match &sent[0].1 {
        Msg::Document { name, bytes, caption } => {
            assert_eq!(name, "VWC-26-27-00001.pdf");
            let pdf = String::from_utf8_lossy(bytes);
            assert!(pdf.starts_with("%PDF"));
            for want in ["TAX INVOICE", "GSTIN 37AAAAA0000A1Z5", "Chicken Fried Rice", "Hot Wings 6pc", "CGST 2.5%", "Rs 389", "Lakshmi Prasanna"] {
                assert!(pdf.contains(want), "bill lacks {want}");
            }
            assert!(caption.contains("₹389"));
        }
        other => panic!("not a document: {other:?}"),
    }
}

#[test]
fn a_platform_order_pays_out_less_commission_its_gst_and_tds_and_is_not_billed_by_us() {
    let mut b = shop("platform");
    play(&mut b, r#"{"t":"order","id":"SW-1","channel":"swiggy","at":"2026-11-13T13:00","items":[{"code":"R3","qty":1}],"locality":"Gajuwaka","km":14}"#).unwrap();
    let v = b.values(b.order("SW-1").unwrap());
    assert_eq!((v.taxable, v.commission, v.commission_gst, v.tds, v.payout), (16_000, 4_000, 720, 16, 11_264));
    assert!(b.send_bill("SW-1").unwrap_err().contains("swiggy issues"));
    assert!(b.wa.sent().is_empty());
    // a platform order has no customer on this phone at all
    assert!(b.book.customers.is_empty());
}

#[test]
fn a_typed_whatsapp_order_is_read_against_the_menu() {
    let p = profile();
    assert_eq!(
        parse_order(&p, "2 chicken fried rice, wings x1 and a veg noodles").unwrap(),
        vec![("R3".to_string(), 2), ("C3".to_string(), 1), ("N1".to_string(), 1)]
    );
    assert_eq!(parse_order(&p, "one bucket\n2x popcorn").unwrap(), vec![("C2".to_string(), 1), ("C4".to_string(), 2)]);
    assert_eq!(parse_order(&p, "schezwan chicken fried rice").unwrap(), vec![("R4".to_string(), 1)]);
    assert_eq!(parse_order(&p, "crispy fried chicken 2pc x3").unwrap(), vec![("C1".to_string(), 3)]);
    // "fried rice" alone could be five dishes: ask, do not guess
    assert!(parse_order(&p, "2 fried rice").unwrap_err().contains("unclear"));
    assert!(parse_order(&p, "1 biryani").unwrap_err().contains("biryani"));
}

#[test]
fn the_rider_is_tracked_only_while_carrying_and_only_that_customer_sees_it() {
    let mut b = shop("track");
    wa_order(&mut b, "W-1", "2026-11-13T13:05", r#"[{"code":"C1","qty":2}]"#, "cod");
    // not out yet: nothing to share, and a position is not kept
    assert!(b.share_location("W-1").unwrap_err().contains("not out for delivery"));
    assert!(play(&mut b, r#"{"t":"gps","rider":"R1","lat":17.73,"lon":83.31,"at":"2026-11-13T13:10"}"#).unwrap().contains("not kept"));
    assert!(b.book.rider_fix.is_empty());
    assert!(play(&mut b, r#"{"t":"status","id":"W-1","status":"out","at":"2026-11-13T13:18"}"#).unwrap_err().contains("assign a rider"));
    play(&mut b, r#"{"t":"assign","id":"W-1","rider":"R1","at":"2026-11-13T13:18"}"#).unwrap();
    let out = play(&mut b, r#"{"t":"status","id":"W-1","status":"out","at":"2026-11-13T13:20"}"#).unwrap();
    assert!(out.contains("Ravi's location sent to La***"), "{out}");
    // a fix a minute later is kept but not re-sent (at most one every three minutes)
    play(&mut b, r#"{"t":"gps","rider":"R1","lat":17.735,"lon":83.32,"at":"2026-11-13T13:21"}"#).unwrap();
    let locs = |b: &Business| b.wa.sent().iter().filter(|(_, m)| matches!(m, Msg::Location { .. })).count();
    assert_eq!(locs(&b), 1);
    play(&mut b, r#"{"t":"gps","rider":"R1","lat":17.74,"lon":83.33,"at":"2026-11-13T13:25"}"#).unwrap();
    assert_eq!(locs(&b), 2);
    // every location went to the customer of the order, nobody else
    for (to, m) in b.wa.sent() {
        if let Msg::Location { lat, label, .. } = m {
            assert_eq!(to, "+91 90000 12345");
            assert!(*lat > 17.0 && label.contains("Ravi") && label.contains("W-1"));
        }
    }
    // the rider got the address (they need it); it went to the rider's own number
    assert!(b.wa.sent().iter().any(|(to, m)| to == "+91 90000 00101" && matches!(m, Msg::Text(t) if t.contains("Sea Breeze") && t.contains("collect ₹389"))));
    play(&mut b, r#"{"t":"status","id":"W-1","status":"delivered","at":"2026-11-13T13:40"}"#).unwrap();
    assert!(b.book.rider_fix.is_empty(), "a rider with nothing to carry is not tracked");
    assert!(b.share_location("W-1").is_err());
}

#[test]
fn riders_are_settled_to_the_rupee_and_cannot_be_overpaid() {
    let mut b = shop("riders");
    for (id, pay) in [("W-1", "cod"), ("W-2", "upi")] {
        wa_order(&mut b, id, "2026-11-13T13:05", r#"[{"code":"C1","qty":2}]"#, pay);
        play(&mut b, &format!(r#"{{"t":"assign","id":"{id}","rider":"Ravi","at":"2026-11-13T13:18"}}"#)).unwrap();
        play(&mut b, &format!(r#"{{"t":"status","id":"{id}","status":"out","at":"2026-11-13T13:20"}}"#)).unwrap();
        play(&mut b, &format!(r#"{{"t":"status","id":"{id}","status":"delivered","at":"2026-11-13T13:40"}}"#)).unwrap();
    }
    let s = b.rider_sheet("2026-11-13");
    // two drops of 4.5 km: 2 x (25 + 27) = 104; one was cash: 340 + 30 + 5% = 388.50 -> 389
    assert_eq!((s[0].deliveries, s[0].earnings, s[0].cash_held, s[0].due), (2, 10_400, 38_900, 10_400));
    assert_eq!(s[1].deliveries, 0);
    assert!(b.pay_rider("R1", 20_000).unwrap_err().contains("is due ₹104"));
    assert!(b.pay_rider("R2", 100).unwrap_err().contains("nothing due"));
    assert!(b.cash_in("R1", 50_000).unwrap_err().contains("holds ₹389"));
    b.cash_in("R1", 38_900).unwrap();
    assert!(b.pay_rider("R1", 10_400).unwrap().contains("payslip sent"));
    let s = b.rider_sheet("2026-11-13");
    assert_eq!((s[0].due, s[0].cash_held, s[0].paid), (0, 0, 10_400));
    assert!(b.pay_rider("R1", 10_400).is_err(), "paid once");
}

fn month(tag: &str, scheme: &str) -> Business {
    let mut p = profile();
    p.gst_scheme = scheme.into();
    let mut b = Business::open(p, dir(tag), &[9u8; 32], Box::new(SimWhatsapp::default())).unwrap();
    let mut text = String::new();
    for d in 0..14 {
        let day = add_days("2026-10-12", d);
        text.push_str(&day_events(&b.profile, &day, 7, 1.0, 0.0).join("\n"));
        text.push('\n');
    }
    let (ok, bad) = import(&mut b, &text).unwrap();
    assert!(ok > 3000 && bad.is_empty(), "{ok} applied, errors: {bad:?}");
    b
}

#[test]
fn platforms_are_reconciled_and_only_the_exact_shortfall_can_be_claimed() {
    let mut b = month("recon", "restaurant_5");
    for pf in ["swiggy", "zomato"] {
        let cut = if pf == "zomato" { 480.0 } else { 0.0 };
        let ev = payout_event(&b, pf, "2026-10-12", "2026-10-18", "2026-10-21T11:00", cut);
        play(&mut b, &ev).unwrap();
    }
    let r = b.reconcile();
    assert_eq!(r.len(), 2);
    let z = r.iter().find(|r| r.platform == "zomato").unwrap();
    // short by the orders cancelled after cooking plus the deduction nobody agreed to
    assert_eq!(z.short(), z.cancelled_amount + 48_000);
    let (can, why) = b.claimable("zomato");
    assert_eq!(can, z.short());
    assert!(why.contains("deductions not in the agreement (₹480)"), "{why}");
    assert!(b.claim("zomato", can + 100).unwrap_err().contains("is short by"));
    let done = b.claim("Zomato", can).unwrap();
    assert!(done.contains("CLM-ZOMATO-1"), "{done}");
    assert_eq!(b.claimable("zomato").0, 0);
    assert!(b.claim("zomato", can).unwrap_err().contains("nothing to claim"));
    // the second week has not been paid: it is awaiting, not short
    assert!(b.awaiting("zomato") > 0);
    assert!(b.payouts_text().contains("SHORT"));
}

#[test]
fn gst_separates_what_we_pay_from_what_platforms_pay_and_credit_follows_the_scheme() {
    let mut b = month("gst5", "restaurant_5");
    let g = b.gst("2026-10");
    assert!(g.invoices > 100 && g.direct_taxable > 0);
    assert_eq!(g.direct_cgst, g.direct_sgst);
    // each invoice rounds its own tax, so the total is within a paisa per invoice of 2.5%
    assert!((g.direct_cgst - pct(g.direct_taxable, 2.5)).abs() <= g.invoices as i64);
    assert_eq!(g.eco.len(), 2);
    assert!(g.input_purchases > 0 && g.input_commission > 0);
    // 5% restaurant rate: no credit; all input tax is a cost and the whole output tax is paid
    assert_eq!((g.itc_claimable, g.itc_blocked), (0, g.input_purchases + g.input_commission));
    assert_eq!(g.payable, g.direct_cgst + g.direct_sgst);
    // a month in progress cannot be filed
    assert!(b.file_gst("2026-10").unwrap_err().contains("not over"));
    b.tick("2026-11-02T10:00");
    let said = b.file_gst("2026-10").unwrap();
    assert!(said.contains("simulated portal"), "{said}");
    assert!(b.file_gst("2026-10").unwrap_err().contains("already filed"));
    let r1 = std::fs::read_to_string(std::env::temp_dir().join(format!("biz-gst5-{}", std::process::id())).join("returns/gstr1-2026-10.json")).unwrap();
    assert!(r1.contains("supplies_through_eco_9_5") && r1.contains("37AAAAA0000A1Z5"));
    // the return is totals: no customer is in it
    assert!(!r1.contains("90000") && !r1.contains("Residency"));

    let c = month("gst18", "regular_18");
    let h = c.gst("2026-10");
    assert!(h.itc_claimable > 0 && h.itc_blocked == 0);
    assert_eq!(h.payable, h.direct_cgst + h.direct_sgst - h.itc_claimable);
}

#[test]
fn the_plan_follows_the_weekday_and_buys_whole_packs() {
    let b = month("plan", "restaurant_5");
    // the book ends on Sunday 2026-10-25, so the plan is for a Monday
    assert_eq!(b.today(), "2026-10-25");
    let p = agentos_business::plan::plan(&b);
    assert_eq!(WEEKDAYS[weekday(&p.for_day)], "Monday");
    let (f, t): (u32, u32) = p.items.iter().fold((0, 0), |a, i| (a.0 + i.forecast, a.1 + i.today));
    assert!(f < t, "a Monday is quieter than a Sunday: {f} vs {t}");
    assert_eq!(p.items[0].code, "R3", "the best seller leads");
    let chicken = p.buy.iter().find(|x| x.name == "chicken").unwrap();
    assert!(chicken.packs as f64 >= chicken.need && (chicken.packs as f64) < chicken.need + 1.0);
    assert!(p.peak[0].0 == 20 || p.peak[0].0 == 13, "rush at lunch or dinner: {:?}", p.peak);
    let text = p.text();
    assert!(text.contains("Not sold in a week") || text.contains("Crispy Chicken Burger"), "{text}");
    assert!(text.contains("Of every ₹100 sold"), "{text}");
}

#[test]
fn nothing_about_a_customer_is_on_disk_or_in_any_tool_answer() {
    let d = dir("privacy");
    let mut b = Business::open(profile(), d.clone(), &[9u8; 32], Box::new(SimWhatsapp::default())).unwrap();
    b.tick("2026-11-13T12:00");
    wa_order(&mut b, "W-1", "2026-11-13T13:05", r#"[{"code":"R3","qty":1}]"#, "cod");
    play(&mut b, r#"{"t":"assign","id":"W-1","rider":"R1","at":"2026-11-13T13:18"}"#).unwrap();
    play(&mut b, r#"{"t":"status","id":"W-1","status":"out","at":"2026-11-13T13:20"}"#).unwrap();
    let secrets = ["Lakshmi", "Prasanna", "9000012345", "90000 12345", "Sea Breeze", "Flat 204"];
    // on disk: the book file and every day file
    fn files(p: &std::path::Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(p).unwrap().flatten() {
            if e.path().is_dir() { files(&e.path(), out) } else { out.push(e.path()) }
        }
    }
    let mut all = Vec::new();
    files(&d, &mut all);
    assert!(all.iter().any(|f| f.ends_with("days/2026-11-13.sealed")) && all.iter().any(|f| f.ends_with("book.sealed")), "{all:?}");
    for f in &all {
        let bytes = std::fs::read(f).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        for s in secrets {
            assert!(!text.contains(s), "{s} is on disk in the clear in {}", f.display());
        }
    }
    // in every read-only answer and in the dashboard's data
    let shared = Rc::new(RefCell::new(b));
    let mut tools = BusinessTools(shared.clone());
    for t in tools.tools().iter().filter(|t| t.tier == Tier::Low) {
        let args: Vec<(&str, &str)> = t.args.iter().map(|a| (*a, "2026-11")).collect();
        let out = tools.call(&act(t.name, &args)).unwrap();
        for s in secrets {
            assert!(!out.contains(s), "{} leaks {s}: {out}", t.name);
        }
    }
    let state = shared.borrow().state().to_string();
    for s in secrets {
        assert!(!state.contains(s), "the dashboard data leaks {s}");
    }
    // the log of what left the phone holds masks only
    for s in secrets {
        assert!(!serde_json::to_string(&shared.borrow().book.outbox).unwrap().contains(s));
    }
    // the same book opens again with the key and not with another
    drop(tools);
    let b = Rc::try_unwrap(shared).ok().unwrap().into_inner();
    drop(b);
    assert!(Business::open(profile(), d.clone(), &[1u8; 32], Box::new(SimWhatsapp::default())).is_err());
    let again = Business::open(profile(), d, &[9u8; 32], Box::new(SimWhatsapp::default())).unwrap();
    assert_eq!(again.book.orders.len(), 1);
    assert_eq!(again.book.customers.values().next().unwrap().name, "Lakshmi Prasanna");
}

#[test]
fn customers_are_erased_after_the_limit_and_the_books_stay_whole() {
    let mut b = month("forget", "restaurant_5");
    let before = b.gst("2026-10");
    let (customers, orders) = (b.book.customers.len(), b.book.orders.len());
    assert!(customers > 20);
    b.tick("2026-11-20T10:00");
    let said = b.forget(30).unwrap();
    let kept = b.book.customers.len();
    assert!(kept < customers && said.contains("erased"), "{said}");
    assert!(b.book.customers.values().all(|c| c.last_day.as_str() >= "2026-10-21"));
    assert_eq!(b.book.orders.len(), orders);
    let after = b.gst("2026-10");
    assert_eq!((before.direct_taxable, before.invoices), (after.direct_taxable, after.invoices));
    // an erased customer's old order can no longer be billed to anyone
    let old = b.book.orders.iter().find(|o| b.is_direct(o) && o.customer.as_ref().map(|t| !b.book.customers.contains_key(t)).unwrap_or(false)).unwrap().id.clone();
    assert!(b.send_bill(&old).unwrap_err().contains("erased"));
    assert_eq!(b.who(b.order(&old).unwrap()), "(erased)");
}

#[test]
fn bad_orders_are_refused_with_a_reason() {
    let mut b = shop("bad");
    wa_order(&mut b, "W-1", "2026-11-13T13:05", r#"[{"code":"R3","qty":1}]"#, "upi");
    let dup = play(&mut b, &format!(r#"{{"t":"order","id":"W-1","channel":"whatsapp","items":[{{"code":"R3","qty":1}}],{LAKSHMI},"locality":"MVP Colony","km":4.5}}"#));
    assert!(dup.unwrap_err().contains("already in the book"));
    let far = play(&mut b, &format!(r#"{{"t":"order","id":"W-2","channel":"whatsapp","items":[{{"code":"R3","qty":1}}],{LAKSHMI},"locality":"Gajuwaka","km":14}}"#));
    assert!(far.unwrap_err().contains("we deliver up to 8 km"));
    let nophone = play(&mut b, r#"{"t":"order","id":"W-3","channel":"whatsapp","items":[{"code":"R3","qty":1}],"locality":"Siripuram","km":2}"#);
    assert!(nophone.unwrap_err().contains("needs the customer's number"));
    let menu = play(&mut b, r#"{"t":"order","id":"ZO-9","channel":"zomato","items":[{"code":"X9","name":"mutton biryani","qty":1}],"locality":"Siripuram","km":2}"#);
    assert!(menu.unwrap_err().contains("not on the menu"));
    assert_eq!(b.book.orders.len(), 1);
    assert!(b.set_status("W-1", "teleported", None).is_err());
    b.set_status("W-1", "cancelled", None).unwrap();
    assert!(b.set_status("W-1", "delivered", None).unwrap_err().contains("already cancelled"));
}

#[test]
fn the_generator_is_repeatable() {
    let p = profile();
    assert_eq!(day_events(&p, "2026-11-13", 7, 1.0, 0.0), day_events(&p, "2026-11-13", 7, 1.0, 0.0));
    assert_ne!(day_events(&p, "2026-11-13", 7, 1.0, 0.0), day_events(&p, "2026-11-13", 8, 1.0, 0.0));
    assert_ne!(day_events(&p, "2026-11-13", 7, 1.0, 0.0).len(), day_events(&p, "2026-11-14", 7, 1.0, 0.0).len());
}

#[test]
fn an_event_rewrites_only_its_own_day_and_the_book_reopens_whole() {
    let d = std::env::temp_dir().join(format!("biz-days-{}", std::process::id()));
    let mut b = month("days", "restaurant_5");
    let stamp = |day: &str| std::fs::metadata(d.join("days").join(format!("{day}.sealed"))).unwrap().modified().unwrap();
    assert_eq!(std::fs::read_dir(d.join("days")).unwrap().count(), 14, "one sealed file per day of orders");
    let (old, orders, sales) = (stamp("2026-10-12"), b.book.orders.len(), b.gst("2026-10").direct_taxable);
    // the small book file holds no orders, so it does not grow with the months
    assert!(std::fs::metadata(d.join("book.sealed")).unwrap().len() < 200_000);
    std::thread::sleep(std::time::Duration::from_millis(20));
    wa_order(&mut b, "W-NEW", "2026-10-26T13:05", r#"[{"code":"R3","qty":1}]"#, "upi");
    assert_eq!(stamp("2026-10-12"), old, "an old day is not rewritten by a new order");
    assert!(d.join("days/2026-10-26.sealed").exists());
    drop(b);
    let again = Business::open(profile(), d.clone(), &[9u8; 32], Box::new(SimWhatsapp::default())).unwrap();
    assert_eq!(again.book.orders.len(), orders + 1);
    assert_eq!(again.gst("2026-10").direct_taxable, sales + 19_000);
    assert_eq!(again.book.outbox.len(), 1, "the sent-log comes back with its day");
    assert_eq!(again.order("W-NEW").unwrap().invoice_no, again.book.orders.last().unwrap().invoice_no);
}
