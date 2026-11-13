//! The fleet as tools, behind the executor's tiers.
//! - Low: reading (the dashboard, cars, drivers' sheets, checks, payouts, plan, the month).
//! - Medium: recording riders' cash a driver handed in.
//! - High (approval key): handing a driver their earnings, and claiming from a platform. Both
//!   take the exact amount the book says, so a wrong figure is refused even after approval.
use crate::Fleet;
use agentos_action::{Action, Tier, Tool};
use agentos_business::util::parse_inr;
use agentos_executor::Connector;
use std::cell::RefCell;
use std::rc::Rc;

pub struct FleetTools(pub Rc<RefCell<Fleet>>);

fn arg<'a>(a: &'a Action, name: &str) -> Result<&'a str, String> {
    a.args.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).ok_or_else(|| format!("missing {name}"))
}
const TOOLS: &[(&str, &[&str], Tier)] = &[
    ("fleet_dashboard", &[], Tier::Low),
    ("fleet_cars", &[], Tier::Low),
    ("fleet_drivers", &[], Tier::Low),
    ("fleet_issues", &[], Tier::Low),
    ("fleet_payouts", &[], Tier::Low),
    ("fleet_plan", &[], Tier::Low),
    ("fleet_month", &["period"], Tier::Low),
    ("fleet_cash_in", &["driver", "amount"], Tier::Medium),
    ("fleet_pay_driver", &["driver", "amount"], Tier::High),
    ("fleet_claim", &["platform", "amount"], Tier::High),
];
impl Connector for FleetTools {
    fn name(&self) -> &str {
        "fleet"
    }
    fn tools(&self) -> Vec<Tool> {
        TOOLS.iter().map(|(name, args, tier)| Tool { name, args, tier: *tier }).collect()
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        let mut f = self.0.borrow_mut();
        match a.tool.as_str() {
            "fleet_dashboard" => Ok(f.dashboard()),
            "fleet_cars" => Ok(f.cars_text()),
            "fleet_drivers" => Ok(f.drivers_text()),
            "fleet_issues" => Ok(f.issues_text()),
            "fleet_payouts" => Ok(f.payouts_text()),
            "fleet_plan" => Ok(f.plan_text()),
            "fleet_month" => Ok(f.month_text(arg(a, "period")?)),
            "fleet_cash_in" => {
                let at = f.now().to_string();
                f.cash_in(arg(a, "driver")?, parse_inr(arg(a, "amount")?)?, &at)
            }
            "fleet_pay_driver" => f.pay_driver(arg(a, "driver")?, parse_inr(arg(a, "amount")?)?),
            "fleet_claim" => f.claim(arg(a, "platform")?, parse_inr(arg(a, "amount")?)?),
            other => Err(format!("the fleet cannot {other}")),
        }
    }
}
/// Phrases the owner can say with no model involved.
pub const INTENTS: &[(&str, &str)] = &[
    ("fleet dashboard", "fleet_dashboard"),
    ("fleet today", "fleet_dashboard"),
    ("cars today", "fleet_cars"),
    ("driver sheet", "fleet_drivers"),
    ("fleet issues", "fleet_issues"),
    ("fleet payouts", "fleet_payouts"),
    ("fleet plan", "fleet_plan"),
    ("fleet month {period}", "fleet_month"),
    ("driver cash {amount} from {driver}", "fleet_cash_in"),
    ("pay {amount} to driver {driver}", "fleet_pay_driver"),
    ("fleet claim {amount} from {platform}", "fleet_claim"),
];
pub fn describe(tool: &str) -> &'static str {
    match tool {
        "fleet_dashboard" => "the taxi fleet today in one view: cars out, trips, fares, what is kept, drivers, platforms, what needs the owner",
        "fleet_cars" => "each car today: trips, km, how busy, what it kept, fuel or charging",
        "fleet_drivers" => "each driver today: trips, hours, earnings due, riders' cash to hand in",
        "fleet_issues" => "the fleet's checks for today: cash held, odometer, fuel bills, hours, papers, service",
        "fleet_payouts" => "what the ride platforms paid against what their trips come to",
        "fleet_plan" => "tomorrow for the fleet: which platform pays when, charging, cars and drivers to see first",
        "fleet_month" => "each car's profit over a month (period like 2026-10), and electric against CNG",
        "fleet_cash_in" => "record riders' cash (rupees) a driver handed in",
        "fleet_pay_driver" => "hand a driver today's earnings (rupees, as the sheet says)",
        "fleet_claim" => "claim a payout shortfall (rupees) from a ride platform",
        _ => "",
    }
}
