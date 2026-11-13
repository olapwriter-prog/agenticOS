//! Tomorrow's plan from the book: how many of each dish to expect, what to buy for them, when
//! the rush comes, and what today's numbers say to change. Plain arithmetic, explained in the
//! answer, so the owner can check it.
use crate::util::*;
use crate::Business;
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct ItemPlan {
    pub code: String,
    pub name: String,
    pub today: u32,
    pub forecast: u32,
}
#[derive(Debug, Clone)]
pub struct Buy {
    pub name: String,
    pub unit: String,
    pub need: f64,
    pub packs: u32,
    pub cost: Paise,
    pub gst: Paise,
}
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub for_day: String,
    pub basis: String,
    pub items: Vec<ItemPlan>,
    pub buy: Vec<Buy>,
    /// (hour, expected orders), busiest first.
    pub peak: Vec<(usize, f64)>,
    pub riders: u32,
    pub notes: Vec<String>,
}

/// 0.5 x the same weekday (last four), 0.3 x the last seven days, 0.2 x today; parts with no
/// data drop out and the rest are re-weighted.
fn blend(same: Option<f64>, week: Option<f64>, today: Option<f64>) -> f64 {
    let parts = [(same, 0.5), (week, 0.3), (today, 0.2)];
    let w: f64 = parts.iter().filter(|(v, _)| v.is_some()).map(|(_, w)| w).sum();
    if w == 0.0 {
        return 0.0;
    }
    parts.iter().filter_map(|(v, wt)| v.map(|v| v * wt)).sum::<f64>() / w
}

pub fn plan(b: &Business) -> Plan {
    let today = b.today();
    let tomorrow = add_days(&today, 1);
    // per day: dish quantities, orders per hour, direct orders per hour (delivered or still open)
    let mut qty: BTreeMap<String, BTreeMap<String, u32>> = BTreeMap::new();
    let mut hours: BTreeMap<String, [u32; 24]> = BTreeMap::new();
    let mut direct_hours: BTreeMap<String, [u32; 24]> = BTreeMap::new();
    // the forecast looks back four weeks: older orders are not read
    let since = add_days(&today, -29);
    for o in &b.book.orders {
        if o.status == "cancelled" || day_of(&o.at) < since.as_str() {
            continue;
        }
        let d = day_of(&o.at).to_string();
        let q = qty.entry(d.clone()).or_default();
        for l in &o.lines {
            *q.entry(l.code.clone()).or_default() += l.qty;
        }
        hours.entry(d.clone()).or_insert([0; 24])[hour_of(&o.at)] += 1;
        if b.is_direct(o) {
            direct_hours.entry(d).or_insert([0; 24])[hour_of(&o.at)] += 1;
        }
    }
    let same_days: Vec<String> = (1..=4).map(|w| add_days(&tomorrow, -7 * w)).filter(|d| qty.contains_key(d)).collect();
    let week_days: Vec<String> = (0..7).map(|i| add_days(&today, -i)).filter(|d| qty.contains_key(d)).collect();
    let mean = |days: &[String], f: &dyn Fn(&String) -> f64| -> Option<f64> {
        (!days.is_empty()).then(|| days.iter().map(f).sum::<f64>() / days.len() as f64)
    };
    let has_today = qty.contains_key(&today);
    let mut p = Plan {
        for_day: tomorrow.clone(),
        basis: format!(
            "{} past {}s, the last {} days, and today",
            same_days.len(),
            WEEKDAYS[weekday(&tomorrow)],
            week_days.len()
        ),
        ..Plan::default()
    };
    let mut need: BTreeMap<String, f64> = BTreeMap::new();
    for m in &b.profile.menu {
        let get = |d: &String| qty.get(d).and_then(|q| q.get(&m.code)).copied().unwrap_or(0) as f64;
        let f = blend(mean(&same_days, &get), mean(&week_days, &get), has_today.then(|| get(&today)));
        let forecast = (f - 1e-9).ceil().max(0.0) as u32;
        for (ing, per) in &m.recipe {
            *need.entry(ing.clone()).or_default() += per * forecast as f64;
        }
        p.items.push(ItemPlan { code: m.code.clone(), name: m.name.clone(), today: get(&today) as u32, forecast });
    }
    p.items.sort_by(|a, b| b.forecast.cmp(&a.forecast).then(a.code.cmp(&b.code)));
    for ing in &b.profile.ingredients {
        let n = need.get(&ing.name).copied().unwrap_or(0.0) * (1.0 + b.profile.buffer_pct / 100.0);
        if n <= 0.0 {
            continue;
        }
        let packs = (n / ing.pack - 1e-9).ceil().max(1.0) as u32;
        let cost = rs(ing.pack_price) * packs as Paise;
        p.buy.push(Buy { name: ing.name.clone(), unit: ing.unit.clone(), need: n, packs, cost, gst: pct(cost, ing.gst_pct) });
    }
    // the rush
    let hour_mean = |src: &BTreeMap<String, [u32; 24]>, h: usize| -> f64 {
        let f = |d: &String| src.get(d).map(|a| a[h]).unwrap_or(0) as f64;
        blend(mean(&same_days, &f), mean(&week_days, &f), has_today.then(|| f(&today)))
    };
    let mut peak: Vec<(usize, f64)> = (0..24).map(|h| (h, hour_mean(&hours, h))).filter(|x| x.1 > 0.0).collect();
    peak.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(&b.0)));
    peak.truncate(3);
    p.peak = peak;
    let direct_peak = (0..24).map(|h| hour_mean(&direct_hours, h)).fold(0.0, f64::max);
    // a rider does about three drops an hour inside the delivery radius
    p.riders = ((direct_peak / 3.0).ceil() as u32).max(1);

    // what today says
    let s = b.day_stats(&today);
    let total_f: u32 = p.items.iter().map(|i| i.forecast).sum();
    let total_t: u32 = p.items.iter().map(|i| i.today).sum();
    if total_t > 0 {
        let ch = (total_f as f64 / total_t as f64 - 1.0) * 100.0;
        p.notes.push(format!(
            "Expect about {total_f} plates on {} ({:+.0}% against today's {total_t}).",
            WEEKDAYS[weekday(&tomorrow)],
            ch
        ));
    }
    if p.riders as usize > b.profile.riders.len() {
        p.notes.push(format!(
            "The direct-order rush needs {} riders and you have {}: call one in or stop taking WhatsApp orders at the peak.",
            p.riders,
            b.profile.riders.len()
        ));
    }
    let per100: Vec<String> = s
        .channels
        .iter()
        .filter(|(_, c)| c.sales > 0)
        .map(|(n, c)| format!("{n} ₹{:.0}", c.net as f64 * 100.0 / c.sales as f64))
        .collect();
    if per100.len() > 1 {
        p.notes.push(format!("Of every ₹100 sold, what reaches you: {}. Move regulars to the channel that keeps most.", per100.join(", ")));
    }
    for (n, c) in &s.channels {
        if c.orders >= 5 && c.cancelled * 100 / c.orders >= 8 {
            p.notes.push(format!("{n}: {} of {} orders cancelled today; check prep time and stock before the rush.", c.cancelled, c.orders));
        }
    }
    let slow: Vec<&str> = b
        .profile
        .menu
        .iter()
        .filter(|m| week_days.len() >= 5 && week_days.iter().all(|d| qty.get(d).and_then(|q| q.get(&m.code)).is_none()))
        .map(|m| m.name.as_str())
        .collect();
    if !slow.is_empty() {
        p.notes.push(format!("Not sold in a week: {}. Promote or drop.", slow.join(", ")));
    }
    let repeat = b
        .book
        .orders
        .iter()
        .filter(|o| day_of(&o.at) == today && b.is_direct(o))
        .filter(|o| o.customer.as_ref().and_then(|t| b.book.customers.get(t)).map(|c| c.orders > 1).unwrap_or(false))
        .count();
    let direct_n = s.channels.iter().filter(|(n, _)| b.profile.platform(n).is_none()).map(|(_, c)| c.orders).sum::<u32>();
    if direct_n > 0 {
        p.notes.push(format!("{repeat} of {direct_n} direct orders today were from returning customers."));
    }
    for pf in &b.profile.platforms {
        let (can, _) = b.claimable(&pf.name.to_lowercase());
        if can > 0 {
            p.notes.push(format!("{} is short by {} in past payouts: file the claim.", pf.name, inr(can)));
        }
    }
    p
}

impl Plan {
    pub fn text(&self) -> String {
        let mut out = vec![format!("Plan for {} {} (from {})", WEEKDAYS[weekday(&self.for_day)], self.for_day, self.basis)];
        out.push("Prepare for:".into());
        for i in self.items.iter().filter(|i| i.forecast > 0) {
            out.push(format!("  {:<30} {:>3}  (today {})", i.name, i.forecast, i.today));
        }
        out.push(format!("Buy (with {}):", "the buffer"));
        let (mut cost, mut gst) = (0, 0);
        for b in &self.buy {
            cost += b.cost;
            gst += b.gst;
            out.push(format!("  {:<18} {:>8.1} {:<3} = {:>2} packs  {}", b.name, b.need, b.unit, b.packs, inr(b.cost)));
        }
        out.push(format!("  total {} + GST {}", inr(cost), inr(gst)));
        out.push(format!(
            "Rush: {}. Riders needed: {}.",
            self.peak.iter().map(|(h, n)| format!("{h}:00 (~{n:.0} orders)")).collect::<Vec<_>>().join(", "),
            self.riders
        ));
        for n in &self.notes {
            out.push(format!("- {n}"));
        }
        out.join("\n")
    }
    pub fn json(&self) -> Value {
        json!({
            "for_day": self.for_day, "weekday": WEEKDAYS[weekday(&self.for_day)], "basis": self.basis,
            "items": self.items.iter().map(|i| json!({"code": i.code, "name": i.name, "today": i.today, "forecast": i.forecast})).collect::<Vec<_>>(),
            "buy": self.buy.iter().map(|b| json!({"name": b.name, "unit": b.unit, "need": b.need, "packs": b.packs, "cost": b.cost, "gst": b.gst})).collect::<Vec<_>>(),
            "peak": self.peak.iter().map(|(h, n)| json!({"hour": h, "orders": n})).collect::<Vec<_>>(),
            "riders": self.riders, "notes": self.notes,
        })
    }
}
