//! The context firewall: everything bound for the commercial model passes through here.
//! It redacts personal data, blocks never-send items, and returns a report for the ledger.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub emails: usize,
    pub phones: usize,
    pub id_numbers: usize,
    pub cards: usize,
}

impl Report {
    pub fn total(&self) -> usize {
        self.emails + self.phones + self.id_numbers + self.cards
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Safe to send. The text is redacted and the report says what was removed.
    Allowed { text: String, report: Report },
    /// Contains something on the never-send list. Nothing leaves the phone.
    Blocked { reason: String },
}

fn luhn_ok(digits: &str) -> bool {
    let mut sum = 0;
    for (i, c) in digits.chars().rev().enumerate() {
        let mut d = c.to_digit(10).unwrap();
        if i % 2 == 1 {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
    }
    sum % 10 == 0
}

fn is_email_char(c: char) -> bool {
    c.is_alphanumeric() || "._%+-".contains(c)
}

fn redact_emails(s: &str, n: &mut usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' && !out.is_empty() {
            // walk back over the local part already copied to `out`
            let local_len = out.chars().rev().take_while(|c| is_email_char(*c)).count();
            // walk forward over the domain
            let mut j = i + 1;
            while j < chars.len()
                && (chars[j].is_alphanumeric() || chars[j] == '.' || chars[j] == '-')
            {
                j += 1;
            }
            let domain: String = chars[i + 1..j].iter().collect();
            let domain = domain.trim_end_matches('.');
            if local_len > 0 && domain.contains('.') {
                for _ in 0..local_len {
                    out.pop();
                }
                out.push_str("[EMAIL]");
                *n += 1;
                i += 1 + domain.chars().count();
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Replace runs of digits (allowing single spaces or dashes inside) by class.
fn redact_numbers(s: &str, r: &mut Report) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let mut j = i;
            let mut digits = String::new();
            while j < chars.len() {
                if chars[j].is_ascii_digit() {
                    digits.push(chars[j]);
                    j += 1;
                } else if (chars[j] == ' ' || chars[j] == '-')
                    && j + 1 < chars.len()
                    && chars[j + 1].is_ascii_digit()
                {
                    j += 1;
                } else {
                    break;
                }
            }
            let len = digits.len();
            let tag = if (13..=19).contains(&len) && luhn_ok(&digits) {
                r.cards += 1;
                Some("[CARD]")
            } else if len == 12 {
                r.id_numbers += 1;
                Some("[ID]")
            } else if len == 10 || (len == 12 && digits.starts_with("91")) {
                r.phones += 1;
                Some("[PHONE]")
            } else {
                None
            };
            match tag {
                Some(t) => out.push_str(t),
                None => out.extend(&chars[i..j]),
            }
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

pub struct Firewall {
    never_send: Vec<String>,
}

impl Firewall {
    pub fn new(never_send: &[&str]) -> Self {
        Self {
            never_send: never_send.iter().map(|s| s.to_lowercase()).collect(),
        }
    }

    pub fn check(&self, text: &str) -> Verdict {
        let lower = text.to_lowercase();
        for term in &self.never_send {
            if lower.contains(term) {
                return Verdict::Blocked {
                    reason: format!("contains never-send term '{term}'"),
                };
            }
        }
        let mut r = Report {
            emails: 0,
            phones: 0,
            id_numbers: 0,
            cards: 0,
        };
        let t = redact_emails(text, &mut r.emails);
        let t = redact_numbers(&t, &mut r);
        Verdict::Allowed { text: t, report: r }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fw() -> Firewall {
        Firewall::new(&["vault key", "passphrase"])
    }
    fn allowed(v: Verdict) -> (String, Report) {
        match v {
            Verdict::Allowed { text, report } => (text, report),
            other => panic!("expected Allowed, got {other:?}"),
        }
    }

    #[test]
    fn plain_text_passes_unchanged() {
        let (t, r) = allowed(fw().check("Plan a trip to Goa in March"));
        assert_eq!(t, "Plan a trip to Goa in March");
        assert_eq!(r.total(), 0);
    }
    #[test]
    fn email_is_redacted() {
        let (t, r) = allowed(fw().check("Send it to asha.rao@example.co.in please"));
        assert_eq!(t, "Send it to [EMAIL] please");
        assert_eq!(r.emails, 1);
    }
    #[test]
    fn at_sign_without_a_domain_is_left_alone() {
        let (t, r) = allowed(fw().check("meet @ 5 at the cafe"));
        assert_eq!(t, "meet @ 5 at the cafe");
        assert_eq!(r.emails, 0);
    }
    #[test]
    fn ten_digit_phone_is_redacted() {
        let (t, r) = allowed(fw().check("call 98765 43210 tomorrow"));
        assert_eq!(t, "call [PHONE] tomorrow");
        assert_eq!(r.phones, 1);
    }
    #[test]
    fn twelve_digit_id_is_redacted() {
        let (t, r) = allowed(fw().check("id 1234 5678 9012 on file"));
        assert_eq!(t, "id [ID] on file");
        assert_eq!(r.id_numbers, 1);
    }
    #[test]
    fn valid_card_is_redacted_but_a_random_long_number_is_not() {
        let (t, r) = allowed(fw().check("card 4111 1111 1111 1111 expires soon"));
        assert_eq!(t, "card [CARD] expires soon");
        assert_eq!(r.cards, 1);
        let (t2, r2) = allowed(fw().check("order 4111 1111 1111 1112 shipped"));
        assert_eq!(r2.cards, 0);
        assert!(t2.contains("4111"));
    }
    #[test]
    fn short_numbers_survive() {
        let (t, _) = allowed(fw().check("I need 3 rooms for 12 people at 5000 rupees"));
        assert_eq!(t, "I need 3 rooms for 12 people at 5000 rupees");
    }
    #[test]
    fn never_send_blocks_everything() {
        let v = fw().check("here is my Vault Key: abc");
        assert!(matches!(v, Verdict::Blocked { .. }));
    }
    #[test]
    fn multiple_items_are_all_counted() {
        let (t, r) = allowed(fw().check("a@b.com and c@d.org, phone 9876543210"));
        assert_eq!(r.emails, 2);
        assert_eq!(r.phones, 1);
        assert!(!t.contains('@'));
    }
}
