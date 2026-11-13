//! The parts that must be exactly right: money, dates, GSTINs, GST arithmetic and set-off, and
//! reading bills and feeds. The services themselves are tested against the simulated world in
//! crates/phonesim/tests/partner.rs.
use crate::books::*;
use crate::*;

#[test]
fn rupees_are_written_the_indian_way() {
    assert_eq!(inr(0), "₹0.00");
    assert_eq!(inr(99), "₹0.99");
    assert_eq!(inr(472_000), "₹4,720.00");
    assert_eq!(inr(12_345_678), "₹1,23,456.78");
    assert_eq!(inr(1_000_000_000), "₹1,00,00,000.00");
    assert_eq!(inr(-150_050), "-₹1,500.50");
    assert_eq!(parse_inr("₹ 4,720.00"), Some(472_000));
    assert_eq!(parse_inr("Rs. 99"), Some(9_900));
    assert_eq!(parse_inr("1234.5"), Some(123_450));
    assert_eq!(parse_inr("abc"), None);
}

#[test]
fn dates_and_financial_years() {
    let d = Date { y: 2026, m: 11, d: 10 };
    assert_eq!(Date::from_days(d.days()), d);
    assert_eq!(Date::from_days(0), Date { y: 1970, m: 1, d: 1 });
    assert_eq!(d.add_days(30), Date { y: 2026, m: 12, d: 10 });
    assert_eq!(Date { y: 2028, m: 2, d: 28 }.add_days(1), Date { y: 2028, m: 2, d: 29 });
    assert_eq!(d.fy(), "2026-27");
    assert_eq!(Date { y: 2027, m: 3, d: 31 }.fy(), "2026-27");
    assert_eq!(Date { y: 2027, m: 4, d: 1 }.fy(), "2027-28");
    assert_eq!(Date::parse("15/11/2026"), Some(Date { y: 2026, m: 11, d: 15 }));
    assert_eq!(parse_period("last month", d), Ok((2026, 10)));
    assert_eq!(parse_period("this month", Date { y: 2027, m: 1, d: 5 }), Ok((2027, 1)));
    assert_eq!(parse_period("last month", Date { y: 2027, m: 1, d: 5 }), Ok((2026, 12)));
    assert_eq!(parse_period("2026-09", d), Ok((2026, 9)));
    assert_eq!(parse_period("december", d), Ok((2025, 12)), "a month not yet come is last year's");
}

#[test]
fn gstins_are_checked_like_the_portal_does() {
    // the GST portal's own documentation example
    assert_eq!(valid_gstin("27aapfu0939f1zv"), Ok("27AAPFU0939F1ZV".into()));
    assert!(valid_gstin("27AAPFU0939F1ZW").unwrap_err().contains("check character"));
    assert!(valid_gstin("99AAPFU0939F1ZV").is_err(), "no state 99");
    assert!(valid_gstin("27AAPFU0939F1Z").is_err());
    assert_eq!(gstin_check("29AAACM1234A1Z"), Some('4'));
    assert!(valid_gstin(&Config::default().gstin).is_ok(), "the default GSTIN is valid");
}

#[test]
fn input_credit_is_set_off_in_the_order_the_law_sets() {
    // liability IGST 1000, CGST 500, SGST 500; credit IGST 1200: IGST first, then CGST
    let (cash, left) = set_off([1000, 500, 500], [1200, 0, 0]);
    assert_eq!(cash, [0, 300, 500]);
    assert_eq!(left, [0, 0, 0]);
    // CGST credit pays CGST, then IGST, never SGST
    let (cash, left) = set_off([300, 100, 100], [0, 500, 0]);
    assert_eq!(cash, [0, 0, 100]);
    assert_eq!(left, [0, 100, 0]);
    // more credit than tax: carried forward
    let (cash, left) = set_off([0, 100, 100], [0, 150, 150]);
    assert_eq!(cash, [0, 0, 0]);
    assert_eq!(left, [0, 50, 50]);
}

fn shop() -> Partner {
    let mut p = Partner::in_memory(Config::default()); // Karnataka (29)
    p.data.items.push(Item { name: "Widget".into(), price: 50_000, rate: 1800, hsn: "8479".into() });
    p.data.items.push(Item { name: "Repair".into(), price: 150_000, rate: 1800, hsn: "9987".into() });
    p.data.items.push(Item { name: "Notebook".into(), price: 4_550, rate: 1200, hsn: "4820".into() });
    p.data.customers.push(Customer { name: "Sharma Traders".into(), gstin: Some("29AABCT1332L1ZA".into()), phone: "919811111111".into(), email: String::new(), state: "29".into() });
    p.data.customers.push(Customer { name: "Patil Stores".into(), gstin: Some("27AAPFU0939F1ZV".into()), phone: "919822222222".into(), email: String::new(), state: "27".into() });
    p.data.customers.push(Customer { name: "Asha".into(), gstin: None, phone: "919833333333".into(), email: String::new(), state: "29".into() });
    p
}

#[test]
fn invoices_carry_cgst_and_sgst_within_the_state_and_igst_across() {
    let p = shop();
    let d = Date { y: 2026, m: 11, d: 10 };
    let local = make_invoice(&p, &p.data.customers[0], "2 x Widget, 1 x Repair", d, 1).unwrap();
    assert_eq!(local.number, "INV/2026-27/0001");
    assert_eq!(local.taxable, 250_000);
    assert_eq!((local.cgst, local.sgst, local.igst), (22_500, 22_500, 0));
    assert_eq!(local.total, 295_000);
    let other = make_invoice(&p, &p.data.customers[1], "2 x Widget, 1 x Repair", d, 2).unwrap();
    assert_eq!((other.cgst, other.sgst, other.igst), (0, 0, 45_000));
    assert_eq!(other.pos, "27");
    // 3 notebooks at 45.50 + 12%: 136.50 + 16.38 = 152.88, rounded to 153.00
    let small = make_invoice(&p, &p.data.customers[2], "3 notebooks", d, 3).unwrap();
    assert_eq!(small.taxable, 13_650);
    assert_eq!(small.cgst + small.sgst, 1_638);
    assert_eq!((small.total, small.round_off), (15_300, 12));
    // a price given in words wins over the catalog; an unknown item needs one
    let custom = make_invoice(&p, &p.data.customers[0], "1 x Repair @ 1200", d, 4).unwrap();
    assert_eq!(custom.taxable, 120_000);
    assert!(make_invoice(&p, &p.data.customers[0], "1 x Gizmo", d, 5).unwrap_err().contains("no price"));
}

#[test]
fn the_month_s_returns_follow_from_the_invoices_and_bills() {
    let mut p = shop();
    let d = Date { y: 2026, m: 11, d: 10 };
    for (i, (c, items)) in [(0, "2 x Widget, 1 x Repair"), (1, "2 x Widget, 1 x Repair"), (2, "3 notebooks")].iter().enumerate() {
        let inv = make_invoice(&p, &p.data.customers[*c], items, d, i + 1).unwrap();
        p.data.invoices.push(inv);
    }
    // a bill paid this month with GST from a Karnataka supplier: CGST + SGST credit
    p.data.expenses.push(Expense { id: "m1".into(), supplier: "Power".into(), gstin: Some("29AAFCD5862R1ZR".into()), date: d, amount: 472_000, cgst: 36_000, sgst: 36_000, igst: 0, paid_ref: "x".into() });
    let g = compute_gst(&p, 2026, 11);
    assert_eq!(g.period, "112026");
    assert_eq!(g.b2b.len(), 2);
    assert_eq!(g.b2cs.len(), 1, "the consumer sale is summed by state and rate");
    assert_eq!(g.out, [513_650, 45_000, 23_319, 23_319]);
    assert_eq!(g.itc, [0, 36_000, 36_000]);
    // CGST credit 360 pays CGST 233.19, then 126.81 of IGST; SGST credit likewise: IGST
    // 450 - 126.81 - 126.81 = 196.38 is left to pay in cash
    assert_eq!(g.cash, [19_638, 0, 0]);
    assert_eq!(g.carry, [0, 0, 0]);
    assert_eq!(g.docs.0, 3);
    // nothing in October
    let oct = compute_gst(&p, 2026, 10);
    assert_eq!(oct.out, [0, 0, 0, 0]);
}

#[test]
fn bills_are_read_from_emails() {
    let b = parse_bill("m7", "billing@citypower.in", "Bill from City Power", "Dear customer,\nAmount due: ₹ 4,720.00\nGST: ₹ 720.00\nGSTIN: 29AAFCD5862R1ZR\nDue date: 2026-11-15\nPay to UPI: citypower@upi\n").unwrap();
    assert_eq!((b.supplier.as_str(), b.amount, b.gst), ("City Power", 472_000, 72_000));
    assert_eq!(b.due, Some(Date { y: 2026, m: 11, d: 15 }));
    assert_eq!(b.vpa, "citypower@upi");
    assert!(b.gstin.is_some());
    assert!(parse_bill("m8", "a@b.c", "Lunch?", "are you free on friday").is_none());
}

#[test]
fn rss_items_are_read_with_their_categories() {
    let xml = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Tech</title>
<item><title><![CDATA[Chips & phones: what's next]]></title><link>https://n.example/1</link>
<description>&lt;p&gt;New &amp; faster.&lt;/p&gt;</description><category>Tech</category><category>Phones</category></item>
<item><title>Rain in Bengaluru</title><link>https://n.example/2</link></item></channel></rss>"#;
    let a = life::parse_rss(xml, "tech");
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].title, "Chips & phones: what's next");
    assert_eq!(a[0].description, "New & faster.");
    assert_eq!(a[0].categories, vec!["tech", "phones"]);
    assert!(a[1].categories.is_empty());
}

#[test]
fn upi_addresses_and_screens() {
    assert!(wallet::valid_vpa("citypower@upi"));
    assert!(wallet::valid_vpa("ravi.k-2@okhdfc"));
    assert!(!wallet::valid_vpa("no-at-sign"));
    assert!(!wallet::valid_vpa("a b@upi"));
    let ui = Ui::new("Bills").card(ui::Card::new("City Power").badge("₹4,720.00").line("due in 5 days").button("Pay", "pay bill m7"));
    let (back, text) = Ui::from_reply(&ui.to_reply()).unwrap();
    assert_eq!(back, ui);
    assert!(text.contains("City Power [₹4,720.00]") && text.contains("say \"pay bill m7\" to pay"), "{text}");
}
