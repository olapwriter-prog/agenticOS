//! Customers and people on WhatsApp Business and email, as one inbox. For each thread waiting for
//! an answer the agent drafts a reply from what it knows (the catalog, prices with GST, open
//! invoices) and proposes the business step that follows (an invoice, a payment check). Sending
//! is High tier: drafted here, sent when the human presses the key.
//!
//! WhatsApp: the Cloud API's message send (POST /{phone-number-id}/messages). Messages arrive at
//! a webhook in the real API; the world (simulator or gateway) keeps them per conversation and
//! the agent reads GET /{phone-number-id}/conversations.
use crate::books::{wa_id, Invoice};
use crate::ui::{Card, Ui};
use crate::{inr, Partner};
use serde_json::{json, Value};

pub fn send_whatsapp(p: &Partner, to: &str, text: &str) -> Result<String, String> {
    let v = p.post(
        &format!("/whatsapp/v19.0/{}/messages", p.cfg.wa_phone_id),
        &json!({"messaging_product": "whatsapp", "recipient_type": "individual", "to": to, "type": "text", "text": {"preview_url": false, "body": text}}),
    )?;
    v["messages"][0]["id"].as_str().map(String::from).ok_or(format!("WhatsApp did not accept the message: {v}"))
}

#[derive(Debug, Clone)]
pub struct Thread {
    /// "wa:<id>" or "mail:<address>"
    pub key: String,
    pub name: String,
    pub channel: &'static str,
    pub address: String,
    /// (incoming?, text, message id)
    pub messages: Vec<(bool, String, String)>,
}

impl Thread {
    fn last_in(&self) -> Option<&(bool, String, String)> {
        self.messages.iter().rev().find(|m| m.0)
    }
    fn waiting(&self, p: &Partner) -> bool {
        match (self.messages.last(), self.last_in()) {
            (Some(last), Some(li)) => last.0 && p.data.answered.get(&self.key) != Some(&li.2),
            _ => false,
        }
    }
}

fn threads(p: &Partner) -> Result<Vec<Thread>, String> {
    let mut out = vec![];
    let v = p.get(&format!("/whatsapp/v19.0/{}/conversations", p.cfg.wa_phone_id))?;
    for c in v["data"].as_array().cloned().unwrap_or_default() {
        let id = c["wa_id"].as_str().unwrap_or("").to_string();
        out.push(Thread {
            key: format!("wa:{id}"),
            name: c["name"].as_str().unwrap_or(&id).to_string(),
            channel: "WhatsApp",
            address: id,
            messages: msgs(&c["messages"]),
        });
    }
    // email: everything but bills (those are the books' business)
    let m = p.get("/mail/inbox")?;
    for x in m["messages"].as_array().cloned().unwrap_or_default() {
        let subject = x["subject"].as_str().unwrap_or("");
        let body = x["body"].as_str().unwrap_or("");
        if crate::books::parse_bill(x["id"].as_str().unwrap_or(""), "", subject, body).is_some() {
            continue;
        }
        let from = x["from"].as_str().unwrap_or("").to_string();
        let name = x["from_name"].as_str().map(String::from).unwrap_or_else(|| from.clone());
        let key = format!("mail:{from}");
        let text = format!("{subject}: {body}");
        match out.iter_mut().find(|t| t.key == key) {
            Some(t) => t.messages.push((true, text, x["id"].as_str().unwrap_or("").into())),
            None => out.push(Thread { key, name, channel: "Email", address: from, messages: vec![(true, text, x["id"].as_str().unwrap_or("").into())] }),
        }
    }
    Ok(out)
}

fn msgs(v: &Value) -> Vec<(bool, String, String)> {
    v.as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|m| (m["direction"].as_str() == Some("in"), m["text"].as_str().unwrap_or("").to_string(), m["id"].as_str().unwrap_or("").to_string()))
        .collect()
}

fn find_thread(p: &Partner, who: &str) -> Result<Thread, String> {
    let w = who.trim().to_lowercase();
    let ts = threads(p)?;
    let digits = wa_id(who);
    ts.iter()
        .find(|t| t.name.to_lowercase() == w || t.address.to_lowercase() == w || (!digits.is_empty() && t.address == digits))
        .or_else(|| ts.iter().find(|t| t.name.to_lowercase().starts_with(&w)))
        .cloned()
        .ok_or(format!("no conversation with {who}"))
}

/// Catalog items a message mentions, each with a quantity if it says one: a number just before
/// the item ("10 widgets", "2 x repair"), or, with one item, a lone number elsewhere ("widgets?
/// We may need 40").
fn mentioned<'a>(p: &'a Partner, lower: &str) -> Vec<(&'a crate::books::Item, Option<u32>)> {
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let num = |w: &str| w.parse::<u32>().ok().or_else(|| word_number(w));
    let mut out = vec![];
    for it in &p.data.items {
        let n = it.name.to_lowercase();
        let first = n.split_whitespace().next().unwrap_or("").trim_end_matches('s').to_string();
        let Some(at) = words.iter().position(|w| w.trim_end_matches('s') == first) else { continue };
        let before = words[..at].iter().rev().take(2).find_map(|w| num(w));
        out.push((it, before));
    }
    if out.len() == 1 && out[0].1.is_none() {
        let nums: Vec<u32> = words.iter().filter_map(|w| num(w)).collect();
        if nums.len() == 1 {
            out[0].1 = Some(nums[0]);
        }
    }
    out
}

/// What a reply should say, and the business step it suggests (a label and the words for it).
pub fn draft(p: &Partner, t: &Thread) -> (String, Option<(String, String)>) {
    let first = t.name.split_whitespace().next().unwrap_or(&t.name);
    let Some((_, text, _)) = t.last_in() else { return (String::new(), None) };
    let lower = text.to_lowercase();
    let customer = p.data.customers.iter().find(|c| (t.channel == "WhatsApp" && c.phone == t.address) || (!c.email.is_empty() && c.email == t.address) || c.name.eq_ignore_ascii_case(&t.name));
    let cname = customer.map(|c| c.name.clone()).unwrap_or_else(|| t.name.clone());
    let open: Vec<&Invoice> = p.data.invoices.iter().filter(|i| i.status != "paid" && i.customer.eq_ignore_ascii_case(&cname)).collect();
    let add_first = || (format!("Add {} as a customer", t.name), format!("add customer {} phone {}", t.name, t.address));
    let has = |ws: &[&str]| ws.iter().any(|w| lower.contains(w));
    let with_tax = |i: &crate::books::Item| i.price + (i.price * i.rate as i64 + 5_000) / 10_000;
    let items = mentioned(p, &lower);
    let asks_price = has(&["price", "rate", "cost", "how much", "quote", "quotation", "catalog", "catalogue", "menu"]);
    let orders = has(&["want", "need", "order", "send", "deliver", "take", "book", "buy", "supply"]);

    if has(&["paid", "payment done", "transferred", "sent the money", "sent the payment"]) && !open.is_empty() {
        let inv = open[0];
        return (format!("Thank you {first}! I'll confirm the payment for {} ({}) shortly.", inv.number, inr(inv.total)), Some(("Check payments".into(), "invoices".into())));
    }
    if has(&["invoice", "bill me", "send the bill"]) && !open.is_empty() {
        let inv = open[0];
        return (format!("Hi {first}, sending invoice {} ({}) now.", inv.number, inr(inv.total)), Some((format!("Send {}", inv.number), format!("send invoice {}", inv.number))));
    }
    if !items.is_empty() {
        let priced: Vec<String> = items.iter().map(|(i, _)| format!("{} is {} + {}% GST ({} in all)", i.name, inr(i.price), i.rate / 100, inr(with_tax(i)))).collect();
        let counted: Vec<(&crate::books::Item, u32)> = items.iter().filter_map(|(i, q)| q.map(|q| (*i, q))).collect();
        let step = |list: &[(&crate::books::Item, u32)]| -> (String, String) {
            let spec = list.iter().map(|(i, q)| format!("{q} x {}", i.name)).collect::<Vec<_>>().join(", ");
            match customer {
                Some(c) => (format!("Invoice {spec}"), format!("invoice {} for {spec}", c.name)),
                None => add_first(),
            }
        };
        let total = |list: &[(&crate::books::Item, u32)]| inr(list.iter().map(|(i, q)| with_tax(i) * *q as i64).sum());
        if asks_price {
            if counted.is_empty() {
                return (format!("Hi {first}, {}. How many would you like?", priced.join("; ")), None);
            }
            let spec = counted.iter().map(|(i, q)| format!("{q} x {}", i.name)).collect::<Vec<_>>().join(", ");
            return (format!("Hi {first}, {}. For {spec} that is {} including GST. Shall I send an invoice?", priced.join("; "), total(&counted)), Some(step(&counted)));
        }
        if orders || !counted.is_empty() {
            let list: Vec<(&crate::books::Item, u32)> = items.iter().map(|(i, q)| (*i, q.unwrap_or(1))).collect();
            let spec = list.iter().map(|(i, q)| format!("{q} x {}", i.name)).collect::<Vec<_>>().join(", ");
            return (format!("Sure {first}! {spec} comes to {} including GST. I'll send the invoice with a UPI link.", total(&list)), Some(step(&list)));
        }
        return (format!("Hi {first}, {}. How many would you like?", priced.join("; ")), None);
    }
    if (asks_price || has(&["price list", "prices"])) && !p.data.items.is_empty() {
        let list: Vec<String> = p.data.items.iter().take(5).map(|i| format!("{} {}", i.name, inr(i.price))).collect();
        return (format!("Hi {first}, our prices (+GST): {}. What would you like?", list.join(", ")), None);
    }
    if has(&["when", "status", "where is", "delivery", "dispatch"]) {
        return (format!("Hi {first}, your order is being prepared; I'll send you an update today."), None);
    }
    let step = if customer.is_none() { Some(add_first()) } else { None };
    (format!("Hi {first}, thanks for your message. I'll get back to you shortly."), step)
}

fn word_number(w: &str) -> Option<u32> {
    ["one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "dozen"].iter().position(|x| *x == w).map(|i| if i == 10 { 12 } else { i as u32 + 1 })
}

fn thread_card(p: &Partner, t: &Thread, full: bool) -> Card {
    let (reply, step) = draft(p, t);
    let mut c = Card::new(&t.name).badge(t.channel).tone(if t.waiting(p) { "warn" } else { "" });
    let shown: Vec<&(bool, String, String)> = if full { t.messages.iter().rev().take(8).rev().collect() } else { t.last_in().into_iter().collect() };
    for (incoming, text, _) in shown {
        c = c.line(format!("{} {}", if *incoming { "›" } else { "‹ you:" }, text));
    }
    if t.waiting(p) && !reply.is_empty() {
        c = c.line(format!("draft: {reply}")).button("Send reply", format!("reply to {}: {reply}", t.name));
    }
    if let Some((label, say)) = step {
        c = c.button(label, say);
    }
    c
}

pub fn inbox(p: &mut Partner) -> Result<Ui, String> {
    let ts = threads(p)?;
    let waiting: Vec<&Thread> = ts.iter().filter(|t| t.waiting(p)).collect();
    let mut ui = Ui::new("Inbox").subtitle(format!("{} waiting for a reply · WhatsApp and email", waiting.len()));
    for t in &waiting {
        ui = ui.card(thread_card(p, t, false));
    }
    if waiting.is_empty() {
        ui = ui.card(Card::new("Everyone has an answer").tone("ok"));
    }
    Ok(ui)
}

pub fn read_thread(p: &mut Partner, who: &str) -> Result<Ui, String> {
    let t = find_thread(p, who)?;
    Ok(Ui::new(format!("{} on {}", t.name, t.channel)).card(thread_card(p, &t, true)))
}

pub fn reply(p: &mut Partner, to: &str, text: &str) -> Result<Ui, String> {
    let t = find_thread(p, to)?;
    let text = text.trim();
    if text.is_empty() {
        return Err("an empty reply".into());
    }
    let id = match t.channel {
        "WhatsApp" => send_whatsapp(p, &t.address, text)?,
        _ => {
            let subject = t.last_in().and_then(|m| m.1.split_once(':')).map(|s| format!("Re: {}", s.0)).unwrap_or_else(|| "Re:".into());
            let v = p.post("/mail/send", &json!({"to": t.address, "subject": subject, "body": text}))?;
            v["id"].as_str().unwrap_or("").to_string()
        }
    };
    if let Some(li) = t.last_in() {
        p.data.answered.insert(t.key.clone(), li.2.clone());
    }
    Ok(Ui::new(format!("Replied to {}", t.name)).card(Card::new(t.channel).subtitle(format!("message {id}")).line(text.to_string()).tone("ok")))
}
