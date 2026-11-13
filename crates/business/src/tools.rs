//! The business as tools, so chat, intents, the model and an MCP client all reach it the same
//! way, through the executor's tiers.
//!
//! - Low: reading. Every answer is masked, so it is safe to show an MCP client.
//! - Medium: changes to the book, and the two messages whose recipient and content are fixed by
//!   code (a customer's own bill, their own rider's position). These are the owner's standing
//!   rules; they cannot be pointed at anyone else.
//! - High (approval key): money handed over, anything lodged with a platform or the tax portal,
//!   and erasing customers.
use crate::util::{add_days, parse_inr};
use serde_json::{json, Value};
use crate::Business;
use agentos_action::{Action, Tier, Tool};
use agentos_executor::Connector;
use std::cell::RefCell;
use std::rc::Rc;

pub struct BusinessTools(pub Rc<RefCell<Business>>);

fn arg<'a>(a: &'a Action, name: &str) -> Result<&'a str, String> {
    a.args.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).ok_or_else(|| format!("missing {name}"))
}

const TOOLS: &[(&str, &[&str], Tier)] = &[
    ("biz_dashboard", &[], Tier::Low),
    ("biz_orders", &[], Tier::Low),
    ("biz_plan", &[], Tier::Low),
    ("biz_gst", &["period"], Tier::Low),
    ("biz_riders", &[], Tier::Low),
    ("biz_payouts", &[], Tier::Low),
    ("biz_privacy", &[], Tier::Low),
    ("biz_issues", &[], Tier::Low),
    ("biz_status", &["order", "status"], Tier::Medium),
    ("biz_assign", &["order", "rider"], Tier::Medium),
    ("biz_send_bill", &["order"], Tier::Medium),
    ("biz_share_location", &["order"], Tier::Medium),
    ("biz_purchase", &["supplier", "item", "amount", "gst"], Tier::Medium),
    ("biz_cash_in", &["rider", "amount"], Tier::Medium),
    ("biz_pay_rider", &["rider", "amount"], Tier::High),
    ("biz_claim", &["platform", "amount"], Tier::High),
    ("biz_file_gst", &["period"], Tier::High),
    ("biz_forget", &["days"], Tier::High),
];

/// True for the tools that only read (their answers can be prepared ahead of time).
pub fn reads_only(tool: &str) -> bool {
    TOOLS.iter().any(|(n, _, t)| *n == tool && *t == Tier::Low)
}

impl Connector for BusinessTools {
    fn name(&self) -> &str {
        "business"
    }
    fn tools(&self) -> Vec<Tool> {
        TOOLS.iter().map(|(name, args, tier)| Tool { name, args, tier: *tier }).collect()
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        let mut b = self.0.borrow_mut();
        match a.tool.as_str() {
            "biz_dashboard" => Ok(b.dashboard()),
            "biz_orders" => Ok(b.orders_text()),
            "biz_plan" => Ok(crate::plan::plan(&b).text()),
            "biz_gst" => Ok(b.gst_text(arg(a, "period")?)),
            "biz_riders" => Ok(b.riders_text()),
            "biz_payouts" => Ok(b.payouts_text()),
            "biz_privacy" => Ok(b.privacy_text()),
            "biz_issues" => {
                let i = b.issues();
                Ok(if i.is_empty() { "nothing needs you right now".into() } else { i.join("\n") })
            }
            "biz_status" => b.set_status(arg(a, "order")?, arg(a, "status")?, None),
            "biz_assign" => b.assign(arg(a, "order")?, arg(a, "rider")?),
            "biz_send_bill" => b.send_bill(arg(a, "order")?),
            "biz_share_location" => b.share_location(arg(a, "order")?),
            "biz_purchase" => {
                let (amount, gst) = (parse_inr(arg(a, "amount")?)?, parse_inr(arg(a, "gst")?)?);
                b.purchase(arg(a, "supplier")?, arg(a, "item")?, amount, gst)
            }
            "biz_cash_in" => b.cash_in(arg(a, "rider")?, parse_inr(arg(a, "amount")?)?),
            "biz_pay_rider" => b.pay_rider(arg(a, "rider")?, parse_inr(arg(a, "amount")?)?),
            "biz_claim" => b.claim(arg(a, "platform")?, parse_inr(arg(a, "amount")?)?),
            "biz_file_gst" => b.file_gst(arg(a, "period")?),
            "biz_forget" => {
                let days: i64 = arg(a, "days")?.trim().parse().map_err(|_| "days is a number".to_string())?;
                b.forget(days)
            }
            other => Err(format!("business cannot {other}")),
        }
    }
}

/// Phrases the owner can say with no model involved: (pattern, tool).
pub const INTENTS: &[(&str, &str)] = &[
    ("dashboard", "biz_dashboard"),
    ("today's dashboard", "biz_dashboard"),
    ("how is business today", "biz_dashboard"),
    ("today's orders", "biz_orders"),
    ("show orders", "biz_orders"),
    ("plan for tomorrow", "biz_plan"),
    ("tomorrow's plan", "biz_plan"),
    ("file gst for {period}", "biz_file_gst"),
    ("gst for {period}", "biz_gst"),
    ("rider settlement", "biz_riders"),
    ("rider sheet", "biz_riders"),
    ("platform payouts", "biz_payouts"),
    ("privacy report", "biz_privacy"),
    ("what needs me", "biz_issues"),
    ("any issues", "biz_issues"),
    ("mark {order} as {status}", "biz_status"),
    ("assign {order} to {rider}", "biz_assign"),
    ("send bill for {order}", "biz_send_bill"),
    ("share location for {order}", "biz_share_location"),
    ("bought {item} from {supplier} for {amount} gst {gst}", "biz_purchase"),
    ("cash {amount} from {rider}", "biz_cash_in"),
    ("pay {amount} to rider {rider}", "biz_pay_rider"),
    ("claim {amount} from {platform}", "biz_claim"),
    ("forget customers older than {days} days", "biz_forget"),
];

/// One line per tool for the model's prompt.
pub fn describe(tool: &str) -> &'static str {
    match tool {
        "biz_dashboard" => "today's business in one view: orders, sales, riders, platforms, GST",
        "biz_orders" => "today's orders (customers masked)",
        "biz_plan" => "tomorrow's plan: dishes to expect, what to buy, rush hours",
        "biz_gst" => "GST figures for a month (period like 2026-11) with input tax credit",
        "biz_riders" => "what each delivery rider earned and holds today",
        "biz_payouts" => "what the food platforms paid against what they owe; what to claim",
        "biz_privacy" => "what customer data is kept and what left the phone today",
        "biz_issues" => "what needs the owner's attention right now",
        "biz_status" => "set an order's status: preparing, out, delivered or cancelled",
        "biz_assign" => "give a direct order to a rider",
        "biz_send_bill" => "send the bill PDF to the customer of that order on WhatsApp",
        "biz_share_location" => "send the rider's position to the customer of that order",
        "biz_purchase" => "record a purchase with its GST",
        "biz_cash_in" => "record cash-on-delivery money a rider handed in",
        "biz_pay_rider" => "hand a rider today's earnings (amount in rupees) and send the payslip",
        "biz_claim" => "claim a payout shortfall (rupees) from a platform",
        "biz_file_gst" => "prepare and lodge the GST return for a finished month",
        "biz_forget" => "erase customers who have not ordered for that many days",
        _ => "",
    }
}

/// How owners ask, for the classifier. It learns more from every request the owner confirms.
pub const SEEDS: &[(&str, &[&str])] = &[
    ("biz_dashboard", &["how is business today", "show me the dashboard", "how are sales today", "today's numbers", "how did the shop do today", "business summary"]),
    ("biz_orders", &["show today's orders", "list the orders", "which orders came in", "what orders are open", "order list"]),
    ("biz_plan", &["plan for tomorrow", "what should i prepare tomorrow", "what do i need to buy for tomorrow", "tomorrow's forecast", "how much to cook tomorrow"]),
    ("biz_gst", &["gst this month", "how much gst do i owe", "show the gst figures", "input tax credit this month", "gst summary for october"]),
    ("biz_file_gst", &["file the gst return", "file gst for last month", "submit the gst return", "lodge gstr for october"]),
    ("biz_riders", &["rider settlement", "what do i owe the delivery boys", "delivery boys earnings today", "how much cash do the riders have", "rider sheet"]),
    ("biz_payouts", &["platform payouts", "did swiggy pay correctly", "what does zomato owe me", "reconcile the platform payments", "check the payouts"]),
    ("biz_privacy", &["privacy report", "what customer data do we keep", "what left the phone today", "who saw my customers' details"]),
    ("biz_issues", &["what needs me", "any issues", "anything wrong today", "what should i look at", "problems right now"]),
    ("biz_status", &["mark the order as delivered", "order is out for delivery", "cancel the order", "order delivered", "set the order to preparing"]),
    ("biz_assign", &["assign the order to the rider", "give the order to ravi", "send suresh with the order", "rider for the order"]),
    ("biz_send_bill", &["send the bill for the order", "resend the invoice to the customer", "bill the customer for the order"]),
    ("biz_share_location", &["share the rider's location for the order", "send live location to the customer", "where is the order tell the customer"]),
    ("biz_pay_rider", &["pay the rider", "settle ravi for today", "hand over today's earnings to the delivery boy", "pay suresh his earnings"]),
    ("biz_cash_in", &["rider handed in the cash", "took the cash from ravi", "cash collected from the rider", "the rider gave the cod cash", "received the cash on delivery money from the delivery boy"]),
    ("biz_claim", &["claim the shortfall from zomato", "raise a claim with swiggy", "claim the missing payout", "file a claim with the platform"]),
    ("biz_forget", &["forget old customers", "erase customers older than a month", "delete old customer data"]),
];

/// A number the owner said as a word of its own ("526", "₹1,430.50"); not the 1 in "R1".
fn first_number(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| c == '₹' || c == ',' || c == '.' || c == '?' || c == '!').replace(',', ""))
        .find(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_digit() || c == '.') && w.parse::<f64>().is_ok())
}
const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

impl Business {
    fn order_in(&self, text: &str) -> Option<String> {
        text.split(|c: char| c.is_whitespace() || c == ',' || c == '?')
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-'))
            .find(|w| !w.is_empty() && self.order(w).is_ok())
            .map(|w| self.order(w).unwrap().id.clone())
    }
    fn rider_in(&self, text: &str) -> Option<String> {
        let l = text.to_lowercase();
        let words: Vec<&str> = l.split(|c: char| !c.is_alphanumeric()).collect();
        self.profile
            .riders
            .iter()
            .find(|r| words.contains(&r.name.to_lowercase().as_str()) || words.contains(&r.id.to_lowercase().as_str()))
            .map(|r| r.id.clone())
    }
    fn platform_in(&self, text: &str) -> Option<String> {
        let l = text.to_lowercase();
        self.profile.platforms.iter().map(|p| p.name.to_lowercase()).find(|n| l.contains(n.as_str()))
    }
    /// "2026-10", "october", "last month", "this month"; `default_last` picks when nothing is said.
    fn period_in(&self, text: &str, default_last: bool) -> String {
        let today = self.today();
        let this = today.get(..7).unwrap_or("1970-01").to_string();
        let last = add_days(&format!("{this}-01"), -1)[..7].to_string();
        let l = text.to_lowercase();
        if let Some(p) = l.split(|c: char| !(c.is_ascii_digit() || c == '-')).find(|w| w.len() == 7 && w.as_bytes()[4] == b'-') {
            return p.to_string();
        }
        for (i, m) in MONTHS.iter().enumerate() {
            if l.split(|c: char| !c.is_alphabetic()).any(|w| w.starts_with(m) && w.len() >= 3 && (w.len() == 3 || "january february march april may june july august september october november december".contains(w))) {
                // the most recent such month that is not in the future
                let y: i32 = this[..4].parse().unwrap_or(1970);
                let cand = format!("{y}-{:02}", i + 1);
                return if cand <= this { cand } else { format!("{}-{:02}", y - 1, i + 1) };
            }
        }
        if l.contains("last month") || l.contains("previous month") {
            return last;
        }
        if l.contains("this month") {
            return this;
        }
        if default_last { last } else { this }
    }

    /// Make a money action's amount current. The amount is the number the owner said in this
    /// request; if they said none ("settle ravi", "claim it from zomato"), it is what the book
    /// says is due now. An action remembered from another day, or a phrase pattern that took a
    /// word for the amount, therefore cannot carry a wrong figure to the approval screen.
    pub fn fresh_args(&self, tool: &str, args: &mut [(String, String)], text: &str) {
        let get = |k: &str| args.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
        let today = self.today();
        let book = match tool {
            "biz_pay_rider" | "biz_cash_in" => self.profile.rider(&get("rider")).and_then(|r| {
                let s = self.rider_sheet(&today).into_iter().find(|s| s.id == r.id)?;
                Some(if tool == "biz_pay_rider" { s.due } else { s.cash_held })
            }),
            "biz_claim" => self.profile.platform(&get("platform")).map(|p| self.claimable(&p.name.to_lowercase()).0),
            _ => return,
        };
        let amount = match (first_number(text), book) {
            (Some(said), _) => said,
            (None, Some(p)) if p > 0 => format!("{}", p as f64 / 100.0),
            _ => return,
        };
        if let Some(a) = args.iter_mut().find(|(n, _)| n == "amount") {
            a.1 = amount;
        }
    }

    /// Turn a request the classifier recognised into a tool call by rules. None when a detail
    /// is missing; then the model is asked. Amounts the owner leaves out are read from the book
    /// (and still shown on the approval screen).
    pub fn command(&self, label: &str, text: &str) -> Option<Value> {
        let rupees = |p: i64| format!("{}", p as f64 / 100.0);
        let args = match label {
            "biz_dashboard" | "biz_orders" | "biz_plan" | "biz_riders" | "biz_payouts" | "biz_privacy" | "biz_issues" => json!({}),
            "biz_gst" => json!({"period": self.period_in(text, false)}),
            "biz_file_gst" => json!({"period": self.period_in(text, true)}),
            "biz_send_bill" | "biz_share_location" => json!({"order": self.order_in(text)?}),
            "biz_assign" => json!({"order": self.order_in(text)?, "rider": self.rider_in(text)?}),
            "biz_status" => {
                let l = text.to_lowercase();
                let status = ["delivered", "cancel", "out", "preparing", "ready", "picked"].iter().find(|s| l.contains(**s))?;
                json!({"order": self.order_in(text)?, "status": if *status == "cancel" { "cancelled" } else { status }})
            }
            "biz_pay_rider" => {
                let rider = self.rider_in(text)?;
                let due = self.rider_sheet(&self.today()).into_iter().find(|s| s.id == rider)?.due;
                json!({"rider": rider, "amount": first_number(text).unwrap_or_else(|| rupees(due))})
            }
            "biz_cash_in" => {
                let rider = self.rider_in(text)?;
                let held = self.rider_sheet(&self.today()).into_iter().find(|s| s.id == rider)?.cash_held;
                json!({"rider": rider, "amount": first_number(text).unwrap_or_else(|| rupees(held))})
            }
            "biz_claim" => {
                let platform = self.platform_in(text)?;
                let can = self.claimable(&platform).0;
                json!({"platform": platform, "amount": first_number(text).unwrap_or_else(|| rupees(can))})
            }
            "biz_forget" => json!({"days": first_number(text).unwrap_or_else(|| self.profile.retain_customer_days.to_string())}),
            _ => return None,
        };
        Some(json!({"tool": label, "args": args}))
    }
}
