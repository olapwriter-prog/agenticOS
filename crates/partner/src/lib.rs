//! The phone as a business partner and a worker, and a help in personal life.
//!
//! One connector, `Partner`, with tools in five areas:
//!   channels  WhatsApp Business and email as one inbox, with a reply drafted for each thread
//!   books     customers, a catalog, GST invoices, payments received, GSTR-1/3B, filing, bills
//!   wallet    balance, payments, history
//!   shop      order history, the best price across platforms for what you usually buy, buying
//!   life      news ranked by your interests, suggestions (films, music, adventures, events), booking
//!
//! Anything that sends, pays or files is High tier: the agent prepares it, the human presses the
//! key. Every answer is a small UI document (a screen to show) with a text fallback (for a phone
//! without a screen); see `ui`.
//!
//! The outside systems are reached over HTTP at `Config::world_url`, with request shapes taken
//! from the real services (WhatsApp Cloud API messages, a GST Suvidha Provider's returns, a payout
//! API, RSS). `agentos-phonesim world` serves them all for development and tests; a real account
//! is a URL and a token, not new code.
mod books;
mod channels;
mod life;
mod shop;
pub use agentos_screen as ui;
mod wallet;

use agentos_action::{Action, Tier, Tool};
use agentos_executor::Connector;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub use ui::Ui;

/// The business and the person, from agentd.toml's `[partner]` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Where the outside services are (the simulator, or a gateway to the real ones).
    pub world_url: String,
    /// Sent as a bearer token to every service, if set.
    pub token: Option<String>,
    pub business_name: String,
    /// The business's GSTIN; its first two digits are the state code.
    pub gstin: String,
    /// UPI address customers pay to (on invoices).
    pub vpa: String,
    /// WhatsApp Business phone number id (Cloud API).
    pub wa_phone_id: String,
    pub city: String,
    /// What the person cares about, to start with (learned from then on).
    pub interests: Vec<String>,
    /// Shopping platforms to compare.
    pub platforms: Vec<String>,
    /// News feeds (names under the world's /news/rss/, or full URLs).
    pub feeds: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            world_url: "http://127.0.0.1:18900".into(),
            token: None,
            business_name: "My Business".into(),
            gstin: "29AAACM1234A1Z4".into(),
            vpa: "mybusiness@upi".into(),
            wa_phone_id: "100200300".into(),
            city: "Bengaluru".into(),
            interests: vec![],
            platforms: vec!["shopkart".into(), "bazaarnow".into(), "quickmart".into()],
            feeds: vec!["business".into(), "tech".into(), "travel".into(), "music".into(), "movies".into(), "sports".into()],
        }
    }
}

/// Everything the partner keeps, in one JSON file in agentd's data directory.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Data {
    pub customers: Vec<books::Customer>,
    pub items: Vec<books::Item>,
    pub invoices: Vec<books::Invoice>,
    pub expenses: Vec<books::Expense>,
    pub filings: Vec<books::Filing>,
    pub payments: Vec<wallet::Payment>,
    pub orders: Vec<shop::Order>,
    pub bookings: Vec<life::Booking>,
    /// topic -> weight, learned from what the person reads, books and says they like
    pub interests: BTreeMap<String, f64>,
    /// day number the interests were last decayed on
    pub interests_day: i64,
    /// threads already answered: thread id -> id of the last message answered
    pub answered: BTreeMap<String, String>,
    /// the last comparison's options, so "buy best" means what was shown
    pub last_options: Vec<shop::Option_>,
}

pub struct Partner {
    pub cfg: Config,
    pub data: Data,
    path: Option<PathBuf>,
    /// today's date, for tests; None = the clock
    pub today: Option<Date>,
}

impl Partner {
    /// Load the partner's data from `path` (created on first save).
    pub fn open(cfg: Config, path: PathBuf) -> Result<Partner, String> {
        let data = match std::fs::read_to_string(&path) {
            Ok(t) => serde_json::from_str(&t).map_err(|e| format!("{}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Data::default(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let mut p = Partner { cfg, data, path: Some(path), today: None };
        p.seed_interests();
        Ok(p)
    }

    pub fn in_memory(cfg: Config) -> Partner {
        let mut p = Partner { cfg, data: Data::default(), path: None, today: None };
        p.seed_interests();
        p
    }

    fn seed_interests(&mut self) {
        for t in self.cfg.interests.clone() {
            self.data.interests.entry(t.to_lowercase()).or_insert(3.0);
        }
    }

    fn save(&self) -> Result<(), String> {
        let Some(p) = &self.path else { return Ok(()) };
        let tmp = p.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&self.data).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, p).map_err(|e| e.to_string())
    }

    pub fn today(&self) -> Date {
        self.today.unwrap_or_else(Date::now)
    }

    // ---- the outside world, over HTTP ----

    fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            return path.to_string();
        }
        format!("{}{}", self.cfg.world_url.trim_end_matches('/'), path)
    }

    fn auth(&self, r: ureq::Request) -> ureq::Request {
        let r = r.timeout(std::time::Duration::from_secs(20));
        match &self.cfg.token {
            Some(t) => r.set("Authorization", &format!("Bearer {t}")),
            None => r,
        }
    }

    pub(crate) fn get(&self, path: &str) -> Result<Value, String> {
        let url = self.url(path);
        self.auth(ureq::get(&url))
            .call()
            .map_err(|e| http_err(&url, e))?
            .into_json()
            .map_err(|e| format!("{url}: {e}"))
    }

    pub(crate) fn get_text(&self, path: &str) -> Result<String, String> {
        let url = self.url(path);
        self.auth(ureq::get(&url)).call().map_err(|e| http_err(&url, e))?.into_string().map_err(|e| format!("{url}: {e}"))
    }

    pub(crate) fn post(&self, path: &str, body: &Value) -> Result<Value, String> {
        let url = self.url(path);
        self.auth(ureq::post(&url))
            .send_json(body.clone())
            .map_err(|e| http_err(&url, e))?
            .into_json()
            .map_err(|e| format!("{url}: {e}"))
    }
}

fn http_err(url: &str, e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, r) => {
            let body = r.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().or(v["error"].as_str()).map(String::from))
                .unwrap_or(body);
            format!("{url}: {code} {msg}")
        }
        e => format!("{url}: {e}"),
    }
}

// ---- tools -----------------------------------------------------------------------------------

const fn t(name: &'static str, args: &'static [&'static str], tier: Tier) -> Tool {
    Tool { name, args, tier }
}

pub const TOOLS: &[Tool] = &[
    // channels
    t("inbox", &[], Tier::Low),
    t("read_thread", &["who"], Tier::Low),
    t("reply", &["to", "text"], Tier::High),
    // books
    t("add_customer", &["name", "gstin", "phone"], Tier::Medium),
    t("add_item", &["name", "price", "gst"], Tier::Medium),
    t("invoice", &["customer", "items"], Tier::Medium),
    t("send_invoice", &["number"], Tier::High),
    t("mark_paid", &["number"], Tier::Medium),
    t("invoices", &[], Tier::Low),
    t("gst_summary", &["period"], Tier::Low),
    t("file_gst", &["form", "period"], Tier::High),
    t("bills", &[], Tier::Low),
    t("pay_bill", &["id"], Tier::High),
    t("expenses", &["period"], Tier::Low),
    // wallet
    t("wallet", &[], Tier::Low),
    t("pay", &["to", "amount", "note"], Tier::High),
    // shop
    t("orders", &[], Tier::Low),
    t("best_price", &["item"], Tier::Low),
    t("buy", &["option"], Tier::High),
    // life
    t("news", &["topic"], Tier::Low),
    t("interests", &[], Tier::Low),
    t("like", &["topic"], Tier::Medium),
    t("suggest", &["kind"], Tier::Low),
    t("book", &["event", "qty"], Tier::High),
];

/// What each tool does, for the model's system prompt.
pub fn describe(tool: &str) -> &'static str {
    match tool {
        "inbox" => "WhatsApp Business and email threads waiting for a reply, with a drafted reply each",
        "read_thread" => "one customer's or person's messages, and a drafted reply",
        "reply" => "send a reply on WhatsApp (or email if that is where they wrote)",
        "add_customer" => "save a customer; gstin may be 'none' for a consumer",
        "add_item" => "add a product or service to the catalog: price in rupees before GST, gst rate in %",
        "invoice" => "make a GST invoice; items like '2 x Widget, 1 x Repair @ 1500'",
        "send_invoice" => "send an invoice to the customer on WhatsApp or email, with a UPI payment link",
        "mark_paid" => "record that an invoice was paid",
        "invoices" => "invoices and who still owes what (payments received are matched automatically)",
        "gst_summary" => "GSTR-1 and GSTR-3B figures for a month: tax collected, input credit, tax to pay",
        "file_gst" => "file GSTR-1 or GSTR-3B for a month through the GST Suvidha Provider",
        "bills" => "bills received by email, with due dates",
        "pay_bill" => "pay a bill from the wallet (its GST becomes input credit)",
        "expenses" => "what was spent in a month, by supplier",
        "wallet" => "wallet balance and recent payments",
        "pay" => "pay an amount to a UPI address",
        "orders" => "past orders on every shopping platform, and what is bought often",
        "best_price" => "compare an item across shopping platforms and show the best option",
        "buy" => "buy the option shown by best_price ('best' or platform:id)",
        "news" => "news ranked by the person's interests ('any' or a topic)",
        "interests" => "what the person is interested in, as learned",
        "like" => "tell the agent the person likes a topic",
        "suggest" => "things to do: films, music, adventures, events ('any' or a kind), by interests",
        "book" => "book an event shown by suggest, for a number of people",
        _ => "",
    }
}

fn arg<'a>(a: &'a Action, name: &str) -> Result<&'a str, String> {
    a.args.iter().find(|(k, _)| k == name).map(|(_, v)| v.trim()).ok_or_else(|| format!("missing {name}"))
}
fn arg_or<'a>(a: &'a Action, name: &str, default: &'a str) -> &'a str {
    a.args.iter().find(|(k, _)| k == name).map(|(_, v)| v.trim()).filter(|v| !v.is_empty()).unwrap_or(default)
}

impl Connector for Partner {
    fn name(&self) -> &str {
        "partner"
    }
    fn tools(&self) -> Vec<Tool> {
        TOOLS.to_vec()
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        let r = match a.tool.as_str() {
            "inbox" => channels::inbox(self),
            "read_thread" => channels::read_thread(self, arg(a, "who")?),
            "reply" => channels::reply(self, arg(a, "to")?, arg(a, "text")?),
            "add_customer" => books::add_customer(self, arg(a, "name")?, arg_or(a, "gstin", "none"), arg_or(a, "phone", "")),
            "add_item" => books::add_item(self, arg(a, "name")?, arg(a, "price")?, arg_or(a, "gst", "18")),
            "invoice" => books::invoice(self, arg(a, "customer")?, arg(a, "items")?),
            "send_invoice" => books::send_invoice(self, arg(a, "number")?),
            "mark_paid" => books::mark_paid(self, arg(a, "number")?),
            "invoices" => books::invoices(self),
            "gst_summary" => books::gst_summary(self, arg_or(a, "period", "this month")),
            "file_gst" => books::file_gst(self, arg(a, "form")?, arg_or(a, "period", "last month")),
            "bills" => books::bills(self),
            "pay_bill" => books::pay_bill(self, arg(a, "id")?),
            "expenses" => books::expenses(self, arg_or(a, "period", "this month")),
            "wallet" => wallet::wallet(self),
            "pay" => wallet::pay_tool(self, arg(a, "to")?, arg(a, "amount")?, arg_or(a, "note", "payment")),
            "orders" => shop::orders(self),
            "best_price" => shop::best_price(self, arg(a, "item")?),
            "buy" => shop::buy(self, arg(a, "option")?),
            "news" => life::news(self, arg_or(a, "topic", "any")),
            "interests" => life::interests(self),
            "like" => life::like(self, arg(a, "topic")?),
            "suggest" => life::suggest(self, arg_or(a, "kind", "any")),
            "book" => life::book(self, arg(a, "event")?, arg_or(a, "qty", "1")),
            other => return Err(format!("partner cannot {other}")),
        }?;
        self.save()?;
        Ok(r.to_reply())
    }
}

// ---- money and dates -----------------------------------------------------------------------

/// Paise to "₹1,23,456.78" (Indian digit grouping).
pub fn inr(paise: i64) -> String {
    let neg = paise < 0;
    let p = paise.unsigned_abs();
    let (r, ps) = (p / 100, p % 100);
    let s = r.to_string();
    let grouped = if s.len() <= 3 {
        s
    } else {
        let (head, last3) = s.split_at(s.len() - 3);
        let mut parts: Vec<String> = Vec::new();
        let mut h = head.to_string();
        while h.len() > 2 {
            parts.insert(0, h[h.len() - 2..].to_string());
            h.truncate(h.len() - 2);
        }
        if !h.is_empty() {
            parts.insert(0, h);
        }
        format!("{},{last3}", parts.join(","))
    };
    format!("{}₹{grouped}.{ps:02}", if neg { "-" } else { "" })
}

/// "1,234.50", "₹ 4,720", "Rs. 99" -> paise.
pub fn parse_inr(s: &str) -> Option<i64> {
    let cleaned: String = s.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
    let cleaned = cleaned.trim_start_matches('.');
    if cleaned.is_empty() {
        return None;
    }
    let (r, p) = match cleaned.split_once('.') {
        Some((r, p)) => (r, p),
        None => (cleaned, ""),
    };
    let rupees: i64 = if r.is_empty() { 0 } else { r.parse().ok()? };
    let mut ps: String = p.chars().take(2).collect();
    while ps.len() < 2 {
        ps.push('0');
    }
    Some(rupees * 100 + ps.parse::<i64>().ok()?)
}

/// A calendar date (proleptic Gregorian).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Date {
    pub y: i32,
    pub m: u32,
    pub d: u32,
}

impl Date {
    pub fn now() -> Date {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        // India: UTC+5:30
        Date::from_days(((secs + 19_800) / 86_400) as i64)
    }
    /// Days since 1970-01-01 to a date (Howard Hinnant's civil_from_days).
    pub fn from_days(z: i64) -> Date {
        let z = z + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        Date { y: (y + i64::from(m <= 2)) as i32, m, d }
    }
    pub fn days(self) -> i64 {
        let y = self.y as i64 - i64::from(self.m <= 2);
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let mp = (self.m as i64 + 9) % 12;
        let doy = (153 * mp + 2) / 5 + self.d as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }
    pub fn parse(s: &str) -> Option<Date> {
        let p: Vec<&str> = s.trim().split(['-', '/']).collect();
        if p.len() != 3 {
            return None;
        }
        let (a, b, c): (i32, u32, u32) = (p[0].parse().ok()?, p[1].parse().ok()?, p[2].parse().ok()?);
        let d = if a > 31 { Date { y: a, m: b, d: c } } else { Date { y: c as i32, m: b, d: a as u32 } };
        (1..=12).contains(&d.m).then_some(d).filter(|d| (1..=31).contains(&d.d))
    }
    pub fn add_days(self, n: i64) -> Date {
        Date::from_days(self.days() + n)
    }
    /// The Indian financial year this date is in, as "2026-27".
    pub fn fy(self) -> String {
        let start = if self.m >= 4 { self.y } else { self.y - 1 };
        format!("{start}-{:02}", (start + 1) % 100)
    }
}

impl std::fmt::Display for Date {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.y, self.m, self.d)
    }
}

const MONTHS: [&str; 12] = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];

/// A month: "this month", "last month", "2026-11", "november", "nov 2026" -> (year, month).
pub fn parse_period(s: &str, today: Date) -> Result<(i32, u32), String> {
    let s = s.trim().to_lowercase();
    let prev = |y: i32, m: u32| if m == 1 { (y - 1, 12) } else { (y, m - 1) };
    if s.is_empty() || s == "this month" || s == "now" || s == "current" {
        return Ok((today.y, today.m));
    }
    if s == "last month" || s == "previous month" {
        return Ok(prev(today.y, today.m));
    }
    if let Some((y, m)) = s.split_once('-') {
        if let (Ok(y), Ok(m)) = (y.parse::<i32>(), m.parse::<u32>()) {
            if (1..=12).contains(&m) {
                return Ok((y, m));
            }
        }
    }
    for (i, name) in MONTHS.iter().enumerate() {
        let m = i as u32 + 1;
        if s.starts_with(&name[..3]) {
            let year = s.split_whitespace().find_map(|w| w.parse::<i32>().ok()).unwrap_or(if m <= today.m { today.y } else { today.y - 1 });
            return Ok((year, m));
        }
    }
    Err(format!("which month is {s:?}? say 'last month', 'november' or '2026-11'"))
}

pub fn month_name(y: i32, m: u32) -> String {
    let n = MONTHS[(m - 1) as usize];
    format!("{}{} {y}", n[..1].to_uppercase(), &n[1..])
}

#[cfg(test)]
mod tests;
