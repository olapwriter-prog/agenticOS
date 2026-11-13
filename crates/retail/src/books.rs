//! The store's books: each audited day, the suppliers' invoices, the fixes proposed and
//! approved, and what was filed. From them: the month's GST return, the year's tally and the
//! income-tax computation. Sealed on disk with a key from the vault.
use crate::{audit_day, fixes_for, Bill, DayBook, Exception, Memory, Profile};
use agentos_business::util::{add_days, inr, rs, seal, unseal, Paise};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// A supplier's invoice (goods, rent, a service).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Inward {
    pub month: String,
    pub supplier: String,
    pub gstin: String,
    pub no: String,
    pub taxable: Paise,
    pub gst: Paise,
    pub igst: bool,
    /// goods, rent, electricity, services, marketing, staff_food, packaging, bank_charges, fitout, repairs
    pub category: String,
    /// The supplier reported it, so it shows in the store's GSTR-2B.
    pub in_2b: bool,
    pub tds_deducted: bool,
    pub cash: bool,
}
/// Credit is not allowed on these whatever the invoice says (food for staff, building work).
pub const BLOCKED: [&str; 2] = ["staff_food", "fitout"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    pub fix: String,
    pub by: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct Meta {
    inward: Vec<Inward>,
    proposals: Vec<Proposal>,
    /// period -> (when, output tax, credit claimed, cash paid) as filed.
    gst_filed: BTreeMap<String, (String, Paise, Paise, Paise)>,
    itr_filed: Option<String>,
    now: String,
}

#[derive(Debug, Clone, Default)]
pub struct Itc {
    pub eligible: Paise,
    pub not_in_2b: Paise,
    pub blocked: Paise,
    pub duplicate: Paise,
    pub not_in_2b_n: u32,
    pub duplicate_n: u32,
}
#[derive(Debug, Clone, Default)]
pub struct MonthGst {
    pub period: String,
    pub days: u32,
    pub bills: u32,
    pub first_no: u64,
    pub last_no: u64,
    pub by_rate: BTreeMap<u32, (Paise, Paise)>,
    pub by_hsn: BTreeMap<String, (i64, Paise, Paise)>,
    pub cgst: Paise,
    pub sgst: Paise,
    pub igst: Paise,
    pub b2b_taxable: Paise,
    pub b2b_bills: u32,
    /// Tax the tills under-charged: owed on top of what was billed.
    pub short_charged: Paise,
    /// Tax the tills over-charged; it stays payable unless a credit note returns it.
    pub over_charged: Paise,
    pub credit_notes: Paise,
    pub exceptions: BTreeMap<String, u32>,
    pub unfixed: u32,
    pub itc: Itc,
}
impl MonthGst {
    pub fn taxable(&self) -> Paise {
        self.by_rate.values().map(|v| v.0).sum()
    }
    pub fn billed_tax(&self) -> Paise {
        self.by_rate.values().map(|v| v.1).sum()
    }
    pub fn output(&self) -> Paise {
        self.billed_tax() + self.short_charged - self.credit_notes
    }
}

#[derive(Debug, Clone, Default)]
pub struct Year {
    pub days: u32,
    pub bills: u64,
    pub turnover: Paise,
    pub output_tax: Paise,
    pub received: Paise,
    pub gross_billed: Paise,
    pub cash_short: Paise,
    pub cogs: Paise,
    pub purchases: Paise,
    pub closing_stock: Paise,
    /// (head, amount) for the profit and loss account.
    pub expenses: Vec<(String, Paise)>,
    pub disallowed: Vec<(String, Paise)>,
    pub profit: Paise,
    pub taxable_income: Paise,
    pub tax: Paise,
    pub surcharge: Paise,
    pub cess: Paise,
    pub itc: Itc,
    pub gst_cash: Paise,
    pub unfiled: Vec<String>,
    pub exceptions: BTreeMap<String, u32>,
}

pub struct Books {
    pub profile: Profile,
    dir: PathBuf,
    key: [u8; 32],
    pub days: BTreeMap<String, DayBook>,
    meta: Meta,
    mem: Memory,
    pub rev: u64,
}

impl Books {
    pub fn open(profile: Profile, dir: PathBuf, key: &[u8]) -> Result<Books, String> {
        std::fs::create_dir_all(dir.join("days")).map_err(|e| e.to_string())?;
        let key: [u8; 32] = key.try_into().map_err(|_| "the key must be 32 bytes".to_string())?;
        let read = |p: &PathBuf| -> Result<Vec<u8>, String> { unseal(&key, &std::fs::read(p).map_err(|e| e.to_string())?) };
        let meta_path = dir.join("books.sealed");
        let meta: Meta = if meta_path.exists() { serde_json::from_slice(&read(&meta_path)?).map_err(|e| e.to_string())? } else { Meta::default() };
        let mut days = BTreeMap::new();
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("days")).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).collect();
        files.sort();
        for f in files.iter().filter(|f| f.extension().is_some_and(|x| x == "sealed")) {
            let d: DayBook = serde_json::from_slice(&read(f)?).map_err(|e| format!("{}: {e}", f.display()))?;
            days.insert(d.day.clone(), d);
        }
        // the series continues from the last day on disk, and the last weeks' bill numbers and
        // totals (kept for matching returns; no shopper is in them) are read back
        let mut mem = Memory { last_no: days.values().map(|d| d.last_no).max().unwrap_or(0), ..Memory::default() };
        let mut recent: Vec<PathBuf> = std::fs::read_dir(dir.join("recent")).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        recent.sort();
        if let Some(f) = recent.iter().find(|f| f.ends_with("refunded.sealed")) {
            mem.refunded = serde_json::from_slice::<Vec<(u64, Paise)>>(&read(f)?).map_err(|e| e.to_string())?.into_iter().collect();
        }
        for f in recent.iter().filter(|f| f.extension().is_some_and(|x| x == "sealed") && !f.ends_with("refunded.sealed")) {
            let day = f.file_stem().unwrap_or_default().to_string_lossy().to_string();
            let rows: Vec<(u64, Paise)> = serde_json::from_slice(&read(f)?).map_err(|e| format!("{}: {e}", f.display()))?;
            for (no, total) in &rows {
                mem.recent.insert(*no, (*total, day.clone()));
            }
            mem.recent_days.push((day, rows.into_iter().map(|r| r.0).collect()));
        }
        Ok(Books { profile, dir, key, days, meta, mem, rev: 1 })
    }
    fn write(&self, name: &str, value: &impl Serialize) -> Result<(), String> {
        let path = self.dir.join(name);
        let tmp = path.with_extension("tmp");
        let plain = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, seal(&self.key, &plain)?).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }
    fn save_meta(&mut self) -> Result<(), String> {
        self.rev += 1;
        self.write("books.sealed", &self.meta)
    }
    pub fn last_no(&self) -> u64 {
        self.mem.last_no
    }
    pub fn now(&self) -> &str {
        &self.meta.now
    }
    pub fn tick(&mut self, at: &str) {
        if at > self.meta.now.as_str() {
            self.meta.now = at.to_string();
        }
    }

    /// Audit a day's bills and keep the day. The bills themselves are not kept.
    pub fn import_day(&mut self, day: &str, bills: &[Bill], cash_banked: Paise) -> Result<String, String> {
        if self.days.contains_key(day) {
            return Err(format!("{day} is already in the books"));
        }
        if let Some(last) = self.days.keys().next_back() {
            if day < last.as_str() {
                return Err(format!("{day} is before {last}, the last day in the books: days go in order"));
            }
        }
        let d = audit_day(&self.profile, day, bills, cash_banked, &mut self.mem);
        let said = format!(
            "{day}: {} bills ({} returns), {} before tax, tax {}; {} exception(s){}; audited in {:.0} ms",
            d.bills,
            d.returns,
            inr(d.taxable()),
            inr(d.tax()),
            d.exceptions.len(),
            if d.exceptions.is_empty() { String::new() } else { format!(" [{}]", count_kinds(&d.exceptions).iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ")) },
            d.audit_ms
        );
        self.write(&format!("days/{day}.sealed"), &d)?;
        // the day's bill numbers and totals, so a return next week (or after a restart) can be
        // matched; days older than the return window are deleted
        std::fs::create_dir_all(self.dir.join("recent")).map_err(|e| e.to_string())?;
        let rows: Vec<(u64, Paise)> = self.mem.recent_days.last().map(|(_, nos)| nos.iter().filter_map(|n| self.mem.recent.get(n).map(|t| (*n, t.0))).collect()).unwrap_or_default();
        self.write(&format!("recent/{day}.sealed"), &rows)?;
        self.write("recent/refunded.sealed", &self.mem.refunded.iter().map(|(k, v)| (*k, *v)).collect::<Vec<(u64, Paise)>>())?;
        let mut keep: BTreeSet<String> = self.mem.recent_days.iter().map(|(d, _)| format!("{d}.sealed")).collect();
        keep.insert("refunded.sealed".into());
        if let Ok(rd) = std::fs::read_dir(self.dir.join("recent")) {
            for e in rd.flatten() {
                if !keep.contains(&e.file_name().to_string_lossy().to_string()) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        self.tick(&format!("{day}T23:00"));
        self.days.insert(day.to_string(), d);
        self.save_meta()?;
        Ok(said)
    }
    pub fn add_inward(&mut self, rows: Vec<Inward>) -> Result<String, String> {
        let n = rows.len();
        self.meta.inward.extend(rows);
        self.save_meta()?;
        Ok(format!("{n} supplier invoices recorded"))
    }

    fn in_period<'a>(&'a self, period: &'a str) -> impl Iterator<Item = &'a DayBook> + 'a {
        self.days.range(period.to_string()..).take_while(move |(k, _)| k.starts_with(period)).map(|(_, d)| d)
    }

    /// Input tax credit for a month: only what the supplier reported, once, and not on the
    /// things the law blocks.
    pub fn itc(&self, period: &str) -> Itc {
        let mut out = Itc::default();
        let mut seen: BTreeSet<(&str, &str)> = self.meta.inward.iter().filter(|x| x.month.as_str() < period).map(|x| (x.gstin.as_str(), x.no.as_str())).collect();
        for x in self.meta.inward.iter().filter(|x| x.month.starts_with(period)) {
            if x.gst == 0 {
                continue;
            }
            if !seen.insert((x.gstin.as_str(), x.no.as_str())) {
                out.duplicate += x.gst;
                out.duplicate_n += 1;
            } else if BLOCKED.contains(&x.category.as_str()) {
                out.blocked += x.gst;
            } else if !x.in_2b {
                out.not_in_2b += x.gst;
                out.not_in_2b_n += 1;
            } else {
                out.eligible += x.gst;
            }
        }
        out
    }

    pub fn month(&self, period: &str) -> MonthGst {
        let mut m = MonthGst { period: period.to_string(), ..MonthGst::default() };
        for d in self.in_period(period) {
            m.days += 1;
            m.bills += d.bills;
            if m.first_no == 0 {
                m.first_no = d.first_no;
            }
            m.last_no = m.last_no.max(d.last_no);
            for (k, v) in &d.by_rate {
                let e = m.by_rate.entry(*k).or_default();
                e.0 += v.0;
                e.1 += v.1;
            }
            for (k, v) in &d.by_hsn {
                let e = m.by_hsn.entry(k.clone()).or_default();
                e.0 += v.0;
                e.1 += v.1;
                e.2 += v.2;
            }
            m.cgst += d.cgst;
            m.sgst += d.sgst;
            m.igst += d.igst;
            m.b2b_taxable += d.b2b_taxable;
            m.b2b_bills += d.b2b_bills;
            for x in &d.exceptions {
                *m.exceptions.entry(x.kind.clone()).or_default() += 1;
                if x.fixed.is_none() {
                    m.unfixed += 1;
                }
                if x.tax_effect < 0 {
                    m.short_charged += -x.tax_effect;
                } else if x.tax_effect > 0 {
                    m.over_charged += x.tax_effect;
                    if x.fixed.as_deref() == Some("credit_note") {
                        m.credit_notes += x.tax_effect;
                    }
                }
            }
        }
        m.itc = self.itc(period);
        m
    }

    // ---- fixes: anyone may propose; only the owner's key applies ----
    /// Propose one fix for every open exception of a kind in a period (a month or a day).
    pub fn propose(&mut self, period: &str, kind: &str, fix: &str, by: &str) -> Result<String, String> {
        if !fixes_for(kind).contains(&fix) {
            return Err(format!("{fix:?} is not a fix for {kind}: choose from {}", fixes_for(kind).join(", ")));
        }
        let ids: Vec<(String, Paise)> = self
            .in_period(period)
            .flat_map(|d| d.exceptions.iter())
            .filter(|x| x.kind == kind && x.fixed.is_none())
            .filter(|x| match fix {
                // a credit note returns tax that was over-charged; a debit note asks for tax short-charged
                "credit_note" => x.tax_effect > 0,
                "debit_note" => x.tax_effect < 0,
                _ => true,
            })
            .map(|x| (x.id.clone(), x.tax_effect))
            .collect();
        if ids.is_empty() {
            return Err(format!("no open {kind} exceptions in {period} that {fix} applies to"));
        }
        let have: BTreeSet<String> = self.meta.proposals.iter().map(|p| p.id.clone()).collect();
        let new: Vec<&(String, Paise)> = ids.iter().filter(|(id, _)| !have.contains(id)).collect();
        let effect: Paise = new.iter().map(|x| x.1).sum();
        let n = new.len();
        for (id, _) in new {
            self.meta.proposals.push(Proposal { id: id.clone(), fix: fix.to_string(), by: by.to_string() });
        }
        self.save_meta()?;
        Ok(format!("{n} {kind} exception(s) in {period} proposed for {fix} (tax effect {}); nothing changes until the owner approves", inr(effect)))
    }
    pub fn proposals(&self, period: &str) -> Vec<&Proposal> {
        self.meta.proposals.iter().filter(|p| p.id.starts_with(period)).collect()
    }
    /// Apply every proposal for a period. Refused once the period's return is filed.
    pub fn apply(&mut self, period: &str) -> Result<String, String> {
        if self.meta.gst_filed.keys().any(|f| period.starts_with(f.as_str())) {
            return Err(format!("the return for {} is filed: corrections go in the next return as amendments", &period[..7.min(period.len())]));
        }
        let todo: Vec<Proposal> = self.meta.proposals.iter().filter(|p| p.id.starts_with(period)).cloned().collect();
        if todo.is_empty() {
            return Err(format!("no proposed fixes for {period}"));
        }
        let mut touched: BTreeSet<String> = BTreeSet::new();
        let mut by_fix: BTreeMap<String, u32> = BTreeMap::new();
        for p in &todo {
            let day = p.id.split('#').next().unwrap_or_default().to_string();
            if let Some(x) = self.days.get_mut(&day).and_then(|d| d.exceptions.iter_mut().find(|x| x.id == p.id)) {
                x.fixed = Some(p.fix.clone());
                *by_fix.entry(p.fix.clone()).or_default() += 1;
                touched.insert(day);
            }
        }
        for day in &touched {
            self.write(&format!("days/{day}.sealed"), &self.days[day])?;
        }
        self.meta.proposals.retain(|p| !p.id.starts_with(period));
        self.save_meta()?;
        Ok(format!("applied in {period}: {}", by_fix.iter().map(|(k, n)| format!("{n} × {k}")).collect::<Vec<_>>().join(", ")))
    }

    /// Credit carried into a month from the months before (credit above the output tax).
    fn carried_into(&self, period: &str) -> Paise {
        let mut carry = 0;
        let months: BTreeSet<String> = self.days.keys().map(|d| d[..7].to_string()).filter(|m| m.as_str() < period).collect();
        for m in months {
            let g = self.month(&m);
            carry = (carry + g.itc.eligible - g.output()).max(0);
        }
        carry
    }
    pub fn gst_cash(&self, period: &str) -> (Paise, Paise) {
        let g = self.month(period);
        let credit = g.itc.eligible + self.carried_into(period);
        ((g.output() - credit).max(0), (credit - g.output()).max(0))
    }

    /// GSTR-1 and GSTR-3B for a finished month, as files, lodged with the simulated portal.
    pub fn file_gst(&mut self, period: &str) -> Result<String, String> {
        let period = period.trim();
        if period.len() != 7 || !self.days.keys().any(|d| d.starts_with(period)) {
            return Err(format!("no days in the books for {period:?} (a month, like 2025-06)"));
        }
        let next = add_days(&format!("{period}-28"), 4)[..7].to_string();
        if self.now() < format!("{next}-01").as_str() {
            return Err(format!("{period} is not over yet"));
        }
        if self.meta.gst_filed.contains_key(period) {
            return Err(format!("{period} was already filed"));
        }
        let g = self.month(period);
        let (cash, carry) = self.gst_cash(period);
        let gstr1 = json!({
            "gstin": self.profile.gstin, "period": period,
            "b2c_by_rate": g.by_rate.iter().map(|(r, (t, x))| json!({"rate": *r as f64 / 100.0, "taxable": t, "tax": x})).collect::<Vec<_>>(),
            "b2b": {"invoices": g.b2b_bills, "taxable": g.b2b_taxable},
            "hsn": g.by_hsn.iter().map(|(h, (q, t, x))| json!({"hsn": h, "pieces": q, "taxable": t, "tax": x})).collect::<Vec<_>>(),
            "documents": {"from": format!("{}{:08}", self.profile.invoice_prefix, g.first_no), "to": format!("{}{:08}", self.profile.invoice_prefix, g.last_no), "issued": g.bills},
            "amounts_in": "paise",
        });
        let gstr3b = json!({
            "gstin": self.profile.gstin, "period": period,
            "outward": {"taxable": g.taxable(), "cgst": g.cgst, "sgst": g.sgst, "igst": g.igst, "short_charged_added": g.short_charged, "credit_notes": g.credit_notes},
            "itc": {"claimed": g.itc.eligible, "not_in_2b_deferred": g.itc.not_in_2b, "blocked": g.itc.blocked, "duplicate_refused": g.itc.duplicate},
            "tax_payable": g.output(), "paid_in_cash": cash, "credit_carried_forward": carry,
            "amounts_in": "paise",
        });
        let dir = self.dir.join("returns");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        for (name, v) in [("gstr1", &gstr1), ("gstr3b", &gstr3b)] {
            std::fs::write(dir.join(format!("{name}-{period}.json")), serde_json::to_string_pretty(v).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        }
        let now = self.now().to_string();
        self.meta.gst_filed.insert(period.to_string(), (now, g.output(), g.itc.eligible, cash));
        self.save_meta()?;
        Ok(format!(
            "GSTR-1 and GSTR-3B for {period} lodged with the simulated portal: output tax {}, credit {}, paid in cash {}{}",
            inr(g.output()),
            inr(g.itc.eligible),
            inr(cash),
            if g.unfixed > 0 { format!("; {} exception(s) were still open", g.unfixed) } else { String::new() }
        ))
    }
    pub fn filed(&self) -> &BTreeMap<String, (String, Paise, Paise, Paise)> {
        &self.meta.gst_filed
    }

    /// The year: turnover against money received and against the returns, the profit and loss
    /// account, and the tax on it.
    pub fn year(&self) -> Year {
        let p = &self.profile;
        let mut y = Year::default();
        let months: BTreeSet<String> = self.days.keys().map(|d| d[..7].to_string()).collect();
        for d in self.days.values() {
            y.days += 1;
            y.bills += d.bills as u64;
            y.turnover += d.taxable();
            y.gross_billed += d.taxable() + d.tax();
            y.received += d.cash_banked + d.upi + d.card;
            y.cash_short += (d.cash - d.cash_banked).max(0);
            y.cogs += d.cost;
            for x in &d.exceptions {
                *y.exceptions.entry(x.kind.clone()).or_default() += 1;
            }
        }
        for m in &months {
            let g = self.month(m);
            y.output_tax += g.output();
            y.itc.eligible += g.itc.eligible;
            y.itc.blocked += g.itc.blocked;
            y.itc.not_in_2b += g.itc.not_in_2b;
            y.itc.duplicate += g.itc.duplicate;
            y.itc.not_in_2b_n += g.itc.not_in_2b_n;
            y.itc.duplicate_n += g.itc.duplicate_n;
            y.gst_cash += self.gst_cash(m).0;
            if !self.meta.gst_filed.contains_key(m) {
                y.unfiled.push(m.clone());
            }
        }
        // the expense heads, from the suppliers' invoices (each counted once)
        let mut heads: BTreeMap<String, Paise> = BTreeMap::new();
        let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
        let mut rent_no_tds = 0;
        let mut cash_over = 0;
        for x in &self.meta.inward {
            if !seen.insert((x.gstin.as_str(), x.no.as_str())) && !x.gstin.is_empty() {
                continue;
            }
            match x.category.as_str() {
                "goods" => y.purchases += x.taxable,
                "fitout" => {}
                c => *heads.entry(c.to_string()).or_default() += x.taxable,
            }
            if x.category == "rent" && !x.tds_deducted {
                rent_no_tds += x.taxable;
            }
            if x.cash && x.taxable > rs(p.tax.cash_limit) {
                cash_over += x.taxable;
            }
        }
        let lost = (y.cogs as f64 * p.expenses.shrinkage_pct / 100.0) as Paise;
        y.closing_stock = (y.purchases - y.cogs - lost).max(0);
        let n = months.len() as Paise;
        y.expenses = heads.into_iter().collect();
        y.expenses.push(("salaries".into(), rs(p.expenses.salaries) * n));
        y.expenses.push(("depreciation on fit-out".into(), (rs(p.expenses.fitout) as f64 * p.expenses.depreciation_pct / 100.0 * n as f64 / 12.0) as Paise));
        y.expenses.push(("GST with no credit (blocked)".into(), y.itc.blocked - rs(p.expenses.fitout * 0.18).min(y.itc.blocked)));
        y.expenses.push(("cash short at the tills".into(), y.cash_short));
        if lost > 0 {
            y.expenses.push(("stock lost (shrinkage)".into(), lost));
        }
        let spent: Paise = y.expenses.iter().map(|e| e.1).sum();
        y.profit = y.turnover - y.cogs - spent;
        if rent_no_tds > 0 {
            y.disallowed.push(("rent paid without deducting tax at source".into(), (rent_no_tds as f64 * p.tax.tds_default_disallow_pct / 100.0) as Paise));
        }
        if cash_over > 0 {
            y.disallowed.push((format!("cash payments above ₹{:.0}", p.tax.cash_limit), cash_over));
        }
        y.taxable_income = (y.profit + y.disallowed.iter().map(|d| d.1).sum::<Paise>()).max(0);
        y.tax = (y.taxable_income as f64 * p.tax.rate_pct / 100.0).round() as Paise;
        y.surcharge = (y.tax as f64 * p.tax.surcharge_pct / 100.0).round() as Paise;
        y.cess = ((y.tax + y.surcharge) as f64 * p.tax.cess_pct / 100.0).round() as Paise;
        y
    }
    pub fn file_itr(&mut self) -> Result<String, String> {
        if self.now() <= self.profile.fy_end.as_str() {
            return Err(format!("the year ends on {}: the return is for a finished year", self.profile.fy_end));
        }
        let y = self.year();
        if !y.unfiled.is_empty() {
            return Err(format!("GST returns are not filed for {}: file them first, so the turnover in both agrees", y.unfiled.join(", ")));
        }
        if self.meta.itr_filed.is_some() {
            return Err("the income-tax return was already filed".into());
        }
        let v = json!({
            "name": self.profile.name, "year": format!("{} to {}", self.profile.fy_start, self.profile.fy_end),
            "turnover": y.turnover, "cost_of_goods_sold": y.cogs, "purchases": y.purchases, "closing_stock": y.closing_stock,
            "expenses": y.expenses.iter().map(|(k, v)| json!({"head": k, "amount": v})).collect::<Vec<_>>(),
            "profit_per_books": y.profit,
            "disallowed": y.disallowed.iter().map(|(k, v)| json!({"reason": k, "amount": v})).collect::<Vec<_>>(),
            "taxable_income": y.taxable_income, "tax": y.tax, "surcharge": y.surcharge, "cess": y.cess,
            "total_tax": y.tax + y.surcharge + y.cess, "amounts_in": "paise",
        });
        let dir = self.dir.join("returns");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("income-tax-computation.json"), serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        self.meta.itr_filed = Some(self.now().to_string());
        self.save_meta()?;
        Ok(format!("income-tax computation lodged with the simulated portal: income {}, tax {}", inr(y.taxable_income), inr(y.tax + y.surcharge + y.cess)))
    }

    // ---- answers, as text ----
    pub fn exceptions<'a>(&'a self, period: &'a str, kind: &str) -> Vec<&'a Exception> {
        self.in_period(period).flat_map(|d| d.exceptions.iter()).filter(|x| kind.is_empty() || x.kind == kind).collect()
    }
    pub fn audit_text(&self, period: &str) -> String {
        let all = self.exceptions(period, "");
        let (days, bills): (u32, u64) = self.in_period(period).fold((0, 0), |a, d| (a.0 + 1, a.1 + d.bills as u64));
        if days == 0 {
            return format!("no days in the books for {period}");
        }
        let mut kinds: BTreeMap<&str, (u32, u32, Paise, Paise)> = BTreeMap::new();
        for x in &all {
            let e = kinds.entry(x.kind.as_str()).or_default();
            e.0 += 1;
            if x.fixed.is_none() {
                e.1 += 1;
            }
            if x.tax_effect < 0 {
                e.2 += -x.tax_effect;
            } else {
                e.3 += x.tax_effect;
            }
        }
        let mut out = vec![format!("Audit of {period}: {bills} bills over {days} day(s), {} exception(s), {} open", all.len(), all.iter().filter(|x| x.fixed.is_none()).count())];
        for (k, (n, open, short, over)) in &kinds {
            let money = match (short, over) {
                (0, 0) => String::new(),
                _ => format!(" — tax short-charged {}, over-charged {}", inr(*short), inr(*over)),
            };
            out.push(format!("  {k:<17} {n:>5} ({open} open){money}; fixes: {}", fixes_for(k).join(" / ")));
        }
        let pending = self.proposals(period).len();
        if pending > 0 {
            out.push(format!("{pending} fix(es) proposed and waiting for the owner's key"));
        }
        out.join("\n")
    }
    pub fn exceptions_text(&self, period: &str, kind: &str) -> String {
        let v = self.exceptions(period, kind);
        if v.is_empty() {
            return format!("no {kind} exceptions in {period}");
        }
        let mut out = vec![format!("{} {kind} exception(s) in {period}; the first {}:", v.len(), v.len().min(12))];
        for x in v.iter().take(12) {
            out.push(format!(
                "  {} invoice {}{:08}: {}{}",
                x.id,
                self.profile.invoice_prefix,
                x.bill,
                x.detail,
                x.fixed.as_ref().map(|f| format!(" [fixed: {f}]")).unwrap_or_default()
            ));
        }
        out.join("\n")
    }
    pub fn gst_text(&self, period: &str) -> String {
        let g = self.month(period);
        if g.days == 0 {
            return format!("no days in the books for {period}");
        }
        let (cash, carry) = self.gst_cash(period);
        let mut out = vec![format!("GST for {period} — {} (GSTIN {}): {} bills, invoices {:08} to {:08}", self.profile.name, self.profile.gstin, g.bills, g.first_no, g.last_no)];
        for (r, (t, x)) in &g.by_rate {
            out.push(format!("  at {:>4}%: taxable {:>16}, tax {:>14}", *r as f64 / 100.0, inr(*t), inr(*x)));
        }
        out.push(format!("  CGST {}, SGST {}, IGST {}; to registered buyers: {} bills, {}", inr(g.cgst), inr(g.sgst), inr(g.igst), g.b2b_bills, inr(g.b2b_taxable)));
        out.push(format!(
            "Output tax: billed {} + short-charged by the tills {} − credit notes {} = {}",
            inr(g.billed_tax()),
            inr(g.short_charged),
            inr(g.credit_notes),
            inr(g.output())
        ));
        if g.over_charged > g.credit_notes {
            out.push(format!("  ({} was over-charged to customers and stays payable unless credit notes return it)", inr(g.over_charged - g.credit_notes)));
        }
        out.push(format!(
            "Input tax credit: claim {}; held back {} ({} invoice(s) the supplier has not reported); not allowed {} (staff food, building work); refused {} ({} entered twice)",
            inr(g.itc.eligible),
            inr(g.itc.not_in_2b),
            g.itc.not_in_2b_n,
            inr(g.itc.blocked),
            inr(g.itc.duplicate),
            g.itc.duplicate_n
        ));
        out.push(format!("To pay in cash: {}{}", inr(cash), if carry > 0 { format!("; credit carried forward {}", inr(carry)) } else { String::new() }));
        match self.meta.gst_filed.get(period) {
            Some((at, ..)) => out.push(format!("Filed on {at}")),
            None => out.push(format!("Not filed. {} exception(s) open.", g.unfixed)),
        }
        out.join("\n")
    }
    pub fn year_text(&self) -> String {
        let y = self.year();
        if y.days == 0 {
            return "no days in the books".into();
        }
        let total = y.tax + y.surcharge + y.cess;
        let mut out = vec![
            format!("{} — {} to {}: {} days, {} bills", self.profile.name, self.days.keys().next().unwrap(), self.days.keys().next_back().unwrap(), y.days, y.bills),
            format!("Turnover (before GST) {}; GST on it {}; billed in all {}", inr(y.turnover), inr(y.gross_billed - y.turnover), inr(y.gross_billed)),
            format!(
                "Tally with money received: {} came in (cash banked, UPI, card), {} less than billed; {} of that is cash short at the tills, the rest is bills whose payment or total did not match (see payment_mismatch and total_mismatch)",
                inr(y.received),
                inr(y.gross_billed - y.received),
                inr(y.cash_short)
            ),
            format!(
                "Tally with the GST returns: output tax {}, credit claimed {}, paid in cash {}; {}",
                inr(y.output_tax),
                inr(y.itc.eligible),
                inr(y.gst_cash),
                if y.unfiled.is_empty() { "every month filed".to_string() } else { format!("not filed: {}", y.unfiled.join(", ")) }
            ),
            format!(
                "Credit not taken: {} held back until suppliers report {} invoice(s); {} blocked; {} refused as entered twice",
                inr(y.itc.not_in_2b),
                y.itc.not_in_2b_n,
                inr(y.itc.blocked),
                inr(y.itc.duplicate)
            ),
            "Profit and loss".to_string(),
            format!("  turnover                              {:>18}", inr(y.turnover)),
            format!("  cost of goods sold                    {:>18}   (purchases {}, closing stock {})", inr(y.cogs), inr(y.purchases), inr(y.closing_stock)),
        ];
        for (k, v) in &y.expenses {
            out.push(format!("  {:<37} {:>18}", k.replace('_', " "), inr(*v)));
        }
        out.push(format!("  profit per books                      {:>18}   ({:.1}% of turnover)", inr(y.profit), y.profit as f64 * 100.0 / y.turnover.max(1) as f64));
        for (k, v) in &y.disallowed {
            out.push(format!("  add back: {:<27} {:>18}", k, inr(*v)));
        }
        out.push(format!("  taxable income                        {:>18}", inr(y.taxable_income)));
        out.push(format!(
            "Income tax at {}% {} + surcharge {} + cess {} = {}",
            self.profile.tax.rate_pct,
            inr(y.tax),
            inr(y.surcharge),
            inr(y.cess),
            inr(total)
        ));
        out.push(format!(
            "Advance tax, if paid on time: {} by 15 Jun, {} by 15 Sep, {} by 15 Dec, {} by 15 Mar",
            inr(total * 15 / 100),
            inr(total * 45 / 100),
            inr(total * 75 / 100),
            inr(total)
        ));
        out.push(format!("Exceptions in the year: {}", y.exceptions.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ")));
        out.join("\n")
    }
    /// The figures as data (for a dashboard). Totals only.
    pub fn state(&self) -> Value {
        let y = self.year();
        let months: BTreeSet<String> = self.days.keys().map(|d| d[..7].to_string()).collect();
        json!({
            "store": self.profile.name, "days": y.days, "bills": y.bills, "turnover": y.turnover, "gross_billed": y.gross_billed,
            "received": y.received, "cash_short": y.cash_short, "output_tax": y.output_tax, "itc": y.itc.eligible,
            "itc_not_in_2b": y.itc.not_in_2b, "itc_blocked": y.itc.blocked, "itc_duplicate": y.itc.duplicate, "gst_cash": y.gst_cash,
            "cogs": y.cogs, "purchases": y.purchases, "closing_stock": y.closing_stock,
            "expenses": y.expenses, "disallowed": y.disallowed, "profit": y.profit, "taxable_income": y.taxable_income,
            "income_tax": y.tax + y.surcharge + y.cess, "exceptions": y.exceptions, "unfiled": y.unfiled,
            "months": months.iter().map(|m| { let g = self.month(m); let (cash, carry) = self.gst_cash(m);
                json!({"month": m, "bills": g.bills, "taxable": g.taxable(), "output": g.output(), "short_charged": g.short_charged,
                    "over_charged": g.over_charged, "credit_notes": g.credit_notes, "itc": g.itc.eligible, "cash": cash, "carry": carry,
                    "exceptions": g.exceptions, "open": g.unfixed, "filed": self.meta.gst_filed.contains_key(m)}) }).collect::<Vec<_>>(),
            "audit_ms": self.days.values().map(|d| d.audit_ms).sum::<f64>() / y.days.max(1) as f64,
            "amounts_in": "paise",
        })
    }
}

pub fn count_kinds(v: &[Exception]) -> BTreeMap<String, u32> {
    let mut m = BTreeMap::new();
    for x in v {
        *m.entry(x.kind.clone()).or_default() += 1;
    }
    m
}
