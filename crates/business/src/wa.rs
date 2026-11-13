//! WhatsApp Business: what goes out (text, a document, a location) and reading an order that a
//! customer typed. The simulated channel keeps what was sent; a real one would call the
//! WhatsApp Business Cloud API with the same three message kinds.
use crate::profile::Profile;

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Text(String),
    Document { name: String, bytes: Vec<u8>, caption: String },
    Location { lat: f64, lon: f64, label: String },
}
impl Msg {
    pub fn kind(&self) -> &'static str {
        match self {
            Msg::Text(_) => "text",
            Msg::Document { .. } => "document",
            Msg::Location { .. } => "location",
        }
    }
    pub fn size(&self) -> usize {
        match self {
            Msg::Text(t) => t.len(),
            Msg::Document { bytes, .. } => bytes.len(),
            Msg::Location { .. } => 16,
        }
    }
}

pub trait Whatsapp {
    fn send(&mut self, to: &str, msg: Msg) -> Result<(), String>;
    /// Simulator only: everything sent so far.
    fn sent(&self) -> &[(String, Msg)] {
        &[]
    }
}
#[derive(Default)]
pub struct SimWhatsapp {
    pub out: Vec<(String, Msg)>,
}
impl Whatsapp for SimWhatsapp {
    fn send(&mut self, to: &str, msg: Msg) -> Result<(), String> {
        if to.trim().is_empty() {
            return Err("no recipient".into());
        }
        self.out.push((to.to_string(), msg));
        Ok(())
    }
    fn sent(&self) -> &[(String, Msg)] {
        &self.out
    }
}

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !["a", "of", "plate", "plates", "pls", "please", "and", "with", "one"].contains(w))
        .map(|w| w.trim_end_matches('s').to_string())
        .collect()
}

/// Which menu item a phrase means: the item (by name or alias) whose words are all in the
/// phrase, preferring the longest name. `None` if nothing fits or two fit equally.
fn match_item<'a>(p: &'a Profile, phrase: &str) -> Option<&'a str> {
    let have = words(phrase);
    let mut best: Option<(usize, &str)> = None;
    let mut tie = false;
    for m in &p.menu {
        for name in std::iter::once(&m.name).chain(m.aliases.iter()) {
            let need = words(name);
            if need.is_empty() || !need.iter().all(|w| have.contains(w)) {
                continue;
            }
            match best {
                Some((n, code)) if n == need.len() && code != m.code => tie = true,
                Some((n, _)) if n > need.len() => {}
                _ => {
                    best = Some((need.len(), m.code.as_str()));
                    tie = false;
                }
            }
        }
    }
    if tie {
        None
    } else {
        best.map(|(_, c)| c)
    }
}

/// "2 chicken fried rice, wings x1 and a veg noodles" -> [(code, qty)], or what was not understood.
pub fn parse_order(p: &Profile, text: &str) -> Result<Vec<(String, u32)>, String> {
    let mut lines: Vec<(String, u32)> = Vec::new();
    let mut unknown = Vec::new();
    let norm = text.replace(" and ", ",").replace('\n', ",").replace('+', ",");
    for part in norm.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let mut qty = 1u32;
        let mut rest = Vec::new();
        for w in part.split_whitespace() {
            let t = w.trim_matches(|c| c == 'x' || c == 'X' || c == '*');
            match t.parse::<u32>() {
                // "6pc" stays part of the name; a bare number (or 2x / x2) is the quantity
                Ok(n) if n > 0 && n <= 50 && t.len() + 1 >= w.len() => qty = n,
                _ if w.eq_ignore_ascii_case("one") => qty = 1,
                _ if w.eq_ignore_ascii_case("two") => qty = 2,
                _ if w.eq_ignore_ascii_case("three") => qty = 3,
                _ => rest.push(w),
            }
        }
        let phrase = rest.join(" ");
        match match_item(p, &phrase) {
            Some(code) => match lines.iter_mut().find(|(c, _)| c == code) {
                Some(l) => l.1 += qty,
                None => lines.push((code.to_string(), qty)),
            },
            None => unknown.push(part.to_string()),
        }
    }
    if !unknown.is_empty() {
        return Err(format!("not on the menu or unclear: {}", unknown.join("; ")));
    }
    if lines.is_empty() {
        return Err("no items found in the message".into());
    }
    Ok(lines)
}
