//! A retail store's books, audited on the phone. A day's bills come in from the tills (thousands
//! of them); every line of every bill is checked by rules, never by a model:
//!   the GST rate against the item's HSN, its price slab and the date; the arithmetic; the bill's
//!   total and how it was paid; the invoice series (duplicates, gaps); business buyers' GSTINs
//!   and the place of supply; returns against their original bill; the till's cash.
//! What is kept is each day's totals and its exceptions, sealed. The bills themselves are not
//! kept, and a shopper's phone number is dropped as the bill is read.
//!
//! From the days come the month's GST return (output tax by rate and HSN, input tax credit
//! checked against the suppliers' filings), the year's tally (turnover against returns and
//! against money received), and the income-tax computation.
pub mod books;
pub mod gen;
pub mod tools;

use agentos_business::util::{inr, rs, Paise};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

// ---------------------------------------------------------------------------- the store's profile
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub gstin: String,
    pub state_code: String,
    pub invoice_prefix: String,
    /// The financial year, "2025-04-01" to "2026-03-31".
    pub fy_start: String,
    pub fy_end: String,
    pub bills_per_day: f64,
    pub rate_rules: Vec<RateRule>,
    pub categories: Vec<Category>,
    pub expenses: Expenses,
    pub tax: Tax,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "My store".into(),
            gstin: String::new(),
            state_code: String::new(),
            invoice_prefix: "INV/".into(),
            fy_start: "2025-04-01".into(),
            fy_end: "2026-03-31".into(),
            bills_per_day: 5000.0,
            rate_rules: vec![],
            categories: vec![],
            expenses: Expenses::default(),
            tax: Tax::default(),
        }
    }
}
/// The GST rate for goods under an HSN prefix between two dates. With a slab, a piece whose
/// value before tax is at most `slab` rupees takes `low`, else `high`.
#[derive(Debug, Clone, Deserialize)]
pub struct RateRule {
    pub hsn: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub slab: f64,
    #[serde(default)]
    pub low: f64,
    pub high: f64,
}
#[derive(Debug, Clone, Deserialize)]
pub struct Category {
    pub code: String,
    pub name: String,
    pub hsn: String,
    /// Price points in rupees, tax included (as on the tag).
    pub prices: Vec<f64>,
    /// What the goods cost the store, as a percentage of the tag price.
    #[serde(default = "fifty")]
    pub cost_pct: f64,
    #[serde(default = "one")]
    pub weight: f64,
}
fn fifty() -> f64 {
    50.0
}
fn one() -> f64 {
    1.0
}
/// Monthly running costs in rupees, and how they are taxed.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Expenses {
    pub rent: f64,
    pub rent_gst_pct: f64,
    pub rent_tds_pct: f64,
    pub salaries: f64,
    pub electricity: f64,
    pub security_housekeeping: f64,
    pub marketing: f64,
    pub staff_food: f64,
    pub packaging: f64,
    /// Card and UPI charges as a percentage of what is paid that way.
    pub card_fee_pct: f64,
    /// What the head office charges the store for buying, warehousing and transport, as a
    /// percentage of sales before tax.
    pub head_office_pct: f64,
    /// Stock lost to theft and damage, as a percentage of the cost of goods sold.
    pub shrinkage_pct: f64,
    /// Shop fit-out capitalised at the start of the year, and its depreciation rate.
    pub fitout: f64,
    pub depreciation_pct: f64,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Tax {
    pub rate_pct: f64,
    pub surcharge_pct: f64,
    pub cess_pct: f64,
    /// Expense disallowed when tax was not deducted at source, percent of the payment.
    pub tds_default_disallow_pct: f64,
    /// A cash payment above this (rupees, to one person in a day) is not allowed as an expense.
    pub cash_limit: f64,
}
impl Default for Tax {
    fn default() -> Self {
        Self { rate_pct: 22.0, surcharge_pct: 10.0, cess_pct: 4.0, tds_default_disallow_pct: 30.0, cash_limit: 10_000.0 }
    }
}

impl Profile {
    pub fn parse(text: &str) -> Result<Profile, String> {
        let p: Profile = toml::from_str(text).map_err(|e| format!("store profile: {e}"))?;
        if p.categories.is_empty() || p.rate_rules.is_empty() {
            return Err("store profile: needs [[categories]] and [[rate_rules]]".into());
        }
        if !gstin_ok(&p.gstin) {
            return Err(format!("store profile: {} is not a valid GSTIN", p.gstin));
        }
        for c in &p.categories {
            if p.rule(&c.hsn, &p.fy_start).is_none() {
                return Err(format!("store profile: no rate rule covers HSN {} ({})", c.hsn, c.name));
            }
        }
        Ok(p)
    }
    pub fn load(path: &std::path::Path) -> Result<Profile, String> {
        Self::parse(&std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?)
    }
    /// The most specific rule (longest HSN prefix) in force on a day.
    pub fn rule(&self, hsn: &str, day: &str) -> Option<&RateRule> {
        self.rate_rules
            .iter()
            .filter(|r| !r.hsn.is_empty() && hsn.starts_with(&r.hsn) && day >= r.from.as_str() && day <= r.to.as_str())
            .max_by_key(|r| r.hsn.len())
    }
    /// The rate a piece must carry. Tags are tax-inclusive, and the slab is on the value before
    /// tax, so the piece is in the low slab when its price less tax at the low rate fits.
    pub fn rate(&self, hsn: &str, day: &str, price_incl: Paise) -> Option<f64> {
        let r = self.rule(hsn, day)?;
        Some(if r.slab > 0.0 && (price_incl as f64) * 100.0 <= rs(r.slab) as f64 * (100.0 + r.low) { r.low } else { r.high })
    }
    /// SKU -> (HSN, tag price). A SKU is the category code and the price: "TS-399".
    pub fn sku(&self, sku: &str) -> Option<(&Category, Paise)> {
        let (code, price) = sku.rsplit_once('-')?;
        let c = self.categories.iter().find(|c| c.code == code)?;
        let p = rs(price.parse::<f64>().ok()?);
        c.prices.iter().any(|x| rs(*x) == p).then_some((c, p))
    }
}

/// The check character of a GSTIN (15 characters; the last is computed from the first 14).
pub fn gstin_ok(g: &str) -> bool {
    const CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let b = g.as_bytes();
    if b.len() != 15 || !b[..2].iter().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let mut sum = 0usize;
    for (i, c) in b[..14].iter().enumerate() {
        let Some(v) = CHARS.iter().position(|x| x == c) else { return false };
        let p = v * if i % 2 == 0 { 1 } else { 2 };
        sum += p / 36 + p % 36;
    }
    CHARS[(36 - sum % 36) % 36] == b[14]
}
/// Complete 14 characters into a valid GSTIN.
pub fn gstin_make(first14: &str) -> String {
    (0..36u8)
        .map(|i| format!("{first14}{}", if i < 10 { (b'0' + i) as char } else { (b'A' + i - 10) as char }))
        .find(|g| gstin_ok(g))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------- a bill
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Line {
    pub sku: String,
    pub hsn: String,
    pub qty: u32,
    /// Price per piece after discount, tax included, paise.
    pub price: Paise,
    /// The rate the till charged, percent.
    pub rate: f64,
    pub taxable: Paise,
    pub tax: Paise,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Bill {
    /// The running number in the invoice series.
    pub no: u64,
    pub at: String,
    pub till: u8,
    pub lines: Vec<Line>,
    /// What the customer paid (negative for a return), paise.
    pub total: Paise,
    pub cash: Paise,
    pub upi: Paise,
    pub card: Paise,
    pub buyer_gstin: Option<String>,
    /// Charged as IGST (a buyer in another state) rather than CGST + SGST.
    pub igst: bool,
    /// A return: the bill it is against.
    pub return_of: Option<u64>,
    /// Loyalty number. Never kept: dropped when the bill is read.
    pub phone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Exception {
    /// "2025-06-14#3": the day and the exception's place in it.
    pub id: String,
    pub kind: String,
    pub bill: u64,
    pub detail: String,
    /// Tax charged less tax due, paise: negative is tax short-charged (the store owes it),
    /// positive is tax over-charged (owed back to the customer, or to the government).
    pub tax_effect: Paise,
    /// The fix the owner approved, if any.
    pub fixed: Option<String>,
}
/// Fixes a person may choose for each kind of exception.
pub fn fixes_for(kind: &str) -> &'static [&'static str] {
    match kind {
        "wrong_rate" | "slab_on_tag" | "old_rate" => &["credit_note", "debit_note", "absorb"],
        "tax_arithmetic" | "total_mismatch" | "payment_mismatch" => &["recompute"],
        "duplicate_no" => &["renumber"],
        "gap" => &["record_void"],
        "hsn_missing" | "hsn_wrong" => &["set_from_catalog"],
        "gstin_invalid" => &["treat_as_b2c", "correct_gstin"],
        "place_of_supply" => &["reissue"],
        "return_unmatched" | "return_excess" => &["hold_refund", "accept"],
        "cash_short" => &["investigate", "accept"],
        _ => &[],
    }
}

/// One day of the store, as kept: totals and exceptions, no bills and no shoppers.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DayBook {
    pub day: String,
    pub bills: u32,
    pub returns: u32,
    pub lines: u32,
    pub first_no: u64,
    pub last_no: u64,
    /// rate (percent x 100) -> (taxable, tax), sales less returns.
    pub by_rate: BTreeMap<u32, (Paise, Paise)>,
    /// HSN -> (pieces, taxable, tax).
    pub by_hsn: BTreeMap<String, (i64, Paise, Paise)>,
    pub cgst: Paise,
    pub sgst: Paise,
    pub igst: Paise,
    /// Taxable value sold to registered buyers, and how many such bills.
    pub b2b_taxable: Paise,
    pub b2b_bills: u32,
    pub cash: Paise,
    pub upi: Paise,
    pub card: Paise,
    pub cash_banked: Paise,
    /// What the goods sold cost the store (from the category's cost share).
    pub cost: Paise,
    pub exceptions: Vec<Exception>,
    pub loyalty_seen: u32,
    pub audit_ms: f64,
}
impl DayBook {
    pub fn taxable(&self) -> Paise {
        self.by_rate.values().map(|v| v.0).sum()
    }
    pub fn tax(&self) -> Paise {
        self.by_rate.values().map(|v| v.1).sum()
    }
    pub fn received(&self) -> Paise {
        self.cash + self.upi + self.card
    }
}

/// What the auditor remembers between days: where the series stands, and recent bills (for
/// returns). Kept in memory; rebuilt from the day files' series numbers after a restart.
#[derive(Default)]
pub struct Memory {
    pub last_no: u64,
    /// bill number -> (total, the day it was issued), for bills of the last days.
    pub recent: HashMap<u64, (Paise, String)>,
    pub recent_days: Vec<(String, Vec<u64>)>,
    /// bill number -> what has been refunded against it so far.
    pub refunded: HashMap<u64, Paise>,
}
/// How many days back a return may refer.
pub const RETURN_DAYS: usize = 30;

fn tax_of(amount_incl: Paise, rate: f64) -> (Paise, Paise) {
    let taxable = (amount_incl as f64 * 100.0 / (100.0 + rate)).round() as Paise;
    (taxable, amount_incl - taxable)
}

/// Audit one day's bills and reduce them to a DayBook. `cash_banked` is what the tills' cash
/// came to when counted.
pub fn audit_day(p: &Profile, day: &str, bills: &[Bill], cash_banked: Paise, mem: &mut Memory) -> DayBook {
    let t0 = std::time::Instant::now();
    let mut d = DayBook { day: day.to_string(), cash_banked, ..DayBook::default() };
    let raise = |d: &mut DayBook, kind: &str, bill: u64, detail: String, tax_effect: Paise| {
        let id = format!("{day}#{}", d.exceptions.len() + 1);
        d.exceptions.push(Exception { id, kind: kind.into(), bill, detail, tax_effect, fixed: None });
    };
    let mut seen: HashSet<u64> = HashSet::with_capacity(bills.len());
    let mut todays: Vec<u64> = Vec::with_capacity(bills.len());
    let mut expect = mem.last_no + 1;
    for b in bills {
        // ---- the series
        let duplicate = !seen.insert(b.no) || (b.no <= mem.last_no && b.no != 0);
        if duplicate {
            raise(&mut d, "duplicate_no", b.no, format!("invoice {}{:08} was already issued", p.invoice_prefix, b.no), 0);
        } else {
            if b.no > expect {
                let missing = b.no - expect;
                raise(
                    &mut d,
                    "gap",
                    expect,
                    format!("{missing} invoice number(s) from {}{:08} were never issued or were voided without a record", p.invoice_prefix, expect),
                    0,
                );
            }
            expect = b.no + 1;
        }
        if d.first_no == 0 {
            d.first_no = b.no;
        }
        d.last_no = d.last_no.max(b.no);
        let sign: Paise = if b.return_of.is_some() { -1 } else { 1 };
        // a return carries the rate of the bill it reverses, so its lines are judged as of that day
        let original = b.return_of.and_then(|o| mem.recent.get(&o).cloned());
        let rate_day: &str = original.as_ref().map(|o| o.1.as_str()).unwrap_or(day);
        d.bills += 1;
        if b.phone.is_some() {
            d.loyalty_seen += 1; // counted, not kept
        }
        // ---- the buyer
        let mut b2b = false;
        if let Some(g) = &b.buyer_gstin {
            if !gstin_ok(g) {
                raise(&mut d, "gstin_invalid", b.no, "the buyer's GSTIN fails its check character: their credit will not match".into(), 0);
            } else {
                b2b = true;
                let other_state = g[..2] != p.state_code[..];
                if other_state != b.igst {
                    let want = if other_state { "IGST (buyer in another state)" } else { "CGST + SGST (buyer in this state)" };
                    raise(&mut d, "place_of_supply", b.no, format!("should carry {want}"), 0);
                }
            }
        }
        // ---- the lines
        let mut sum: Paise = 0;
        for l in &b.lines {
            d.lines += 1;
            let amount = l.price * l.qty as Paise;
            sum += amount;
            let cat = p.sku(&l.sku);
            let hsn = match (&l.hsn, cat) {
                (h, Some((c, _))) if h.is_empty() => {
                    raise(&mut d, "hsn_missing", b.no, format!("{} has no HSN on the bill (catalogue: {})", l.sku, c.hsn), 0);
                    c.hsn.as_str()
                }
                (h, Some((c, _))) if *h != c.hsn => {
                    raise(&mut d, "hsn_wrong", b.no, format!("{} billed under HSN {h}, catalogue says {}", l.sku, c.hsn), 0);
                    c.hsn.as_str()
                }
                (h, _) => h.as_str(),
            };
            let (taxable, tax) = (l.taxable, l.tax);
            // the arithmetic at the rate the till used
            let (want_taxable, want_tax) = tax_of(amount, l.rate);
            if (tax - want_tax).abs() > 2 || (taxable - want_taxable).abs() > 2 {
                raise(
                    &mut d,
                    "tax_arithmetic",
                    b.no,
                    format!("{}: tax {} on {} at {}% should be {}", l.sku, inr(tax), inr(amount), l.rate, inr(want_tax)),
                    sign * (tax - want_tax),
                );
            }
            // the rate itself
            // (a return whose original cannot be found is reported as that; its rate cannot be judged)
            let judge = b.return_of.is_none() || original.is_some();
            if let Some(right) = p.rate(hsn, rate_day, l.price).filter(|_| judge) {
                if (right - l.rate).abs() > 0.001 {
                    let (_, due) = tax_of(amount, right);
                    let rule = p.rule(hsn, rate_day).unwrap();
                    // why, where the pattern says: the day before's rule, or the slab read off the tag
                    let before = agentos_business::util::add_days(&rule.from, -1);
                    let old = p.rate(hsn, &before, l.price);
                    let on_tag = rule.slab > 0.0 && l.price > rs(rule.slab) && (l.rate - rule.high).abs() < 0.001 && right == rule.low;
                    let kind = if old.is_some_and(|o| (o - l.rate).abs() < 0.001) && day_gap(&rule.from, rate_day) <= 7 {
                        "old_rate"
                    } else if on_tag {
                        "slab_on_tag"
                    } else {
                        "wrong_rate"
                    };
                    raise(
                        &mut d,
                        kind,
                        b.no,
                        format!("{} at {}: charged {}%, the rate is {}%", l.sku, inr(l.price), l.rate, right),
                        sign * (want_tax - due),
                    );
                }
            }
            let e = d.by_rate.entry((l.rate * 100.0).round() as u32).or_default();
            e.0 += sign * taxable;
            e.1 += sign * tax;
            let h = d.by_hsn.entry(hsn.to_string()).or_default();
            h.0 += sign * l.qty as i64;
            h.1 += sign * taxable;
            h.2 += sign * tax;
            if b.igst {
                d.igst += sign * tax;
            } else {
                d.cgst += sign * (tax / 2);
                d.sgst += sign * (tax - tax / 2);
            }
            if b2b {
                d.b2b_taxable += sign * taxable;
            }
            if let Some((c, tag)) = cat {
                d.cost += sign * ((tag as f64 * c.cost_pct / 100.0) as Paise) * l.qty as Paise;
            }
        }
        if b2b {
            d.b2b_bills += 1;
        }
        // ---- the bill
        if (b.total.abs() - sum).abs() > 0 {
            raise(&mut d, "total_mismatch", b.no, format!("bill says {}, its lines add to {}", inr(b.total.abs()), inr(sum)), 0);
        }
        if b.cash + b.upi + b.card != b.total {
            raise(
                &mut d,
                "payment_mismatch",
                b.no,
                format!("paid {} (cash {}, UPI {}, card {}) against a bill of {}", inr(b.cash + b.upi + b.card), inr(b.cash), inr(b.upi), inr(b.card), inr(b.total)),
                0,
            );
        }
        d.cash += b.cash;
        d.upi += b.upi;
        d.card += b.card;
        match b.return_of {
            Some(orig) => {
                d.returns += 1;
                match &original {
                    None => raise(&mut d, "return_unmatched", b.no, format!("refund of {} against invoice {orig:08}, which is not among the last {RETURN_DAYS} days' bills", inr(sum)), 0),
                    Some((t, _)) => {
                        // everything refunded against one bill, over all its returns, cannot pass its total
                        let so_far = mem.refunded.get(&orig).copied().unwrap_or(0);
                        if so_far + sum > *t {
                            let again = if so_far > 0 { format!(", on top of {} already refunded", inr(so_far)) } else { String::new() };
                            raise(&mut d, "return_excess", b.no, format!("refund of {} against invoice {orig:08} of {}{again}", inr(sum), inr(*t)), 0);
                        }
                        *mem.refunded.entry(orig).or_default() += sum;
                    }
                }
            }
            // a second bill under a number already used is not the bill a return refers to
            None if duplicate => {}
            None => {
                mem.recent.insert(b.no, (b.total, day.to_string()));
                todays.push(b.no);
            }
        }
    }
    if d.cash - cash_banked > rs(100.0) {
        let said = format!("cash sales {} but the tills held {}: {} short", inr(d.cash), inr(cash_banked), inr(d.cash - cash_banked));
        raise(&mut d, "cash_short", 0, said, 0);
    }
    mem.last_no = mem.last_no.max(d.last_no);
    mem.recent_days.push((day.to_string(), todays));
    while mem.recent_days.len() > RETURN_DAYS {
        for no in mem.recent_days.remove(0).1 {
            mem.recent.remove(&no);
            mem.refunded.remove(&no);
        }
    }
    d.audit_ms = t0.elapsed().as_secs_f64() * 1000.0;
    d
}

fn day_gap(a: &str, b: &str) -> i64 {
    use agentos_business::util::day_number;
    day_number(b).unwrap_or(0) - day_number(a).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_gstin_check_character() {
        // a published sample GSTIN
        assert!(gstin_ok("27AAPFU0939F1ZV"));
        assert!(!gstin_ok("27AAPFU0939F1ZW"));
        assert!(!gstin_ok("27AAPFU0939F1Z"));
        assert!(!gstin_ok("XXAAPFU0939F1ZV"));
        let g = gstin_make("37AABCT1234C1Z");
        assert!(gstin_ok(&g) && g.len() == 15);
    }
}
