//! The phone's data as typed records, parsed from the read tools' text, so questions are
//! answered by exact code over every record rather than by a model reading text.

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// true: they sent it to you
    pub incoming: bool,
    /// None when the number is not a saved contact
    pub name: Option<String>,
    pub number: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    /// true: they called you
    pub incoming: bool,
    pub name: Option<String>,
    pub number: String,
    pub state: String,
}

fn who(s: &str, number_end: &str) -> (Option<String>, String, String) {
    if let Some(open) = s.find(" (") {
        if let Some(close) = s[open..].find(')') {
            let close = open + close;
            return (Some(s[..open].trim().to_string()), s[open + 2..close].to_string(), s[close + 1..].to_string());
        }
    }
    let end = s.find(number_end).unwrap_or(s.len());
    (None, s[..end].trim().to_string(), s[end..].to_string())
}

fn dir(line: &str) -> Option<(bool, &str)> {
    line.strip_prefix("from ")
        .map(|r| (true, r))
        .or_else(|| line.strip_prefix("to ").map(|r| (false, r)))
}

/// `from NAME (NUMBER): TEXT` / `to ...` / `from NUMBER: TEXT`, one per line.
pub fn messages(text: &str) -> Vec<Message> {
    text.lines()
        .filter_map(|line| {
            let (incoming, rest) = dir(line)?;
            let (name, number, after) = who(rest, ": ");
            let text = after.strip_prefix(": ").or_else(|| after.strip_prefix(':')).unwrap_or(&after).to_string();
            Some(Message { incoming, name, number, text })
        })
        .collect()
}

/// `from NAME (NUMBER) STATE` / `to ...`, one per line.
pub fn calls(text: &str) -> Vec<Call> {
    text.lines()
        .filter_map(|line| {
            let (incoming, rest) = dir(line)?;
            let (name, number, after) = who(rest, " ");
            Some(Call { incoming, name, number, state: after.trim().to_string() })
        })
        .collect()
}

/// `NAME: NUMBER`, one per line.
pub fn contacts(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.split_once(": "))
        .map(|(n, num)| (n.trim().to_string(), num.trim().to_string()))
        .collect()
}

/// Does a record's party match a person (a contact name, or a number's last 10 digits)?
pub fn is(name: &Option<String>, number: &str, person: &str) -> bool {
    if name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(person)) {
        return true;
    }
    let digits = |s: &str| s.chars().filter(char::is_ascii_digit).collect::<String>();
    let (a, b) = (digits(number), digits(person));
    b.len() >= 7 && a.ends_with(&b[b.len().saturating_sub(10)..])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lines_become_records() {
        let m = messages("from Mum (+91999): dinner: at 8?\nto Ravi Kumar (+91888): ok\nfrom +91800: sale\nno messages");
        assert_eq!(m.len(), 3);
        assert_eq!(m[0], Message { incoming: true, name: Some("Mum".into()), number: "+91999".into(), text: "dinner: at 8?".into() });
        assert_eq!(m[1].name.as_deref(), Some("Ravi Kumar"));
        assert!(!m[1].incoming);
        assert_eq!(m[2].name, None);
        assert_eq!(m[2].text, "sale");
        let c = calls("from Ravi (+91888) ended\nto +91800 active");
        assert_eq!(c[0].state, "ended");
        assert_eq!(c[1].number, "+91800");
        assert_eq!(contacts("Mum: +91999\nRavi: +91888"), vec![("Mum".into(), "+91999".into()), ("Ravi".into(), "+91888".into())]);
        assert!(is(&None, "+919876543210", "98765 43210"));
        assert!(is(&Some("Mum".into()), "", "mum"));
    }
}
