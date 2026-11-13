//! A taxi fleet run from the phone. Trips arrive from the ride platforms, fuel and charging
//! bills from the drivers, odometer readings at the start and end of each shift. From them,
//! by arithmetic (no model): each driver's sheet (earnings, riders' cash held), what each
//! platform owes, what each car cost and earned, the checks an owner would make by hand, and
//! tomorrow's plan.
//!
//! Privacy: the platforms do not give the fleet a rider's name or number, and none is kept;
//! a trip is two areas of the city, a distance and a fare. A driver's position is not tracked:
//! the book holds shift times and odometer readings. The book is sealed with a key from the vault.
pub mod gen;
pub mod report;
pub mod tools;

use agentos_business::util::{day_of, pct, rs, seal, unseal, Paise};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

// ---------------------------------------------------------------------------- the fleet's profile
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub city: String,
    /// Chargers at the depot (one car each overnight).
    pub depot_chargers: u32,
    pub cars: Vec<Car>,
    pub drivers: Vec<Driver>,
    pub platforms: Vec<Platform>,
    pub energy: Energy,
    pub rules: Rules,
}
impl Default for Profile {
    fn default() -> Self {
        Self { name: "My fleet".into(), city: String::new(), depot_chargers: 0, cars: vec![], drivers: vec![], platforms: vec![], energy: Energy::default(), rules: Rules::default() }
    }
}
#[derive(Debug, Clone, Deserialize)]
pub struct Car {
    pub id: String,
    pub model: String,
    /// "cng" or "ev".
    pub fuel: String,
    /// Ex-showroom price, rupees.
    pub price: f64,
    pub emi_month: f64,
    /// Insurance, permit and road tax for a year, rupees.
    pub fixed_year: f64,
    pub maint_per_km: f64,
    /// Kilometres per kg of CNG, or per kWh.
    pub efficiency: f64,
    /// Tank in kg, or battery in kWh.
    pub capacity: f64,
    pub service_every_km: f64,
    #[serde(default)]
    pub odometer: f64,
    #[serde(default)]
    pub last_service_km: f64,
    #[serde(default)]
    pub insurance_until: String,
    #[serde(default)]
    pub permit_until: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct Driver {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub phone: String,
    /// The car this driver usually takes.
    pub car: String,
    #[serde(default)]
    pub licence_until: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct Platform {
    pub name: String,
    #[serde(default)]
    pub commission_pct: f64,
    #[serde(default)]
    pub commission_gst_pct: f64,
    #[serde(default)]
    pub tds_pct: f64,
    /// A flat fee per driver per day worked on it, rupees (a subscription platform).
    #[serde(default)]
    pub daily_fee: f64,
    /// True: the platform collects online fares and pays the fleet later. False: the rider
    /// pays the driver or the fleet's own QR directly.
    #[serde(default)]
    pub settles: bool,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Energy {
    pub cng_per_kg: f64,
    pub depot_per_kwh: f64,
    pub public_per_kwh: f64,
}
impl Default for Energy {
    fn default() -> Self {
        Self { cng_per_kg: 89.0, depot_per_kwh: 9.0, public_per_kwh: 21.0 }
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Rules {
    /// The driver's share of the fares after the platform's cut, percent.
    pub driver_share_pct: f64,
    pub max_shift_hours: f64,
    /// Riders' cash a driver may still hold after the day's deposit, rupees.
    pub cash_tolerance: f64,
    /// Kilometres on the odometer beyond trips and the runs between them, per shift.
    pub personal_km_tolerance: f64,
    /// A fill that gives less than this share of the car's rated distance is questioned.
    pub fuel_floor_pct: f64,
    /// A driver cancelling more than this share of trips (of at least eight) is flagged.
    pub cancel_pct: f64,
    /// Papers expiring within this many days are flagged.
    pub papers_days: i64,
}
impl Default for Rules {
    fn default() -> Self {
        Self { driver_share_pct: 35.0, max_shift_hours: 12.0, cash_tolerance: 100.0, personal_km_tolerance: 10.0, fuel_floor_pct: 80.0, cancel_pct: 20.0, papers_days: 15 }
    }
}
impl Profile {
    pub fn parse(text: &str) -> Result<Profile, String> {
        let p: Profile = toml::from_str(text).map_err(|e| format!("fleet profile: {e}"))?;
        if p.cars.is_empty() || p.platforms.is_empty() {
            return Err("fleet profile: needs [[cars]] and [[platforms]]".into());
        }
        for d in &p.drivers {
            if p.car(&d.car).is_none() {
                return Err(format!("fleet profile: driver {} is on car {}, which is not in [[cars]]", d.name, d.car));
            }
        }
        Ok(p)
    }
    pub fn load(path: &std::path::Path) -> Result<Profile, String> {
        Self::parse(&std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?)
    }
    pub fn car(&self, id: &str) -> Option<&Car> {
        self.cars.iter().find(|c| c.id.eq_ignore_ascii_case(id.trim()))
    }
    pub fn driver(&self, id: &str) -> Option<&Driver> {
        let id = id.trim();
        self.drivers.iter().find(|d| d.id.eq_ignore_ascii_case(id) || d.name.eq_ignore_ascii_case(id))
    }
    pub fn platform(&self, name: &str) -> Option<&Platform> {
        self.platforms.iter().find(|p| p.name.eq_ignore_ascii_case(name.trim()))
    }
}

// ---------------------------------------------------------------------------- what happens
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Trip {
    pub id: String,
    pub car: String,
    pub driver: String,
    pub platform: String,
    pub at: String,
    pub mins: f64,
    pub km: f64,
    /// Kilometres driven empty to reach the rider.
    pub dead_km: f64,
    pub fare: Paise,
    pub toll: Paise,
    pub tip: Paise,
    /// "cash" (the driver holds it) or "online".
    pub pay: String,
    pub from: String,
    pub to: String,
    /// "rider" or "driver" if the trip was cancelled (no fare).
    pub cancelled: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Shift {
    pub car: String,
    pub driver: String,
    pub start: String,
    pub end: String,
    pub odo_start: f64,
    pub odo_end: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Fill {
    pub car: String,
    pub driver: String,
    pub at: String,
    /// "cng", "depot" or "public".
    pub kind: String,
    /// kg or kWh.
    pub qty: f64,
    pub amount: Paise,
    pub odometer: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Money {
    pub who: String,
    pub at: String,
    pub amount: Paise,
    pub note: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Payout {
    pub platform: String,
    pub from: String,
    pub to: String,
    pub amount: Paise,
    pub reference: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Day {
    pub trips: Vec<Trip>,
    pub shifts: Vec<Shift>,
    pub fills: Vec<Fill>,
    /// Riders' cash handed in by a driver.
    pub cash_in: Vec<Money>,
    /// Earnings handed to a driver.
    pub paid: Vec<Money>,
    /// Repairs, service, tyres, fines: `who` is the car, `note` says what ("service", "repair", "fine").
    pub expenses: Vec<Money>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct Meta {
    now: String,
    payouts: Vec<Payout>,
    claims: Vec<(String, String, Paise, String)>,
}

/// The money in one trip.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TripMoney {
    /// The platform's cut with its GST, and tax deducted.
    pub cut: Paise,
    /// Fare less the cut: what the trip brings the fleet (before the driver's share).
    pub net: Paise,
    /// What the driver holds in riders' cash from this trip.
    pub cash: Paise,
    /// What the platform owes the fleet for it (negative: the fleet owes the platform its cut
    /// on a cash trip).
    pub platform_owes: Paise,
}

pub struct Fleet {
    pub profile: Profile,
    dir: PathBuf,
    key: [u8; 32],
    pub days: BTreeMap<String, Day>,
    meta: Meta,
    dirty: BTreeSet<String>,
    pub live: bool,
    pub rev: u64,
}

impl Fleet {
    pub fn open(profile: Profile, dir: PathBuf, key: &[u8]) -> Result<Fleet, String> {
        std::fs::create_dir_all(dir.join("days")).map_err(|e| e.to_string())?;
        let key: [u8; 32] = key.try_into().map_err(|_| "the key must be 32 bytes".to_string())?;
        let read = |p: &PathBuf| -> Result<Vec<u8>, String> { unseal(&key, &std::fs::read(p).map_err(|e| e.to_string())?) };
        let mp = dir.join("fleet.sealed");
        let meta: Meta = if mp.exists() { serde_json::from_slice(&read(&mp)?).map_err(|e| e.to_string())? } else { Meta::default() };
        let mut days = BTreeMap::new();
        for e in std::fs::read_dir(dir.join("days")).map_err(|e| e.to_string())?.flatten() {
            let f = e.path();
            if f.extension().is_some_and(|x| x == "sealed") {
                let d: Day = serde_json::from_slice(&read(&f)?).map_err(|e| format!("{}: {e}", f.display()))?;
                days.insert(f.file_stem().unwrap_or_default().to_string_lossy().to_string(), d);
            }
        }
        Ok(Fleet { profile, dir, key, days, meta, dirty: BTreeSet::new(), live: true, rev: 1 })
    }
    pub fn save(&mut self) -> Result<(), String> {
        self.rev += 1;
        if !self.live {
            return Ok(());
        }
        let write = |path: PathBuf, plain: Vec<u8>, key: &[u8; 32]| -> Result<(), String> {
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, seal(key, &plain)?).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
        };
        for day in std::mem::take(&mut self.dirty) {
            if let Some(d) = self.days.get(&day) {
                write(self.dir.join("days").join(format!("{day}.sealed")), serde_json::to_vec(d).map_err(|e| e.to_string())?, &self.key)?;
            }
        }
        write(self.dir.join("fleet.sealed"), serde_json::to_vec(&self.meta).map_err(|e| e.to_string())?, &self.key)
    }
    pub fn set_live(&mut self, live: bool) -> Result<(), String> {
        self.live = live;
        self.save()
    }
    pub fn tick(&mut self, at: &str) {
        if at.len() >= 16 && at > self.meta.now.as_str() {
            self.meta.now = at[..16].to_string();
        }
    }
    pub fn now(&self) -> &str {
        &self.meta.now
    }
    pub fn today(&self) -> String {
        day_of(&self.meta.now).to_string()
    }
    fn day_mut(&mut self, at: &str) -> &mut Day {
        let day = day_of(at).to_string();
        self.dirty.insert(day.clone());
        self.days.entry(day).or_default()
    }
    pub fn payouts(&self) -> &[Payout] {
        &self.meta.payouts
    }
    pub fn claims(&self) -> &[(String, String, Paise, String)] {
        &self.meta.claims
    }

    pub fn trip_money(&self, t: &Trip) -> TripMoney {
        if t.cancelled.is_some() {
            return TripMoney::default();
        }
        let Some(pf) = self.profile.platform(&t.platform) else {
            return TripMoney { net: t.fare, cash: if t.pay == "cash" { t.fare + t.toll + t.tip } else { 0 }, ..TripMoney::default() };
        };
        let commission = pct(t.fare, pf.commission_pct);
        let cut = commission + pct(commission, pf.commission_gst_pct) + pct(t.fare, pf.tds_pct);
        let cash = if t.pay == "cash" { t.fare + t.toll + t.tip } else { 0 };
        let platform_owes = if !pf.settles {
            0
        } else if t.pay == "cash" {
            -cut
        } else {
            t.fare + t.toll + t.tip - cut
        };
        TripMoney { cut, net: t.fare - cut, cash, platform_owes }
    }

    // ---- events in (as a platform feed, the driver's app, or a file of them would deliver) ----
    pub fn add_trip(&mut self, t: Trip) -> Result<String, String> {
        if self.profile.car(&t.car).is_none() {
            return Err(format!("trip {}: no car {}", t.id, t.car));
        }
        if self.profile.driver(&t.driver).is_none() {
            return Err(format!("trip {}: no driver {}", t.id, t.driver));
        }
        if self.profile.platform(&t.platform).is_none() {
            return Err(format!("trip {}: no platform {}", t.id, t.platform));
        }
        if t.at.len() < 16 {
            return Err(format!("trip {}: no time", t.id));
        }
        if self.days.get(day_of(&t.at)).is_some_and(|d| d.trips.iter().any(|x| x.id == t.id)) {
            return Err(format!("trip {} is already in the book", t.id));
        }
        self.tick(&t.at);
        let m = self.trip_money(&t);
        let said = format!("{} {} {} {:.1} km, brings {}", t.id, t.platform, t.car, t.km, agentos_business::util::inr(m.net));
        self.day_mut(&t.at.clone()).trips.push(t);
        self.save()?;
        Ok(said)
    }
    pub fn add_shift(&mut self, s: Shift) -> Result<String, String> {
        if self.profile.car(&s.car).is_none() || self.profile.driver(&s.driver).is_none() {
            return Err(format!("shift: no car {} or driver {}", s.car, s.driver));
        }
        if s.odo_end < s.odo_start || s.end < s.start {
            return Err(format!("shift of {} on {}: it ends before it starts", s.driver, s.car));
        }
        self.tick(&s.end);
        let said = format!("{} drove {} for {:.1} h, {:.0} km on the odometer", s.driver, s.car, hours(&s.start, &s.end), s.odo_end - s.odo_start);
        self.day_mut(&s.start.clone()).shifts.push(s);
        self.save()?;
        Ok(said)
    }
    pub fn add_fill(&mut self, f: Fill) -> Result<String, String> {
        let car = self.profile.car(&f.car).ok_or_else(|| format!("fill: no car {}", f.car))?;
        if (car.fuel == "ev") == (f.kind == "cng") {
            return Err(format!("{} runs on {}: a {} bill cannot be for it", car.id, car.fuel, f.kind));
        }
        if f.qty <= 0.0 || f.amount <= 0 {
            return Err("a fill needs a quantity and an amount".into());
        }
        self.tick(&f.at);
        let said = format!("{} took {:.1} {} for {}", f.car, f.qty, if f.kind == "cng" { "kg" } else { "kWh" }, agentos_business::util::inr(f.amount));
        self.day_mut(&f.at.clone()).fills.push(f);
        self.save()?;
        Ok(said)
    }
    pub fn add_expense(&mut self, car: &str, at: &str, amount: Paise, note: &str) -> Result<String, String> {
        let id = self.profile.car(car).map(|c| c.id.clone()).ok_or_else(|| format!("no car {car}"))?;
        if amount <= 0 {
            return Err("an expense needs an amount".into());
        }
        self.tick(at);
        self.day_mut(at).expenses.push(Money { who: id.clone(), at: at.to_string(), amount, note: note.to_string() });
        self.save()?;
        Ok(format!("{note} on {id}: {}", agentos_business::util::inr(amount)))
    }
    pub fn add_payout(&mut self, p: Payout) -> Result<String, String> {
        let name = self.profile.platform(&p.platform).filter(|x| x.settles).map(|x| x.name.to_lowercase()).ok_or_else(|| format!("{} does not pay the fleet", p.platform))?;
        let said = format!("{name} paid {} for {} to {}", agentos_business::util::inr(p.amount), p.from, p.to);
        self.meta.payouts.push(Payout { platform: name, ..p });
        self.save()?;
        Ok(said)
    }
    /// A driver hands in riders' cash. No more than they hold.
    pub fn cash_in(&mut self, driver: &str, amount: Paise, at: &str) -> Result<String, String> {
        let d = self.profile.driver(driver).cloned().ok_or_else(|| format!("no driver {driver}"))?;
        let day = day_of(at).to_string();
        let held = self.driver_sheet(&day).into_iter().find(|s| s.id == d.id).map(|s| s.cash_held).unwrap_or(0);
        if amount <= 0 || amount > held {
            return Err(format!("{} holds {} of riders' cash", d.name, agentos_business::util::inr(held)));
        }
        self.tick(at);
        self.day_mut(at).cash_in.push(Money { who: d.id.clone(), at: at.to_string(), amount, note: String::new() });
        self.save()?;
        Ok(format!("{} handed in {}; still holds {}", d.name, agentos_business::util::inr(amount), agentos_business::util::inr(held - amount)))
    }
    /// Record a driver's earnings for today as handed over. Exactly what the sheet says.
    pub fn pay_driver(&mut self, driver: &str, amount: Paise) -> Result<String, String> {
        let d = self.profile.driver(driver).cloned().ok_or_else(|| format!("no driver {driver}"))?;
        let day = self.today();
        let s = self.driver_sheet(&day).into_iter().find(|s| s.id == d.id).ok_or_else(|| format!("{} did not drive today", d.name))?;
        if s.due <= 0 {
            return Err(format!("{} has nothing due today", d.name));
        }
        if amount != s.due {
            return Err(format!("{} is due {} today, not {}", d.name, agentos_business::util::inr(s.due), agentos_business::util::inr(amount)));
        }
        let at = self.meta.now.clone();
        self.day_mut(&at).paid.push(Money { who: d.id.clone(), at: at.clone(), amount, note: String::new() });
        self.save()?;
        Ok(format!("{} recorded as handed to {} for {} trips", agentos_business::util::inr(amount), d.name, s.trips))
    }
    pub fn claim(&mut self, platform: &str, amount: Paise) -> Result<String, String> {
        let name = self.profile.platform(platform).map(|p| p.name.to_lowercase()).ok_or_else(|| format!("no platform {platform}"))?;
        let (can, why) = self.claimable(&name);
        if can <= 0 {
            return Err(format!("nothing to claim from {name}"));
        }
        if amount != can {
            return Err(format!("{name} is short by {}, not {}", agentos_business::util::inr(can), agentos_business::util::inr(amount)));
        }
        let reference = format!("CLM-{}-{}", name.to_uppercase().replace(' ', ""), self.meta.claims.len() + 1);
        self.meta.claims.push((self.today(), name.clone(), amount, reference.clone()));
        self.save()?;
        Ok(format!("claim {reference} for {} filed with {name} (simulated): {why}", agentos_business::util::inr(amount)))
    }
}

/// Hours between two local times on the same or following day.
pub fn hours(a: &str, b: &str) -> f64 {
    (agentos_business::util::minutes(b) - agentos_business::util::minutes(a)) as f64 / 60.0
}
pub fn rupees(x: f64) -> Paise {
    rs(x)
}
