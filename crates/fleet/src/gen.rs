//! Events as lines of JSON, and a seeded generator of a fleet's days with problems planted at
//! known places, so the checks can be scored: every planted problem must be found, and nothing
//! else reported. The same events are what a platform's trip feed, a driver's app and the
//! owner would deliver on a real phone:
//!   {"t":"trip", ...}  {"t":"shift", ...}  {"t":"fill", ...}
//!   {"t":"cash","driver","at","amount"}  {"t":"expense","car","at","amount","note"}
//!   {"t":"payout","platform","from","to","amount","reference"}
//! Amounts in cash, expense and payout events are rupees; a trip's and a fill's are paise.
//! The drivers are invented, and no rider appears at all.
use crate::{Fill, Fleet, Payout, Profile, Shift, Trip};
use agentos_business::dataset::Rng;
use agentos_business::util::{day_number, rs, weekday, Paise};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Event {
    Trip(Trip),
    Shift(Shift),
    Fill(Fill),
    Cash { driver: String, at: String, amount: f64 },
    Expense { car: String, at: String, amount: f64, note: String },
    Payout { platform: String, from: String, to: String, amount: f64, reference: String },
}

/// Apply one event. The answer is what the phone would tell the owner.
pub fn play(f: &mut Fleet, e: &Event) -> Result<String, String> {
    match e.clone() {
        Event::Trip(t) => f.add_trip(t),
        Event::Shift(s) => f.add_shift(s),
        Event::Fill(x) => f.add_fill(x),
        Event::Cash { driver, at, amount } => f.cash_in(&driver, rs(amount), &at),
        Event::Expense { car, at, amount, note } => f.add_expense(&car, &at, rs(amount), &note),
        Event::Payout { platform, from, to, amount, reference } => f.add_payout(Payout { platform, from, to, amount: rs(amount), reference }),
    }
}
/// Apply a file of events (one JSON object a line). Returns (applied, what was refused).
pub fn play_text(f: &mut Fleet, text: &str) -> (usize, Vec<String>) {
    let (mut ok, mut bad) = (0, Vec::new());
    for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        match serde_json::from_str::<Event>(line).map_err(|e| format!("line {}: not an event: {e}", i + 1)).and_then(|e| play(f, &e)) {
            Ok(_) => ok += 1,
            Err(e) => bad.push(e),
        }
    }
    (ok, bad)
}

/// A planted problem: the kind of check that must fire, and for whom (a driver or a car).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Truth {
    pub kind: &'static str,
    pub who: String,
}
/// The checks the generator plants problems for. The others (papers, service, a short payout)
/// follow from the profile and the payouts, not from a day's driving.
pub const PLANTED: [&str; 6] = ["cash_held", "personal_km", "fuel_mismatch", "overtime", "cancels", "idle"];

pub struct Gen {
    rng: Rng,
    /// The first day generated: planted problems are placed by the day's distance from it.
    pub day0: String,
    odo: Vec<f64>,
    /// Fuel or charge left in each car (kg or kWh).
    level: Vec<f64>,
    /// Whether the car has had a CNG fill (the first one has nothing to be compared with).
    filled: Vec<bool>,
    trip_no: u64,
}
const ZONES: [&str; 10] = ["Koramangala", "Indiranagar", "Whitefield", "Electronic City", "Majestic", "Jayanagar", "Hebbal", "Marathahalli", "HSR Layout", "Yeshwanthpur"];

impl Gen {
    pub fn new(p: &Profile, day0: &str, seed: u64) -> Gen {
        Gen {
            rng: Rng::new(seed),
            day0: day0.to_string(),
            odo: p.cars.iter().map(|c| c.odometer).collect(),
            level: p.cars.iter().map(|c| c.capacity).collect(),
            filled: vec![false; p.cars.len()],
            trip_no: 0,
        }
    }
    /// Carry on after a restart: the odometers and the trip count come from the book. Tanks
    /// and batteries are taken as full (the book does not hold them).
    pub fn resume(f: &Fleet, seed: u64) -> Option<Gen> {
        let day0 = f.days.keys().next()?.clone();
        let last = f.days.keys().next_back()?.clone();
        let mut g = Gen::new(&f.profile, &day0, seed ^ f.days.len() as u64);
        for (i, c) in f.profile.cars.iter().enumerate() {
            g.odo[i] = f.odometer(&c.id, &last).0;
            // a CNG tank holds what it did at its last fill less the distance since; an
            // electric car was charged to full at the depot overnight
            let last_fill = f.days.values().rev().flat_map(|d| d.fills.iter().rev()).find(|x| x.car == c.id && x.kind == "cng").map(|x| x.odometer);
            g.filled[i] = last_fill.is_some();
            if let Some(at) = last_fill {
                g.level[i] = (c.capacity - (g.odo[i] - at) / c.efficiency).max(0.0);
            }
        }
        g.trip_no = f.days.values().map(|d| d.trips.len() as u64).sum();
        Some(g)
    }

    /// One day of the whole fleet, as events in time order, and what was planted in it.
    pub fn day(&mut self, p: &Profile, day: &str) -> (Vec<Event>, Vec<Truth>) {
        let n = day_number(day).unwrap_or(0) - day_number(&self.day0).unwrap_or(0);
        let wd = weekday(day);
        let rain = n == 7 || n == 20;
        let mut ev: Vec<(i64, usize, Event)> = Vec::new();
        let mut truth = Vec::new();
        let at = |min: i64| format!("{day}T{:02}:{:02}", (min / 60).min(23), if min >= 1440 { 59 } else { min % 60 });
        let regular = p.cars.len().min(p.drivers.len());
        for ci in 0..p.cars.len() {
            let car = p.cars[ci].clone();
            // ---- planted: this car is in the workshop, or simply does not go out
            if ci == 11 && (n == 12 || n == 13) {
                let k = ev.len();
                ev.push((9 * 60, k, Event::Expense { car: car.id.clone(), at: at(9 * 60), amount: if n == 12 { 8500.0 } else { 1200.0 }, note: if n == 12 { "repair: clutch".into() } else { "repair: parts".into() } }));
                continue;
            }
            if ci == 7 && n == 20 {
                truth.push(Truth { kind: "idle", who: car.id.clone() });
                continue;
            }
            // ---- who drives: the car's own driver, or a relief driver on their day off
            let off = ci < regular && wd == ci % 7;
            let di = if off { regular + ci / 7 } else { ci };
            let Some(driver) = p.drivers.get(di).or_else(|| p.drivers.get(ci)).cloned() else { continue };
            let di = p.drivers.iter().position(|d| d.id == driver.id).unwrap_or(ci);
            let long = di == 2 && [5, 12, 19].contains(&n);
            let cancels = di == 10 && (10..=16).contains(&n);
            let inflate = ci == 3 && (10..=16).contains(&n);
            let start = 6 * 60 + self.rng.below(90) as i64;
            let end = start + if long { 14 * 60 } else { 600 + self.rng.below(90) as i64 };
            let (odo_start, mut t, mut cash, mut offers, mut cancelled) = (self.odo[ci], start, 0 as Paise, 0u32, 0u32);
            let push = |ev: &mut Vec<(i64, usize, Event)>, min: i64, e: Event| {
                let k = ev.len();
                ev.push((min, k, e));
            };
            loop {
                let hour = t / 60;
                let peak = (8..11).contains(&hour) || (17..21).contains(&hour);
                let wait = if rain { 2 + self.rng.below(6) } else if peak { 3 + self.rng.below(8) } else { 5 + self.rng.below(14) } as i64;
                let dead = 0.8 + 2.7 * self.rng.unit();
                let airport = self.rng.unit() < 0.10;
                let km = if airport { 34.0 + 6.0 * self.rng.unit() } else { 2.5 + 12.0 * self.rng.unit() * self.rng.unit() };
                let mins = (km * 3.1 * (0.85 + 0.4 * self.rng.unit()) * if rain { 1.2 } else { 1.0 }).round();
                if t + wait + (dead * 3.0) as i64 + mins as i64 > end {
                    break;
                }
                t += wait + (dead * 3.0) as i64;
                self.trip_no += 1;
                offers += 1;
                let uber = self.rng.unit() < if peak { 0.6 } else { 0.4 };
                let pf = if uber { p.platforms[0].name.clone() } else { p.platforms.last().unwrap().name.clone() };
                let (from, to) = (ZONES[self.rng.below(10)], if airport { "Airport" } else { ZONES[self.rng.below(10)] });
                let mut trip = Trip {
                    id: format!("T{:06}", self.trip_no),
                    car: car.id.clone(),
                    driver: driver.id.clone(),
                    platform: pf,
                    at: at(t),
                    dead_km: (dead * 10.0).round() / 10.0,
                    from: from.into(),
                    to: to.into(),
                    ..Trip::default()
                };
                self.odo[ci] += trip.dead_km;
                self.level[ci] -= trip.dead_km / car.efficiency;
                // a driver who cancels: every fourth offer when planted, else once a day at most
                let by_driver = if cancels { offers % 4 == 0 } else { cancelled == 0 && self.rng.unit() < 0.02 };
                if by_driver || self.rng.unit() < 0.04 {
                    if by_driver {
                        cancelled += 1;
                    }
                    trip.cancelled = Some(if by_driver { "driver" } else { "rider" }.into());
                    push(&mut ev, t, Event::Trip(trip));
                    continue;
                }
                trip.km = (km * 10.0).round() / 10.0;
                trip.mins = mins;
                let surge = if !uber { if rain { 1.1 } else { 1.0 } } else if rain { 1.3 + 0.4 * self.rng.unit() } else if peak { 1.0 + 0.4 * self.rng.unit() } else { 1.0 };
                let base = if uber { 45.0 + 12.5 * trip.km + 1.3 * mins } else { 35.0 + 19.0 * trip.km };
                trip.fare = rs((base * surge).round());
                trip.toll = if airport { rs(120.0) } else { 0 };
                trip.tip = if self.rng.unit() < 0.06 { rs(10.0 + 10.0 * self.rng.below(4) as f64) } else { 0 };
                trip.pay = if self.rng.unit() < if uber { 0.30 } else { 0.45 } { "cash" } else { "online" }.into();
                if trip.pay == "cash" {
                    cash += trip.fare + trip.toll + trip.tip;
                }
                self.odo[ci] += trip.km;
                self.level[ci] -= trip.km / (car.efficiency * (0.95 + 0.10 * self.rng.unit()));
                push(&mut ev, t, Event::Trip(trip));
                t += mins as i64;
                // ---- energy: a public fast charge when the battery runs low, CNG filled to full
                if car.fuel == "ev" && self.level[ci] < 0.18 * car.capacity && t + 45 < end {
                    let qty = 0.8 * car.capacity - self.level[ci];
                    push(&mut ev, t, Event::Fill(Fill { car: car.id.clone(), driver: driver.id.clone(), at: at(t), kind: "public".into(), qty: (qty * 10.0).round() / 10.0, amount: rs((qty * p.energy.public_per_kwh).round()), odometer: self.odo[ci] }));
                    self.level[ci] = 0.8 * car.capacity;
                    t += 35;
                } else if car.fuel == "cng" && self.level[ci] < 0.15 * car.capacity {
                    let mut qty = car.capacity - self.level[ci];
                    if inflate {
                        // planted: the bill shows more gas than the tank took
                        qty *= 1.35;
                        if self.filled[ci] {
                            truth.push(Truth { kind: "fuel_mismatch", who: car.id.clone() });
                        }
                    }
                    push(&mut ev, t, Event::Fill(Fill { car: car.id.clone(), driver: driver.id.clone(), at: at(t), kind: "cng".into(), qty: (qty * 100.0).round() / 100.0, amount: rs((qty * p.energy.cng_per_kg).round()), odometer: self.odo[ci] }));
                    self.level[ci] = car.capacity;
                    self.filled[ci] = true;
                    t += 8;
                }
            }
            // ---- the shift ends: odometer, the depot charger, riders' cash handed in
            if di == 6 && [4, 9, 15, 22].contains(&n) {
                // planted: the car is used after the last trip
                let extra = 25.0 + 35.0 * self.rng.unit();
                self.odo[ci] += extra.round();
                self.level[ci] -= extra / car.efficiency;
                truth.push(Truth { kind: "personal_km", who: car.id.clone() });
            }
            // an odometer shows tenths of a kilometre
            self.odo[ci] = (self.odo[ci] * 10.0).round() / 10.0;
            let done = if long { end } else { t.max(start + 60) };
            if long {
                truth.push(Truth { kind: "overtime", who: driver.id.clone() });
            }
            if cancels && offers >= 8 {
                truth.push(Truth { kind: "cancels", who: driver.id.clone() });
            }
            push(&mut ev, done, Event::Shift(Shift { car: car.id.clone(), driver: driver.id.clone(), start: at(start), end: at(done), odo_start, odo_end: self.odo[ci] }));
            if car.fuel == "ev" {
                let qty = car.capacity - self.level[ci].max(0.0);
                push(&mut ev, done + 20, Event::Fill(Fill { car: car.id.clone(), driver: driver.id.clone(), at: at(done + 20), kind: "depot".into(), qty: (qty * 10.0).round() / 10.0, amount: rs((qty * p.energy.depot_per_kwh).round()), odometer: self.odo[ci] }));
                self.level[ci] = car.capacity;
            }
            let mut hand_in = cash;
            if self.rng.unit() < 0.03 && cash > rs(700.0) {
                // planted: some of the riders' cash is kept back
                hand_in -= rs(300.0 + 100.0 * self.rng.below(4) as f64);
                truth.push(Truth { kind: "cash_held", who: driver.id.clone() });
            }
            if hand_in > 0 {
                push(&mut ev, done + 10, Event::Cash { driver: driver.id.clone(), at: at(done + 10), amount: hand_in as f64 / 100.0 });
            }
        }
        ev.sort_by_key(|e| (e.0, e.1));
        (ev.into_iter().map(|e| e.2).collect(), truth)
    }
}
