//! Events as lines of JSON, and a seeded generator of them. The same events drive the import
//! of past books, the simulator, and (on a real phone) the channel adapters:
//!   {"t":"order", ...}  {"t":"assign","id","rider"}  {"t":"status","id","status","by"}
//!   {"t":"gps","rider","lat","lon"}  {"t":"purchase","supplier","item","amount","gst"}
//!   {"t":"payout","platform","from","to","amount","reference"}  {"t":"clock"}
//! Every event carries "at" (local time). Amounts in events are rupees.
//!
//! The generator invents a shop's days from its profile. The people in it are made up: names
//! from a short list, numbers in a block that is not issued.
use crate::profile::Profile;
use crate::util::*;
use crate::{Business, CustomerIn, ItemIn, OrderIn, Payout};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Apply one event. The answer is what the phone would tell the owner.
pub fn play(b: &mut Business, line: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("not an event: {e}"))?;
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    let at = s("at");
    if !at.is_empty() {
        b.tick(&at);
    }
    match v["t"].as_str().unwrap_or_default() {
        "order" => {
            let o: OrderIn = serde_json::from_value(v.clone()).map_err(|e| format!("order: {e}"))?;
            let direct = b.profile.platform(&o.channel).is_none();
            let id = o.id.clone();
            let mut said = b.ingest(o)?;
            if direct && b.live && b.profile.auto_bill {
                match b.send_bill(&id) {
                    Ok(t) => said.push_str(&format!("; {t}")),
                    Err(e) => said.push_str(&format!("; bill not sent: {e}")),
                }
            }
            Ok(said)
        }
        "assign" => b.assign(&s("id"), &s("rider")),
        "status" => b.set_status(&s("id"), &s("status"), v["by"].as_str()),
        "gps" => b.rider_fix(&s("rider"), v["lat"].as_f64().unwrap_or(0.0), v["lon"].as_f64().unwrap_or(0.0), &at),
        "purchase" => {
            b.purchase(&s("supplier"), &s("item"), rs(v["amount"].as_f64().unwrap_or(0.0)), rs(v["gst"].as_f64().unwrap_or(0.0)))
        }
        "payout" => b.payout_in(Payout {
            platform: s("platform"),
            from: s("from"),
            to: s("to"),
            amount: rs(v["amount"].as_f64().unwrap_or(0.0)),
            reference: s("reference"),
        }),
        "clock" => Ok(format!("it is {}", b.now())),
        other => Err(format!("unknown event {other:?}")),
    }
}

/// Import a file of past events quietly: nothing is sent, the book is written once.
/// Returns (applied, errors).
pub fn import(b: &mut Business, text: &str) -> Result<(usize, Vec<String>), String> {
    b.live = false;
    let (mut ok, mut bad) = (0, Vec::new());
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match play(b, line) {
            Ok(_) => ok += 1,
            Err(e) => bad.push(e),
        }
    }
    b.set_live(true)?;
    Ok((ok, bad))
}

pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// 0.0 <= x < 1.0
    pub fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.unit() * n as f64) as usize
    }
    pub fn pick(&mut self, weights: &[f64]) -> usize {
        let mut x = self.unit() * weights.iter().sum::<f64>();
        for (i, w) in weights.iter().enumerate() {
            x -= w;
            if x < 0.0 {
                return i;
            }
        }
        weights.len() - 1
    }
}

const FIRST: [&str; 16] = [
    "Lakshmi", "Ravi", "Sai", "Divya", "Kiran", "Padma", "Srinu", "Anusha", "Venkat", "Swathi", "Naveen", "Bhavani", "Prasad", "Jyothi",
    "Harsha", "Sravani",
];
const LAST: [&str; 8] = ["P", "K", "M", "R", "S", "V", "N", "B"];
const HOURS: [(usize, f64); 12] =
    [(11, 2.0), (12, 8.0), (13, 12.0), (14, 7.0), (15, 2.0), (16, 2.0), (17, 3.0), (18, 5.0), (19, 10.0), (20, 14.0), (21, 12.0), (22, 6.0)];
const DAY_BASE: [f64; 7] = [48.0, 50.0, 52.0, 54.0, 66.0, 80.0, 84.0];

fn hhmm(day: &str, min: i64) -> String {
    let min = min.clamp(0, 1439);
    format!("{day}T{:02}:{:02}", min / 60, min % 60)
}

/// One day of a shop's life as events in time order. `scale` multiplies demand (rain, a match
/// on TV); `direct_share` overrides how much comes by WhatsApp (0.0 keeps the default).
pub fn day_events(p: &Profile, day: &str, seed: u64, scale: f64, direct_share: f64) -> Vec<String> {
    let mut r = Rng::new(seed ^ (day_number(day).unwrap_or(0) as u64).wrapping_mul(0xA24B_AED4_963E_E407));
    let dn = day_number(day).unwrap_or(0);
    let n = (DAY_BASE[weekday(day)] * scale * (0.9 + 0.2 * r.unit())).round() as usize;
    let near: Vec<&crate::profile::Locality> = p.localities.iter().filter(|l| l.km <= p.max_delivery_km).collect();
    let menu_w: Vec<f64> = p.menu.iter().map(|m| m.popularity).collect();
    let hour_w: Vec<f64> = HOURS.iter().map(|h| h.1).collect();
    let direct = if direct_share > 0.0 { direct_share } else { 0.22 };
    let mut chan_w = vec![direct];
    for (i, _) in p.platforms.iter().enumerate() {
        chan_w.push((1.0 - direct) * if i == 0 { 0.54 } else { 0.46 / (p.platforms.len() - 1).max(1) as f64 });
    }
    let mut ev: Vec<(i64, usize, Value)> = Vec::new();
    let push = |ev: &mut Vec<(i64, usize, Value)>, min: i64, mut v: Value| {
        v["at"] = json!(hhmm(day, min));
        let k = ev.len();
        ev.push((min, k, v));
    };
    let mut load: BTreeMap<String, i64> = BTreeMap::new();
    let mut usage: BTreeMap<String, f64> = BTreeMap::new();
    for k in 0..n {
        let ch = r.pick(&chan_w);
        let is_direct = ch == 0 || p.platforms.is_empty();
        let channel = if is_direct { "whatsapp".to_string() } else { p.platforms[ch - 1].name.to_lowercase() };
        let t = (HOURS[r.pick(&hour_w)].0 * 60 + r.below(60)) as i64;
        let mut items: Vec<ItemIn> = Vec::new();
        for _ in 0..(1 + r.pick(&[0.5, 0.35, 0.15])) {
            let m = &p.menu[r.pick(&menu_w)];
            if !items.iter().any(|i| i.code == m.code) {
                items.push(ItemIn { code: m.code.clone(), name: String::new(), qty: if r.unit() < 0.8 { 1 } else { 2 } });
            }
        }
        let loc = if is_direct && !near.is_empty() { near[r.below(near.len())] } else { &p.localities[r.below(p.localities.len().max(1)).min(p.localities.len().saturating_sub(1))] };
        let id = match channel.as_str() {
            "whatsapp" => format!("W-{}", (dn - 20000) * 100 + k as i64),
            c => format!("{}-{}", c[..2].to_uppercase(), (dn - 20000) * 1000 + k as i64),
        };
        let cancel = r.unit() < if is_direct { 0.02 } else { 0.04 };
        let mut o = json!({"t": "order", "id": id, "channel": channel, "items": items, "locality": loc.name, "km": loc.km});
        if is_direct {
            let who = (120.0 * r.unit() * r.unit()) as usize; // a few regulars order most
            o["customer"] = serde_json::to_value(CustomerIn {
                name: format!("{} {}", FIRST[who % 16], LAST[(who / 16) % 8]),
                phone: format!("+91 90000 {:05}", 10000 + who * 37),
                address: format!("Flat {}, {} Residency, {}", 101 + who % 40, FIRST[(who * 7) % 16], loc.name),
            })
            .unwrap();
            o["payment"] = json!(if r.unit() < 0.35 { "cod" } else { "upi" });
        } else if r.unit() < 0.2 {
            let total: f64 = items.iter().map(|i| p.item(&i.code).map(|m| m.price).unwrap_or(0.0) * i.qty as f64).sum();
            o["discount"] = json!((total * 0.1).round());
        }
        push(&mut ev, t, o);
        if cancel {
            let by = ["customer", "customer", "platform", "restaurant"][r.below(4)];
            let by = if is_direct { "customer" } else { by };
            if r.unit() < 0.6 {
                push(&mut ev, t + 2, json!({"t": "status", "id": id, "status": "preparing"}));
            }
            push(&mut ev, t + 9, json!({"t": "status", "id": id, "status": "cancelled", "by": by}));
            continue;
        }
        for i in &items {
            if let Some(m) = p.item(&i.code) {
                for (ing, per) in &m.recipe {
                    *usage.entry(ing.clone()).or_default() += per * i.qty as f64;
                }
            }
        }
        push(&mut ev, t + 2, json!({"t": "status", "id": id, "status": "preparing"}));
        if is_direct && !p.riders.is_empty() {
            // the rider who is free soonest
            let rider = p.riders.iter().min_by_key(|x| load.get(&x.id).copied().unwrap_or(0)).unwrap();
            let start = (t + 14).max(load.get(&rider.id).copied().unwrap_or(0));
            let ride = (loc.km * 4.0).ceil() as i64 + 3;
            load.insert(rider.id.clone(), start + ride + (loc.km * 3.0) as i64);
            push(&mut ev, start - 2, json!({"t": "assign", "id": id, "rider": rider.id}));
            push(&mut ev, start, json!({"t": "status", "id": id, "status": "out"}));
            for step in 1..=2 {
                let f = step as f64 / 3.0;
                push(
                    &mut ev,
                    start + (ride as f64 * f) as i64,
                    json!({"t": "gps", "rider": rider.id, "lat": p.lat + (loc.lat - p.lat) * f, "lon": p.lon + (loc.lon - p.lon) * f}),
                );
            }
            push(&mut ev, start + ride, json!({"t": "status", "id": id, "status": "delivered"}));
        } else {
            push(&mut ev, t + 18, json!({"t": "status", "id": id, "status": "out"}));
            push(&mut ev, t + 33 + (loc.km * 2.0) as i64, json!({"t": "status", "id": id, "status": "delivered"}));
        }
    }
    // the morning's buying, one line per supplier, for about what the day will use
    let mut by_supplier: BTreeMap<String, (Vec<String>, f64, f64)> = BTreeMap::new();
    for ing in &p.ingredients {
        let used = usage.get(&ing.name).copied().unwrap_or(0.0);
        if used <= 0.0 {
            continue;
        }
        let cost = (used / ing.pack).ceil() * ing.pack_price;
        let e = by_supplier.entry(if ing.supplier.is_empty() { "Local market".into() } else { ing.supplier.clone() }).or_default();
        e.0.push(ing.name.clone());
        e.1 += cost;
        e.2 += cost * ing.gst_pct / 100.0;
    }
    for (i, (sup, (names, cost, gst))) in by_supplier.into_iter().enumerate() {
        push(&mut ev, 9 * 60 + 10 + i as i64 * 7, json!({"t": "purchase", "supplier": sup, "item": names.join(", "), "amount": cost.round(), "gst": gst.round()}));
    }
    ev.sort_by_key(|e| (e.0, e.1));
    ev.into_iter().map(|e| e.2.to_string()).collect()
}

/// What each platform pays for the days `from..=to`, as it would really arrive: it leaves out
/// orders cancelled after the food was made, and `extra_cut` rupees it was not entitled to.
pub fn payout_event(b: &Business, platform: &str, from: &str, to: &str, at: &str, extra_cut: f64) -> String {
    let paid: Paise = b
        .book
        .orders
        .iter()
        .filter(|o| o.channel == platform && o.status == "delivered" && day_of(&o.at) >= from && day_of(&o.at) <= to)
        .map(|o| b.values(o).payout)
        .sum();
    json!({"t": "payout", "platform": platform, "from": from, "to": to, "at": at,
        "amount": (paid as f64 / 100.0 - extra_cut).max(0.0),
        "reference": format!("{}{}", platform[..2].to_uppercase(), to.replace('-', ""))})
    .to_string()
}
