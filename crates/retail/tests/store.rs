//! The store in examples/retail/profile.toml: rates, the audit scored against planted faults,
//! the month's return, fixes, the year, and what is (not) kept on disk.
use agentos_business::util::{add_days, rs};
use agentos_retail::books::Books;
use agentos_retail::gen::{family, inward, Gen};
use agentos_retail::{audit_day, Bill, Line, Memory, Profile};
use std::collections::BTreeSet;
use std::path::PathBuf;

fn profile() -> Profile {
    Profile::load(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/retail/profile.toml")).unwrap()
}
fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("retail-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}
fn line(p: &Profile, sku: &str, day: &str) -> Line {
    let (c, price) = p.sku(sku).unwrap();
    let rate = p.rate(&c.hsn, day, price).unwrap();
    let taxable = (price as f64 * 100.0 / (100.0 + rate)).round() as i64;
    Line { sku: sku.into(), hsn: c.hsn.clone(), qty: 1, price, rate, taxable, tax: price - taxable }
}
fn bill(no: u64, lines: Vec<Line>) -> Bill {
    let total: i64 = lines.iter().map(|l| l.price * l.qty as i64).sum();
    Bill { no, at: "2025-06-14T12:00".into(), till: 1, lines, total, upi: total, ..Bill::default() }
}
fn kinds(p: &Profile, day: &str, bills: &[Bill]) -> Vec<(String, u64, i64)> {
    let mut mem = Memory::default();
    let total_cash: i64 = bills.iter().map(|b| b.cash).sum();
    audit_day(p, day, bills, total_cash, &mut mem).exceptions.into_iter().map(|x| (x.kind, x.bill, x.tax_effect)).collect()
}

#[test]
fn the_rate_follows_the_hsn_the_value_before_tax_and_the_date() {
    let p = profile();
    let r = |sku: &str, day: &str| {
        let (c, price) = p.sku(sku).unwrap();
        p.rate(&c.hsn, day, price).unwrap()
    };
    // clothing before 22 Sep 2025: 5% up to ₹1,000 before tax, 12% above
    assert_eq!(r("SH-999", "2025-06-14"), 5.0);
    assert_eq!(r("SH-1049", "2025-06-14"), 5.0, "₹1,049 on the tag is ₹999.05 before tax at 5%");
    assert_eq!(r("JN-1299", "2025-06-14"), 12.0);
    // from 22 Sep 2025: 5% up to ₹2,500, 18% above
    assert_eq!(r("JN-1299", "2025-09-22"), 5.0);
    assert_eq!(r("KU-2599", "2025-09-22"), 5.0, "₹2,599 on the tag is ₹2,475.24 before tax");
    assert_eq!(r("KU-2599", "2025-09-21"), 12.0);
    assert_eq!(r("JK-2999", "2025-09-22"), 18.0);
    // footwear moved from 12% to 5% under the slab; bags stay at 18%
    assert_eq!((r("FW-999", "2025-06-14"), r("FW-999", "2025-10-01"), r("FW-1299", "2025-06-14")), (12.0, 5.0, 18.0));
    assert_eq!((r("BG-699", "2025-06-14"), r("BG-699", "2025-10-01")), (18.0, 18.0));
    assert!(p.sku("TS-123").is_none() && p.sku("ZZ-199").is_none());
}

#[test]
fn each_kind_of_fault_is_named_with_its_tax_effect() {
    let p = profile();
    let day = "2025-06-14";
    let clean = bill(1, vec![line(&p, "TS-399", day), line(&p, "JN-1299", day)]);
    assert!(kinds(&p, day, &[clean.clone()]).is_empty());

    // 12% charged on a ₹999 shirt: ₹59.46 over-charged (107.04 − 47.57)
    let mut b = bill(1, vec![line(&p, "SH-999", day)]);
    b.lines[0].rate = 12.0;
    b.lines[0].taxable = 89_196;
    b.lines[0].tax = 10_704;
    assert_eq!(kinds(&p, day, &[b]), vec![("wrong_rate".to_string(), 1, 10_704 - 4_757)]);
    // 5% on ₹1,299 jeans: short-charged
    let mut b = bill(1, vec![line(&p, "JN-1299", day)]);
    b.lines[0].rate = 5.0;
    b.lines[0].taxable = 123_714;
    b.lines[0].tax = 6_186;
    assert_eq!(kinds(&p, day, &[b]), vec![("wrong_rate".to_string(), 1, 6_186 - 13_918)]);
    // the slab read off the tag: ₹1,049 charged 12%
    let mut b = bill(1, vec![line(&p, "SH-1049", day)]);
    b.lines[0].rate = 12.0;
    b.lines[0].taxable = 93_661;
    b.lines[0].tax = 11_239;
    assert_eq!(kinds(&p, day, &[b])[0].0, "slab_on_tag");
    // the day after the change, still on the old rule
    let mut b = bill(1, vec![line(&p, "JN-1299", "2025-09-21")]);
    assert_eq!(b.lines[0].rate, 12.0);
    b.at = "2025-09-22T11:00".into();
    assert_eq!(kinds(&p, "2025-09-22", &[b])[0].0, "old_rate");

    let mut b = bill(1, vec![line(&p, "TS-399", day)]);
    b.lines[0].tax += 300;
    b.lines[0].taxable -= 300;
    assert_eq!(kinds(&p, day, &[b]), vec![("tax_arithmetic".to_string(), 1, 300)]);
    let mut b = bill(1, vec![line(&p, "TS-399", day)]);
    b.lines[0].hsn.clear();
    assert_eq!(kinds(&p, day, &[b])[0].0, "hsn_missing");
    let mut b = bill(1, vec![line(&p, "TS-399", day)]);
    b.lines[0].hsn = "4202".into();
    assert_eq!(kinds(&p, day, &[b]), vec![("hsn_wrong".to_string(), 1, 0)], "judged by the catalogue's HSN, so no rate exception on top");
    let mut b = bill(1, vec![line(&p, "TS-399", day)]);
    b.total += 1000;
    b.upi = b.total;
    assert_eq!(kinds(&p, day, &[b])[0].0, "total_mismatch");
    let mut b = bill(1, vec![line(&p, "TS-399", day)]);
    b.upi -= 5000;
    assert_eq!(kinds(&p, day, &[b])[0].0, "payment_mismatch");
    // the series
    let k = kinds(&p, day, &[bill(1, vec![line(&p, "TS-399", day)]), bill(1, vec![line(&p, "TS-299", day)]), bill(4, vec![line(&p, "TS-199", day)])]);
    assert_eq!(k.iter().map(|x| (x.0.as_str(), x.1)).collect::<Vec<_>>(), vec![("duplicate_no", 1), ("gap", 2)]);
    // a registered buyer
    let mut b = bill(1, vec![line(&p, "TS-399", day)]);
    b.buyer_gstin = Some("37AABCS1111A1ZX".into());
    assert_eq!(kinds(&p, day, &[b])[0].0, "gstin_invalid");
    let mut b = bill(1, vec![line(&p, "TS-399", day)]);
    b.buyer_gstin = Some(agentos_retail::gstin_make("29AAECR4444D1Z"));
    assert_eq!(kinds(&p, day, &[b.clone()])[0].0, "place_of_supply", "a Karnataka buyer needs IGST");
    b.igst = true;
    assert!(kinds(&p, day, &[b]).is_empty());
    // returns: against a bill we know, at that bill's rate even after the rates changed
    let mut mem = Memory::default();
    let sold = bill(1, vec![line(&p, "JN-1299", "2025-09-20")]);
    assert!(audit_day(&p, "2025-09-20", &[sold.clone()], 0, &mut mem).exceptions.is_empty());
    let mut back = bill(2, sold.lines.clone());
    back.return_of = Some(1);
    back.total = -back.total;
    back.upi = back.total;
    let d = audit_day(&p, "2025-09-25", &[back.clone()], 0, &mut mem);
    assert!(d.exceptions.is_empty(), "refunded at the 12% it was sold at: {:?}", d.exceptions);
    assert_eq!((d.taxable(), d.tax()), (-sold.lines[0].taxable, -sold.lines[0].tax));
    let mut ghost = back.clone();
    ghost.no = 3;
    ghost.return_of = Some(777);
    let mut big = back.clone();
    big.no = 4;
    big.lines[0].qty = 3;
    big.total = -3 * 129_900;
    big.upi = big.total;
    let d = audit_day(&p, "2025-09-26", &[ghost, big], 0, &mut mem);
    assert_eq!(d.exceptions.iter().map(|x| x.kind.as_str()).collect::<Vec<_>>(), vec!["return_unmatched", "tax_arithmetic", "return_excess"]);
    // the same bill refunded twice is caught on the second refund, though each fits on its own
    let mut mem = Memory::default();
    audit_day(&p, "2025-09-20", &[sold.clone()], 0, &mut mem);
    let mut second = back.clone();
    second.no = 3;
    let d = audit_day(&p, "2025-09-25", &[back.clone(), second], 0, &mut mem);
    assert_eq!(d.exceptions.iter().map(|x| (x.kind.as_str(), x.bill)).collect::<Vec<_>>(), vec![("return_excess", 3)]);
    assert!(d.exceptions[0].detail.contains("already refunded"), "{}", d.exceptions[0].detail);
    // a second bill issued under a used number does not become the bill a later return is judged against
    let mut mem = Memory::default();
    let cheap = bill(1, vec![line(&p, "TS-199", "2025-09-20")]);
    let d = audit_day(&p, "2025-09-20", &[sold.clone(), cheap], 0, &mut mem);
    assert_eq!(d.exceptions[0].kind, "duplicate_no");
    let d = audit_day(&p, "2025-09-25", &[back.clone()], 0, &mut mem);
    assert!(d.exceptions.is_empty(), "the refund of the ₹1,299 jeans is fine: {:?}", d.exceptions);
    // the till's cash
    let mut b = bill(1, vec![line(&p, "TS-399", day)]);
    b.upi = 0;
    b.cash = b.total;
    let mut mem = Memory::default();
    let d = audit_day(&p, day, &[b], 0, &mut mem);
    assert_eq!(d.exceptions[0].kind, "cash_short");
}

/// Run days through the generator and the auditor; return (planted, found) as (bill, family).
fn score(p: &Profile, from: &str, days: i64, seed: u64) -> (BTreeSet<(String, u64, String)>, BTreeSet<(String, u64, String)>, Vec<String>, f64) {
    let (mut g, mut mem) = (Gen::new(seed), Memory::default());
    let (mut planted, mut found, mut fine, mut ms) = (BTreeSet::new(), BTreeSet::new(), Vec::new(), 0.0);
    for i in 0..days {
        let day = add_days(from, i);
        let d = g.day(p, &day);
        for t in &d.truth {
            planted.insert((day.clone(), t.bill, t.kind.to_string()));
        }
        let book = audit_day(p, &day, &d.bills, d.cash_banked, &mut mem);
        ms += book.audit_ms;
        for x in &book.exceptions {
            found.insert((day.clone(), x.bill, family(&x.kind).to_string()));
            fine.push(x.kind.clone());
        }
    }
    (planted, found, fine, ms / days as f64)
}

#[test]
fn every_planted_fault_is_found_and_nothing_else_is_reported() {
    let p = profile();
    // June has the till that reads the slab off the tag; 20-25 September has the rate change
    for (from, days, must) in [("2025-06-01", 6, "slab_on_tag"), ("2025-09-19", 7, "old_rate"), ("2026-01-10", 4, "wrong_rate")] {
        let (planted, found, fine, ms) = score(&p, from, days, 11);
        let missed: Vec<_> = planted.difference(&found).collect();
        let extra: Vec<_> = found.difference(&planted).collect();
        assert!(missed.is_empty(), "{from}: planted but not found: {:?}", &missed[..missed.len().min(5)]);
        assert!(extra.is_empty(), "{from}: reported but not planted: {:?}", &extra[..extra.len().min(5)]);
        assert!(planted.len() > 40, "{from}: only {} faults planted", planted.len());
        assert!(fine.iter().any(|k| k == must), "{from}: no {must} among {:?}", fine.iter().collect::<BTreeSet<_>>());
        assert!(ms < 200.0, "a day of ~5,000 bills audited in {ms:.0} ms");
    }
}

fn june(tag: &str, days: i64) -> Books {
    let p = profile();
    let mut b = Books::open(p, dir(tag), &[5u8; 32]).unwrap();
    let mut g = Gen::new(11);
    for i in 0..days {
        let day = add_days("2025-06-01", i);
        let d = g.day(&b.profile, &day);
        b.import_day(&day, &d.bills, d.cash_banked).unwrap();
    }
    let m = b.month("2025-06");
    let rows = inward(&b.profile, "2025-06", (m.taxable() as f64 * 0.55) as i64, m.taxable(), m.taxable(), 11);
    b.add_inward(rows).unwrap();
    b
}

#[test]
fn the_month_adds_up_and_credit_is_only_what_the_law_allows() {
    let b = june("month", 5);
    let m = b.month("2025-06");
    assert!(m.bills > 20_000 && m.days == 5);
    // the tax by rate is the tax by head (halves of a paisa aside), and by HSN
    assert!((m.billed_tax() - (m.cgst + m.sgst + m.igst)).abs() <= m.bills as i64);
    assert_eq!(m.billed_tax(), m.by_hsn.values().map(|v| v.2).sum::<i64>());
    assert_eq!(m.taxable(), m.by_hsn.values().map(|v| v.1).sum::<i64>());
    assert!(m.short_charged > 0 && m.over_charged > 0);
    assert_eq!(m.output(), m.billed_tax() + m.short_charged, "short-charged tax is owed on top; over-charged tax stays payable");
    // credit: staff food is blocked, the invoice entered twice is refused once, the unreported wait
    let all: i64 = b.itc("2025-06").eligible + m.itc.blocked + m.itc.not_in_2b + m.itc.duplicate;
    assert!(m.itc.eligible > 0 && m.itc.blocked == rs(120_000.0 * 0.05));
    assert!(m.itc.eligible < all);
    let text = b.gst_text("2025-06");
    assert!(text.contains("Input tax credit: claim") && text.contains("Not filed"), "{text}");
}

#[test]
fn fixes_are_proposed_by_anyone_and_change_nothing_until_applied() {
    let mut b = june("fixes", 4);
    let before = b.month("2025-06");
    assert!(b.propose("2025-06", "slab_on_tag", "teleport", "x").unwrap_err().contains("is not a fix"));
    let said = b.propose("2025-06", "slab_on_tag", "credit_note", "a Claude client").unwrap();
    assert!(said.contains("nothing changes until the owner approves"), "{said}");
    assert_eq!(b.month("2025-06").output(), before.output(), "a proposal is not a change");
    assert!(b.audit_text("2025-06").contains("waiting for the owner's key"));
    // proposing the same again adds nothing
    assert!(b.propose("2025-06", "slab_on_tag", "credit_note", "x").unwrap().starts_with("0 "));
    let done = b.apply("2025-06").unwrap();
    assert!(done.contains("× credit_note"), "{done}");
    let after = b.month("2025-06");
    let slab_over: i64 = b.exceptions("2025-06", "slab_on_tag").iter().map(|x| x.tax_effect).sum();
    assert!(slab_over > 0);
    assert_eq!(after.credit_notes, slab_over);
    assert_eq!(after.output(), before.output() - slab_over, "tax returned to customers by credit note is no longer payable");
    assert!(b.apply("2025-06").unwrap_err().contains("no proposed fixes"));
    // the month cannot be filed before it is over; once filed, it cannot be changed here
    assert!(b.file_gst("2025-06").unwrap_err().contains("not over"));
    b.tick("2025-07-05T10:00");
    assert!(b.file_gst("2025-06").unwrap().contains("simulated portal"));
    assert!(b.file_gst("2025-06").unwrap_err().contains("already filed"));
    b.propose("2025-06", "tax_arithmetic", "recompute", "x").unwrap();
    assert!(b.apply("2025-06").unwrap_err().contains("is filed"));
}

#[test]
fn the_books_keep_totals_not_bills_or_shoppers_and_reopen_whole() {
    let d = std::env::temp_dir().join(format!("retail-disk-{}", std::process::id()));
    let b = june("disk", 3);
    let (taxable, n) = (b.month("2025-06").taxable(), b.exceptions("2025-06", "").len());
    assert!(b.days.values().map(|x| x.loyalty_seen).sum::<u32>() > 500, "phones were seen");
    let mut size = 0;
    for f in std::fs::read_dir(d.join("days")).unwrap().flatten().chain(std::fs::read_dir(&d).unwrap().flatten()).chain(std::fs::read_dir(d.join("recent")).unwrap().flatten()) {
        let bytes = std::fs::read(f.path()).unwrap_or_default();
        size += bytes.len();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("90000") && !text.contains("VVF/") && !text.contains("exceptions"), "{} is readable", f.path().display());
    }
    assert!(size < 900_000, "three days of 5,000 bills keep {size} bytes: totals, exceptions, and bill numbers for matching returns");
    // a day cannot be imported twice, or out of order
    let mut b = b;
    assert!(b.import_day("2025-06-02", &[], 0).unwrap_err().contains("already in the books"));
    assert!(b.import_day("2025-05-30", &[], 0).unwrap_err().contains("days go in order"));
    drop(b);
    assert!(Books::open(profile(), d.clone(), &[6u8; 32]).is_err(), "another key does not open it");
    let again = Books::open(profile(), d, &[5u8; 32]).unwrap();
    assert_eq!((again.month("2025-06").taxable(), again.exceptions("2025-06", "").len()), (taxable, n));
}

#[test]
fn the_year_tallies_and_the_tax_follows_the_disallowances() {
    let mut b = june("year", 3);
    let y = b.year();
    assert_eq!(y.gross_billed - y.turnover, b.month("2025-06").billed_tax());
    // money received differs from money billed only by what the audit already named
    let named = b.exceptions("2025-06", "").iter().filter(|x| ["cash_short", "payment_mismatch", "total_mismatch"].contains(&x.kind.as_str())).count();
    assert!((y.gross_billed - y.received).abs() <= named as i64 * rs(5000.0), "{} vs {}", y.gross_billed, y.received);
    assert!(y.cogs > 0 && y.purchases > 0);
    assert_eq!(y.cess, ((y.tax + y.surcharge) as f64 * 0.04).round() as i64);
    // the electrician paid ₹18,500 in cash is added back in full
    assert!(y.disallowed.iter().any(|(k, v)| k.contains("cash payments") && *v == rs(18_500.0)), "{:?}", y.disallowed);
    assert!(b.file_itr().unwrap_err().contains("finished year"));
    b.tick("2026-04-10T10:00");
    assert!(b.file_itr().unwrap_err().contains("GST returns are not filed for 2025-06"));
    b.file_gst("2025-06").unwrap();
    assert!(b.file_itr().unwrap().contains("income-tax computation lodged"));
    let text = b.year_text();
    for want in ["Tally with money received", "Tally with the GST returns", "rent", "salaries", "taxable income", "Income tax at 22%"] {
        assert!(text.contains(want), "year lacks {want}:\n{text}");
    }
}

#[test]
fn a_restart_does_not_lose_the_series_or_the_bills_returns_refer_to() {
    let p = profile();
    let d = dir("restart");
    let mut g = Gen::new(11);
    let mut planted = BTreeSet::new();
    let mut found = BTreeSet::new();
    for i in 0..6 {
        // the phone restarts every two days
        let mut b = Books::open(p.clone(), d.clone(), &[5u8; 32]).unwrap();
        assert_eq!(b.last_no() + 1, g.next_no, "the series carries on");
        for _ in 0..1 {
            let day = add_days("2025-07-01", i);
            let x = g.day(&p, &day);
            for t in &x.truth {
                planted.insert((day.clone(), t.bill, t.kind.to_string()));
            }
            b.import_day(&day, &x.bills, x.cash_banked).unwrap();
            for e in &b.days[&day].exceptions {
                found.insert((day.clone(), e.bill, family(&e.kind).to_string()));
            }
        }
    }
    assert_eq!(planted, found, "returns against last week's bills still match after restarts");
    assert_eq!(std::fs::read_dir(d.join("recent")).unwrap().count(), 7, "six days of bill numbers and the refunds so far");
}
