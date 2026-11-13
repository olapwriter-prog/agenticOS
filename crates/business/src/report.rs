//! Everything read from the book: the day's figures, the riders' sheet, what the platforms owe,
//! the GST return, and the one dashboard. All of it is arithmetic over the sealed book, so the
//! answers are exact and no model sees the data.
use crate::util::*;
use crate::{Business, Order};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct ChannelStat {
    pub orders: u32,
    pub delivered: u32,
    pub cancelled: u32,
    /// Taxable value of delivered orders.
    pub sales: Paise,
    /// What reaches the business: sales less platform cuts (platforms) or rider pay (direct).
    pub net: Paise,
}
#[derive(Debug, Clone, Default)]
pub struct DayStats {
    pub day: String,
    pub channels: BTreeMap<String, ChannelStat>,
    pub items: BTreeMap<String, u32>,
    pub hours: [u32; 24],
    pub open: Vec<String>,
    pub gst_direct: Paise,
    pub cod: Paise,
}
impl DayStats {
    pub fn orders(&self) -> u32 {
        self.channels.values().map(|c| c.orders).sum()
    }
    pub fn delivered(&self) -> u32 {
        self.channels.values().map(|c| c.delivered).sum()
    }
    pub fn cancelled(&self) -> u32 {
        self.channels.values().map(|c| c.cancelled).sum()
    }
    pub fn sales(&self) -> Paise {
        self.channels.values().map(|c| c.sales).sum()
    }
    pub fn net(&self) -> Paise {
        self.channels.values().map(|c| c.net).sum()
    }
}

#[derive(Debug, Clone)]
pub struct RiderSheet {
    pub id: String,
    pub name: String,
    pub deliveries: u32,
    pub km: f64,
    pub earnings: Paise,
    /// Cash-on-delivery money collected and not yet handed over.
    pub cash_held: Paise,
    pub paid: Paise,
    pub due: Paise,
}
#[derive(Debug, Clone)]
pub struct Recon {
    pub platform: String,
    pub from: String,
    pub to: String,
    pub expected: Paise,
    pub received: Paise,
    pub cancelled_orders: u32,
    pub cancelled_amount: Paise,
    pub reference: String,
}
impl Recon {
    pub fn short(&self) -> Paise {
        self.expected - self.received
    }
}
#[derive(Debug, Clone, Default)]
pub struct Gst {
    pub period: String,
    pub invoices: u32,
    pub cancelled_invoices: u32,
    pub first_invoice: String,
    pub last_invoice: String,
    pub direct_taxable: Paise,
    pub direct_cgst: Paise,
    pub direct_sgst: Paise,
    /// (platform, taxable value, tax the platform pays).
    pub eco: Vec<(String, Paise, Paise)>,
    pub input_purchases: Paise,
    pub input_commission: Paise,
    pub itc_claimable: Paise,
    pub itc_blocked: Paise,
    pub payable: Paise,
}

impl Business {
    fn on_day<'a>(&'a self, day: &'a str) -> impl Iterator<Item = &'a Order> + 'a {
        self.book.orders.iter().filter(move |o| day_of(&o.at) == day)
    }
    pub fn rider_cost(&self, o: &Order) -> Paise {
        rs(self.profile.rider_pay_per_delivery) + rs(self.profile.rider_pay_per_km * o.km)
    }

    pub fn day_stats(&self, day: &str) -> DayStats {
        let mut s = DayStats { day: day.to_string(), ..DayStats::default() };
        for o in self.on_day(day) {
            let v = self.values(o);
            let direct = self.is_direct(o);
            let c = s.channels.entry(o.channel.clone()).or_default();
            c.orders += 1;
            s.hours[hour_of(&o.at)] += 1;
            match o.status.as_str() {
                "delivered" => {
                    c.delivered += 1;
                    c.sales += v.taxable;
                    if direct {
                        c.net += v.taxable - self.rider_cost(o);
                        s.gst_direct += v.cgst + v.sgst;
                        if o.payment == "cod" {
                            s.cod += v.total;
                        }
                    } else {
                        c.net += v.payout;
                    }
                    for l in &o.lines {
                        *s.items.entry(l.code.clone()).or_default() += l.qty;
                    }
                }
                "cancelled" => c.cancelled += 1,
                _ => s.open.push(format!("{} {} ({})", o.id, o.status, o.channel)),
            }
        }
        s
    }

    pub fn rider_sheet(&self, day: &str) -> Vec<RiderSheet> {
        let p = &self.profile;
        p.riders
            .iter()
            .map(|r| {
                let mine: Vec<&Order> = self
                    .on_day(day)
                    .filter(|o| o.status == "delivered" && o.rider.as_deref() == Some(r.id.as_str()))
                    .collect();
                let km: f64 = mine.iter().map(|o| o.km).sum();
                let earnings: Paise = mine.iter().map(|o| self.rider_cost(o)).sum();
                let cod: Paise = mine.iter().filter(|o| o.payment == "cod").map(|o| self.values(o).total).sum();
                let sum = |v: &[crate::RiderMoney]| -> Paise {
                    v.iter().filter(|m| m.day == day && m.rider == r.id).map(|m| m.amount).sum()
                };
                let paid = sum(&self.book.rider_paid);
                RiderSheet {
                    id: r.id.clone(),
                    name: r.name.clone(),
                    deliveries: mine.len() as u32,
                    km,
                    earnings,
                    cash_held: cod - sum(&self.book.rider_cash),
                    paid,
                    due: earnings - paid,
                }
            })
            .collect()
    }

    /// Each payout a platform made, against what the book says it owed for those days.
    pub fn reconcile(&self) -> Vec<Recon> {
        let rev = self.rev.get();
        if let Some((at, rows)) = &*self.recon.borrow() {
            if *at == rev {
                return rows.clone();
            }
        }
        // one pass over the orders: what each platform owes for each day
        let mut owed: BTreeMap<(&str, &str), (Paise, u32, Paise)> = BTreeMap::new();
        for o in &self.book.orders {
            if self.platform_owes(o) {
                let pay = self.values(o).payout;
                let e = owed.entry((o.channel.as_str(), day_of(&o.at))).or_default();
                e.0 += pay;
                if o.status == "cancelled" {
                    e.1 += 1;
                    e.2 += pay;
                }
            }
        }
        let rows: Vec<Recon> = self
            .book
            .payouts
            .iter()
            .map(|p| {
                let mut r = Recon {
                    platform: p.platform.clone(),
                    from: p.from.clone(),
                    to: p.to.clone(),
                    expected: 0,
                    received: p.amount,
                    cancelled_orders: 0,
                    cancelled_amount: 0,
                    reference: p.reference.clone(),
                };
                for (_, (pay, n, amt)) in owed.range((p.platform.as_str(), p.from.as_str())..=(p.platform.as_str(), p.to.as_str())) {
                    r.expected += pay;
                    r.cancelled_orders += n;
                    r.cancelled_amount += amt;
                }
                r
            })
            .collect();
        *self.recon.borrow_mut() = Some((rev, rows.clone()));
        rows
    }
    /// What a platform still owes for days it has not paid for yet.
    pub fn awaiting(&self, platform: &str) -> Paise {
        let paid_to = self.book.payouts.iter().filter(|p| p.platform == platform).map(|p| p.to.clone()).max().unwrap_or_default();
        self.book
            .orders
            .iter()
            .filter(|o| o.channel == platform && day_of(&o.at) > paid_to.as_str() && self.platform_owes(o))
            .map(|o| self.values(o).payout)
            .sum()
    }
    /// What can be claimed from a platform now, and why. Shortfalls under ₹1 are rounding.
    pub fn claimable(&self, platform: &str) -> (Paise, String) {
        let rows: Vec<Recon> = self.reconcile().into_iter().filter(|r| r.platform == platform && r.short() > 100).collect();
        let short: Paise = rows.iter().map(|r| r.short()).sum();
        let claimed: Paise = self.book.claims.iter().filter(|c| c.platform == platform).map(|c| c.amount).sum();
        let (n, amt): (u32, Paise) = rows.iter().fold((0, 0), |a, r| (a.0 + r.cancelled_orders, a.1 + r.cancelled_amount));
        let other = short - amt.min(short);
        let mut why = Vec::new();
        if n > 0 {
            why.push(format!("{n} orders cancelled after the food was made, unpaid ({})", inr(amt.min(short))));
        }
        if other > 0 {
            why.push(format!("deductions not in the agreement ({})", inr(other)));
        }
        let periods: Vec<String> = rows.iter().map(|r| format!("{} to {} ref {}", r.from, r.to, r.reference)).collect();
        ((short - claimed).max(0), format!("{}; payouts {}", why.join("; "), periods.join(", ")))
    }

    /// The month's GST figures. `period` is "YYYY-MM".
    pub fn gst(&self, period: &str) -> Gst {
        let p = &self.profile;
        let mut g = Gst { period: period.to_string(), ..Gst::default() };
        let mut eco: BTreeMap<String, (Paise, Paise)> = BTreeMap::new();
        for o in self.book.orders.iter().filter(|o| o.at.starts_with(period)) {
            let v = self.values(o);
            if self.is_direct(o) {
                g.invoices += 1;
                let inv = o.invoice_no.clone().unwrap_or_default();
                if g.first_invoice.is_empty() {
                    g.first_invoice = inv.clone();
                }
                g.last_invoice = inv;
                if o.status == "cancelled" {
                    g.cancelled_invoices += 1;
                } else {
                    // an open order is still a supply once it is delivered; count it as billed
                    g.direct_taxable += v.taxable;
                    g.direct_cgst += v.cgst;
                    g.direct_sgst += v.sgst;
                }
            } else if o.status == "delivered" {
                let e = eco.entry(o.channel.clone()).or_default();
                e.0 += v.taxable;
                e.1 += v.cgst + v.sgst;
                g.input_commission += v.commission_gst;
            }
        }
        g.eco = eco.into_iter().map(|(k, (t, x))| (k, t, x)).collect();
        g.input_purchases = self.book.purchases.iter().filter(|x| x.day.starts_with(period)).map(|x| x.gst).sum();
        let input = g.input_purchases + g.input_commission;
        let output = g.direct_cgst + g.direct_sgst;
        if p.itc_allowed() {
            g.itc_claimable = input.min(output);
            g.itc_blocked = 0;
        } else {
            g.itc_blocked = input;
        }
        g.payable = output - g.itc_claimable;
        g
    }
    /// GSTR-1 and GSTR-3B figures as files, ready for the portal. Only a finished month.
    pub fn file_gst(&mut self, period: &str) -> Result<String, String> {
        let period = period.trim();
        if period.len() != 7 || day_number(&format!("{period}-01")).is_none() {
            return Err("the period is a month, like 2026-10".into());
        }
        let today = self.today();
        if period >= &today[..7.min(today.len())] {
            return Err(format!("{period} is not over yet; file it after the month ends"));
        }
        if self.book.gst_filed.iter().any(|(p, _)| p == period) {
            return Err(format!("{period} was already filed"));
        }
        let g = self.gst(period);
        if g.invoices == 0 && g.eco.is_empty() {
            return Err(format!("no sales in {period}"));
        }
        let half = self.profile.food_gst_pct / 2.0;
        let gstr1 = json!({
            "gstin": self.profile.gstin, "period": period,
            "b2c_others": [{"place_of_supply": self.profile.state_code, "rate": self.profile.food_gst_pct,
                "taxable": g.direct_taxable, "cgst": g.direct_cgst, "sgst": g.direct_sgst}],
            "supplies_through_eco_9_5": g.eco.iter().map(|(n, t, x)| json!({"operator": n, "taxable": t, "tax_paid_by_operator": x})).collect::<Vec<_>>(),
            "hsn": [{"sac": self.profile.sac, "taxable": g.direct_taxable, "cgst_rate": half, "sgst_rate": half}],
            "documents": {"from": g.first_invoice, "to": g.last_invoice, "issued": g.invoices, "cancelled": g.cancelled_invoices},
            "amounts_in": "paise"
        });
        let gstr3b = json!({
            "gstin": self.profile.gstin, "period": period,
            "outward_taxable_3_1_a": {"taxable": g.direct_taxable, "cgst": g.direct_cgst, "sgst": g.direct_sgst},
            "through_eco_paid_by_operator_3_1_1_ii": g.eco.iter().map(|(_, t, _)| t).sum::<Paise>(),
            "itc": {"scheme": self.profile.gst_scheme, "claimed": g.itc_claimable, "not_available": g.itc_blocked},
            "tax_payable_in_cash": g.payable,
            "amounts_in": "paise"
        });
        let dir = self.dir.join("returns");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        for (name, v) in [("gstr1", &gstr1), ("gstr3b", &gstr3b)] {
            let text = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
            std::fs::write(dir.join(format!("{name}-{period}.json")), text).map_err(|e| e.to_string())?;
        }
        let now = self.book.now.clone();
        self.book.gst_filed.push((period.to_string(), now));
        self.record("gst portal (simulated)", "GSTN", "return", &format!("GSTR-1 and GSTR-3B {period}"), gstr1.to_string().len() + gstr3b.to_string().len());
        self.save()?;
        Ok(format!(
            "GSTR-1 and GSTR-3B for {period} prepared in {} and lodged with the simulated portal: tax to pay {}",
            dir.display(),
            inr(g.payable)
        ))
    }

    // ---- answers, as text ----
    pub fn dashboard(&self) -> String {
        let day = self.today();
        let s = self.day_stats(&day);
        let mut out = vec![format!("{} — {} {}", self.profile.name, WEEKDAYS[weekday(&day)], day)];
        out.push(format!(
            "Orders {}: delivered {}, cancelled {}, open {}. Sales {} · reaches you {}",
            s.orders(),
            s.delivered(),
            s.cancelled(),
            s.open.len(),
            inr(s.sales()),
            inr(s.net())
        ));
        for (name, c) in &s.channels {
            out.push(format!(
                "  {:<9} {:>3} orders  sales {:>9}  reaches you {:>9}  ({:.0}%)",
                name,
                c.orders,
                inr(c.sales),
                inr(c.net),
                if c.sales > 0 { c.net as f64 * 100.0 / c.sales as f64 } else { 0.0 }
            ));
        }
        let mut top: Vec<(&String, &u32)> = s.items.iter().collect();
        top.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        out.push(format!(
            "Top: {}",
            top.iter()
                .take(5)
                .map(|(c, q)| format!("{} ×{q}", self.profile.item(c).map(|m| m.name.as_str()).unwrap_or(c)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        if !s.open.is_empty() {
            out.push(format!("Open now: {}", s.open.join(", ")));
        }
        let riders = self.rider_sheet(&day);
        let (due, held): (Paise, Paise) = riders.iter().fold((0, 0), |a, r| (a.0 + r.due, a.1 + r.cash_held));
        out.push(format!("Riders: {} to hand over as earnings, {} of customers' cash with them", inr(due), inr(held)));
        for pf in &self.profile.platforms {
            let n = pf.name.to_lowercase();
            let (can, _) = self.claimable(&n);
            out.push(format!(
                "{}: {} awaiting payout{}",
                pf.name,
                inr(self.awaiting(&n)),
                if can > 0 { format!(", {} short in past payouts — claim it", inr(can)) } else { String::new() }
            ));
        }
        let g = self.gst(&day[..7]);
        out.push(format!(
            "GST this month: {} to pay on direct sales; platforms pay on {}",
            inr(g.payable),
            inr(g.eco.iter().map(|e| e.1).sum())
        ));
        let issues = self.issues();
        if !issues.is_empty() {
            out.push("Needs you:".into());
            out.extend(issues.iter().map(|i| format!("  ! {i}")));
        }
        out.join("\n")
    }

    /// What needs the owner now, found by rules over the book. Each line says what to do.
    pub fn issues(&self) -> Vec<String> {
        let day = self.today();
        let now = self.now().to_string();
        let mut out = Vec::new();
        let late: Vec<String> = self
            .on_day(&day)
            .filter(|o| !matches!(o.status.as_str(), "delivered" | "cancelled") && minutes(&now) - minutes(&o.at) > 45)
            .map(|o| format!("{} ({}, {} min)", o.id, o.status, minutes(&now) - minutes(&o.at)))
            .collect();
        if !late.is_empty() {
            out.push(format!("{} order(s) open for over 45 minutes: {}", late.len(), late.join(", ")));
        }
        let unbilled = self.on_day(&day).filter(|o| self.is_direct(o) && !o.bill_sent && o.status != "cancelled").count();
        if unbilled > 0 {
            out.push(format!("{unbilled} direct order(s) today have no bill sent: say 'send bill for <order>'"));
        }
        for r in self.rider_sheet(&day) {
            if r.cash_held >= 200_000 {
                out.push(format!("{} is carrying {} of customers' cash: take it in", r.name, inr(r.cash_held)));
            }
        }
        let s = self.day_stats(&day);
        for (n, c) in &s.channels {
            if c.orders >= 8 && c.cancelled * 100 / c.orders >= 10 {
                out.push(format!("{n}: {} of {} orders cancelled today", c.cancelled, c.orders));
            }
        }
        for pf in &self.profile.platforms {
            let (can, _) = self.claimable(&pf.name.to_lowercase());
            if can > 0 {
                // the amount is read from the book when the owner does not say one
                out.push(format!("{} underpaid {}: say 'claim it from {}'", pf.name, inr(can), pf.name.to_lowercase()));
            }
        }
        // last month's return: due once the month is over
        if day.len() >= 10 {
            let last = add_days(&format!("{}-01", &day[..7]), -1)[..7].to_string();
            let g = self.gst(&last);
            if (g.invoices > 0 || !g.eco.is_empty()) && !self.book.gst_filed.iter().any(|(p, _)| p == &last) {
                out.push(format!("GST return for {last} is not filed: say 'file gst for {last}'"));
            }
        }
        let cut = add_days(&day, -self.profile.retain_customer_days);
        let stale = self.book.customers.values().filter(|c| c.last_day < cut).count();
        if stale > 0 && !self.profile.auto_erase {
            out.push(format!(
                "{stale} customer(s) are past the {}-day limit: say 'forget customers older than {} days'",
                self.profile.retain_customer_days, self.profile.retain_customer_days
            ));
        }
        out
    }

    pub fn orders_text(&self) -> String {
        let day = self.today();
        let mut rows: Vec<String> = self
            .on_day(&day)
            .map(|o| {
                format!(
                    "{} {} {:<9} {:<10} {:>7}  {} · {}{}",
                    &o.at[11..],
                    o.id,
                    o.channel,
                    o.status,
                    inr(self.values(o).total),
                    self.who(o),
                    o.locality,
                    o.rider.as_ref().map(|r| format!(" · rider {r}")).unwrap_or_default()
                )
            })
            .collect();
        if rows.is_empty() {
            return format!("no orders on {day}");
        }
        let n = rows.len();
        if n > 12 {
            rows = rows.split_off(n - 12);
            rows.insert(0, format!("{n} orders on {day}; the last 12:"));
        }
        rows.join("\n")
    }

    pub fn riders_text(&self) -> String {
        let day = self.today();
        let rows: Vec<String> = self
            .rider_sheet(&day)
            .iter()
            .map(|r| {
                format!(
                    "{} ({}): {} deliveries, {:.1} km → earnings {}, paid {}, due {}; customers' cash to hand in {}",
                    r.name, r.id, r.deliveries, r.km, inr(r.earnings), inr(r.paid), inr(r.due), inr(r.cash_held)
                )
            })
            .collect();
        format!("Riders on {day}\n{}", rows.join("\n"))
    }

    pub fn payouts_text(&self) -> String {
        let mut out = Vec::new();
        for pf in &self.profile.platforms {
            let n = pf.name.to_lowercase();
            out.push(format!("{} (commission {}% + GST, TDS {}%)", pf.name, pf.commission_pct, pf.tds_pct));
            for r in self.reconcile().iter().filter(|r| r.platform == n) {
                out.push(format!(
                    "  {} to {}: owed {}, paid {}{}",
                    r.from,
                    r.to,
                    inr(r.expected),
                    inr(r.received),
                    if r.short() > 100 { format!(" — SHORT {}", inr(r.short())) } else { " — ok".into() }
                ));
            }
            out.push(format!("  awaiting payout: {}", inr(self.awaiting(&n))));
            let (can, why) = self.claimable(&n);
            if can > 0 {
                out.push(format!("  to claim: {} — {why}", inr(can)));
            }
            for c in self.book.claims.iter().filter(|c| c.platform == n) {
                out.push(format!("  claimed {} on {} ({})", inr(c.amount), c.day, c.reference));
            }
        }
        out.join("\n")
    }

    pub fn gst_text(&self, period: &str) -> String {
        let period = if period.trim().len() == 7 { period.trim().to_string() } else { self.today()[..7].to_string() };
        let g = self.gst(&period);
        let eco_taxable: Paise = g.eco.iter().map(|e| e.1).sum();
        let mut out = vec![
            format!("GST for {period} — {} (GSTIN {})", self.profile.name, self.profile.gstin),
            format!(
                "Direct sales (you collect and pay): {} invoices {}..{} ({} cancelled), taxable {}, CGST {}, SGST {}",
                g.invoices, g.first_invoice, g.last_invoice, g.cancelled_invoices, inr(g.direct_taxable), inr(g.direct_cgst), inr(g.direct_sgst)
            ),
            format!("Through platforms (they collect and pay the tax, you only report): taxable {}", inr(eco_taxable)),
        ];
        for (n, t, x) in &g.eco {
            out.push(format!("  {n}: taxable {}, tax paid by {n} {}", inr(*t), inr(*x)));
        }
        out.push(format!(
            "Input tax paid: {} on purchases, {} on platform commission",
            inr(g.input_purchases),
            inr(g.input_commission)
        ));
        out.push(if self.profile.itc_allowed() {
            format!("Input tax credit claimed: {}", inr(g.itc_claimable))
        } else {
            format!(
                "Input tax credit: none. At the {}% restaurant rate credit is not allowed, so {} of input tax is a cost",
                self.profile.food_gst_pct,
                inr(g.itc_blocked)
            )
        });
        out.push(format!("Tax to pay in cash: {}", inr(g.payable)));
        if let Some((_, at)) = self.book.gst_filed.iter().find(|(p, _)| p == &period) {
            out.push(format!("Filed on {at}"));
        }
        out.join("\n")
    }

    pub fn privacy_text(&self) -> String {
        let day = self.today();
        let mut kinds: BTreeMap<String, u32> = BTreeMap::new();
        for s in self.book.outbox.iter().filter(|s| day_of(&s.at) == day) {
            *kinds.entry(format!("{} ({})", s.kind, s.channel)).or_default() += 1;
        }
        let cut = add_days(&day, -self.profile.retain_customer_days);
        let stale = self.book.customers.values().filter(|c| c.last_day < cut).count();
        format!(
            "Customers on this phone: {} (sealed with the vault key), {} erased so far, {} past the {}-day limit{}\n\
             Riders tracked right now: {}\n\
             Left the phone today: {}\n\
             Never leaves: the order book, customers, purchases, the plan. Platforms and the tax return get totals only.",
            self.book.customers.len(),
            self.book.erased,
            stale,
            self.profile.retain_customer_days,
            if stale > 0 { " — say: forget customers older than N days" } else { "" },
            self.book.rider_fix.len(),
            if kinds.is_empty() { "nothing".into() } else { kinds.iter().map(|(k, n)| format!("{n} × {k}")).collect::<Vec<_>>().join(", ") }
        )
    }

    /// Everything the dashboard shows, as data. No names, numbers or addresses.
    pub fn state(&self) -> Value {
        let day = self.today();
        let s = self.day_stats(&day);
        let hist: Vec<Value> = (0..14)
            .rev()
            .map(|i| {
                let d = add_days(&day, -i);
                let x = self.day_stats(&d);
                json!({"day": d, "orders": x.orders(), "sales": x.sales(), "net": x.net(),
                    "channels": x.channels.iter().map(|(k, c)| (k.clone(), json!(c.sales))).collect::<serde_json::Map<_, _>>()})
            })
            .collect();
        let g = self.gst(&day[..7.min(day.len())]);
        let plan = crate::plan::plan(self);
        let mut sent_by_kind: BTreeMap<String, u32> = BTreeMap::new();
        for x in self.book.outbox.iter().filter(|x| day_of(&x.at) == day) {
            *sent_by_kind.entry(x.kind.clone()).or_default() += 1;
        }
        json!({
            "business": self.profile.name, "city": self.profile.city, "day": day, "now": self.now(),
            "weekday": WEEKDAYS[weekday(&day)],
            "totals": {"orders": s.orders(), "delivered": s.delivered(), "cancelled": s.cancelled(), "open": s.open.len(),
                "sales": s.sales(), "net": s.net(), "gst_direct": s.gst_direct, "cod": s.cod},
            "channels": s.channels.iter().map(|(k, c)| json!({"name": k, "orders": c.orders, "delivered": c.delivered,
                "cancelled": c.cancelled, "sales": c.sales, "net": c.net})).collect::<Vec<_>>(),
            "hours": s.hours.to_vec(),
            "items": s.items.iter().map(|(c, q)| json!({"code": c, "name": self.profile.item(c).map(|m| m.name.clone()).unwrap_or_default(), "qty": q})).collect::<Vec<_>>(),
            "open": s.open,
            "riders": self.rider_sheet(&day).iter().map(|r| json!({"id": r.id, "name": r.name, "deliveries": r.deliveries,
                "km": r.km, "earnings": r.earnings, "paid": r.paid, "due": r.due, "cash_held": r.cash_held})).collect::<Vec<_>>(),
            "platforms": self.profile.platforms.iter().map(|pf| { let n = pf.name.to_lowercase(); let (can, why) = self.claimable(&n);
                json!({"name": pf.name, "awaiting": self.awaiting(&n), "claimable": can, "why": why,
                    "claimed": self.book.claims.iter().filter(|c| c.platform == n).map(|c| c.amount).sum::<Paise>(),
                    "payouts": self.reconcile().iter().filter(|r| r.platform == n).map(|r| json!({"from": r.from, "to": r.to,
                        "expected": r.expected, "received": r.received})).collect::<Vec<_>>()}) }).collect::<Vec<_>>(),
            "gst": {"period": g.period, "direct_taxable": g.direct_taxable, "output": g.direct_cgst + g.direct_sgst,
                "eco_taxable": g.eco.iter().map(|e| e.1).sum::<Paise>(), "input_purchases": g.input_purchases,
                "input_commission": g.input_commission, "itc_claimable": g.itc_claimable, "itc_blocked": g.itc_blocked,
                "payable": g.payable, "scheme": self.profile.gst_scheme, "filed": self.book.gst_filed},
            "history": hist,
            "plan": plan.json(),
            "sent_by_kind": sent_by_kind,
            "privacy": {"customers": self.book.customers.len(), "erased": self.book.erased, "riders_tracked": self.book.rider_fix.len(),
                "sent_today": self.book.outbox.iter().filter(|x| day_of(&x.at) == day).count(),
                "outbox": self.book.outbox.iter().rev().take(12).collect::<Vec<_>>()},
            "amounts_in": "paise"
        })
    }
}
