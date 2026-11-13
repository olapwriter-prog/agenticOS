//! The store's books as tools.
//! - Low: reading the audit, the returns' figures and the year. Totals and invoice numbers only.
//! - Medium: proposing a fix. Anyone connected may propose (a Claude client included); a
//!   proposal changes nothing in the books.
//! - High (approval key): applying proposed fixes, and lodging a GST or income-tax return.
use crate::books::Books;
use agentos_action::{Action, Tier, Tool};
use agentos_executor::Connector;
use std::cell::RefCell;
use std::rc::Rc;

pub struct RetailTools(pub Rc<RefCell<Books>>);

fn arg<'a>(a: &'a Action, name: &str) -> Result<&'a str, String> {
    a.args.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).ok_or_else(|| format!("missing {name}"))
}
const TOOLS: &[(&str, &[&str], Tier)] = &[
    ("shop_audit", &["period"], Tier::Low),
    ("shop_exceptions", &["period", "kind"], Tier::Low),
    ("shop_gst", &["period"], Tier::Low),
    ("shop_year", &[], Tier::Low),
    ("shop_propose_fix", &["period", "kind", "fix"], Tier::Medium),
    ("shop_apply_fixes", &["period"], Tier::High),
    ("shop_file_gst", &["period"], Tier::High),
    ("shop_file_income_tax", &[], Tier::High),
];
impl Connector for RetailTools {
    fn name(&self) -> &str {
        "retail"
    }
    fn tools(&self) -> Vec<Tool> {
        TOOLS.iter().map(|(name, args, tier)| Tool { name, args, tier: *tier }).collect()
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        let mut b = self.0.borrow_mut();
        match a.tool.as_str() {
            "shop_audit" => Ok(b.audit_text(arg(a, "period")?.trim())),
            "shop_exceptions" => Ok(b.exceptions_text(arg(a, "period")?.trim(), arg(a, "kind")?.trim())),
            "shop_gst" => Ok(b.gst_text(arg(a, "period")?.trim())),
            "shop_year" => Ok(b.year_text()),
            "shop_propose_fix" => b.propose(arg(a, "period")?.trim(), arg(a, "kind")?.trim(), arg(a, "fix")?.trim(), "a connected client or the owner"),
            "shop_apply_fixes" => b.apply(arg(a, "period")?.trim()),
            "shop_file_gst" => b.file_gst(arg(a, "period")?),
            "shop_file_income_tax" => b.file_itr(),
            other => Err(format!("the store cannot {other}")),
        }
    }
}
pub const INTENTS: &[(&str, &str)] = &[
    ("audit {period}", "shop_audit"),
    ("show {kind} exceptions in {period}", "shop_exceptions"),
    ("store gst for {period}", "shop_gst"),
    ("year tally", "shop_year"),
    ("income tax for the year", "shop_year"),
    ("propose {fix} for {kind} in {period}", "shop_propose_fix"),
    ("apply fixes for {period}", "shop_apply_fixes"),
    ("file store gst for {period}", "shop_file_gst"),
    ("file income tax", "shop_file_income_tax"),
];
pub fn describe(tool: &str) -> &'static str {
    match tool {
        "shop_audit" => "the audit of the store's bills for a day (2025-06-14), a month (2025-06) or the year (2025 or 2026): exceptions by kind and their tax effect",
        "shop_exceptions" => "list the exceptions of one kind in a period, with invoice numbers",
        "shop_gst" => "the store's GST figures for a month: output tax by rate, input tax credit, cash to pay",
        "shop_year" => "the year's tally: turnover, rent and other expenses, profit, income tax",
        "shop_propose_fix" => "propose a fix for every open exception of a kind in a period (changes nothing until approved)",
        "shop_apply_fixes" => "apply the fixes proposed for a period",
        "shop_file_gst" => "lodge the store's GST return for a finished month",
        "shop_file_income_tax" => "lodge the income-tax computation for the finished year",
        _ => "",
    }
}
