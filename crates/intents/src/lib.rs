//! Intents: the fast path. A phrase pattern with {placeholders} maps straight to a tool call,
//! with no model involved. If no intent matches, the request goes to the model.
use agentos_action::Action;

#[derive(Debug, Clone)]
enum Part {
    Lit(String),
    Slot(String),
}

#[derive(Debug, Clone)]
pub struct Intent {
    tool: String,
    parts: Vec<Part>,
}

impl Intent {
    /// `pattern` like "remind me to {text} at {when}".
    pub fn new(pattern: &str, tool: &str) -> Self {
        let mut parts = Vec::new();
        let mut rest = pattern;
        while let Some(open) = rest.find('{') {
            if open > 0 {
                parts.push(Part::Lit(rest[..open].to_lowercase()));
            }
            let close = rest[open..].find('}').expect("unclosed {") + open;
            parts.push(Part::Slot(rest[open + 1..close].to_string()));
            rest = &rest[close + 1..];
        }
        if !rest.is_empty() {
            parts.push(Part::Lit(rest.to_lowercase()));
        }
        Self {
            tool: tool.to_string(),
            parts,
        }
    }

    pub fn try_match(&self, text: &str) -> Option<Action> {
        let text = text.trim().trim_end_matches(['.', '!', '?']);
        let lower = text.to_lowercase();
        // lowercasing can change byte lengths for some scripts; bail out rather than slice wrongly
        if lower.len() != text.len() {
            return None;
        }
        let mut pos = 0usize;
        let mut args: Vec<(String, String)> = Vec::new();
        let mut i = 0;
        while i < self.parts.len() {
            match &self.parts[i] {
                Part::Lit(l) => {
                    if !lower[pos..].starts_with(l.as_str()) {
                        return None;
                    }
                    pos += l.len();
                }
                Part::Slot(name) => {
                    let end = match self.parts.get(i + 1) {
                        Some(Part::Lit(next)) => {
                            lower[pos..].find(next.as_str()).map(|o| pos + o)?
                        }
                        Some(Part::Slot(_)) => return None, // two slots in a row are ambiguous
                        None => text.len(),
                    };
                    let value = text[pos..end].trim();
                    if value.is_empty() {
                        return None;
                    }
                    args.push((name.clone(), value.to_string()));
                    pos = end;
                }
            }
            i += 1;
        }
        if pos != text.len() {
            return None;
        }
        args.sort();
        Some(Action {
            tool: self.tool.clone(),
            args,
        })
    }
}

#[derive(Default)]
pub struct IntentTable {
    intents: Vec<Intent>,
}

impl IntentTable {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn add(&mut self, pattern: &str, tool: &str) {
        self.intents.push(Intent::new(pattern, tool));
    }
    /// First match wins, so list specific patterns before general ones.
    pub fn resolve(&self, text: &str) -> Option<Action> {
        self.intents.iter().find_map(|i| i.try_match(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> IntentTable {
        let mut t = IntentTable::new();
        t.add("remind me to {text} at {when}", "set_reminder");
        t.add("what's on {day}", "read_calendar");
        t.add("text {to} {text}", "send_message");
        t
    }
    fn get<'a>(a: &'a Action, k: &str) -> &'a str {
        &a.args.iter().find(|(n, _)| n == k).unwrap().1
    }

    #[test]
    fn two_slots_with_a_literal_between() {
        let a = table().resolve("Remind me to call mum at 6pm").unwrap();
        assert_eq!(a.tool, "set_reminder");
        assert_eq!(get(&a, "text"), "call mum");
        assert_eq!(get(&a, "when"), "6pm");
    }
    #[test]
    fn case_is_ignored_in_literals_but_kept_in_values() {
        let a = table().resolve("REMIND ME TO Call Mum AT 6PM").unwrap();
        assert_eq!(get(&a, "text"), "Call Mum");
    }
    #[test]
    fn trailing_punctuation_is_ignored() {
        let a = table().resolve("what's on Monday?").unwrap();
        assert_eq!(get(&a, "day"), "Monday");
    }
    #[test]
    fn no_match_means_ask_the_model() {
        assert!(table().resolve("write me a poem about monsoon").is_none());
    }
    #[test]
    fn an_empty_slot_does_not_match() {
        assert!(table().resolve("remind me to  at 6pm").is_none());
    }
    #[test]
    fn leftover_text_does_not_match() {
        // "what's on {day}" must not match a longer sentence that merely starts the same way
        assert_eq!(
            table()
                .resolve("what's on monday and also tuesday")
                .unwrap()
                .args[0]
                .1,
            "monday and also tuesday"
        );
        assert!(table().resolve("so what's on monday").is_none());
    }
    #[test]
    fn a_space_between_slots_splits_at_the_first_space() {
        let a = table().resolve("text asha hello there").unwrap();
        assert_eq!(get(&a, "to"), "asha");
        assert_eq!(get(&a, "text"), "hello there");
    }
    #[test]
    fn two_slots_with_nothing_between_them_never_match() {
        let mut t = IntentTable::new();
        t.add("{a}{b}", "x");
        assert!(t.resolve("anything at all").is_none());
    }
    #[test]
    fn first_listed_intent_wins() {
        let mut t = IntentTable::new();
        t.add("play {thing}", "specific");
        t.add("play {x}", "general");
        assert_eq!(t.resolve("play jazz").unwrap().tool, "specific");
    }
}
