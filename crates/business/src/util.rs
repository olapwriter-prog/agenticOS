//! Money (integer paise), dates (no clock library), masking, and sealing at rest.
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub type Paise = i64;

/// Rupees (as written in a profile or a message) to paise.
pub fn rs(x: f64) -> Paise {
    (x * 100.0).round() as Paise
}
pub fn pct(amount: Paise, pct: f64) -> Paise {
    (amount as f64 * pct / 100.0).round() as Paise
}
/// "₹1,23,456.50" (Indian grouping); whole rupees drop the paise.
pub fn inr(p: Paise) -> String {
    let neg = p < 0;
    let p = p.abs();
    let (r, ps) = (p / 100, p % 100);
    let s = r.to_string();
    let mut out = String::new();
    let n = s.len();
    for (i, c) in s.chars().enumerate() {
        let left = n - i;
        if i > 0 && (left == 3 || (left > 3 && (left - 3) % 2 == 0)) {
            out.push(',');
        }
        out.push(c);
    }
    let frac = if ps == 0 { String::new() } else { format!(".{ps:02}") };
    format!("{}₹{out}{frac}", if neg { "-" } else { "" })
}
/// "1,234.50" or "₹1234" or "1234" as a person types it.
pub fn parse_inr(s: &str) -> Result<Paise, String> {
    let t: String = s
        .trim()
        .trim_start_matches(['₹', ' '])
        .trim_start_matches("rs")
        .trim_start_matches("Rs")
        .trim_start_matches('.')
        .chars()
        .filter(|c| *c != ',' && *c != ' ')
        .collect();
    t.parse::<f64>()
        .map(rs)
        .map_err(|_| format!("{s:?} is not an amount"))
}

// ---- dates: "YYYY-MM-DD" days and "YYYY-MM-DDTHH:MM" times, local to the business ----
pub fn day_number(day: &str) -> Option<i64> {
    let mut it = day.get(..10)?.split('-');
    let (y, m, d): (i64, i64, i64) = (
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    );
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}
pub fn day_from_number(z: i64) -> String {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}
pub fn add_days(day: &str, n: i64) -> String {
    day_from_number(day_number(day).unwrap_or(0) + n)
}
pub const WEEKDAYS: [&str; 7] = [
    "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday",
];
/// 0 = Monday.
pub fn weekday(day: &str) -> usize {
    (day_number(day).unwrap_or(0) + 3).rem_euclid(7) as usize
}
pub fn day_of(at: &str) -> &str {
    at.get(..10).unwrap_or(at)
}
pub fn hour_of(at: &str) -> usize {
    at.get(11..13).and_then(|h| h.parse().ok()).unwrap_or(0)
}
/// Minutes since the day number 0, for comparing two times.
pub fn minutes(at: &str) -> i64 {
    let d = day_number(at).unwrap_or(0);
    let m: i64 = at.get(14..16).and_then(|m| m.parse().ok()).unwrap_or(0);
    d * 1440 + hour_of(at) as i64 * 60 + m
}
/// Unix milliseconds to local "YYYY-MM-DDTHH:MM".
pub fn local_time(ms: u64, utc_offset_minutes: i32) -> String {
    let mins = (ms / 60_000) as i64 + utc_offset_minutes as i64;
    let (d, m) = (mins.div_euclid(1440), mins.rem_euclid(1440));
    format!("{}T{:02}:{:02}", day_from_number(d), m / 60, m % 60)
}

// ---- what a customer looks like outside the bill ----
pub fn mask_name(name: &str) -> String {
    let first = name.split_whitespace().next().unwrap_or("");
    let head: String = first.chars().take(2).collect();
    if head.is_empty() {
        "(erased)".into()
    } else {
        format!("{head}***")
    }
}
pub fn mask_phone(phone: &str) -> String {
    let d: Vec<char> = phone.chars().filter(|c| c.is_ascii_digit()).collect();
    if d.len() < 4 {
        return "••••".into();
    }
    format!("••••••{}{}", d[d.len() - 2], d[d.len() - 1])
}
pub fn digits(phone: &str) -> String {
    let d: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
    // the last ten digits identify an Indian mobile whatever prefix was typed
    d[d.len().saturating_sub(10)..].to_string()
}
/// A stable name for a customer that does not reveal the phone number (keyed hash).
pub fn token(key: &[u8], phone: &str) -> String {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("any key length");
    m.update(digits(phone).as_bytes());
    let h = m.finalize().into_bytes();
    format!("c_{}", h[..6].iter().map(|b| format!("{b:02x}")).collect::<String>())
}

// ---- at rest: one sealed file, ChaCha20-Poly1305, fresh nonce per write ----
pub fn seal(key: &[u8; 32], plain: &[u8]) -> Result<Vec<u8>, String> {
    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut nonce).map_err(|e| e.to_string())?;
    let ct = ChaCha20Poly1305::new(key.into())
        .encrypt(Nonce::from_slice(&nonce), plain)
        .map_err(|_| "seal failed".to_string())?;
    let mut out = nonce.to_vec();
    out.extend(ct);
    Ok(out)
}
pub fn unseal(key: &[u8; 32], sealed: &[u8]) -> Result<Vec<u8>, String> {
    if sealed.len() < 12 {
        return Err("sealed file is too short".into());
    }
    ChaCha20Poly1305::new(key.into())
        .decrypt(Nonce::from_slice(&sealed[..12]), &sealed[12..])
        .map_err(|_| "wrong key or the file was changed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn money_formats_the_indian_way() {
        assert_eq!(inr(12_345_650), "₹1,23,456.50");
        assert_eq!(inr(99_900), "₹999");
        assert_eq!(inr(-150_000), "-₹1,500");
        assert_eq!(parse_inr("₹1,234.50").unwrap(), 123_450);
        assert_eq!(parse_inr("385").unwrap(), 38_500);
        assert!(parse_inr("lots").is_err());
        assert_eq!(pct(10_000, 2.5), 250);
    }
    #[test]
    fn dates_round_trip_and_know_the_weekday() {
        assert_eq!(WEEKDAYS[weekday("2026-11-13")], "Friday");
        assert_eq!(add_days("2026-02-28", 1), "2026-03-01");
        assert_eq!(add_days("2024-02-28", 1), "2024-02-29");
        assert_eq!(add_days("2026-01-01", -1), "2025-12-31");
        assert_eq!(local_time(0, 330), "1970-01-01T05:30");
        assert_eq!(hour_of("2026-11-13T19:45"), 19);
        assert_eq!(minutes("2026-11-13T19:45") - minutes("2026-11-12T19:40"), 1445);
    }
    #[test]
    fn masks_hide_the_person() {
        assert_eq!(mask_name("Lakshmi Prasanna"), "La***");
        assert_eq!(mask_phone("+91 90000 01234"), "••••••34");
        assert!(!mask_phone("+919000001234").contains("9000"));
    }
    #[test]
    fn a_token_is_stable_and_does_not_contain_the_number() {
        let a = token(b"k", "+91 90000 01234");
        assert_eq!(a, token(b"k", "9000001234"));
        assert_ne!(a, token(b"other", "9000001234"));
        assert!(!a.contains("1234"));
    }
    #[test]
    fn a_sealed_file_opens_only_with_its_key_and_unchanged() {
        let k = [7u8; 32];
        let s = seal(&k, b"Lakshmi, +919000001234").unwrap();
        assert!(!s.windows(7).any(|w| w == b"Lakshmi"));
        assert_eq!(unseal(&k, &s).unwrap(), b"Lakshmi, +919000001234");
        assert!(unseal(&[8u8; 32], &s).is_err());
        let mut t = s.clone();
        *t.last_mut().unwrap() ^= 1;
        assert!(unseal(&k, &t).is_err());
    }
}
