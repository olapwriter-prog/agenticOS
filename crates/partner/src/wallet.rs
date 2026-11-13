//! The wallet: a balance, payments out by UPI (through a payout API), and a record of each. Every
//! payment anywhere in the partner (a bill, a purchase, a booking, a transfer) goes through
//! `payout`, so the balance is checked and the payment recorded the same way.
use crate::ui::{Card, Ui};
use crate::{inr, parse_inr, Date, Partner};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payment {
    pub id: String,
    /// the bank's reference (UTR)
    pub utr: String,
    pub to: String,
    pub amount: i64,
    pub purpose: String,
    pub date: Date,
}

pub fn balance(p: &Partner) -> Result<i64, String> {
    let v = p.get("/pay/v1/balance")?;
    v["balance_paise"].as_i64().ok_or_else(|| format!("no balance in {v}"))
}

/// A UPI address: name@handle, letters, digits, dots, hyphens, underscores.
pub fn valid_vpa(v: &str) -> bool {
    let Some((name, handle)) = v.split_once('@') else { return false };
    !name.is_empty()
        && !handle.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c))
        && handle.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
}

/// Pay `amount` paise to a UPI address: the balance is checked first, the payment recorded after.
pub fn payout(p: &mut Partner, vpa: &str, amount: i64, purpose: &str) -> Result<Payment, String> {
    if !valid_vpa(vpa) {
        return Err(format!("{vpa:?} is not a UPI address"));
    }
    if amount <= 0 {
        return Err("nothing to pay".into());
    }
    let bal = balance(p)?;
    if bal < amount {
        return Err(format!("the wallet has {}, {} is needed", inr(bal), inr(amount)));
    }
    let reference = format!("agentos-{}", p.data.payments.len() + 1);
    let v = p.post("/pay/v1/payouts", &json!({"mode": "UPI", "vpa": vpa, "amount_paise": amount, "purpose": purpose, "reference": reference}))?;
    if v["status"].as_str() != Some("processed") {
        return Err(format!("the payment did not go through: {}", v["status"].as_str().unwrap_or("?")));
    }
    let pay = Payment {
        id: v["id"].as_str().unwrap_or(&reference).to_string(),
        utr: v["utr"].as_str().unwrap_or("").to_string(),
        to: vpa.to_string(),
        amount,
        purpose: purpose.to_string(),
        date: p.today(),
    };
    p.data.payments.push(pay.clone());
    Ok(pay)
}

pub fn wallet(p: &mut Partner) -> Result<Ui, String> {
    let bal = balance(p)?;
    let month = p.today();
    let spent: i64 = p.data.payments.iter().filter(|x| x.date.y == month.y && x.date.m == month.m).map(|x| x.amount).sum();
    let mut ui = Ui::new("Wallet").card(Card::new("Balance").badge(inr(bal)).line(format!("spent this month {}", inr(spent))).tone("ok"));
    for x in p.data.payments.iter().rev().take(8) {
        ui = ui.card(Card::new(&x.purpose).subtitle(format!("{} · {}", x.to, x.date)).badge(inr(x.amount)).line(format!("UTR {}", x.utr)));
    }
    Ok(ui)
}

pub fn pay_tool(p: &mut Partner, to: &str, amount: &str, note: &str) -> Result<Ui, String> {
    let amount = parse_inr(amount).ok_or(format!("{amount:?} is not an amount"))?;
    let pay = payout(p, to.trim(), amount, note)?;
    let bal = balance(p).ok();
    Ok(Ui::new(format!("Paid {}", inr(amount))).card(Card::new(&pay.to).subtitle(note.to_string()).line(format!("UTR {}", pay.utr)).line(bal.map(|b| format!("wallet now {}", inr(b))).unwrap_or_default()).tone("ok")))
}
