//! Everything read from the fleet's book: the drivers' sheets, the cars' days, the checks, what
//! the platforms owe, each car's profit, and tomorrow's plan.
use crate::{hours, Fleet};
use agentos_business::util::{add_days, day_number, hour_of, inr, pct, rs, weekday, Paise, WEEKDAYS};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct DriverSheet {
    pub id: String,
    pub name: String,
    pub car: String,
    pub trips: u32,
    pub cancelled: u32,
    pub offered: u32,
    pub hours: f64,
    pub net: Paise,
    pub tips: Paise,
    pub earnings: Paise,
    pub cash_held: Paise,
    pub paid: Paise,
    pub due: Paise,
}
#[derive(Debug, Clone, Default)]
pub struct CarDay {
    pub id: String,
    pub fuel: String,
    pub trips: u32,
    pub trip_km: f64,
    pub dead_km: f64,
    pub odo_km: f64,
    pub shift_hours: f64,
    pub busy_hours: f64,
    pub fares: Paise,
    pub net: Paise,
    pub energy: Paise,
    pub public_kwh: f64,
    pub fees: Paise,
    pub expenses: Paise,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub day: String,
    /// cash_held, personal_km, fuel_mismatch, overtime, cancels, idle, papers, service_due, payout_short
    pub kind: String,
    /// A driver id, a car id or a platform.
    pub who: String,
    pub text: String,
}
#[derive(Debug, Clone, Default)]
pub struct CarMonth {
    pub id: String,
    pub model: String,
    pub fuel: String,
    pub days_out: u32,
    pub trips: u32,
    pub km: f64,
    pub net: Paise,
    pub driver_share: Paise,
    pub energy: Paise,
    pub fees: Paise,
    pub maintenance: Paise,
    pub expenses: Paise,
    pub emi: Paise,
    pub fixed: Paise,
    pub public_extra: Paise,
}
impl CarMonth {
    /// What the car leaves before its loan instalment and yearly papers.
    pub fn operating(&self) -> Paise {
        self.net - self.driver_share - self.energy - self.fees - self.maintenance - self.expenses
    }
    pub fn profit(&self) -> Paise {
        self.operating() - self.emi - self.fixed
    }
}
#[derive(Debug, Clone)]
pub struct Recon {
    pub platform: String,
    pub from: String,
    pub to: String,
    pub expected: Paise,
    pub received: Paise,
    pub reference: String,
}

impl Fleet {
    pub fn driver_sheet(&self, day: &str) -> Vec<DriverSheet> {
        let Some(d) = self.days.get(day) else { return vec![] };
        let share = self.profile.rules.driver_share_pct;
        let mut out = Vec::new();
        for dr in &self.profile.drivers {
            let mut s = DriverSheet { id: dr.id.clone(), name: dr.name.clone(), car: dr.car.clone(), ..DriverSheet::default() };
            for t in d.trips.iter().filter(|t| t.driver == dr.id) {
                s.offered += 1;
                match t.cancelled.as_deref() {
                    Some("driver") => s.cancelled += 1,
                    Some(_) => {}
                    None => {
                        let m = self.trip_money(t);
                        s.trips += 1;
                        s.net += m.net;
                        s.tips += t.tip;
                        s.cash_held += m.cash;
                        s.car = t.car.clone();
                    }
                }
            }
            s.hours = d.shifts.iter().filter(|x| x.driver == dr.id).map(|x| hours(&x.start, &x.end)).sum();
            s.cash_held -= d.cash_in.iter().filter(|m| m.who == dr.id).map(|m| m.amount).sum::<Paise>();
            s.paid = d.paid.iter().filter(|m| m.who == dr.id).map(|m| m.amount).sum();
            s.earnings = pct(s.net, share) + s.tips;
            s.due = s.earnings - s.paid;
            if s.offered > 0 || s.hours > 0.0 {
                out.push(s);
            }
        }
        out
    }

    pub fn car_day(&self, day: &str) -> Vec<CarDay> {
        let empty = crate::Day::default();
        let d = self.days.get(day).unwrap_or(&empty);
        self.profile
            .cars
            .iter()
            .map(|c| {
                let mut x = CarDay { id: c.id.clone(), fuel: c.fuel.clone(), ..CarDay::default() };
                let mut on: BTreeMap<(&str, &str), ()> = BTreeMap::new();
                for t in d.trips.iter().filter(|t| t.car == c.id) {
                    x.dead_km += t.dead_km;
                    if t.cancelled.is_none() {
                        x.trips += 1;
                        x.trip_km += t.km;
                        x.busy_hours += t.mins / 60.0;
                        x.fares += t.fare;
                        x.net += self.trip_money(t).net;
                        on.insert((t.driver.as_str(), t.platform.as_str()), ());
                    }
                }
                for (_, pf) in on.keys() {
                    x.fees += rs(self.profile.platform(pf).map(|p| p.daily_fee).unwrap_or(0.0));
                }
                for s in d.shifts.iter().filter(|s| s.car == c.id) {
                    x.odo_km += s.odo_end - s.odo_start;
                    x.shift_hours += hours(&s.start, &s.end);
                }
                for f in d.fills.iter().filter(|f| f.car == c.id) {
                    x.energy += f.amount;
                    if f.kind == "public" {
                        x.public_kwh += f.qty;
                    }
                }
                x.expenses = d.expenses.iter().filter(|e| e.who == c.id).map(|e| e.amount).sum();
                x
            })
            .collect()
    }

    /// The car's odometer as last read, and the reading at its last service.
    pub fn odometer(&self, car: &str, upto: &str) -> (f64, f64) {
        let c = self.profile.car(car).unwrap();
        let (mut odo, mut serviced) = (c.odometer, c.last_service_km);
        for (_, d) in self.days.range(..=upto.to_string()) {
            for s in d.shifts.iter().filter(|s| s.car == c.id) {
                odo = odo.max(s.odo_end);
            }
            if d.expenses.iter().any(|e| e.who == c.id && e.note.contains("service")) {
                serviced = odo;
            }
        }
        (odo, serviced)
    }

    pub fn reconcile(&self) -> Vec<Recon> {
        self.payouts()
            .iter()
            .map(|p| {
                let expected = self
                    .days
                    .range(p.from.clone()..=p.to.clone())
                    .flat_map(|(_, d)| d.trips.iter())
                    .filter(|t| t.platform.eq_ignore_ascii_case(&p.platform))
                    .map(|t| self.trip_money(t).platform_owes)
                    .sum();
                Recon { platform: p.platform.clone(), from: p.from.clone(), to: p.to.clone(), expected, received: p.amount, reference: p.reference.clone() }
            })
            .collect()
    }
    pub fn awaiting(&self, platform: &str) -> Paise {
        let paid_to = self.payouts().iter().filter(|p| p.platform == platform).map(|p| p.to.clone()).max().unwrap_or_default();
        self.days
            .iter()
            .filter(|(day, _)| day.as_str() > paid_to.as_str())
            .flat_map(|(_, d)| d.trips.iter())
            .filter(|t| t.platform.eq_ignore_ascii_case(platform))
            .map(|t| self.trip_money(t).platform_owes)
            .sum()
    }
    pub fn claimable(&self, platform: &str) -> (Paise, String) {
        let rows: Vec<Recon> = self.reconcile().into_iter().filter(|r| r.platform == platform && r.expected - r.received > 100).collect();
        let short: Paise = rows.iter().map(|r| r.expected - r.received).sum();
        let claimed: Paise = self.claims().iter().filter(|c| c.1 == platform).map(|c| c.2).sum();
        ((short - claimed).max(0), rows.iter().map(|r| format!("{} to {} ref {}: owed {}, paid {}", r.from, r.to, r.reference, inr(r.expected), inr(r.received))).collect::<Vec<_>>().join("; "))
    }

    /// The checks for one day: what an owner would look for by hand, each with what to do.
    pub fn issues(&self, day: &str) -> Vec<Issue> {
        let r = &self.profile.rules;
        let mut out = Vec::new();
        let mut add = |kind: &str, who: &str, text: String| out.push(Issue { day: day.to_string(), kind: kind.into(), who: who.into(), text });
        let empty = crate::Day::default();
        let d = self.days.get(day).unwrap_or(&empty);
        for s in self.driver_sheet(day) {
            if s.cash_held > rs(r.cash_tolerance) {
                add("cash_held", &s.id, format!("{} still holds {} of riders' cash from {day}: take it in", s.name, inr(s.cash_held)));
            }
            if s.hours > r.max_shift_hours + 0.01 {
                add("overtime", &s.id, format!("{} drove {:.1} hours on {day} (the limit is {:.0}): rest them tomorrow", s.name, s.hours, r.max_shift_hours));
            }
            if s.offered >= 8 && s.cancelled as f64 * 100.0 / s.offered as f64 >= r.cancel_pct {
                add("cancels", &s.id, format!("{} cancelled {} of {} trips on {day}: platforms cut the offers of drivers who cancel", s.name, s.cancelled, s.offered));
            }
        }
        for c in self.car_day(day) {
            let car = self.profile.car(&c.id).unwrap();
            let personal = c.odo_km - c.trip_km - c.dead_km;
            if personal > r.personal_km_tolerance {
                add("personal_km", &c.id, format!("{} ran {:.0} km on {day} beyond its trips and the runs between them", c.id, personal));
            }
            // a CNG fill to full, against the distance since the fill before
            for f in d.fills.iter().filter(|f| f.car == c.id && f.kind == "cng") {
                let before = self
                    .days
                    .range(..=day.to_string())
                    .rev()
                    .flat_map(|(_, x)| x.fills.iter().rev())
                    .filter(|x| x.car == c.id && x.kind == "cng" && x.at < f.at)
                    .map(|x| x.odometer)
                    .next();
                if let Some(prev) = before {
                    let per = (f.odometer - prev) / f.qty;
                    if f.odometer > prev && per < car.efficiency * r.fuel_floor_pct / 100.0 {
                        add("fuel_mismatch", &c.id, format!("{}: {:.1} kg for {:.0} km is {:.1} km/kg; the car does about {:.0}. Check the bill of {}", c.id, f.qty, f.odometer - prev, per, car.efficiency, &f.at[11..]));
                    }
                }
            }
            if c.shift_hours == 0.0 && c.trips == 0 && c.expenses == 0 && !self.days.is_empty() && self.days.contains_key(day) {
                add("idle", &c.id, format!("{} did not go out on {day} and no repair or service is recorded", c.id));
            }
            let (odo, serviced) = self.odometer(&c.id, day);
            if odo - serviced >= car.service_every_km {
                add("service_due", &c.id, format!("{} has run {:.0} km since its last service (due every {:.0})", c.id, odo - serviced, car.service_every_km));
            }
            for (what, until) in [("insurance", &car.insurance_until), ("permit", &car.permit_until)] {
                if let (Some(u), Some(t)) = (day_number(until), day_number(day)) {
                    if u - t <= r.papers_days {
                        add("papers", &c.id, format!("{}'s {what} {} {until}", c.id, if u < t { "expired on" } else { "runs out on" }));
                    }
                }
            }
        }
        for dr in &self.profile.drivers {
            if let (Some(u), Some(t)) = (day_number(&dr.licence_until), day_number(day)) {
                if u - t <= r.papers_days {
                    add("papers", &dr.id, format!("{}'s licence {} {}", dr.name, if u < t { "expired on" } else { "runs out on" }, dr.licence_until));
                }
            }
        }
        for pf in self.profile.platforms.iter().filter(|p| p.settles) {
            let n = pf.name.to_lowercase();
            let (can, _) = self.claimable(&n);
            if can > 0 {
                add("payout_short", &n, format!("{} paid {} less than its trips come to: claim it", pf.name, inr(can)));
            }
        }
        out
    }

    /// Each car over a period (a month "2026-10", or any prefix of a day).
    pub fn car_month(&self, period: &str) -> Vec<CarMonth> {
        let p = &self.profile;
        let days: Vec<&String> = self.days.keys().filter(|d| d.starts_with(period)).collect();
        let span = days.len() as f64;
        p.cars
            .iter()
            .map(|c| {
                let mut m = CarMonth { id: c.id.clone(), model: c.model.clone(), fuel: c.fuel.clone(), ..CarMonth::default() };
                for day in &days {
                    let x = self.car_day(day).into_iter().find(|x| x.id == c.id).unwrap();
                    if x.shift_hours > 0.0 {
                        m.days_out += 1;
                    }
                    m.trips += x.trips;
                    m.km += x.odo_km;
                    m.net += x.net;
                    m.energy += x.energy;
                    m.fees += x.fees;
                    m.expenses += x.expenses;
                    m.public_extra += rs(x.public_kwh * (p.energy.public_per_kwh - p.energy.depot_per_kwh));
                }
                m.driver_share = pct(m.net, p.rules.driver_share_pct);
                m.maintenance = rs(c.maint_per_km * m.km);
                m.emi = rs(c.emi_month * span / 30.4);
                m.fixed = rs(c.fixed_year * span / 365.0);
                m
            })
            .collect()
    }

    // ---- answers, as text ----
    pub fn dashboard(&self) -> String {
        let day = self.today();
        let cars = self.car_day(&day);
        let sheets = self.driver_sheet(&day);
        let out_n = cars.iter().filter(|c| c.shift_hours > 0.0 || c.trips > 0).count();
        let (trips, km, fares, net, energy): (u32, f64, Paise, Paise, Paise) =
            cars.iter().fold((0, 0.0, 0, 0, 0), |a, c| (a.0 + c.trips, a.1 + c.trip_km + c.dead_km, a.2 + c.fares, a.3 + c.net, a.4 + c.energy));
        let earn: Paise = sheets.iter().map(|s| s.earnings - s.tips).sum();
        let mut out = vec![format!("{} — {} {}", self.profile.name, WEEKDAYS[weekday(&day)], day)];
        out.push(format!(
            "{out_n} of {} cars out: {trips} trips, {km:.0} km; fares {}, after the platforms' cut {}; drivers' share {}, fuel and charging {}",
            cars.len(),
            inr(fares),
            inr(net),
            inr(earn),
            inr(energy)
        ));
        let mut by: BTreeMap<String, (u32, Paise, Paise)> = BTreeMap::new();
        for t in self.days.get(&day).map(|d| d.trips.as_slice()).unwrap_or(&[]).iter().filter(|t| t.cancelled.is_none()) {
            let e = by.entry(t.platform.clone()).or_default();
            e.0 += 1;
            e.1 += t.fare;
            e.2 += self.trip_money(t).net;
        }
        for (n, (k, f, x)) in &by {
            out.push(format!("  {n:<12} {k:>4} trips  fares {:>9}  keeps {:>9} ({:.0}%)", inr(*f), inr(*x), *x as f64 * 100.0 / (*f).max(1) as f64));
        }
        let mut ranked: Vec<&crate::report::CarDay> = cars.iter().filter(|c| c.trips > 0).collect();
        ranked.sort_by(|a, b| b.net.cmp(&a.net));
        if let (Some(best), Some(worst)) = (ranked.first(), ranked.last()) {
            out.push(format!("Best car {} ({}), least {} ({})", best.id, inr(best.net), worst.id, inr(worst.net)));
        }
        let (due, held): (Paise, Paise) = sheets.iter().fold((0, 0), |a, s| (a.0 + s.due, a.1 + s.cash_held));
        out.push(format!("Drivers: {} of earnings to hand over, {} of riders' cash still with them", inr(due), inr(held)));
        for pf in self.profile.platforms.iter().filter(|p| p.settles) {
            out.push(format!("{}: {} awaiting payout", pf.name, inr(self.awaiting(&pf.name.to_lowercase()))));
        }
        let issues = self.issues(&day);
        if !issues.is_empty() {
            out.push("Needs you:".into());
            out.extend(issues.iter().map(|i| format!("  ! {}", i.text)));
        }
        out.join("\n")
    }
    pub fn cars_text(&self) -> String {
        let day = self.today();
        let mut out = vec![format!("Cars on {day}")];
        for c in self.car_day(&day) {
            let car = self.profile.car(&c.id).unwrap();
            out.push(if c.shift_hours == 0.0 && c.trips == 0 {
                format!("  {} {:<18} did not go out", c.id, car.model)
            } else {
                format!(
                    "  {} {:<18} {:>2} trips {:>4.0} km ({:.0} empty)  busy {:>3.0}% of {:>4.1} h  keeps {:>7}  {} {:>6}",
                    c.id,
                    car.model,
                    c.trips,
                    c.odo_km,
                    c.dead_km,
                    c.busy_hours * 100.0 / c.shift_hours.max(0.1),
                    c.shift_hours,
                    inr(c.net),
                    if c.fuel == "ev" { "charging" } else { "CNG" },
                    inr(c.energy)
                )
            });
        }
        out.join("\n")
    }
    pub fn drivers_text(&self) -> String {
        let day = self.today();
        let rows: Vec<String> = self
            .driver_sheet(&day)
            .iter()
            .map(|s| {
                format!(
                    "  {} ({}) on {}: {} trips in {:.1} h → earnings {} (share {} + tips {}), paid {}, due {}; riders' cash to hand in {}",
                    s.name, s.id, s.car, s.trips, s.hours, inr(s.earnings), inr(s.earnings - s.tips), inr(s.tips), inr(s.paid), inr(s.due), inr(s.cash_held)
                )
            })
            .collect();
        format!("Drivers on {day} (share {}% of fares after the platform's cut)\n{}", self.profile.rules.driver_share_pct, rows.join("\n"))
    }
    pub fn issues_text(&self) -> String {
        let i = self.issues(&self.today());
        if i.is_empty() {
            "nothing needs you right now".into()
        } else {
            i.iter().map(|x| x.text.clone()).collect::<Vec<_>>().join("\n")
        }
    }
    pub fn payouts_text(&self) -> String {
        let mut out = Vec::new();
        for pf in &self.profile.platforms {
            let n = pf.name.to_lowercase();
            if !pf.settles {
                out.push(format!("{}: riders pay the driver or the fleet's QR directly; the platform takes ₹{:.0} a driver a day", pf.name, pf.daily_fee));
                continue;
            }
            out.push(format!("{} (commission {}% + GST, tax deducted {}%)", pf.name, pf.commission_pct, pf.tds_pct));
            for r in self.reconcile().iter().filter(|r| r.platform == n) {
                let short = r.expected - r.received;
                out.push(format!("  {} to {}: owed {}, paid {}{}", r.from, r.to, inr(r.expected), inr(r.received), if short > 100 { format!(" — SHORT {}", inr(short)) } else { " — ok".into() }));
            }
            out.push(format!("  awaiting payout: {}", inr(self.awaiting(&n))));
            let (can, _) = self.claimable(&n);
            if can > 0 {
                out.push(format!("  to claim: {}", inr(can)));
            }
            for c in self.claims().iter().filter(|c| c.1 == n) {
                out.push(format!("  claimed {} on {} ({})", inr(c.2), c.0, c.3));
            }
        }
        out.join("\n")
    }
    /// (fuel, cars, operating profit a car a day, instalment a day, price) averaged by fuel.
    pub fn by_fuel(&self, period: &str) -> Vec<(String, u32, Paise, Paise, f64, Paise, f64)> {
        let months = self.car_month(period);
        let span = self.days.keys().filter(|d| d.starts_with(period)).count().max(1) as i64;
        let mut out = Vec::new();
        for fuel in ["cng", "ev"] {
            let v: Vec<&CarMonth> = months.iter().filter(|m| m.fuel == fuel).collect();
            if v.is_empty() {
                continue;
            }
            let n = v.len() as i64;
            let op = v.iter().map(|m| m.operating()).sum::<Paise>() / n / span;
            let emi = v.iter().map(|m| m.emi + m.fixed).sum::<Paise>() / n / span;
            let price = self.profile.cars.iter().filter(|c| c.fuel == fuel).map(|c| c.price).sum::<f64>() / n as f64;
            let energy_km = v.iter().map(|m| m.energy).sum::<Paise>() as f64 / v.iter().map(|m| m.km).sum::<f64>().max(1.0);
            out.push((fuel.to_string(), n as u32, op, emi, price, v.iter().map(|m| m.public_extra).sum(), energy_km));
        }
        out
    }
    pub fn month_text(&self, period: &str) -> String {
        let period = if period.trim().len() >= 4 { period.trim().to_string() } else { self.today()[..7.min(self.today().len())].to_string() };
        let months = self.car_month(&period);
        let span = self.days.keys().filter(|d| d.starts_with(&period)).count();
        if span == 0 {
            return format!("no days in the book for {period}");
        }
        let mut out = vec![format!("{} — {period}, {span} day(s): each car, from what it keeps after the platforms' cut to its profit", self.profile.name)];
        out.push(format!("  {:<4} {:<18} {:>4} {:>6} {:>10} {:>9} {:>9} {:>8} {:>8} {:>9} {:>9} {:>10}", "car", "model", "days", "km", "keeps", "drivers", "energy", "upkeep", "repairs", "loan", "papers", "profit"));
        let mut total = 0;
        for m in &months {
            total += m.profit();
            out.push(format!(
                "  {:<4} {:<18} {:>4} {:>6.0} {:>10} {:>9} {:>9} {:>8} {:>8} {:>9} {:>9} {:>10}",
                m.id, m.model, m.days_out, m.km, inr(m.net), inr(m.driver_share), inr(m.energy + m.fees), inr(m.maintenance), inr(m.expenses), inr(m.emi), inr(m.fixed), inr(m.profit())
            ));
        }
        out.push(format!("Fleet profit for the period: {}", inr(total)));
        let f = self.by_fuel(&period);
        for (fuel, n, op, emi, price, _, ekm) in &f {
            out.push(format!(
                "{} cars ({n}, about ₹{:.1} lakh each): a car leaves {} a day before its loan and papers ({} a day), so {} a day; energy ₹{:.2} a km",
                if fuel == "ev" { "Electric" } else { "CNG" },
                price / 100_000.0,
                inr(*op),
                inr(*emi),
                inr(op - emi),
                ekm / 100.0
            ));
        }
        if let (Some(c), Some(e)) = (f.iter().find(|x| x.0 == "cng"), f.iter().find(|x| x.0 == "ev")) {
            let gain = e.2 - c.2;
            if gain > 0 {
                out.push(format!(
                    "An electric car leaves {} a day more than a CNG car before the loan. It costs ₹{:.1} lakh more, which that pays back in about {:.0} months of running.",
                    inr(gain),
                    (e.4 - c.4) / 100_000.0,
                    (e.4 - c.4) * 100.0 / gain as f64 / 30.4
                ));
            } else {
                out.push(format!("At this usage an electric car leaves {} a day less than a CNG car before the loan.", inr(-gain)));
            }
            if e.5 > 0 {
                out.push(format!("Public fast charging cost {} more this period than the same energy at the depot.", inr(e.5)));
            }
        }
        out.join("\n")
    }

    /// Tomorrow: which platform pays at which hours, how the electric cars charge, which cars
    /// and drivers need attention first.
    pub fn plan_text(&self) -> String {
        let today = self.today();
        let tomorrow = add_days(&today, 1);
        let since = add_days(&today, -13);
        let mut out = vec![format!("Plan for {} {} (from the last {} days)", WEEKDAYS[weekday(&tomorrow)], tomorrow, self.days.range(since.clone()..=today.clone()).count())];
        // what an hour on a trip brings, by platform and part of the day
        let blocks = [("06–10", 6, 10), ("10–16", 10, 16), ("16–21", 16, 21), ("21–24", 21, 24)];
        let mut per: BTreeMap<(usize, String), (Paise, f64, u32)> = BTreeMap::new();
        for (_, d) in self.days.range(since.clone()..=today.clone()) {
            for t in d.trips.iter().filter(|t| t.cancelled.is_none()) {
                let h = hour_of(&t.at);
                if let Some(b) = blocks.iter().position(|b| h >= b.1 && h < b.2) {
                    let e = per.entry((b, t.platform.clone())).or_default();
                    e.0 += self.trip_money(t).net;
                    e.1 += t.mins + t.dead_km * 3.0;
                    e.2 += 1;
                }
            }
        }
        out.push("Where an hour of driving pays most (after the platform's cut):".into());
        for (b, (name, _, _)) in blocks.iter().enumerate() {
            let mut v: Vec<(String, f64, u32)> = per.iter().filter(|(k, x)| k.0 == b && x.2 >= 10).map(|(k, x)| (k.1.clone(), x.0 as f64 / 100.0 / (x.1 / 60.0).max(0.1), x.2)).collect();
            v.sort_by(|a, b| b.1.total_cmp(&a.1));
            if let Some(best) = v.first() {
                out.push(format!("  {name}: {} at ₹{:.0} an hour{}", best.0, best.1, v.get(1).map(|o| format!(" ({} ₹{:.0})", o.0, o.1)).unwrap_or_default()));
            }
        }
        // charging: who needs the depot's chargers tonight, who will need a midday top-up
        let mut evs: Vec<(String, f64, f64)> = Vec::new();
        for c in self.profile.cars.iter().filter(|c| c.fuel == "ev") {
            let km: Vec<f64> = self.days.range(add_days(&today, -6)..=today.clone()).filter_map(|(d, _)| self.car_day(d).into_iter().find(|x| x.id == c.id)).map(|x| x.odo_km).filter(|k| *k > 0.0).collect();
            let need = if km.is_empty() { 0.0 } else { km.iter().sum::<f64>() / km.len() as f64 };
            evs.push((c.id.clone(), need, c.capacity * c.efficiency));
        }
        evs.sort_by(|a, b| b.1.total_cmp(&a.1));
        if !evs.is_empty() {
            let n = self.profile.depot_chargers as usize;
            out.push(format!("Charging ({} depot charger(s), {} electric cars):", n, evs.len()));
            for (i, (id, need, range)) in evs.iter().enumerate() {
                let slot = if i < n { "on a depot charger from the end of its shift".to_string() } else { format!("takes over a charger at about {:02}:00, when the first cars are full", 1 + (i - n) % 3) };
                let top = if *need > range * 0.85 { format!("; it runs about {need:.0} km a day on a {range:.0} km battery, so give it 40 minutes at the depot between 13:00 and 15:00 rather than a public charger") } else { String::new() };
                out.push(format!("  {id}: {slot}{top}"));
            }
        }
        let issues = self.issues(&today);
        let first: Vec<&crate::report::Issue> = issues.iter().filter(|i| ["service_due", "papers", "overtime", "idle"].contains(&i.kind.as_str())).collect();
        if !first.is_empty() {
            out.push("Before the cars go out:".into());
            out.extend(first.iter().map(|i| format!("  {}", i.text)));
        }
        let same: Vec<Paise> = (1..=4).map(|w| add_days(&tomorrow, -7 * w)).filter(|d| self.days.contains_key(d)).map(|d| self.car_day(&d).iter().map(|c| c.net).sum()).collect();
        if !same.is_empty() {
            out.push(format!("Expect about {} after the platforms' cut, as on the last {} {}(s).", inr(same.iter().sum::<Paise>() / same.len() as Paise), same.len(), WEEKDAYS[weekday(&tomorrow)]));
        }
        let month = self.car_month(&today[..7.min(today.len())]);
        let losing: Vec<String> = month.iter().filter(|m| m.profit() < 0 && m.days_out > 3).map(|m| format!("{} ({})", m.id, inr(m.profit()))).collect();
        if !losing.is_empty() {
            out.push(format!("Losing money this month: {}.", losing.join(", ")));
        }
        out.join("\n")
    }

    /// The figures as data. Driver names are the owner's own staff; no rider is in the book.
    pub fn state(&self) -> Value {
        let day = self.today();
        let cars = self.car_day(&day);
        let period = &day[..7.min(day.len())];
        json!({
            "fleet": self.profile.name, "day": day, "now": self.now(),
            "cars": cars.iter().map(|c| json!({"id": c.id, "fuel": c.fuel, "trips": c.trips, "km": c.odo_km, "net": c.net, "energy": c.energy,
                "busy_pct": c.busy_hours * 100.0 / c.shift_hours.max(0.1)})).collect::<Vec<_>>(),
            "drivers": self.driver_sheet(&day).iter().map(|s| json!({"id": s.id, "name": s.name, "trips": s.trips, "hours": s.hours,
                "earnings": s.earnings, "paid": s.paid, "due": s.due, "cash_held": s.cash_held})).collect::<Vec<_>>(),
            "issues": self.issues(&day).iter().map(|i| json!({"kind": i.kind, "who": i.who, "text": i.text})).collect::<Vec<_>>(),
            "platforms": self.profile.platforms.iter().filter(|p| p.settles).map(|p| { let n = p.name.to_lowercase();
                json!({"name": p.name, "awaiting": self.awaiting(&n), "claimable": self.claimable(&n).0}) }).collect::<Vec<_>>(),
            "month": self.car_month(period).iter().map(|m| json!({"id": m.id, "model": m.model, "fuel": m.fuel, "days_out": m.days_out, "km": m.km, "net": m.net,
                "driver_share": m.driver_share, "energy": m.energy, "fees": m.fees, "maintenance": m.maintenance, "expenses": m.expenses, "emi": m.emi, "fixed": m.fixed,
                "operating": m.operating(), "profit": m.profit()})).collect::<Vec<_>>(),
            "by_fuel": self.by_fuel(period).iter().map(|f| json!({"fuel": f.0, "cars": f.1, "operating_day": f.2, "loan_papers_day": f.3, "price": f.4, "public_extra": f.5, "energy_per_km": f.6})).collect::<Vec<_>>(),
            "amounts_in": "paise",
        })
    }
}
