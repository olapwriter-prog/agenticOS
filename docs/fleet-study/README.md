# A 15-car taxi fleet run from the phone: the 30-day study

`crates/fleet` lets agentd keep a taxi fleet's book from a profile file
(`examples/fleet/profile.toml`): trips from each ride platform, shifts with odometer readings,
fuel and charging bills, riders' cash handed in, platform payouts and repairs. From them, by
arithmetic and with no model:

- each driver's sheet (share of fares after the platform's cut, tips, riders' cash still held);
- each car's day and month (what it kept, energy, upkeep, loan, papers, profit), and electric
  against CNG;
- checks: cash held back, odometer beyond the trips, a CNG bill for more gas than the distance
  explains, shifts over the limit, a driver cancelling, a car idle with no reason, papers running
  out, service due, a platform paying short;
- tomorrow's plan: which platform pays at which hours, how the electric cars share the depot's
  chargers, which cars and drivers to see first.

Tools: `fleet_dashboard`, `fleet_cars`, `fleet_drivers`, `fleet_issues`, `fleet_payouts`,
`fleet_plan`, `fleet_month` (read); `fleet_cash_in` (record cash handed in); `fleet_pay_driver`
and `fleet_claim` (approval key, and only the exact amount the book says). Set `fleet_profile`
in agentd's config to turn them on.

Run the study (a few seconds):

```bash
cargo build --release -p agentd
```
```bash
python3 scripts/mac/fleet-sim.py
```

It invents 30 days (14 Oct to 12 Nov 2026) with problems planted at known places, scores the
checks against them day by day, has the owner close each day in fixed phrases (cash in, drivers
paid with the key), pays Uber weekly (once ₹2,400 short, which is claimed), has a client read
over MCP, and restarts the phone on day 15. Results: `target/fleet-sim/results.json`; the month,
plan and payouts as the owner sees them: `month-and-plan.txt` here (run of 13 Nov 2026).

## What is simulated, and what is assumed

The fleet, its drivers and its trips are invented by a seeded generator; no rider appears in
any event. Uber and Namma Yatri feeds, the drivers' app and the payouts are simulated: nothing
was read from or sent to a real service, and a claim is only recorded.

Every figure in the profile is an assumption: car prices and instalments, km per kg and per
kWh, CNG and electricity prices, Uber's 25% commission with GST, Namma Yatri's flat daily fee,
the fares, the drivers' 35% share. The results follow from them:

- An electric car left about ₹437 a day more than a CNG car before its loan, and paid back its
  extra ₹4.3 lakh in about 32 months. This depends on the cars running about 145 km a day,
  which the batteries covered with few public charges.
- Namma Yatri paid more per hour than Uber at every time of day, because the assumed fares are
  similar and Uber keeps a quarter. With real surge data this may not hold.

## Limits

- The fuel check needs CNG bills that fill the tank and carry the odometer; it does not check
  electric cars.
- The checks see what the book holds. A driver's position is not tracked, so personal use shows
  only as odometer distance beyond the trips.
- Only fixed phrases work ("fleet dashboard", "pay 800 to driver D03"); the classifier was not
  taught the fleet's wording.
- GST on rides is not worked out.
