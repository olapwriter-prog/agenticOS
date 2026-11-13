//! Shopping from the order history: what is bought often, the best price for it across the
//! platforms (price + delivery, from sellers rated 3.5 or more, the same product as usual when
//! there is one), and buying it from the wallet. One best option is shown, not a list to read.
use crate::ui::{Card, Ui};
use crate::{inr, wallet, Date, Partner};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub platform: String,
    pub order_id: String,
    pub title: String,
    pub qty: u32,
    pub price: i64,
    pub date: Date,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Option_ {
    pub platform: String,
    pub id: String,
    pub title: String,
    pub price: i64,
    pub delivery: i64,
    pub rating: f64,
    pub eta_days: u32,
    pub merchant_vpa: String,
}

impl Option_ {
    pub fn cost(&self) -> i64 {
        self.price + self.delivery
    }
}

fn words(s: &str) -> Vec<String> {
    s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 1).map(String::from).collect()
}

/// How much of `a`'s words are in `b` (0..1).
fn overlap(a: &str, b: &str) -> f64 {
    let (wa, wb) = (words(a), words(b));
    if wa.is_empty() {
        return 0.0;
    }
    wa.iter().filter(|w| wb.contains(w)).count() as f64 / wa.len() as f64
}

/// Every past order: the platforms' and the ones placed here.
fn history(p: &Partner) -> Vec<Order> {
    let mut out: Vec<Order> = vec![];
    for pl in &p.cfg.platforms {
        if let Ok(v) = p.get(&format!("/shop/{pl}/orders")) {
            for o in v["orders"].as_array().cloned().unwrap_or_default() {
                out.push(Order {
                    platform: pl.clone(),
                    order_id: o["order_id"].as_str().unwrap_or("").into(),
                    title: o["title"].as_str().unwrap_or("").into(),
                    qty: o["qty"].as_u64().unwrap_or(1) as u32,
                    price: o["price_paise"].as_i64().unwrap_or(0),
                    date: o["date"].as_str().and_then(Date::parse).unwrap_or(Date { y: 2026, m: 1, d: 1 }),
                });
            }
        }
    }
    for o in &p.data.orders {
        if !out.iter().any(|x| x.order_id == o.order_id && x.platform == o.platform) {
            out.push(o.clone());
        }
    }
    out.sort_by_key(|o| std::cmp::Reverse(o.date));
    out
}

pub fn orders(p: &mut Partner) -> Result<Ui, String> {
    let h = history(p);
    // bought more than once: the same product (by title) on any platform
    let mut often: BTreeMap<String, (u32, Vec<&Order>)> = BTreeMap::new();
    for o in &h {
        let key = words(&o.title).join(" ");
        let e = often.entry(key).or_default();
        e.0 += 1;
        e.1.push(o);
    }
    let mut regular: Vec<(u32, Vec<&Order>)> = often.into_values().filter(|(n, _)| *n >= 2).collect();
    regular.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    let mut ui = Ui::new("Orders").subtitle(format!("{} orders on {} platforms", h.len(), p.cfg.platforms.len()));
    for (n, os) in regular.iter().take(5) {
        let last = os[0];
        ui = ui.card(Card::new(&last.title).subtitle(format!("bought {n} times, last {} on {}", last.date, last.platform)).badge(inr(last.price)).button("Best price now", format!("best price for {}", last.title)));
    }
    let recent: Vec<String> = h.iter().take(6).map(|o| format!("{} · {} · {} · {}", o.date, o.platform, o.title, inr(o.price))).collect();
    if !recent.is_empty() {
        let mut c = Card::new("Recent");
        for r in recent {
            c = c.line(r);
        }
        ui = ui.card(c);
    }
    Ok(ui)
}

pub fn best_price(p: &mut Partner, item: &str) -> Result<Ui, String> {
    let h = history(p);
    // the product usually bought that matches what was asked
    let usual = h.iter().filter(|o| overlap(item, &o.title) >= 0.99).max_by_key(|o| (h.iter().filter(|x| x.title == o.title).count(), o.date)).cloned();
    let query = usual.as_ref().map(|o| o.title.clone()).unwrap_or_else(|| item.to_string());
    let mut opts: Vec<Option_> = vec![];
    for pl in &p.cfg.platforms {
        let q: String = query.split_whitespace().collect::<Vec<_>>().join("+");
        let Ok(v) = p.get(&format!("/shop/{pl}/search?q={q}")) else { continue };
        for r in v["results"].as_array().cloned().unwrap_or_default() {
            if r["in_stock"].as_bool() == Some(false) {
                continue;
            }
            opts.push(Option_ {
                platform: pl.clone(),
                id: r["id"].as_str().unwrap_or("").into(),
                title: r["title"].as_str().unwrap_or("").into(),
                price: r["price_paise"].as_i64().unwrap_or(0),
                delivery: r["delivery_paise"].as_i64().unwrap_or(0),
                rating: r["rating"].as_f64().unwrap_or(0.0),
                eta_days: r["eta_days"].as_u64().unwrap_or(3) as u32,
                merchant_vpa: r["merchant_vpa"].as_str().map(String::from).unwrap_or_else(|| format!("{pl}@upi")),
            });
        }
    }
    // the same product as usual if it is on offer; else what matches the words asked for
    let same: Vec<Option_> = match &usual {
        Some(u) => opts.iter().filter(|o| overlap(&u.title, &o.title) >= 0.85).cloned().collect(),
        None => vec![],
    };
    let mut pool = if same.is_empty() { opts.into_iter().filter(|o| overlap(item, &o.title) >= 0.5).collect() } else { same };
    if pool.is_empty() {
        return Err(format!("no platform has {item:?} in stock"));
    }
    let good: Vec<Option_> = pool.iter().filter(|o| o.rating >= 3.5).cloned().collect();
    if !good.is_empty() {
        pool = good;
    }
    pool.sort_by(|a, b| a.cost().cmp(&b.cost()).then(b.rating.total_cmp(&a.rating)).then(a.eta_days.cmp(&b.eta_days)));
    let best = pool[0].clone();
    let worst = pool.iter().map(Option_::cost).max().unwrap_or(best.cost());
    let mut c = Card::new(&best.title)
        .subtitle(format!("on {} · delivery in {} days · rated {:.1}", best.platform, best.eta_days, best.rating))
        .badge(inr(best.cost()))
        .line(if best.delivery > 0 { format!("{} + {} delivery", inr(best.price), inr(best.delivery)) } else { format!("{} with free delivery", inr(best.price)) })
        .tone("ok")
        .button("Buy", "buy best");
    if let Some(u) = &usual {
        let diff = u.price - best.cost();
        c = c.line(format!("you usually buy this; last paid {} on {} ({})", inr(u.price), u.platform, if diff > 0 { format!("{} cheaper now", inr(diff)) } else if diff < 0 { format!("{} dearer now", inr(-diff)) } else { "same price".into() }));
    }
    if pool.len() > 1 && worst > best.cost() {
        c = c.line(format!("saves up to {} over the {} other offers", inr(worst - best.cost()), pool.len() - 1));
    }
    let mut ui = Ui::new(format!("Best price: {item}")).subtitle(format!("{} platforms compared", p.cfg.platforms.len())).card(c);
    let mut others = Card::new("Other offers");
    for o in pool.iter().skip(1).take(4) {
        others = others.line(format!("{} · {} · {} · {:.1}★", o.platform, o.title, inr(o.cost()), o.rating));
    }
    if pool.len() > 1 {
        ui = ui.card(others);
    }
    p.data.last_options = pool;
    Ok(ui)
}

pub fn buy(p: &mut Partner, option: &str) -> Result<Ui, String> {
    let o = option.trim().to_lowercase();
    let chosen = if o == "best" || o == "it" || o == "that" {
        p.data.last_options.first().cloned().ok_or("compare first: 'best price for <item>'")?
    } else if let Some((pl, id)) = o.split_once(':') {
        p.data.last_options.iter().find(|x| x.platform == pl && x.id.to_lowercase() == id).cloned().ok_or(format!("{option} was not among the offers shown; compare first"))?
    } else {
        return Err(format!("which offer? compare first ('best price for {option}'), then say 'buy best'"));
    };
    // the price now, not the price shown a while ago
    let now = p.get(&format!("/shop/{}/products/{}", chosen.platform, chosen.id))?;
    let price = now["price_paise"].as_i64().unwrap_or(chosen.price);
    let delivery = now["delivery_paise"].as_i64().unwrap_or(chosen.delivery);
    if now["in_stock"].as_bool() == Some(false) {
        return Err(format!("{} is out of stock on {} now", chosen.title, chosen.platform));
    }
    if price + delivery > chosen.cost() {
        return Err(format!("the price went up to {} on {}; compare again", inr(price + delivery), chosen.platform));
    }
    let pay = wallet::payout(p, &chosen.merchant_vpa, price + delivery, &format!("{} on {}", chosen.title, chosen.platform))?;
    let v = p.post(&format!("/shop/{}/orders", chosen.platform), &json!({"product_id": chosen.id, "qty": 1, "payment_ref": pay.utr, "amount_paise": price + delivery}))?;
    let order_id = v["order_id"].as_str().ok_or(format!("the order was not placed: {v}"))?.to_string();
    p.data.orders.push(Order { platform: chosen.platform.clone(), order_id: order_id.clone(), title: chosen.title.clone(), qty: 1, price: price + delivery, date: p.today() });
    p.data.last_options.clear();
    Ok(Ui::new("Ordered").card(
        Card::new(&chosen.title).subtitle(format!("{} order {order_id}", chosen.platform)).badge(inr(price + delivery)).line(format!("arrives in {} days", v["eta_days"].as_u64().unwrap_or(chosen.eta_days as u64))).line(format!("paid from the wallet, UTR {}", pay.utr)).tone("ok"),
    ))
}
