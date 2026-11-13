//! The fleet in examples/fleet/profile.toml: the money in a trip, the drivers' sheets, each
//! check, the platforms' payouts, a car's profit, the generator scored, and what is on disk.
use agentos_business::util::{add_days, rs};
use agentos_fleet::gen::{play, Event, Gen, Truth, PLANTED};
use agentos_fleet::{Fill, Fleet, Payout, Profile, Shift, Trip};
use std::collections::BTreeSet;
use std::path::PathBuf;

fn profile() -> Profile {
    Profile::load(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/fleet/profile.toml")).unwrap()
}
fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fleet-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}
fn fleet(tag: &str) -> Fleet {
    Fleet::open(profile(), dir(tag), &[4u8; 32]).unwrap()
}
fn trip(id: &str, platform: &str, at: &str, fare: f64, pay: &str) -> Trip {
    Trip { id: id.into(), car: "C01".into(), driver: "D01".into(), platform: platform.into(), at: at.into(), mins: 25.0, km: 8.0, dead_km: 2.0, fare: rs(fare), pay: pay.into(), from: "Hebbal".into(), to: "Majestic".into(), ..Trip::default() }
}
fn shift(car: &str, driver: &str, day: &str, hours: i64, odo: f64, km: f64) -> Shift {
    Shift { car: car.into(), driver: driver.into(), start: format!("{day}T07:00"), end: format!("{day}T{:02}:00", 7 + hours), odo_start: odo, odo_end: odo + km }
}
const DAY: &str = "2026-10-14";

#[test]
fn the_money_in_a_trip_depends_on_the_platform_and_how_it_was_paid() {
    let f = fleet("money");
    // Uber, paid in the app: 25% commission, 18% GST on that, 0.1% tax deducted
    let mut t = trip("T1", "Uber", "2026-10-14T09:00", 200.0, "online");
    t.toll = rs(120.0);
    let m = f.trip_money(&t);
    assert_eq!((m.cut, m.net, m.cash, m.platform_owes), (5_920, 14_080, 0, 20_000 + 12_000 - 5_920));
    // Uber, paid in cash: the driver holds it all and the fleet owes Uber its cut
    t.pay = "cash".into();
    let m = f.trip_money(&t);
    assert_eq!((m.net, m.cash, m.platform_owes), (14_080, 32_000, -5_920));
    // Namma Yatri: no cut, and the platform never holds the money
    let m = f.trip_money(&trip("T2", "Namma Yatri", "2026-10-14T09:30", 200.0, "cash"));
    assert_eq!((m.cut, m.net, m.cash, m.platform_owes), (0, 20_000, 20_000, 0));
    let m = f.trip_money(&trip("T3", "namma yatri", "2026-10-14T09:30", 200.0, "online"));
    assert_eq!((m.net, m.cash, m.platform_owes), (20_000, 0, 0));
    // a cancelled trip brings nothing
    let mut c = trip("T4", "Uber", "2026-10-14T10:00", 0.0, "");
    c.cancelled = Some("rider".into());
    assert_eq!(f.trip_money(&c).net, 0);
}

#[test]
fn a_driver_is_paid_exactly_the_sheet_and_hands_in_no_more_than_they_hold() {
    let mut f = fleet("sheet");
    f.add_trip(trip("T1", "Uber", "2026-10-14T09:00", 200.0, "online")).unwrap();
    let mut t = trip("T2", "Namma Yatri", "2026-10-14T10:00", 300.0, "cash");
    t.tip = rs(20.0);
    f.add_trip(t).unwrap();
    f.add_shift(shift("C01", "D01", DAY, 10, 41_200.0, 20.0)).unwrap();
    let s = &f.driver_sheet(DAY)[0];
    // 35% of (140.80 + 300) = 154.28, plus the ₹20 tip; holds ₹320 of riders' cash
    assert_eq!((s.trips, s.earnings, s.cash_held, s.due), (2, 15_428 + 2_000, 32_000, 17_428));
    assert!((s.hours - 10.0).abs() < 1e-9);
    assert!(f.cash_in("D01", rs(500.0), "2026-10-14T17:10").unwrap_err().contains("holds ₹320"));
    assert!(f.cash_in("Manjunath", rs(320.0), "2026-10-14T17:10").unwrap().contains("still holds ₹0"));
    assert!(f.pay_driver("D01", rs(200.0)).unwrap_err().contains("is due ₹174.28 today, not ₹200"));
    assert!(f.pay_driver("D02", rs(100.0)).unwrap_err().contains("did not drive today"));
    assert!(f.pay_driver("D01", 17_428).unwrap().contains("handed to Manjunath for 2 trips"));
    assert!(f.pay_driver("D01", 17_428).unwrap_err().contains("nothing due"));
    // bad events are refused with the reason
    assert!(f.add_trip(trip("T1", "Uber", "2026-10-14T09:00", 200.0, "online")).unwrap_err().contains("already in the book"));
    assert!(f.add_trip(trip("T9", "Ola", "2026-10-14T09:00", 200.0, "online")).unwrap_err().contains("no platform Ola"));
    let ev_gas = Fill { car: "C10".into(), driver: "D10".into(), at: "2026-10-14T12:00".into(), kind: "cng".into(), qty: 5.0, amount: rs(445.0), odometer: 21_500.0 };
    assert!(f.add_fill(ev_gas).unwrap_err().contains("runs on ev"));
}

fn kinds(f: &Fleet, day: &str) -> BTreeSet<(String, String)> {
    f.issues(day).into_iter().map(|i| (i.kind, i.who)).collect()
}
/// A clean day for every car, so that nothing is idle.
fn clean_day(f: &mut Fleet, day: &str) {
    let cars: Vec<(String, f64)> = f.profile.cars.iter().map(|c| (c.id.clone(), f.odometer(&c.id, day).0)).collect();
    for (i, (car, odo)) in cars.iter().enumerate() {
        let driver = format!("D{:02}", i + 1);
        let mut t = trip(&format!("{day}-{car}"), "Uber", &format!("{day}T09:00"), 200.0, "online");
        t.car = car.clone();
        t.driver = driver.clone();
        f.add_trip(t).unwrap();
        f.add_shift(shift(car, &driver, day, 10, *odo, 10.0)).unwrap();
    }
}

#[test]
fn each_check_fires_for_its_own_reason_and_a_clean_day_raises_none() {
    let mut f = fleet("checks");
    clean_day(&mut f, DAY);
    assert_eq!(kinds(&f, DAY), BTreeSet::new(), "{:?}", f.issues(DAY));

    let day = "2026-10-15";
    clean_day(&mut f, day);
    // riders' cash kept back
    let mut t = trip("X1", "Namma Yatri", "2026-10-15T11:00", 900.0, "cash");
    t.car = "C02".into();
    t.driver = "D02".into();
    t.dead_km = 0.0;
    t.km = 0.0;
    f.add_trip(t).unwrap();
    f.cash_in("D02", rs(500.0), "2026-10-15T18:00").unwrap();
    // the odometer ran 40 km beyond the trips
    f.add_shift(Shift { car: "C03".into(), driver: "D03".into(), start: "2026-10-15T18:00".into(), end: "2026-10-15T18:30".into(), odo_start: 52_820.0, odo_end: 52_860.0 }).unwrap();
    // a CNG bill for more gas than the distance since the last fill can explain
    f.add_fill(Fill { car: "C04".into(), driver: "D04".into(), at: "2026-10-14T16:00".into(), kind: "cng".into(), qty: 7.0, amount: rs(623.0), odometer: 36_405.0 }).unwrap();
    f.add_fill(Fill { car: "C04".into(), driver: "D04".into(), at: "2026-10-15T16:00".into(), kind: "cng".into(), qty: 8.0, amount: rs(712.0), odometer: 36_405.0 + 136.0 }).unwrap();
    // a fourteen-hour shift
    f.add_shift(shift("C05", "D05", day, 4, 47_920.0, 0.0)).unwrap();
    // a driver cancelling 3 of 9 offers
    for k in 0..8 {
        let mut c = trip(&format!("X6-{k}"), "Uber", "2026-10-15T12:00", 0.0, "");
        c.car = "C06".into();
        c.driver = "D06".into();
        c.dead_km = 0.0;
        c.km = 0.0;
        c.cancelled = (k < 3).then(|| "driver".to_string());
        f.add_trip(c).unwrap();
    }
    let got = kinds(&f, day);
    let want: BTreeSet<(String, String)> = [("cash_held", "D02"), ("personal_km", "C03"), ("fuel_mismatch", "C04"), ("overtime", "D05"), ("cancels", "D06")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    assert_eq!(got, want, "{:#?}", f.issues(day));
    let text = f.issues(day).iter().map(|i| i.text.clone()).collect::<Vec<_>>().join("\n");
    for s in ["still holds ₹400", "40 km", "17.0 km/kg", "14.0 hours", "cancelled 3 of 9"] {
        assert!(text.contains(s), "no {s:?} in:\n{text}");
    }

    // a car that stays in with no reason is idle; one in the workshop is not
    let day = "2026-10-16";
    clean_day(&mut f, day);
    let day = "2026-10-17";
    let cars: Vec<String> = f.profile.cars.iter().map(|c| c.id.clone()).collect();
    for (i, car) in cars.iter().enumerate().filter(|(i, _)| *i > 1) {
        let odo = f.odometer(car, day).0;
        f.add_shift(shift(car, &format!("D{:02}", i + 1), day, 10, odo, 0.0)).unwrap();
    }
    f.add_expense("C02", "2026-10-17T09:00", rs(8500.0), "repair: clutch").unwrap();
    assert_eq!(kinds(&f, day), [("idle".to_string(), "C01".to_string())].into_iter().collect());

    // service by distance, and papers by date
    let day = "2026-11-06";
    clean_day(&mut f, day);
    f.add_shift(shift("C02", "D02", day, 1, f.odometer("C02", day).0, 1500.0)).unwrap();
    let got = kinds(&f, day);
    assert!(got.contains(&("service_due".to_string(), "C02".to_string())), "{got:?}");
    assert!(got.contains(&("papers".to_string(), "C09".to_string())) && got.contains(&("papers".to_string(), "D12".to_string())), "{got:?}");
    f.add_expense("C02", "2026-11-06T19:00", rs(4200.0), "service").unwrap();
    assert!(!kinds(&f, day).contains(&("service_due".to_string(), "C02".to_string())));
}

#[test]
fn a_platform_is_reconciled_and_only_the_exact_shortfall_can_be_claimed() {
    let mut f = fleet("recon");
    f.add_trip(trip("T1", "Uber", "2026-10-14T09:00", 200.0, "online")).unwrap();
    f.add_trip(trip("T2", "Uber", "2026-10-15T09:00", 400.0, "cash")).unwrap();
    f.add_trip(trip("T3", "Uber", "2026-10-22T09:00", 300.0, "online")).unwrap();
    // owed for the week: 200 − 59.20 for the online trip, less the 118.40 cut on the cash trip
    let owed = 20_000 - 5_920 - 11_840;
    assert!(f.add_payout(Payout { platform: "Namma Yatri".into(), from: DAY.into(), to: "2026-10-20".into(), amount: 1, reference: "x".into() }).unwrap_err().contains("does not pay the fleet"));
    f.add_payout(Payout { platform: "Uber".into(), from: DAY.into(), to: "2026-10-20".into(), amount: owed - rs(15.0), reference: "UB1".into() }).unwrap();
    let r = &f.reconcile()[0];
    assert_eq!((r.expected, r.received), (owed, owed - 1_500));
    assert_eq!(f.claimable("uber").0, 1_500);
    assert_eq!(f.awaiting("uber"), 30_000 - 8_880, "the trip after the paid week is awaited, not short");
    assert!(f.issues("2026-10-22").iter().any(|i| i.kind == "payout_short" && i.who == "uber"));
    assert!(f.claim("uber", rs(20.0)).unwrap_err().contains("is short by ₹15, not ₹20"));
    assert!(f.claim("Uber", 1_500).unwrap().contains("CLM-UBER-1"));
    assert!(f.claim("uber", 1_500).unwrap_err().contains("nothing to claim"));
    assert!(!f.issues("2026-10-22").iter().any(|i| i.kind == "payout_short"));
    assert!(f.payouts_text().contains("SHORT ₹15") && f.payouts_text().contains("riders pay the driver"));
}

#[test]
fn a_cars_profit_is_what_it_keeps_less_driver_energy_upkeep_loan_and_papers() {
    let mut f = fleet("profit");
    f.add_trip(trip("T1", "Namma Yatri", "2026-10-14T09:00", 2000.0, "online")).unwrap();
    f.add_shift(shift("C01", "D01", DAY, 10, 41_200.0, 100.0)).unwrap();
    f.add_fill(Fill { car: "C01".into(), driver: "D01".into(), at: "2026-10-14T15:00".into(), kind: "cng".into(), qty: 4.0, amount: rs(356.0), odometer: 41_290.0 }).unwrap();
    f.add_expense("C01", "2026-10-14T18:00", rs(500.0), "fine").unwrap();
    let m = f.car_month("2026-10").into_iter().find(|m| m.id == "C01").unwrap();
    // keeps 2000; driver 700; gas 356 + the platform's ₹25 day fee; upkeep 100 km x 1.30; fine 500
    assert_eq!((m.net, m.driver_share, m.energy, m.fees, m.maintenance, m.expenses), (200_000, 70_000, 35_600, 2_500, 13_000, 50_000));
    assert_eq!(m.operating(), 200_000 - 70_000 - 35_600 - 2_500 - 13_000 - 50_000);
    // one day of a ₹12,500 instalment and of ₹42,000 a year of insurance and permit
    assert_eq!((m.emi, m.fixed), (rs(12_500.0 / 30.4), rs(42_000.0 / 365.0)));
    assert_eq!(m.profit(), m.operating() - m.emi - m.fixed);
    let text = f.month_text("2026-10");
    assert!(text.contains("C01") && text.contains("Fleet profit for the period"), "{text}");
}

fn run(f: &mut Fleet, g: &mut Gen, from: &str, days: i64) -> (BTreeSet<(String, Truth)>, BTreeSet<(String, Truth)>, usize) {
    let (mut planted, mut found, mut refused) = (BTreeSet::new(), BTreeSet::new(), 0);
    let profile = f.profile.clone();
    for i in 0..days {
        let day = add_days(from, i);
        let (events, truth) = g.day(&profile, &day);
        for e in &events {
            if let Err(why) = play(f, e) {
                refused += 1;
                eprintln!("{day}: refused: {why}");
            }
        }
        planted.extend(truth.into_iter().map(|t| (day.clone(), t)));
        for i in f.issues(&day).into_iter().filter(|i| PLANTED.contains(&i.kind.as_str())) {
            let kind = PLANTED.iter().find(|k| **k == i.kind).unwrap();
            found.insert((day.clone(), Truth { kind, who: i.who }));
        }
    }
    (planted, found, refused)
}

#[test]
fn every_planted_problem_is_found_and_nothing_else_is_reported() {
    let mut f = fleet("gen");
    f.live = false;
    let mut g = Gen::new(&f.profile, DAY, 7);
    let (planted, found, refused) = run(&mut f, &mut g, DAY, 30);
    assert_eq!(refused, 0);
    let missed: Vec<_> = planted.difference(&found).collect();
    let extra: Vec<_> = found.difference(&planted).collect();
    assert!(missed.is_empty(), "planted but not found: {missed:?}");
    assert!(extra.is_empty(), "reported but not planted: {extra:?}");
    for k in PLANTED {
        assert!(planted.iter().any(|(_, t)| t.kind == k), "no {k} was planted in 30 days");
    }
    // the month looks like a fleet: every car out most days, a few thousand trips
    let months: Vec<_> = f.car_month("2026-1");
    assert!(months.iter().all(|m| m.days_out >= 26), "{:?}", months.iter().map(|m| m.days_out).collect::<Vec<_>>());
    let trips: u32 = months.iter().map(|m| m.trips).sum();
    assert!((5_000..12_000).contains(&trips), "{trips} trips");
    // the generator is repeatable
    let mut again = Gen::new(&f.profile, DAY, 7);
    let mut first = Gen::new(&f.profile, DAY, 7);
    let (a, b) = (again.day(&f.profile, DAY), first.day(&f.profile, DAY));
    assert_eq!(serde_json::to_string(&a.0).unwrap(), serde_json::to_string(&b.0).unwrap());
}

#[test]
fn the_book_is_sealed_holds_no_rider_and_reopens_whole() {
    let d = dir("disk");
    let mut f = Fleet::open(profile(), d.clone(), &[4u8; 32]).unwrap();
    let mut g = Gen::new(&f.profile, DAY, 7);
    let (planted, found, _) = run(&mut f, &mut g, DAY, 3);
    assert_eq!(planted, found);
    let mut files = 0;
    for e in std::fs::read_dir(d.join("days")).unwrap().flatten().chain(std::fs::read_dir(&d).unwrap().flatten()) {
        if e.path().is_file() {
            files += 1;
            let text = String::from_utf8_lossy(&std::fs::read(e.path()).unwrap()).to_string();
            for s in ["Koramangala", "Manjunath", "D01", "trips", "Uber"] {
                assert!(!text.contains(s), "{s} readable in {}", e.path().display());
            }
        }
    }
    assert_eq!(files, 4, "three day files and the fleet file");
    // an event file a platform feed would deliver has no field for a rider at all
    let (events, _) = Gen::new(&f.profile, DAY, 7).day(&f.profile, DAY);
    let line = serde_json::to_string(events.iter().find(|e| matches!(e, Event::Trip(_))).unwrap()).unwrap();
    assert!(!line.contains("rider_") && !line.contains("phone") && !line.contains("name"), "{line}");
    let (trips, net): (usize, i64) = (f.days.values().map(|x| x.trips.len()).sum(), f.car_month("2026-10").iter().map(|m| m.net).sum());
    drop(f);
    assert!(Fleet::open(profile(), d.clone(), &[9u8; 32]).is_err(), "another key does not open it");
    let again = Fleet::open(profile(), d, &[4u8; 32]).unwrap();
    assert_eq!((again.days.values().map(|x| x.trips.len()).sum::<usize>(), again.car_month("2026-10").iter().map(|m| m.net).sum::<i64>()), (trips, net));
    // decimals (odometers, kilometres, kilograms) come back bit for bit, so every answer is the same
    assert_eq!(again.state().to_string(), {
        let mut g2 = Gen::new(&again.profile, DAY, 7);
        let mut fresh = Fleet::open(profile(), dir("disk2"), &[4u8; 32]).unwrap();
        fresh.live = false;
        run(&mut fresh, &mut g2, DAY, 3);
        fresh.state().to_string()
    });
    // the generator carries on from the book after a restart
    let g = Gen::resume(&again, 7).unwrap();
    assert_eq!(g.day0, DAY);
}

#[test]
fn after_a_restart_the_generator_knows_how_much_gas_each_tank_holds() {
    // The 30-day run restarted the phone on day 15 and then missed a planted fuel bill: the
    // resumed generator had taken every tank as full, which hid its own planted fault. With
    // the tank worked out from the last fill, a run with a restart every five days scores clean.
    let d = dir("resume");
    let (mut planted, mut found) = (BTreeSet::new(), BTreeSet::new());
    for leg in 0..6 {
        let mut f = Fleet::open(profile(), d.clone(), &[4u8; 32]).unwrap();
        f.live = false;
        let mut g = Gen::resume(&f, 7).unwrap_or_else(|| Gen::new(&f.profile, DAY, 7));
        let (p, x, refused) = run(&mut f, &mut g, &add_days(DAY, leg * 5), 5);
        assert_eq!(refused, 0);
        planted.extend(p);
        found.extend(x);
        f.set_live(true).unwrap();
    }
    assert_eq!(planted, found);
    assert!(planted.iter().filter(|(_, t)| t.kind == "fuel_mismatch").count() >= 3, "{planted:?}");
}

#[test]
fn the_plan_judges_a_car_over_thirty_days_not_the_calendar_month() {
    // one car, a profitable October, then a service bill on 1 November
    let mut f = fleet("recent");
    for i in 0..20 {
        let day = add_days("2026-10-13", i);
        let mut t = trip(&format!("T{i}"), "Namma Yatri", &format!("{day}T09:00"), 2500.0, "online");
        t.dead_km = 0.0;
        f.add_trip(t).unwrap();
        f.add_shift(shift("C01", "D01", &day, 10, 41_200.0 + 8.0 * i as f64, 8.0)).unwrap();
    }
    f.add_expense("C01", "2026-11-01T18:00", rs(4200.0), "service").unwrap();
    f.tick("2026-11-01T22:00");
    let november = f.car_month("2026-11").into_iter().find(|m| m.id == "C01").unwrap();
    assert!(november.profit() < 0, "one day and a service bill");
    let recent = f.car_recent(30).into_iter().find(|m| m.id == "C01").unwrap();
    assert!(recent.profit() > 0 && recent.days_out == 20);
    assert!(!f.plan_text().contains("C01 ("), "{}", f.plan_text());
}
