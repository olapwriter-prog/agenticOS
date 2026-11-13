//! A seeded generator of a store's days, with faults planted at known places, so the audit can
//! be scored: every planted fault must be found, and nothing else reported. The shoppers,
//! suppliers and numbers are invented.
use crate::books::Inward;
use crate::{gstin_make, Bill, Line, Profile};
use agentos_business::dataset::Rng;
use agentos_business::util::{add_days, rs, weekday, Paise};

/// A planted fault: the bill (or the missing number) and the family of exception it must raise.
#[derive(Debug, Clone, PartialEq)]
pub struct Truth {
    pub bill: u64,
    pub kind: &'static str,
}
pub struct Day {
    pub bills: Vec<Bill>,
    pub cash_banked: Paise,
    pub truth: Vec<Truth>,
}
pub struct Gen {
    rng: Rng,
    pub next_no: u64,
    /// Recent sale bills a return can refer to: (number, day, a line, total).
    past: Vec<(u64, String, Line, Paise)>,
}
const WEEK: [f64; 7] = [0.82, 0.85, 0.88, 0.92, 1.05, 1.22, 1.30];
const MONTH: [f64; 12] = [1.15, 0.9, 0.95, 1.0, 1.0, 0.95, 0.9, 1.0, 1.0, 1.3, 1.15, 1.05];

/// The family an exception kind belongs to when scoring (the auditor names the cause of a
/// wrong rate more finely than the generator plants it).
pub fn family(kind: &str) -> &str {
    match kind {
        "wrong_rate" | "slab_on_tag" | "old_rate" => "rate",
        k => k,
    }
}

impl Gen {
    pub fn new(seed: u64) -> Gen {
        Gen { rng: Rng::new(seed), next_no: 1, past: Vec::new() }
    }
    fn line(&mut self, p: &Profile, day: &str, sale: bool) -> Line {
        let w: Vec<f64> = p.categories.iter().map(|c| c.weight).collect();
        let c = &p.categories[self.rng.pick(&w)];
        let tag = c.prices[self.rng.below(c.prices.len())];
        let mut price = rs(tag);
        if self.rng.unit() < if sale { 0.4 } else { 0.15 } {
            let off = [10.0, 20.0, 30.0][self.rng.below(3)];
            price = rs((tag * (100.0 - off) / 100.0).round());
        }
        let qty = if self.rng.unit() < 0.9 { 1 } else { 2 };
        let rate = p.rate(&c.hsn, day, price).unwrap_or(0.0);
        let amount = price * qty as Paise;
        let taxable = (amount as f64 * 100.0 / (100.0 + rate)).round() as Paise;
        Line { sku: format!("{}-{}", c.code, tag), hsn: c.hsn.clone(), qty, price, rate, taxable, tax: amount - taxable }
    }
    fn retax(l: &mut Line, rate: f64) {
        let amount = l.price * l.qty as Paise;
        l.rate = rate;
        l.taxable = (amount as f64 * 100.0 / (100.0 + rate)).round() as Paise;
        l.tax = amount - l.taxable;
    }

    pub fn day(&mut self, p: &Profile, day: &str) -> Day {
        let month: usize = day[5..7].parse().unwrap_or(1);
        let sale = month == 1 || month == 7;
        let n = (p.bills_per_day * WEEK[weekday(day)] * MONTH[month - 1] * (0.94 + 0.12 * self.rng.unit())).round() as usize;
        let mut out = Day { bills: Vec::with_capacity(n), cash_banked: 0, truth: Vec::new() };
        let mut cash: Paise = 0;
        let buyers: Vec<String> = ["37AABCS1111A1Z", "37AACCM2222B1Z", "36AADCK3333C1Z", "29AAECR4444D1Z"].iter().map(|g| gstin_make(g)).collect();
        for k in 0..n {
            let mut b = Bill {
                at: format!("{day}T{:02}:{:02}", 10 + (k * 12 / n.max(1)), self.rng.below(60)),
                till: 1 + self.rng.below(8) as u8,
                ..Bill::default()
            };
            let x = self.rng.unit();
            // ---- the series: a skipped number, or one used twice
            if x < 0.0002 && k > 0 {
                out.truth.push(Truth { bill: self.next_no, kind: "gap" });
                self.next_no += 1;
            }
            b.no = self.next_no;
            self.next_no += 1;
            if (0.0002..0.0004).contains(&x) && k > 0 {
                b.no = out.bills.last().map(|p| p.no).unwrap_or(b.no);
                self.next_no -= 1;
                out.truth.push(Truth { bill: b.no, kind: "duplicate_no" });
            }
            // ---- a return, or a sale
            let is_return = self.rng.unit() < 0.015 && !self.past.is_empty();
            // the same goods refunded a second time, on the next bill
            let mut twice = false;
            if is_return {
                // a bill is returned against once (it leaves the list), unless a fault is planted
                let i = self.rng.below(self.past.len());
                let (no, _, mut l, total) = self.past.swap_remove(i);
                let r = self.rng.unit();
                b.return_of = Some(no);
                if r < 0.01 {
                    b.return_of = Some(900_000_000 + self.rng.below(1000) as u64);
                    out.truth.push(Truth { bill: b.no, kind: "return_unmatched" });
                } else if r < 0.015 {
                    l.qty = (total / l.price.max(1)) as u32 + 2;
                    let same = l.rate;
                    Self::retax(&mut l, same);
                    out.truth.push(Truth { bill: b.no, kind: "return_excess" });
                } else if r < 0.02 && l.price * l.qty as Paise * 2 > total {
                    twice = true;
                }
                b.lines.push(l);
            } else {
                let lines = 1 + self.rng.pick(&[0.30, 0.30, 0.20, 0.12, 0.08]);
                for _ in 0..lines {
                    let l = self.line(p, day, sale);
                    b.lines.push(l);
                }
                // ---- a planted fault on the first line (one per bill at most)
                let f = self.rng.unit();
                let before = add_days(day, -1);
                let l = &mut b.lines[0];
                let rule = p.rule(&l.hsn, day).cloned();
                let old = p.rate(&l.hsn, "2025-09-21", l.price);
                if b.till == 3 && month == 6 && rule.as_ref().is_some_and(|r| r.slab > 0.0 && l.price > rs(r.slab) && l.rate == r.low) {
                    // a till that reads the slab off the tag price instead of the value before tax
                    Self::retax(l, rule.unwrap().high);
                    out.truth.push(Truth { bill: b.no, kind: "rate" });
                } else if (b.till == 5 || b.till == 6) && (day == "2025-09-22" || day == "2025-09-23") && old.is_some_and(|o| o != l.rate) {
                    // two tills not updated when the rates changed
                    Self::retax(l, old.unwrap());
                    out.truth.push(Truth { bill: b.no, kind: "rate" });
                } else if f < 0.0010 {
                    let other = [5.0, 12.0, 18.0].into_iter().find(|r| *r != l.rate).unwrap();
                    Self::retax(l, other);
                    out.truth.push(Truth { bill: b.no, kind: "rate" });
                } else if f < 0.0015 {
                    let slip = rs(1.0 + self.rng.below(5) as f64);
                    l.tax += slip;
                    l.taxable -= slip;
                    out.truth.push(Truth { bill: b.no, kind: "tax_arithmetic" });
                } else if f < 0.0020 {
                    l.hsn.clear();
                    out.truth.push(Truth { bill: b.no, kind: "hsn_missing" });
                } else if f < 0.0023 {
                    let other = p.categories.iter().find(|c| c.hsn != l.hsn).map(|c| c.hsn.clone()).unwrap_or_default();
                    l.hsn = other;
                    out.truth.push(Truth { bill: b.no, kind: "hsn_wrong" });
                }
                let _ = before;
            }
            let sum: Paise = b.lines.iter().map(|l| l.price * l.qty as Paise).sum();
            b.total = if is_return { -sum } else { sum };
            // ---- a business buyer
            if !is_return && self.rng.unit() < 0.003 {
                let g = buyers[self.rng.below(buyers.len())].clone();
                let other_state = g[..2] != p.state_code[..];
                b.igst = other_state;
                let r = self.rng.unit();
                if r < 0.05 {
                    let mut bad = g.clone().into_bytes();
                    bad[14] = if bad[14] == b'A' { b'B' } else { b'A' };
                    b.buyer_gstin = Some(String::from_utf8(bad).unwrap());
                    out.truth.push(Truth { bill: b.no, kind: "gstin_invalid" });
                } else {
                    b.buyer_gstin = Some(g);
                    if r < 0.10 {
                        b.igst = !other_state;
                        out.truth.push(Truth { bill: b.no, kind: "place_of_supply" });
                    }
                }
            }
            // ---- how it was paid
            let y = self.rng.unit();
            let mode = self.rng.pick(&[0.55, 0.25, 0.20]);
            let mut total = b.total;
            if y < 0.0003 && !is_return {
                b.total += rs(10.0);
                total = b.total;
                out.truth.push(Truth { bill: b.no, kind: "total_mismatch" });
            }
            match mode {
                0 => b.upi = total,
                1 => b.card = total,
                _ => b.cash = total,
            }
            if (0.0003..0.0006).contains(&y) && !is_return {
                b.upi -= rs(50.0);
                out.truth.push(Truth { bill: b.no, kind: "payment_mismatch" });
            }
            if self.rng.unit() < 0.10 {
                b.phone = Some(format!("+91 90000 {:05}", self.rng.below(100_000)));
            }
            cash += b.cash;
            if !is_return && b.no % 7 == 0 {
                if self.past.len() >= 3000 {
                    let i = self.rng.below(self.past.len());
                    self.past.swap_remove(i);
                }
                // only bills free of planted faults are returned against, so a return is clean
                if !out.truth.iter().rev().take(2).any(|t| t.bill == b.no) {
                    self.past.push((b.no, day.to_string(), b.lines[0].clone(), b.total));
                }
            }
            let again = twice.then(|| {
                let mut t = b.clone();
                t.no = self.next_no;
                self.next_no += 1;
                t.phone = None;
                t
            });
            out.bills.push(b);
            if let Some(t) = again {
                cash += t.cash;
                out.truth.push(Truth { bill: t.no, kind: "return_excess" });
                out.bills.push(t);
            }
        }
        // a return may only refer to a bill of the last weeks
        let keep_from = add_days(day, -20);
        self.past.retain(|x| x.1.as_str() >= keep_from.as_str());
        out.cash_banked = cash;
        if self.rng.unit() < 0.015 {
            out.cash_banked -= rs(500.0 + self.rng.below(4500) as f64);
            out.truth.push(Truth { bill: 0, kind: "cash_short" });
        }
        out
    }
}

/// A month's purchases and running costs as the suppliers' invoices: goods to replace what was
/// sold, rent, services, and the things on which credit is not allowed. A few are missing
/// from the suppliers' own filings, and one now and then is entered twice.
pub fn inward(p: &Profile, month: &str, cost_of_sales: Paise, sales: Paise, card_upi: Paise, seed: u64) -> Vec<Inward> {
    let mut r = Rng::new(seed ^ month.bytes().fold(0u64, |a, b| a.wrapping_mul(131).wrapping_add(b as u64)));
    let e = &p.expenses;
    let mut out = Vec::new();
    let add = |out: &mut Vec<Inward>, r: &mut Rng, supplier: &str, g14: &str, category: &str, taxable: Paise, pct: f64, k: usize| {
        let gstin = gstin_make(g14);
        let igst = gstin[..2] != p.state_code[..];
        out.push(Inward {
            month: month.to_string(),
            supplier: supplier.to_string(),
            gstin,
            no: format!("{}/{}/{k:03}", &supplier[..2].to_uppercase(), &month[2..]),
            taxable,
            gst: (taxable as f64 * pct / 100.0).round() as Paise,
            igst,
            category: category.to_string(),
            in_2b: r.unit() >= 0.02,
            tds_deducted: true,
            cash: false,
        });
    };
    // goods: a little more than was sold, from eight makers, most of them in other states
    let makers = [("Tirupur Knits", "33AAACT1001A1Z", 5.0), ("Ludhiana Woollens", "03AAACL1002B1Z", 5.0), ("Surat Synthetics", "24AAACS1003C1Z", 5.0),
        ("Bengaluru Denim Works", "29AAACB1004D1Z", 5.0), ("Agra Footwear", "09AAACA1005E1Z", 5.0), ("Kolkata Hosiery", "19AAACK1006F1Z", 5.0),
        ("Vizag Garments", "37AAACV1007G1Z", 5.0), ("Mumbai Accessories", "27AAACM1008H1Z", 18.0)];
    let goods = (cost_of_sales as f64 * (1.01 + 0.04 * r.unit())) as Paise;
    for (i, (name, g, pct)) in makers.iter().enumerate() {
        let share = [0.22, 0.10, 0.16, 0.16, 0.12, 0.10, 0.08, 0.06][i];
        for k in 0..6 {
            add(&mut out, &mut r, name, g, "goods", (goods as f64 * share / 6.0) as Paise, *pct, k + 1);
        }
    }
    add(&mut out, &mut r, "Centre Mall Properties", "37AAACC2001J1Z", "rent", rs(e.rent), e.rent_gst_pct, 1);
    add(&mut out, &mut r, "Eastern Power Distribution", "37AAACE2002K1Z", "electricity", rs(e.electricity), 0.0, 1);
    add(&mut out, &mut r, "Shield Facility Services", "37AAACS2003L1Z", "services", rs(e.security_housekeeping), 18.0, 1);
    add(&mut out, &mut r, "Coastal Media", "37AAACC2004M1Z", "marketing", rs(e.marketing), 18.0, 1);
    add(&mut out, &mut r, "Annapurna Caterers", "37AAACA2005N1Z", "staff_food", rs(e.staff_food), 5.0, 1);
    add(&mut out, &mut r, "Packwell Bags", "37AAACP2006P1Z", "packaging", rs(e.packaging), 18.0, 1);
    add(&mut out, &mut r, "Card and UPI acquirer", "27AAACC2007Q1Z", "bank_charges", (card_upi as f64 * e.card_fee_pct / 100.0) as Paise, 18.0, 1);
    if e.head_office_pct > 0.0 {
        add(&mut out, &mut r, "Value Fashion head office", "27AAACV2009S1Z", "head_office", (sales as f64 * e.head_office_pct / 100.0) as Paise, 18.0, 1);
    }
    if &month[5..] == "04" {
        add(&mut out, &mut r, "Interior Buildcraft", "37AAACI2008R1Z", "fitout", rs(e.fitout), 18.0, 1);
    }
    // the things that go wrong on the purchase side
    if &month[5..] == "08" {
        // rent paid without deducting tax at source
        if let Some(x) = out.iter_mut().find(|x| x.category == "rent") {
            x.tds_deducted = false;
        }
    }
    if ["06", "11", "02"].contains(&&month[5..]) {
        // an electrician paid in cash, above the limit
        out.push(Inward {
            month: month.to_string(),
            supplier: "Local electrical repairs".into(),
            gstin: String::new(),
            no: format!("CASH/{}", &month[2..]),
            taxable: rs(18_500.0),
            gst: 0,
            igst: false,
            category: "repairs".into(),
            in_2b: false,
            tds_deducted: true,
            cash: true,
        });
    }
    if r.unit() < 0.5 {
        // a goods invoice keyed in twice
        let dup = out[r.below(40)].clone();
        out.push(dup);
    }
    out
}
