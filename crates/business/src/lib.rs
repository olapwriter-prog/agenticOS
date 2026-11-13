//! A small business run from the phone. Orders arrive from every channel (WhatsApp Business
//! directly, food platforms through their partner feeds) into one sealed book. From that book,
//! code (not a model) makes the bill, the GST figures, the riders' settlement, the platforms'
//! reconciliation, the dashboard and tomorrow's plan.
//!
//! Privacy rules, enforced here rather than promised:
//! - the book is sealed with a key from the vault (one small file, plus one file per day of
//!   orders, so an event rewrites only its own day); nothing about a customer is on disk in
//!   the clear;
//! - a customer's name, phone and address appear in exactly two places: their own bill and the
//!   message to the rider who delivers to them. Every tool answer shows a mask and a locality;
//! - a bill or a location can only go to the customer of that order, on the channel they used;
//! - a rider's position is kept only while they carry an order, and only the last fix;
//! - customers are erased after `retain_customer_days`; the orders stay (the books need them)
//!   under a keyed token that does not reveal the number.
pub mod dataset;
pub mod pdf;
pub mod plan;
pub mod profile;
pub mod report;
pub mod tools;
pub mod util;
pub mod wa;

use profile::Profile;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use util::*;
use wa::{Msg, Whatsapp};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Line {
    pub code: String,
    pub qty: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub channel: String,
    pub at: String,
    pub lines: Vec<Line>,
    /// A keyed token; the person is in `Book::customers` until erased.
    pub customer: Option<String>,
    pub locality: String,
    pub km: f64,
    /// "upi", "cod" or "platform".
    pub payment: String,
    /// "new", "preparing", "out", "delivered", "cancelled".
    pub status: String,
    pub cancelled_by: Option<String>,
    pub prepared: bool,
    pub rider: Option<String>,
    pub invoice_no: Option<String>,
    /// A discount the business pays for, in paise.
    pub discount: Paise,
    pub bill_sent: bool,
    pub tracked_at: Option<String>,
    pub done_at: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Customer {
    pub name: String,
    pub phone: String,
    pub address: String,
    pub last_day: String,
    pub orders: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Purchase {
    pub day: String,
    pub supplier: String,
    pub item: String,
    pub taxable: Paise,
    pub gst: Paise,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payout {
    pub platform: String,
    pub from: String,
    pub to: String,
    pub amount: Paise,
    pub reference: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiderMoney {
    pub day: String,
    pub rider: String,
    pub amount: Paise,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub day: String,
    pub platform: String,
    pub amount: Paise,
    pub reason: String,
    pub reference: String,
}
/// One thing that left the phone. Never holds a name, a number or an address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sent {
    pub at: String,
    pub channel: String,
    pub to: String,
    pub kind: String,
    pub about: String,
    pub bytes: usize,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Book {
    pub now: String,
    pub orders: Vec<Order>,
    pub customers: BTreeMap<String, Customer>,
    pub erased: u32,
    pub purchases: Vec<Purchase>,
    pub payouts: Vec<Payout>,
    pub rider_paid: Vec<RiderMoney>,
    pub rider_cash: Vec<RiderMoney>,
    pub claims: Vec<Claim>,
    pub gst_filed: Vec<(String, String)>,
    pub invoice_seq: u32,
    /// rider id -> (lat, lon, at). Present only while the rider carries an order.
    pub rider_fix: BTreeMap<String, (f64, f64, String)>,
    pub outbox: Vec<Sent>,
}

/// An order as a channel hands it over.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct OrderIn {
    pub id: String,
    pub channel: String,
    pub at: String,
    pub items: Vec<ItemIn>,
    pub customer: Option<CustomerIn>,
    pub locality: String,
    pub km: f64,
    pub payment: String,
    pub status: String,
    pub cancelled_by: Option<String>,
    pub prepared: bool,
    pub rider: Option<String>,
    pub discount: f64,
}
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct ItemIn {
    pub code: String,
    pub name: String,
    pub qty: u32,
}
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct CustomerIn {
    pub name: String,
    pub phone: String,
    pub address: String,
}

/// The money in one order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Values {
    pub items: Paise,
    pub discount: Paise,
    pub delivery: Paise,
    pub taxable: Paise,
    pub cgst: Paise,
    pub sgst: Paise,
    pub round_off: Paise,
    pub total: Paise,
    pub commission: Paise,
    pub commission_gst: Paise,
    pub tds: Paise,
    /// What the platform should pay for this order (0 for a direct order).
    pub payout: Paise,
}

pub struct Business {
    pub profile: Profile,
    pub book: Book,
    key: [u8; 32],
    dir: PathBuf,
    pub wa: Box<dyn Whatsapp>,
    /// False while importing past books: nothing is sent and the file is written once at the end.
    pub live: bool,
    /// Goes up whenever an answer could change (the book was written, or the day turned).
    /// An answer prepared at one revision is still right while the revision is the same.
    pub rev: std::cell::Cell<u64>,
    /// Days whose orders or sent-log changed since the last write.
    dirty: std::collections::BTreeSet<String>,
    /// The reconciliation at a revision: several answers need it, and it reads every order.
    pub(crate) recon: std::cell::RefCell<Option<(u64, Vec<report::Recon>)>>,
}

/// One day's orders and what was sent that day: its own sealed file.
#[derive(Serialize, Deserialize, Default)]
struct DayFile {
    orders: Vec<Order>,
    outbox: Vec<Sent>,
}

pub const STATUSES: [&str; 5] = ["new", "preparing", "out", "delivered", "cancelled"];

impl Business {
    pub fn open(
        profile: Profile,
        dir: PathBuf,
        key: &[u8],
        wa: Box<dyn Whatsapp>,
    ) -> Result<Business, String> {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let key: [u8; 32] = key
            .try_into()
            .map_err(|_| "the business key must be 32 bytes".to_string())?;
        let path = dir.join("book.sealed");
        let mut book: Book = if path.exists() {
            let plain = unseal(&key, &std::fs::read(&path).map_err(|e| e.to_string())?)?;
            serde_json::from_slice(&plain).map_err(|e| format!("book: {e}"))?
        } else {
            Book::default()
        };
        // a book written before days had their own files still holds its orders: move them out
        let mut dirty: std::collections::BTreeSet<String> =
            book.orders.iter().map(|o| day_of(&o.at).to_string()).chain(book.outbox.iter().map(|s| day_of(&s.at).to_string())).collect();
        let mut days: Vec<PathBuf> = std::fs::read_dir(dir.join("days"))
            .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "sealed")).collect())
            .unwrap_or_default();
        days.sort();
        for f in days {
            let plain = unseal(&key, &std::fs::read(&f).map_err(|e| e.to_string())?)?;
            let d: DayFile = serde_json::from_slice(&plain).map_err(|e| format!("{}: {e}", f.display()))?;
            book.orders.extend(d.orders);
            book.outbox.extend(d.outbox);
        }
        if !dirty.is_empty() {
            dirty.extend(book.orders.iter().map(|o| day_of(&o.at).to_string()));
        }
        Ok(Business { profile, book, key, dir, wa, live: true, rev: std::cell::Cell::new(1), dirty, recon: Default::default() })
    }
    /// Write what changed: the days touched since the last write, then the small book file
    /// (customers, money records, counters). The days are written first, so a crash between
    /// the two leaves an order on disk rather than a counter ahead of its order.
    pub fn save(&mut self) -> Result<(), String> {
        self.rev.set(self.rev.get() + 1);
        if !self.live {
            return Ok(());
        }
        let write = |path: PathBuf, plain: Vec<u8>, key: &[u8; 32]| -> Result<(), String> {
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, seal(key, &plain)?).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
        };
        if !self.dirty.is_empty() {
            std::fs::create_dir_all(self.dir.join("days")).map_err(|e| e.to_string())?;
        }
        for day in std::mem::take(&mut self.dirty) {
            let d = DayFile {
                orders: self.book.orders.iter().filter(|o| day_of(&o.at) == day).cloned().collect(),
                outbox: self.book.outbox.iter().filter(|s| day_of(&s.at) == day).cloned().collect(),
            };
            write(self.dir.join("days").join(format!("{day}.sealed")), serde_json::to_vec(&d).map_err(|e| e.to_string())?, &self.key)?;
        }
        let (orders, outbox) = (std::mem::take(&mut self.book.orders), std::mem::take(&mut self.book.outbox));
        let plain = serde_json::to_vec(&self.book).map_err(|e| e.to_string());
        self.book.orders = orders;
        self.book.outbox = outbox;
        write(self.dir.join("book.sealed"), plain?, &self.key)
    }
    /// Past books are imported quietly; `live(true)` writes the file and turns sending back on.
    pub fn set_live(&mut self, live: bool) -> Result<(), String> {
        self.live = live;
        self.save()
    }

    // ---- time: the business's own clock, moved by events (and by the phone's clock) ----
    pub fn tick(&mut self, at: &str) {
        if at.len() >= 16 && at > self.book.now.as_str() {
            let new_day = day_of(at) != day_of(&self.book.now);
            self.book.now = at[..16].to_string();
            if new_day {
                self.rev.set(self.rev.get() + 1);
                // the retention limit is the owner's standing rule: it is kept without being asked
                if self.profile.auto_erase {
                    let _ = self.forget(self.profile.retain_customer_days);
                }
            }
        }
    }
    pub fn now(&self) -> &str {
        &self.book.now
    }
    pub fn today(&self) -> String {
        day_of(&self.book.now).to_string()
    }

    // ---- reading the book ----
    pub fn is_direct(&self, o: &Order) -> bool {
        self.profile.platform(&o.channel).is_none()
    }
    pub fn order(&self, id: &str) -> Result<&Order, String> {
        self.book
            .orders
            .iter()
            .rev()
            .find(|o| o.id.eq_ignore_ascii_case(id.trim()))
            .ok_or_else(|| format!("no order {id}"))
    }
    fn order_index(&self, id: &str) -> Result<usize, String> {
        self.book
            .orders
            .iter()
            .rposition(|o| o.id.eq_ignore_ascii_case(id.trim()))
            .ok_or_else(|| format!("no order {id}"))
    }
    pub fn values(&self, o: &Order) -> Values {
        let p = &self.profile;
        let items: Paise = o.lines.iter().map(|l| p.price(&l.code) * l.qty as Paise).sum();
        let mut v = Values { items, discount: o.discount.min(items), ..Values::default() };
        let platform = p.platform(&o.channel);
        if platform.is_none() {
            v.delivery = rs(p.delivery_fee);
        }
        v.taxable = v.items - v.discount + v.delivery;
        v.cgst = pct(v.taxable, p.food_gst_pct / 2.0);
        v.sgst = v.cgst;
        let raw = v.taxable + v.cgst + v.sgst;
        v.total = (raw + 50).div_euclid(100) * 100;
        v.round_off = v.total - raw;
        if let Some(pf) = platform {
            v.commission = pct(v.taxable, pf.commission_pct);
            v.commission_gst = pct(v.commission, pf.commission_gst_pct);
            v.tds = pct(v.taxable, pf.tds_pct);
            v.payout = v.taxable - v.commission - v.commission_gst - v.tds;
        }
        v
    }
    /// A platform owes for a delivered order, and for one it or its customer cancelled after
    /// the food was made.
    pub fn platform_owes(&self, o: &Order) -> bool {
        !self.is_direct(o)
            && (o.status == "delivered"
                || (o.status == "cancelled"
                    && o.prepared
                    && o.cancelled_by.as_deref() != Some("restaurant")))
    }
    /// How a customer is shown anywhere except their own bill.
    pub fn who(&self, o: &Order) -> String {
        match o.customer.as_ref().and_then(|t| self.book.customers.get(t)) {
            Some(c) => format!("{} {}", mask_name(&c.name), mask_phone(&c.phone)),
            None if self.is_direct(o) => "(erased)".into(),
            None => format!("{} customer", o.channel),
        }
    }
    pub(crate) fn record(&mut self, channel: &str, to: &str, kind: &str, about: &str, bytes: usize) {
        let at = self.book.now.clone();
        self.dirty.insert(day_of(&at).to_string());
        self.book.outbox.push(Sent {
            at,
            channel: channel.into(),
            to: to.into(),
            kind: kind.into(),
            about: about.into(),
            bytes,
        });
    }

    // ---- orders in ----
    pub fn ingest(&mut self, o: OrderIn) -> Result<String, String> {
        if o.id.trim().is_empty() || o.channel.trim().is_empty() {
            return Err("an order needs an id and a channel".into());
        }
        if self.book.orders.iter().any(|x| x.id.eq_ignore_ascii_case(&o.id)) {
            return Err(format!("order {} is already in the book", o.id));
        }
        let mut lines: Vec<Line> = Vec::new();
        for it in &o.items {
            let code = match self.profile.item(&it.code) {
                Some(m) => m.code.clone(),
                None => wa::parse_order(&self.profile, &it.name)
                    .ok()
                    .and_then(|v| v.first().map(|(c, _)| c.clone()))
                    .ok_or_else(|| format!("{}: not on the menu: {} {}", o.id, it.code, it.name))?,
            };
            lines.push(Line { code, qty: it.qty.max(1) });
        }
        if lines.is_empty() {
            return Err(format!("{}: no items", o.id));
        }
        let channel = o.channel.to_lowercase();
        let direct = self.profile.platform(&channel).is_none();
        let at = if o.at.len() >= 16 { o.at[..16].to_string() } else { self.book.now.clone() };
        self.tick(&at);
        let status = if o.status.is_empty() { "new".to_string() } else { o.status.to_lowercase() };
        if !STATUSES.contains(&status.as_str()) {
            return Err(format!("{}: unknown status {status}", o.id));
        }
        let mut customer = None;
        if direct {
            let c = o.customer.clone().unwrap_or_default();
            if digits(&c.phone).len() < 10 {
                return Err(format!("{}: a direct order needs the customer's number", o.id));
            }
            if o.km > self.profile.max_delivery_km {
                return Err(format!(
                    "{}: {} is {:.1} km away; we deliver up to {:.0} km",
                    o.id, o.locality, o.km, self.profile.max_delivery_km
                ));
            }
            let t = token(&self.key, &c.phone);
            let e = self.book.customers.entry(t.clone()).or_insert(Customer {
                name: c.name.clone(),
                phone: c.phone.clone(),
                address: c.address.clone(),
                last_day: String::new(),
                orders: 0,
            });
            if !c.address.is_empty() {
                e.address = c.address.clone();
            }
            e.last_day = day_of(&at).to_string();
            e.orders += 1;
            customer = Some(t);
        }
        // a tax invoice has a consecutive number, so it is given when the order is taken
        let invoice_no = direct.then(|| {
            self.book.invoice_seq += 1;
            format!("{}{:05}", self.profile.invoice_prefix, self.book.invoice_seq)
        });
        let done = matches!(status.as_str(), "delivered" | "cancelled");
        let order = Order {
            id: o.id.clone(),
            channel,
            at: at.clone(),
            lines,
            customer,
            locality: o.locality,
            km: o.km,
            payment: if o.payment.is_empty() {
                if direct { "upi".into() } else { "platform".into() }
            } else {
                o.payment.to_lowercase()
            },
            status,
            cancelled_by: o.cancelled_by,
            prepared: o.prepared,
            rider: o.rider,
            invoice_no,
            discount: rs(o.discount),
            bill_sent: false,
            tracked_at: None,
            done_at: done.then(|| at.clone()),
        };
        let v = self.values(&order);
        let said = format!(
            "{} {} {} ({} items) {} in {}",
            order.id,
            order.channel,
            inr(v.total),
            order.lines.iter().map(|l| l.qty).sum::<u32>(),
            self.who(&order),
            order.locality
        );
        self.dirty.insert(day_of(&order.at).to_string());
        self.book.orders.push(order);
        self.save()?;
        Ok(said)
    }

    /// A customer typed an order on WhatsApp. `Err` says what to ask them.
    pub fn ingest_text(&mut self, c: CustomerIn, locality: &str, km: f64, payment: &str, text: &str) -> Result<String, String> {
        let lines = wa::parse_order(&self.profile, text)?;
        let n = self.book.orders.iter().filter(|o| o.id.starts_with("W-")).count();
        self.ingest(OrderIn {
            id: format!("W-{}", 1001 + n),
            channel: "whatsapp".into(),
            at: self.book.now.clone(),
            items: lines.into_iter().map(|(code, qty)| ItemIn { code, name: String::new(), qty }).collect(),
            customer: Some(c),
            locality: locality.into(),
            km,
            payment: payment.into(),
            ..OrderIn::default()
        })
    }

    // ---- the bill ----
    pub fn bill_lines(&self, o: &Order) -> Vec<String> {
        let p = &self.profile;
        let v = self.values(o);
        let rule = "-".repeat(58);
        let mut out = vec![
            format!("{:^58}", "TAX INVOICE"),
            format!("{:^58}", p.name),
            format!("{:^58}", p.address),
            format!("{:^58}", format!("GSTIN {}   FSSAI {}", p.gstin, p.fssai)),
            rule.clone(),
            format!("Invoice: {:<26} Date: {}", o.invoice_no.clone().unwrap_or_default(), o.at.replace('T', " ")),
            format!("Order:   {:<26} Place of supply: {} ({})", o.id, p.state, p.state_code),
        ];
        match o.customer.as_ref().and_then(|t| self.book.customers.get(t)) {
            Some(c) => {
                out.push(format!("To:      {}, {}", c.name, c.phone));
                out.push(format!("         {}", c.address));
            }
            None => out.push("To:      (customer details erased)".into()),
        }
        out.push(rule.clone());
        out.push(format!("{:<30} {:>4} {:>10} {:>11}", "Item", "Qty", "Rate", "Amount"));
        for l in &o.lines {
            let (name, rate) = (p.item(&l.code).map(|m| m.name.as_str()).unwrap_or(&l.code), p.price(&l.code));
            out.push(format!("{:<30.30} {:>4} {:>10} {:>11}", name, l.qty, inr(rate), inr(rate * l.qty as Paise)));
        }
        out.push(rule.clone());
        let mut row = |k: &str, a: Paise| out.push(format!("{:<46} {:>11}", k, inr(a)));
        row("Items", v.items);
        if v.discount > 0 {
            row("Discount", -v.discount);
        }
        if v.delivery > 0 {
            row("Delivery", v.delivery);
        }
        row(&format!("Taxable value (SAC {})", p.sac), v.taxable);
        row(&format!("CGST {}%", p.food_gst_pct / 2.0), v.cgst);
        row(&format!("SGST {}%", p.food_gst_pct / 2.0), v.sgst);
        if v.round_off != 0 {
            row("Round off", v.round_off);
        }
        row("TOTAL", v.total);
        out.push(rule);
        out.push(format!("Paid by: {}", if o.payment == "cod" { "cash on delivery" } else { "UPI" }));
        out.push("Thank you. This is a computer-generated invoice.".into());
        out
    }

    /// Send the bill as a PDF to the customer who placed this order. No other recipient is possible.
    pub fn send_bill(&mut self, id: &str) -> Result<String, String> {
        let i = self.order_index(id)?;
        let o = self.book.orders[i].clone();
        if !self.is_direct(&o) {
            return Err(format!("{} issues the customer's bill for its own orders", o.channel));
        }
        let c = o
            .customer
            .as_ref()
            .and_then(|t| self.book.customers.get(t))
            .cloned()
            .ok_or("this customer's details were erased; there is nobody to send it to")?;
        let v = self.values(&o);
        let inv = o.invoice_no.clone().unwrap_or_default();
        let bytes = pdf::text_pdf(&self.bill_lines(&o));
        let n = bytes.len();
        self.wa.send(
            &c.phone,
            Msg::Document {
                name: format!("{}.pdf", inv.replace('/', "-")),
                bytes,
                caption: format!("{}: your bill for order {} is {}. Thank you!", self.profile.name, o.id, inr(v.total)),
            },
        )?;
        self.book.orders[i].bill_sent = true;
        self.dirty.insert(day_of(&o.at).to_string());
        self.record("whatsapp", &self.who(&o), "bill", &format!("{} {}", o.id, inv), n);
        self.save()?;
        Ok(format!("bill {inv} ({}) sent to {} on WhatsApp", inr(v.total), self.who(&o)))
    }

    // ---- kitchen and delivery ----
    pub fn assign(&mut self, id: &str, rider: &str) -> Result<String, String> {
        let i = self.order_index(id)?;
        let o = self.book.orders[i].clone();
        if !self.is_direct(&o) {
            return Err(format!("{} sends its own rider", o.channel));
        }
        if matches!(o.status.as_str(), "delivered" | "cancelled") {
            return Err(format!("{} is already {}", o.id, o.status));
        }
        let r = self.profile.rider(rider).cloned().ok_or_else(|| format!("no rider {rider}"))?;
        self.book.orders[i].rider = Some(r.id.clone());
        self.dirty.insert(day_of(&o.at).to_string());
        if self.live {
            // the rider is the one other person who needs the address
            if let Some(c) = o.customer.as_ref().and_then(|t| self.book.customers.get(t)).cloned() {
                let v = self.values(&o);
                let collect = if o.payment == "cod" { format!("collect {}", inr(v.total)) } else { "already paid".into() };
                let text = format!("Order {}: {} — {}, {}. {}.", o.id, c.name, c.address, c.phone, collect);
                let n = text.len();
                self.wa.send(&r.phone, Msg::Text(text))?;
                self.record("whatsapp", &format!("rider {}", r.id), "delivery job", &o.id, n);
            }
        }
        self.save()?;
        Ok(format!("{} goes with {} ({})", o.id, r.name, r.id))
    }

    pub fn set_status(&mut self, id: &str, status: &str, by: Option<&str>) -> Result<String, String> {
        let status = match status.trim().to_lowercase().as_str() {
            "ready" | "prepared" | "preparing" | "accepted" => "preparing",
            "out" | "out for delivery" | "picked" | "picked up" | "dispatched" => "out",
            "delivered" | "done" => "delivered",
            "cancelled" | "canceled" | "cancel" => "cancelled",
            other => return Err(format!("unknown status {other}: use preparing, out, delivered or cancelled")),
        };
        let i = self.order_index(id)?;
        let direct = self.is_direct(&self.book.orders[i]);
        let now = self.book.now.clone();
        let o = &mut self.book.orders[i];
        if matches!(o.status.as_str(), "delivered" | "cancelled") {
            return Err(format!("{} is already {}", o.id, o.status));
        }
        self.dirty.insert(day_of(&o.at).to_string());
        if status == "out" && direct && o.rider.is_none() {
            return Err(format!("assign a rider to {} first", o.id));
        }
        if status == "cancelled" {
            o.prepared = o.prepared || matches!(o.status.as_str(), "preparing" | "out");
            o.cancelled_by = Some(by.unwrap_or("restaurant").to_string());
        }
        if matches!(status, "preparing" | "out") {
            o.prepared = true;
        }
        o.status = status.to_string();
        let (oid, rider) = (o.id.clone(), o.rider.clone());
        if matches!(status, "delivered" | "cancelled") {
            o.done_at = Some(now);
            // a rider with nothing left to carry is no longer tracked
            if let Some(r) = &rider {
                if !self.book.orders.iter().any(|x| x.status == "out" && x.rider.as_ref() == Some(r)) {
                    self.book.rider_fix.remove(r);
                }
            }
        }
        let mut said = format!("{oid} is {status}");
        if status == "out" && direct && self.live && self.profile.auto_tracking {
            if let Ok(t) = self.share_location(&oid) {
                said.push_str(&format!("; {t}"));
            }
        }
        self.save()?;
        Ok(said)
    }

    /// Send the rider's position to the customer of an order that is on its way.
    pub fn share_location(&mut self, id: &str) -> Result<String, String> {
        let i = self.order_index(id)?;
        let o = self.book.orders[i].clone();
        if !self.is_direct(&o) {
            return Err(format!("{} shows its own rider to its customer", o.channel));
        }
        if o.status != "out" {
            return Err(format!("{} is {}, not out for delivery", o.id, o.status));
        }
        let rid = o.rider.clone().ok_or("no rider on this order")?;
        let rider = self.profile.rider(&rid).cloned().ok_or("unknown rider")?;
        let c = o
            .customer
            .as_ref()
            .and_then(|t| self.book.customers.get(t))
            .cloned()
            .ok_or("this customer's details were erased")?;
        let (lat, lon) = self
            .book
            .rider_fix
            .get(&rid)
            .map(|f| (f.0, f.1))
            .unwrap_or((self.profile.lat, self.profile.lon));
        self.wa.send(
            &c.phone,
            Msg::Location { lat, lon, label: format!("{} is on the way with order {}", rider.name, o.id) },
        )?;
        self.book.orders[i].tracked_at = Some(self.book.now.clone());
        self.dirty.insert(day_of(&o.at).to_string());
        self.record("whatsapp", &self.who(&o), "rider location", &o.id, 16);
        self.save()?;
        Ok(format!("{}'s location sent to {}", rider.name, self.who(&o)))
    }

    /// A position from a rider's phone. Kept only while they carry an order; passed on to the
    /// customers of those orders, at most once every three minutes each.
    pub fn rider_fix(&mut self, rider: &str, lat: f64, lon: f64, at: &str) -> Result<String, String> {
        let r = self.profile.rider(rider).cloned().ok_or_else(|| format!("no rider {rider}"))?;
        self.tick(at);
        let carrying: Vec<(String, Option<String>)> = self
            .book
            .orders
            .iter()
            .filter(|o| o.status == "out" && o.rider.as_deref() == Some(r.id.as_str()))
            .map(|o| (o.id.clone(), o.tracked_at.clone()))
            .collect();
        if carrying.is_empty() {
            self.book.rider_fix.remove(&r.id);
            return Ok(format!("{} is not carrying an order; position not kept", r.name));
        }
        let now = self.book.now.clone();
        self.book.rider_fix.insert(r.id.clone(), (lat, lon, now.clone()));
        let mut sent = 0;
        if self.live && self.profile.auto_tracking {
            for (id, last) in carrying {
                if last.map(|l| minutes(&now) - minutes(&l) >= 3).unwrap_or(true) && self.share_location(&id).is_ok() {
                    sent += 1;
                }
            }
        }
        self.save()?;
        Ok(format!("{} at {lat:.4},{lon:.4}; {sent} customer(s) updated", r.name))
    }

    // ---- money in and out ----
    pub fn purchase(&mut self, supplier: &str, item: &str, taxable: Paise, gst: Paise) -> Result<String, String> {
        if taxable <= 0 || gst < 0 {
            return Err("a purchase needs an amount".into());
        }
        let day = self.today();
        self.book.purchases.push(Purchase { day, supplier: supplier.into(), item: item.into(), taxable, gst });
        self.save()?;
        Ok(format!("purchase recorded: {item} from {supplier}, {} + GST {}", inr(taxable), inr(gst)))
    }
    pub fn payout_in(&mut self, p: Payout) -> Result<String, String> {
        if self.profile.platform(&p.platform).is_none() {
            return Err(format!("no platform {}", p.platform));
        }
        let said = format!("{} paid {} for {} to {}", p.platform, inr(p.amount), p.from, p.to);
        self.book.payouts.push(Payout { platform: p.platform.to_lowercase(), ..p });
        self.save()?;
        Ok(said)
    }
    pub fn cash_in(&mut self, rider: &str, amount: Paise) -> Result<String, String> {
        let r = self.profile.rider(rider).cloned().ok_or_else(|| format!("no rider {rider}"))?;
        let day = self.today();
        let owed = self.rider_sheet(&day).into_iter().find(|s| s.id == r.id).map(|s| s.cash_held).unwrap_or(0);
        if amount <= 0 || amount > owed {
            return Err(format!("{} holds {} of cash-on-delivery money today", r.name, inr(owed)));
        }
        self.book.rider_cash.push(RiderMoney { day, rider: r.id.clone(), amount });
        self.save()?;
        Ok(format!("{} handed over {}; still holds {}", r.name, inr(amount), inr(owed - amount)))
    }
    /// Record that the rider's earnings for today were handed over, and send them a payslip.
    /// The amount must be exactly what the sheet says, so a slip of the tongue cannot overpay.
    pub fn pay_rider(&mut self, rider: &str, amount: Paise) -> Result<String, String> {
        let r = self.profile.rider(rider).cloned().ok_or_else(|| format!("no rider {rider}"))?;
        let day = self.today();
        let s = self.rider_sheet(&day).into_iter().find(|s| s.id == r.id).ok_or("no deliveries today")?;
        if s.due <= 0 {
            return Err(format!("{} has nothing due today", r.name));
        }
        if amount != s.due {
            return Err(format!("{} is due {} today, not {}", r.name, inr(s.due), inr(amount)));
        }
        self.book.rider_paid.push(RiderMoney { day: day.clone(), rider: r.id.clone(), amount });
        if self.live {
            let text = format!(
                "{}: {} — {} deliveries, {:.1} km. Earnings {} handed over. Thank you, {}.",
                self.profile.name, day, s.deliveries, s.km, inr(amount), r.name
            );
            let n = text.len();
            self.wa.send(&r.phone, Msg::Text(text))?;
            self.record("whatsapp", &format!("rider {}", r.id), "payslip", &day, n);
        }
        self.save()?;
        Ok(format!("{} recorded as handed to {} for {} deliveries; payslip sent", inr(amount), r.name, s.deliveries))
    }
    /// File a claim with a platform for what its payouts fell short by.
    pub fn claim(&mut self, platform: &str, amount: Paise) -> Result<String, String> {
        let name = self.profile.platform(platform).map(|p| p.name.to_lowercase()).ok_or_else(|| format!("no platform {platform}"))?;
        let (can, reason) = self.claimable(&name);
        if can <= 0 {
            return Err(format!("nothing to claim from {name}"));
        }
        if amount != can {
            return Err(format!("{name} is short by {}, not {}", inr(can), inr(amount)));
        }
        let reference = format!("CLM-{}-{}", name.to_uppercase(), self.book.claims.len() + 1);
        let day = self.today();
        self.record(&format!("{name} partner support"), &name, "claim", &format!("{reference} {}", inr(amount)), reason.len());
        self.book.claims.push(Claim { day, platform: name.clone(), amount, reason: reason.clone(), reference: reference.clone() });
        self.save()?;
        Ok(format!("claim {reference} for {} filed with {name}: {reason}", inr(amount)))
    }
    /// Erase customers who have not ordered for `days` days. Their orders stay, without them.
    pub fn forget(&mut self, days: i64) -> Result<String, String> {
        if days < 0 {
            return Err("days cannot be negative".into());
        }
        let cut = add_days(&self.today(), -days);
        let before = self.book.customers.len();
        self.book.customers.retain(|_, c| c.last_day >= cut);
        let n = before - self.book.customers.len();
        self.book.erased += n as u32;
        self.save()?;
        Ok(format!("erased {n} customers who last ordered before {cut}; {} kept", self.book.customers.len()))
    }
    /// The bill as the customer got it (operator only: this is not a tool).
    pub fn bill_pdf(&self, id: &str) -> Result<Vec<u8>, String> {
        let o = self.order(id)?;
        if !self.is_direct(o) {
            return Err(format!("{} issues the customer's bill for its own orders", o.channel));
        }
        Ok(pdf::text_pdf(&self.bill_lines(o)))
    }
}
