//! The business's books: customers, a catalog, GST invoices, payments received, bills paid, and
//! the GST returns that follow from them.
//!
//! GST as India's law has it, in paise throughout:
//!   - a sale inside the seller's state carries CGST + SGST (half the rate each); to another
//!     state, IGST at the full rate. The place of supply is the buyer's state (from its GSTIN, or
//!     the seller's own state for a consumer without one).
//!   - invoices are numbered in one series per financial year (April to March).
//!   - GSTR-1 lists the month's sales: B2B invoice by invoice, B2C summed by state and rate, and
//!     an HSN summary. GSTR-3B is the month's tax: output tax, input tax credit from purchases,
//!     and what remains to pay in cash after set-off (IGST credit first: to IGST, then CGST, then
//!     SGST; CGST credit to CGST, then IGST; SGST credit to SGST, then IGST).
use crate::ui::{Card, Ui};
use crate::{inr, month_name, parse_inr, parse_period, wallet, Date, Partner};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Customer {
    pub name: String,
    pub gstin: Option<String>,
    /// WhatsApp id: digits with the country code
    pub phone: String,
    #[serde(default)]
    pub email: String,
    /// two-digit state code (place of supply)
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub name: String,
    /// paise, before GST
    pub price: i64,
    /// basis points: 1800 = 18%
    pub rate: u32,
    #[serde(default)]
    pub hsn: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Line {
    pub item: String,
    pub hsn: String,
    pub qty: u32,
    pub unit: i64,
    pub rate: u32,
    pub taxable: i64,
    pub cgst: i64,
    pub sgst: i64,
    pub igst: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invoice {
    pub number: String,
    pub date: Date,
    pub customer: String,
    pub gstin: Option<String>,
    /// place of supply: state code
    pub pos: String,
    pub lines: Vec<Line>,
    pub taxable: i64,
    pub cgst: i64,
    pub sgst: i64,
    pub igst: i64,
    pub round_off: i64,
    pub total: i64,
    /// issued, sent, paid
    pub status: String,
    #[serde(default)]
    pub paid_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Expense {
    /// the bill's id (the email's), so it is paid once
    pub id: String,
    pub supplier: String,
    pub gstin: Option<String>,
    pub date: Date,
    pub amount: i64,
    pub cgst: i64,
    pub sgst: i64,
    pub igst: i64,
    pub paid_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Filing {
    pub form: String,
    /// MMYYYY, as the GST portal writes it
    pub period: String,
    pub arn: String,
    pub filed_on: Date,
}

// ---- GSTIN -------------------------------------------------------------------------------------

const GST_CHARS: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// The 15th character of a GSTIN from the first 14 (the GSTN's mod-36 check).
pub fn gstin_check(first14: &str) -> Option<char> {
    let mut sum = 0u32;
    for (i, c) in first14.chars().enumerate() {
        let v = GST_CHARS.find(c)? as u32;
        let p = v * if i % 2 == 0 { 1 } else { 2 };
        sum += p / 36 + p % 36;
    }
    GST_CHARS.chars().nth(((36 - sum % 36) % 36) as usize)
}

/// A valid GSTIN: state code 01–38, a PAN (5 letters, 4 digits, a letter), an entity digit,
/// 'Z', and the check character.
pub fn valid_gstin(g: &str) -> Result<String, String> {
    let g = g.trim().to_uppercase();
    let b = g.as_bytes();
    let ok_shape = g.len() == 15
        && b[0..2].iter().all(u8::is_ascii_digit)
        && b[2..7].iter().all(u8::is_ascii_uppercase)
        && b[7..11].iter().all(u8::is_ascii_digit)
        && b[11].is_ascii_uppercase()
        && b[12].is_ascii_alphanumeric()
        && b[13] == b'Z';
    if !ok_shape {
        return Err(format!("{g} is not a GSTIN (15 characters: state, PAN, entity, Z, check)"));
    }
    let state: u32 = g[..2].parse().unwrap_or(0);
    if !(1..=38).contains(&state) && state != 97 {
        return Err(format!("{g}: no state has code {:02}", state));
    }
    if gstin_check(&g[..14]) != g.chars().nth(14) {
        return Err(format!("{g}: the check character is wrong (a typo?)"));
    }
    Ok(g)
}

fn state_name(code: &str) -> &'static str {
    match code {
        "01" => "Jammu & Kashmir", "02" => "Himachal Pradesh", "03" => "Punjab", "04" => "Chandigarh",
        "05" => "Uttarakhand", "06" => "Haryana", "07" => "Delhi", "08" => "Rajasthan", "09" => "Uttar Pradesh",
        "10" => "Bihar", "11" => "Sikkim", "12" => "Arunachal Pradesh", "13" => "Nagaland", "14" => "Manipur",
        "15" => "Mizoram", "16" => "Tripura", "17" => "Meghalaya", "18" => "Assam", "19" => "West Bengal",
        "20" => "Jharkhand", "21" => "Odisha", "22" => "Chhattisgarh", "23" => "Madhya Pradesh", "24" => "Gujarat",
        "26" => "Dadra & Nagar Haveli and Daman & Diu", "27" => "Maharashtra", "29" => "Karnataka", "30" => "Goa",
        "31" => "Lakshadweep", "32" => "Kerala", "33" => "Tamil Nadu", "34" => "Puducherry",
        "35" => "Andaman & Nicobar", "36" => "Telangana", "37" => "Andhra Pradesh", "38" => "Ladakh",
        _ => "other",
    }
}

fn our_state(p: &Partner) -> String {
    p.cfg.gstin.get(..2).unwrap_or("29").to_string()
}

/// Digits for WhatsApp: "+91 98765 43210" or "98765 43210" -> "919876543210".
pub fn wa_id(phone: &str) -> String {
    let d: String = phone.chars().filter(char::is_ascii_digit).collect();
    if d.len() == 10 { format!("91{d}") } else { d }
}

fn rate_str(bp: u32) -> String {
    if bp.is_multiple_of(100) { format!("{}%", bp / 100) } else { format!("{}.{:02}%", bp / 100, bp % 100) }
}

fn find_customer<'a>(p: &'a Partner, name: &str) -> Result<&'a Customer, String> {
    let n = name.trim().to_lowercase();
    let mut hits = p.data.customers.iter().filter(|c| c.name.to_lowercase() == n);
    if let Some(c) = hits.next() {
        return Ok(c);
    }
    let hits: Vec<&Customer> = p.data.customers.iter().filter(|c| c.name.to_lowercase().starts_with(&n) || c.name.to_lowercase().contains(&n)).collect();
    match hits.as_slice() {
        [c] => Ok(c),
        [] => Err(format!("no customer called {name}; add one: 'add customer {name} phone 98xxxxxxxx'")),
        many => Err(format!("which one: {}?", many.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", "))),
    }
}

// ---- customers and catalog ---------------------------------------------------------------------

pub fn add_customer(p: &mut Partner, name: &str, gstin: &str, phone: &str) -> Result<Ui, String> {
    let gstin = match gstin.trim().to_lowercase().as_str() {
        "" | "none" | "no" | "-" | "consumer" => None,
        _ => Some(valid_gstin(gstin)?),
    };
    let state = gstin.as_ref().map(|g| g[..2].to_string()).unwrap_or_else(|| our_state(p));
    // the contact is a phone (WhatsApp) or an email address
    let (phone, email) = if phone.contains('@') { (String::new(), phone.trim().to_string()) } else { (wa_id(phone), String::new()) };
    let c = Customer { name: name.trim().to_string(), gstin: gstin.clone(), phone, email, state: state.clone() };
    p.data.customers.retain(|x| !x.name.eq_ignore_ascii_case(&c.name));
    p.data.customers.push(c.clone());
    Ok(Ui::new(format!("Customer {} saved", c.name)).card(
        Card::new(&c.name)
            .line(match &gstin {
                Some(g) => format!("GSTIN {g} (business, B2B)"),
                None => "no GSTIN (consumer, B2C)".into(),
            })
            .line(format!("place of supply: {} ({state})", state_name(&state)))
            .line(if !c.phone.is_empty() { format!("WhatsApp +{}", c.phone) } else if !c.email.is_empty() { format!("email {}", c.email) } else { "no phone or email".into() })
            .tone("ok"),
    ))
}

pub fn add_item(p: &mut Partner, name: &str, price: &str, gst: &str) -> Result<Ui, String> {
    let price = parse_inr(price).ok_or(format!("{price:?} is not a price"))?;
    let rate = parse_rate(gst)?;
    let it = Item { name: name.trim().to_string(), price, rate, hsn: String::new() };
    p.data.items.retain(|x| !x.name.eq_ignore_ascii_case(&it.name));
    p.data.items.push(it.clone());
    Ok(Ui::new(format!("{} added to the catalog", it.name)).card(
        Card::new(&it.name).badge(inr(it.price)).line(format!("GST {} -> {} with tax", rate_str(rate), inr(it.price + it.price * rate as i64 / 10_000))).tone("ok"),
    ))
}

fn parse_rate(s: &str) -> Result<u32, String> {
    let s = s.trim().trim_end_matches('%').trim();
    let r: f64 = s.parse().map_err(|_| format!("{s:?} is not a GST rate"))?;
    let bp = (r * 100.0).round() as u32;
    if ![0, 25, 300, 500, 1200, 1800, 2800].contains(&bp) {
        return Err(format!("{r}% is not a GST rate (0, 0.25, 3, 5, 12, 18 or 28)"));
    }
    Ok(bp)
}

// ---- invoices ----------------------------------------------------------------------------------

/// "2 x Widget, 1 x Repair @ 1500, 3 notebooks" -> (qty, name, price if given)
fn parse_items(s: &str) -> Result<Vec<(u32, String, Option<i64>)>, String> {
    let mut out = vec![];
    for part in s.split([',', ';']).flat_map(|x| x.split(" and ")) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (body, price) = match part.split_once('@') {
            Some((b, pr)) => (b.trim(), Some(parse_inr(pr).ok_or(format!("{pr:?} is not a price"))?)),
            None => (part, None),
        };
        let mut words = body.split_whitespace().peekable();
        let mut qty = 1u32;
        if let Some(w) = words.peek() {
            if let Ok(n) = w.parse::<u32>() {
                qty = n;
                words.next();
                if words.peek().is_some_and(|w| *w == "x" || *w == "X" || *w == "*") {
                    words.next();
                }
            }
        }
        let name: Vec<&str> = words.collect();
        if name.is_empty() {
            return Err(format!("what is {part:?}?"));
        }
        out.push((qty.max(1), name.join(" "), price));
    }
    if out.is_empty() {
        return Err("no items: say '2 x Widget, 1 x Repair @ 1500'".into());
    }
    Ok(out)
}

fn find_item<'a>(p: &'a Partner, name: &str) -> Option<&'a Item> {
    let n = name.to_lowercase();
    let n1 = n.trim_end_matches('s');
    p.data
        .items
        .iter()
        .find(|i| i.name.to_lowercase() == n)
        .or_else(|| p.data.items.iter().find(|i| i.name.to_lowercase().trim_end_matches('s') == n1))
        .or_else(|| p.data.items.iter().find(|i| i.name.to_lowercase().contains(n1)))
}

pub fn make_invoice(p: &Partner, cust: &Customer, items: &str, date: Date, seq: usize) -> Result<Invoice, String> {
    let ours = our_state(p);
    let intra = cust.state == ours;
    let mut lines = vec![];
    for (qty, name, price) in parse_items(items)? {
        let it = find_item(p, &name);
        let (unit, rate, label, hsn) = match (it, price) {
            (Some(i), Some(pr)) => (pr, i.rate, i.name.clone(), i.hsn.clone()),
            (Some(i), None) => (i.price, i.rate, i.name.clone(), i.hsn.clone()),
            (None, Some(pr)) => (pr, 1800, name.clone(), String::new()),
            (None, None) => {
                let have: Vec<&str> = p.data.items.iter().map(|i| i.name.as_str()).collect();
                return Err(format!("no price for {name:?}: give one ('{name} @ 500') or add it to the catalog{}", if have.is_empty() { String::new() } else { format!(" (have: {})", have.join(", ")) }));
            }
        };
        let taxable = unit * qty as i64;
        let tax = (taxable * rate as i64 + 5_000) / 10_000;
        let (cgst, sgst, igst) = if intra { (tax / 2, tax - tax / 2, 0) } else { (0, 0, tax) };
        lines.push(Line { item: label, hsn, qty, unit, rate, taxable, cgst, sgst, igst });
    }
    let taxable: i64 = lines.iter().map(|l| l.taxable).sum();
    let (cgst, sgst, igst) = (lines.iter().map(|l| l.cgst).sum::<i64>(), lines.iter().map(|l| l.sgst).sum::<i64>(), lines.iter().map(|l| l.igst).sum::<i64>());
    let exact = taxable + cgst + sgst + igst;
    let total = (exact + 50) / 100 * 100; // to the rupee
    Ok(Invoice {
        number: format!("INV/{}/{:04}", date.fy(), seq),
        date,
        customer: cust.name.clone(),
        gstin: cust.gstin.clone(),
        pos: cust.state.clone(),
        lines,
        taxable,
        cgst,
        sgst,
        igst,
        round_off: total - exact,
        total,
        status: "issued".into(),
        paid_ref: String::new(),
    })
}

fn invoice_card(p: &Partner, inv: &Invoice) -> Card {
    let mut c = Card::new(&inv.number).subtitle(format!("{} · {}", inv.customer, inv.date)).badge(inr(inv.total));
    for l in &inv.lines {
        c = c.line(format!("{} x {} @ {} = {} (GST {})", l.qty, l.item, inr(l.unit), inr(l.taxable), rate_str(l.rate)));
    }
    c = c.line(format!("taxable {}", inr(inv.taxable)));
    if inv.igst > 0 || inv.cgst + inv.sgst == 0 && inv.pos != our_state(p) {
        c = c.line(format!("IGST {} (to {})", inr(inv.igst), state_name(&inv.pos)));
    } else {
        c = c.line(format!("CGST {} + SGST {} (within {})", inr(inv.cgst), inr(inv.sgst), state_name(&inv.pos)));
    }
    if inv.round_off != 0 {
        c = c.line(format!("round off {}", inr(inv.round_off)));
    }
    c.line(format!("total {} · {}", inr(inv.total), inv.status)).tone(if inv.status == "paid" { "ok" } else { "info" })
}

pub fn invoice(p: &mut Partner, customer: &str, items: &str) -> Result<Ui, String> {
    let cust = find_customer(p, customer)?.clone();
    let today = p.today();
    let fy = today.fy();
    let seq = p.data.invoices.iter().filter(|i| i.date.fy() == fy).count() + 1;
    let inv = make_invoice(p, &cust, items, today, seq)?;
    let ui = Ui::new(format!("Invoice for {}", cust.name))
        .subtitle(if cust.gstin.is_some() { "B2B tax invoice" } else { "B2C tax invoice" })
        .card(invoice_card(p, &inv))
        .action("Send on WhatsApp", format!("send invoice {}", inv.number));
    p.data.invoices.push(inv);
    Ok(ui)
}

/// The UPI link customers tap to pay (NPCI's upi://pay deep link).
fn upi_link(p: &Partner, inv: &Invoice) -> String {
    let enc = |s: &str| s.replace(' ', "%20").replace('/', "%2F").replace('&', "%26");
    format!("upi://pay?pa={}&pn={}&am={}.{:02}&cu=INR&tn={}", p.cfg.vpa, enc(&p.cfg.business_name), inv.total / 100, inv.total % 100, enc(&inv.number))
}

pub fn send_invoice(p: &mut Partner, number: &str) -> Result<Ui, String> {
    let i = p.data.invoices.iter().position(|i| i.number.eq_ignore_ascii_case(number.trim())).ok_or(format!("no invoice {number}"))?;
    let inv = p.data.invoices[i].clone();
    let cust = find_customer(p, &inv.customer)?.clone();
    let mut text = format!("{}\nTax invoice {} dated {}\n", p.cfg.business_name, inv.number, inv.date);
    for l in &inv.lines {
        text.push_str(&format!("{} x {} = {}\n", l.qty, l.item, inr(l.taxable)));
    }
    if inv.igst > 0 {
        text.push_str(&format!("IGST {}\n", inr(inv.igst)));
    } else {
        text.push_str(&format!("CGST {} SGST {}\n", inr(inv.cgst), inr(inv.sgst)));
    }
    text.push_str(&format!("Total {}\nGSTIN {}\nPay by UPI: {}", inr(inv.total), p.cfg.gstin, upi_link(p, &inv)));
    let via = if !cust.phone.is_empty() {
        let id = crate::channels::send_whatsapp(p, &cust.phone, &text)?;
        format!("WhatsApp +{} ({id})", cust.phone)
    } else if !cust.email.is_empty() {
        p.post("/mail/send", &json!({"to": cust.email, "subject": format!("Invoice {}", inv.number), "body": text}))?;
        format!("email {}", cust.email)
    } else {
        return Err(format!("{} has no phone or email", cust.name));
    };
    if p.data.invoices[i].status == "issued" {
        p.data.invoices[i].status = "sent".into();
    }
    Ok(Ui::new(format!("Invoice {} sent", inv.number)).card(Card::new(&cust.name).subtitle(via).badge(inr(inv.total)).line("with a UPI link; payment is matched automatically").tone("ok")))
}

pub fn mark_paid(p: &mut Partner, number: &str) -> Result<Ui, String> {
    let inv = p.data.invoices.iter_mut().find(|i| i.number.eq_ignore_ascii_case(number.trim())).ok_or(format!("no invoice {number}"))?;
    inv.status = "paid".into();
    if inv.paid_ref.is_empty() {
        inv.paid_ref = "marked by hand".into();
    }
    let (n, c, t) = (inv.number.clone(), inv.customer.clone(), inv.total);
    Ok(Ui::new(format!("{n} paid")).card(Card::new(c).badge(inr(t)).tone("ok")))
}

/// Match payments received (UPI collections) to open invoices by number and amount.
fn reconcile(p: &mut Partner) -> Vec<String> {
    let Ok(v) = p.get("/pay/v1/collections") else { return vec![] };
    let mut matched = vec![];
    for c in v["items"].as_array().cloned().unwrap_or_default() {
        let note = c["note"].as_str().unwrap_or("").to_uppercase();
        let amount = c["amount_paise"].as_i64().unwrap_or(0);
        let id = c["id"].as_str().unwrap_or("").to_string();
        for inv in p.data.invoices.iter_mut().filter(|i| i.status != "paid") {
            if note.contains(&inv.number.to_uppercase()) && amount >= inv.total {
                inv.status = "paid".into();
                inv.paid_ref = id.clone();
                matched.push(inv.number.clone());
            }
        }
    }
    matched
}

pub fn invoices(p: &mut Partner) -> Result<Ui, String> {
    let matched = reconcile(p);
    let today = p.today();
    let open: Vec<&Invoice> = p.data.invoices.iter().filter(|i| i.status != "paid").collect();
    let owed: i64 = open.iter().map(|i| i.total).sum();
    let mut ui = Ui::new("Invoices").subtitle(format!("{} open, {} owed", open.len(), inr(owed)));
    for n in &matched {
        ui = ui.card(Card::new(format!("{n} paid")).subtitle("payment received by UPI and matched").tone("ok"));
    }
    for i in p.data.invoices.iter().rev().take(12) {
        let age = today.days() - i.date.days();
        let mut c = Card::new(&i.number).subtitle(format!("{} · {}", i.customer, i.date)).badge(inr(i.total));
        c = match i.status.as_str() {
            "paid" => c.line("paid").tone("ok"),
            "sent" => c.line(format!("sent, unpaid for {age} days")).tone(if age > 30 { "warn" } else { "info" }).button("Remind", format!("reply to {} payment reminder for {}: {}", i.customer, i.number, inr(i.total))),
            _ => c.line("not sent yet").button("Send", format!("send invoice {}", i.number)),
        };
        ui = ui.card(c);
    }
    if p.data.invoices.is_empty() {
        ui = ui.card(Card::new("No invoices yet").subtitle("say 'invoice <customer> for 2 x <item>'"));
    }
    Ok(ui)
}

// ---- GST returns -------------------------------------------------------------------------------

pub struct Gst {
    pub period: String,
    pub b2b: Vec<Value>,
    pub b2cs: Vec<Value>,
    pub hsn: Vec<Value>,
    pub out: [i64; 4],     // taxable, igst, cgst, sgst
    pub itc: [i64; 3],     // igst, cgst, sgst
    pub cash: [i64; 3],    // igst, cgst, sgst payable in cash
    pub carry: [i64; 3],   // credit left over
    pub docs: (usize, String, String),
}

/// Use credit against liability in the order the law sets; returns (cash payable, credit left).
pub fn set_off(liab: [i64; 3], itc: [i64; 3]) -> ([i64; 3], [i64; 3]) {
    let (mut l, mut c) = (liab, itc);
    let use_ = |from: usize, to: usize, l: &mut [i64; 3], c: &mut [i64; 3]| {
        let u = c[from].min(l[to]);
        c[from] -= u;
        l[to] -= u;
    };
    // 0 = IGST, 1 = CGST, 2 = SGST
    for (from, to) in [(0, 0), (0, 1), (0, 2), (1, 1), (1, 0), (2, 2), (2, 0)] {
        use_(from, to, &mut l, &mut c);
    }
    (l, c)
}

pub fn compute_gst(p: &Partner, y: i32, m: u32) -> Gst {
    let in_month = |d: &Date| d.y == y && d.m == m;
    let invs: Vec<&Invoice> = p.data.invoices.iter().filter(|i| in_month(&i.date)).collect();
    let mut b2b = vec![];
    let mut b2cs: BTreeMap<(String, u32), [i64; 4]> = BTreeMap::new();
    let mut hsn: BTreeMap<(String, u32), (u32, [i64; 4])> = BTreeMap::new();
    let mut out = [0i64; 4];
    for i in &invs {
        let mut by_rate: BTreeMap<u32, [i64; 4]> = BTreeMap::new();
        for l in &i.lines {
            let r = by_rate.entry(l.rate).or_default();
            r[0] += l.taxable;
            r[1] += l.igst;
            r[2] += l.cgst;
            r[3] += l.sgst;
            let h = hsn.entry((if l.hsn.is_empty() { "-".into() } else { l.hsn.clone() }, l.rate)).or_default();
            h.0 += l.qty;
            for k in 0..4 {
                h.1[k] += [l.taxable, l.igst, l.cgst, l.sgst][k];
            }
        }
        for (k, v) in out.iter_mut().enumerate() {
            *v += [i.taxable, i.igst, i.cgst, i.sgst][k];
        }
        match &i.gstin {
            Some(g) => b2b.push(json!({
                "ctin": g, "inum": i.number, "idt": format!("{:02}-{:02}-{}", i.date.d, i.date.m, i.date.y),
                "val": i.total as f64 / 100.0, "pos": i.pos, "rchrg": "N", "inv_typ": "R",
                "itms": by_rate.iter().map(|(r, v)| json!({"rt": *r as f64 / 100.0, "txval": v[0] as f64 / 100.0, "iamt": v[1] as f64 / 100.0, "camt": v[2] as f64 / 100.0, "samt": v[3] as f64 / 100.0})).collect::<Vec<_>>(),
            })),
            None => {
                for (r, v) in by_rate {
                    let e = b2cs.entry((i.pos.clone(), r)).or_default();
                    for k in 0..4 {
                        e[k] += v[k];
                    }
                }
            }
        }
    }
    let mut itc = [0i64; 3];
    for e in p.data.expenses.iter().filter(|e| in_month(&e.date) && e.gstin.is_some()) {
        itc[0] += e.igst;
        itc[1] += e.cgst;
        itc[2] += e.sgst;
    }
    let (cash, carry) = set_off([out[1], out[2], out[3]], itc);
    let mut nums: Vec<&str> = invs.iter().map(|i| i.number.as_str()).collect();
    nums.sort();
    Gst {
        period: format!("{m:02}{y}"),
        b2b,
        b2cs: b2cs.into_iter().map(|((pos, r), v)| json!({"pos": pos, "typ": "OE", "rt": r as f64 / 100.0, "txval": v[0] as f64 / 100.0, "iamt": v[1] as f64 / 100.0, "camt": v[2] as f64 / 100.0, "samt": v[3] as f64 / 100.0})).collect(),
        hsn: hsn.into_iter().map(|((h, r), (q, v))| json!({"hsn_sc": h, "rt": r as f64 / 100.0, "qty": q, "txval": v[0] as f64 / 100.0, "iamt": v[1] as f64 / 100.0, "camt": v[2] as f64 / 100.0, "samt": v[3] as f64 / 100.0})).collect(),
        out,
        itc,
        cash,
        carry,
        docs: (nums.len(), nums.first().unwrap_or(&"-").to_string(), nums.last().unwrap_or(&"-").to_string()),
    }
}

pub fn gst_summary(p: &mut Partner, period: &str) -> Result<Ui, String> {
    let (y, m) = parse_period(period, p.today())?;
    let g = compute_gst(p, y, m);
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    let filed = |form: &str| p.data.filings.iter().find(|f| f.form == form && f.period == g.period).map(|f| f.arn.clone());
    let mut ui = Ui::new(format!("GST for {}", month_name(y, m))).subtitle(format!("GSTIN {}", p.cfg.gstin));
    ui = ui.card(
        Card::new("Sales (outward supplies)")
            .badge(inr(g.out[0]))
            .line(format!("{} invoices ({} B2B, B2C summed in {} rows), {} to {}", g.docs.0, g.b2b.len(), g.b2cs.len(), g.docs.1, g.docs.2))
            .line(format!("IGST {} · CGST {} · SGST {}", inr(g.out[1]), inr(g.out[2]), inr(g.out[3]))),
    );
    ui = ui.card(Card::new("Input tax credit (bills paid)").badge(inr(g.itc.iter().sum())).line(format!("IGST {} · CGST {} · SGST {}", inr(g.itc[0]), inr(g.itc[1]), inr(g.itc[2]))));
    let cash: i64 = g.cash.iter().sum();
    ui = ui.card(
        Card::new("Tax to pay in cash (GSTR-3B)")
            .badge(inr(cash))
            .line(format!("IGST {} · CGST {} · SGST {}", inr(g.cash[0]), inr(g.cash[1]), inr(g.cash[2])))
            .line(if g.carry.iter().sum::<i64>() > 0 { format!("credit carried forward {}", inr(g.carry.iter().sum())) } else { "no credit left over".into() })
            .tone(if cash > 0 { "warn" } else { "ok" }),
    );
    for (form, due) in [("GSTR-1", 11), ("GSTR-3B", 20)] {
        let c = Card::new(form).subtitle(format!("due {ny}-{nm:02}-{due:02}"));
        ui = ui.card(match filed(form) {
            Some(arn) => c.line(format!("filed, ARN {arn}")).tone("ok"),
            None => c.line("not filed").button(format!("File {form}"), format!("file {} for {}-{:02}", form.to_lowercase(), y, m)),
        });
    }
    Ok(ui)
}

pub fn file_gst(p: &mut Partner, form: &str, period: &str) -> Result<Ui, String> {
    let f = form.trim().to_uppercase().replace(' ', "");
    let form = match f.as_str() {
        "GSTR-1" | "GSTR1" => "GSTR-1",
        "GSTR-3B" | "GSTR3B" => "GSTR-3B",
        _ => return Err(format!("{form}: GSTR-1 or GSTR-3B")),
    };
    let (y, m) = parse_period(period, p.today())?;
    let g = compute_gst(p, y, m);
    if let Some(f) = p.data.filings.iter().find(|f| f.form == form && f.period == g.period) {
        return Err(format!("{form} for {} was filed already (ARN {})", month_name(y, m), f.arn));
    }
    let r = |x: i64| x as f64 / 100.0;
    let data = if form == "GSTR-1" {
        json!({"b2b": g.b2b, "b2cs": g.b2cs, "hsn": {"data": g.hsn}, "doc_issue": {"doc_det": [{"doc_num": 1, "docs": [{"from": g.docs.1, "to": g.docs.2, "totnum": g.docs.0, "cancel": 0, "net_issue": g.docs.0}]}]}})
    } else {
        json!({
            "sup_details": {"osup_det": {"txval": r(g.out[0]), "iamt": r(g.out[1]), "camt": r(g.out[2]), "samt": r(g.out[3]), "csamt": 0}},
            "itc_elg": {"itc_avl": [{"ty": "OTH", "iamt": r(g.itc[0]), "camt": r(g.itc[1]), "samt": r(g.itc[2]), "csamt": 0}]},
            "tx_pmt": {"cash": {"iamt": r(g.cash[0]), "camt": r(g.cash[1]), "samt": r(g.cash[2])}},
        })
    };
    let path = format!("/gsp/v1/returns/{}", form.to_lowercase().replace('-', ""));
    let v = p.post(&path, &json!({"gstin": p.cfg.gstin, "ret_period": g.period, "data": data}))?;
    let arn = v["arn"].as_str().ok_or(format!("the GSP did not file: {v}"))?.to_string();
    p.data.filings.push(Filing { form: form.into(), period: g.period.clone(), arn: arn.clone(), filed_on: p.today() });
    Ok(Ui::new(format!("{form} filed for {}", month_name(y, m))).card(Card::new(format!("ARN {arn}")).subtitle(format!("through the GSP, {}", p.today())).line(if form == "GSTR-3B" { format!("tax paid in cash {}", inr(g.cash.iter().sum())) } else { format!("{} invoices reported", g.docs.0) }).tone("ok")))
}

// ---- bills and expenses ------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Bill {
    pub id: String,
    pub supplier: String,
    pub amount: i64,
    pub gst: i64,
    pub gstin: Option<String>,
    pub due: Option<Date>,
    pub vpa: String,
}

/// A bill from an email: "Amount due: ₹4,720.00", "Due date: 2026-11-15", "GSTIN: ...",
/// "GST: ₹720.00", "Pay to UPI: power@upi" (one per line, in any order).
pub fn parse_bill(id: &str, from: &str, subject: &str, body: &str) -> Option<Bill> {
    let field = |names: &[&str]| -> Option<String> {
        body.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            let k = k.trim().to_lowercase();
            names.iter().any(|n| k == *n).then(|| v.trim().to_string())
        })
    };
    let amount = parse_inr(&field(&["amount due", "total due", "amount payable", "total"])?)?;
    let supplier = subject.trim().strip_prefix("Bill from").or_else(|| subject.strip_prefix("Bill:")).map(|s| s.trim().to_string()).unwrap_or_else(|| from.split('@').next().unwrap_or(from).to_string());
    Some(Bill {
        id: id.to_string(),
        supplier,
        amount,
        gst: field(&["gst", "gst amount", "tax"]).and_then(|s| parse_inr(&s)).unwrap_or(0),
        gstin: field(&["gstin", "supplier gstin"]).and_then(|g| valid_gstin(&g).ok()),
        due: field(&["due date", "due", "pay by"]).and_then(|d| Date::parse(&d)),
        vpa: field(&["pay to upi", "upi", "vpa", "upi id"]).unwrap_or_default(),
    })
}

fn bills_due(p: &Partner) -> Result<Vec<Bill>, String> {
    let v = p.get("/mail/inbox")?;
    let paid: Vec<&str> = p.data.expenses.iter().map(|e| e.id.as_str()).collect();
    Ok(v["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|m| parse_bill(m["id"].as_str()?, m["from"].as_str()?, m["subject"].as_str()?, m["body"].as_str()?))
        .filter(|b| !paid.contains(&b.id.as_str()))
        .collect())
}

pub fn bills(p: &mut Partner) -> Result<Ui, String> {
    let today = p.today();
    let mut bs = bills_due(p)?;
    bs.sort_by_key(|b| b.due.map(|d| d.days()).unwrap_or(i64::MAX));
    let total: i64 = bs.iter().map(|b| b.amount).sum();
    let mut ui = Ui::new("Bills to pay").subtitle(format!("{} bills, {}", bs.len(), inr(total)));
    for b in &bs {
        let days = b.due.map(|d| d.days() - today.days());
        let mut c = Card::new(&b.supplier).badge(inr(b.amount));
        c = c.subtitle(match days {
            Some(d) if d < 0 => format!("overdue by {} days", -d),
            Some(0) => "due today".into(),
            Some(d) => format!("due in {d} days ({})", b.due.unwrap()),
            None => "no due date".into(),
        });
        if b.gst > 0 {
            c = c.line(format!("GST {} {}", inr(b.gst), if b.gstin.is_some() { "(input credit)" } else { "(no GSTIN: no credit)" }));
        }
        c = c.line(if b.vpa.is_empty() { "no UPI address on the bill".into() } else { format!("pay to {}", b.vpa) });
        c = c.tone(if days.is_some_and(|d| d <= 2) { "warn" } else { "info" });
        if !b.vpa.is_empty() {
            c = c.button("Pay", format!("pay bill {}", b.id));
        }
        ui = ui.card(c);
    }
    if bs.is_empty() {
        ui = ui.card(Card::new("Nothing due").tone("ok"));
    }
    Ok(ui)
}

pub fn pay_bill(p: &mut Partner, id: &str) -> Result<Ui, String> {
    let b = bills_due(p)?.into_iter().find(|b| b.id.eq_ignore_ascii_case(id.trim())).ok_or(format!("no unpaid bill {id}"))?;
    if b.vpa.is_empty() {
        return Err(format!("the bill from {} has no UPI address", b.supplier));
    }
    let pay = wallet::payout(p, &b.vpa, b.amount, &format!("bill {} {}", b.supplier, b.id))?;
    // the bill's GST as input credit: within the state CGST + SGST, else IGST
    let (cgst, sgst, igst) = match &b.gstin {
        Some(g) if g[..2] == our_state(p) => (b.gst / 2, b.gst - b.gst / 2, 0),
        Some(_) => (0, 0, b.gst),
        None => (0, 0, 0),
    };
    p.data.expenses.push(Expense { id: b.id.clone(), supplier: b.supplier.clone(), gstin: b.gstin.clone(), date: p.today(), amount: b.amount, cgst, sgst, igst, paid_ref: pay.id.clone() });
    let bal = wallet::balance(p).ok();
    Ok(Ui::new(format!("Paid {}", b.supplier)).card(
        Card::new(inr(b.amount)).subtitle(format!("UPI to {} · ref {}", b.vpa, pay.utr)).line(if cgst + sgst + igst > 0 { format!("input credit {}", inr(cgst + sgst + igst)) } else { "no input credit".into() }).line(bal.map(|b| format!("wallet now {}", inr(b))).unwrap_or_default()).tone("ok"),
    ))
}

pub fn expenses(p: &mut Partner, period: &str) -> Result<Ui, String> {
    let (y, m) = parse_period(period, p.today())?;
    let es: Vec<&Expense> = p.data.expenses.iter().filter(|e| e.date.y == y && e.date.m == m).collect();
    let mut by: BTreeMap<&str, (i64, i64)> = BTreeMap::new();
    for e in &es {
        let x = by.entry(e.supplier.as_str()).or_default();
        x.0 += e.amount;
        x.1 += e.cgst + e.sgst + e.igst;
    }
    let total: i64 = es.iter().map(|e| e.amount).sum();
    let itc: i64 = es.iter().map(|e| e.cgst + e.sgst + e.igst).sum();
    let mut ui = Ui::new(format!("Expenses, {}", month_name(y, m))).subtitle(format!("{} · input credit {}", inr(total), inr(itc)));
    for (s, (a, t)) in by {
        ui = ui.card(Card::new(s).badge(inr(a)).line(format!("GST credit {}", inr(t))));
    }
    if es.is_empty() {
        ui = ui.card(Card::new("No expenses recorded this month"));
    }
    Ok(ui)
}
