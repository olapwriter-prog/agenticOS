//! Understanding requests without an LLM: a Naive Bayes classifier that learns online, and
//! rules that pull out the details (who, which day, how much, on or off, what to say). The phone
//! answers regular requests with this and exact code; the LLM is only asked when the classifier
//! is not confident or a detail is missing.
pub mod records;

use std::collections::{BTreeMap, HashMap};

/// Words of a request, lowercased, lightly stemmed, plus neighbouring pairs (so "call me" and
/// "me call" differ, and "not in my contacts" carries more than its words).
/// Words that carry no meaning for what is being asked. Pronouns stay: "did I send" and "did
/// Ravi send me" ask different things.
const FILLER: &[&str] = &[
    "the", "a", "an", "to", "please", "could", "would", "can", "you", "it", "its", "is", "are",
    "be", "of", "and", "or", "this", "that", "just", "now", "hey", "ok", "okay", "will", "kindly",
];

pub fn features(text: &str) -> Vec<String> {
    let words: Vec<String> = text
        .to_lowercase()
        .replace('\'', "")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !FILLER.contains(w))
        .map(|w| {
            // a number's shape is the signal, not its value: "80" and "30" are both a level
            if w.chars().all(|c| c.is_ascii_digit()) {
                if w.len() >= 7 {
                    "#phone".to_string()
                } else {
                    "#num".to_string()
                }
            } else {
                // contractions lose their apostrophe above: "what's" -> "whats" -> "what"
                match w {
                    "whats" => "what".into(),
                    "hows" => "how".into(),
                    "wheres" => "where".into(),
                    "whos" => "who".into(),
                    "im" => "i".into(),
                    _ => stem(w),
                }
            }
        })
        .collect();
    let mut f = words.clone();
    // the first word says a lot: "put"/"call" start commands, "what"/"how"/"did" questions
    if let Some(first) = words.first() {
        f.push(format!("^{first}"));
    }
    for w in words.windows(2) {
        f.push(format!("{}_{}", w[0], w[1]));
    }
    f
}

fn stem(w: &str) -> String {
    let w = w.to_string();
    for suf in ["ing", "ed", "es", "s"] {
        if w.len() > suf.len() + 3 && w.ends_with(suf) {
            return w[..w.len() - suf.len()].to_string();
        }
    }
    w
}

#[derive(Default, Clone)]
struct Label {
    docs: u32,
    total: u32,
    counts: HashMap<String, u32>,
}

/// Multinomial Naive Bayes with Laplace smoothing. Trains in microseconds and keeps learning:
/// every confirmed request is one more example.
#[derive(Default, Clone)]
pub struct Classifier {
    labels: BTreeMap<String, Label>,
    vocab: HashMap<String, ()>,
    docs: u32,
}

impl Classifier {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn train(&mut self, label: &str, text: &str) {
        let l = self.labels.entry(label.to_string()).or_default();
        l.docs += 1;
        self.docs += 1;
        for f in features(text) {
            *l.counts.entry(f.clone()).or_default() += 1;
            l.total += 1;
            self.vocab.insert(f, ());
        }
    }
    pub fn labels(&self) -> usize {
        self.labels.len()
    }
    /// Every label with its probability, best first.
    pub fn predict(&self, text: &str) -> Vec<(String, f32)> {
        let f = features(text);
        let v = self.vocab.len().max(1) as f64;
        let known: Vec<&String> = f.iter().filter(|w| self.vocab.contains_key(*w)).collect();
        // weight each word by how few kinds of request use it (inverse document frequency over
        // labels): "signal" says a lot, "my" almost nothing. Weighted Naive Bayes, after Rennie
        // et al., "Tackling the poor assumptions of naive Bayes text classifiers" (2003).
        let n_labels = self.labels.len().max(1) as f64;
        let weight = |w: &String| {
            let df = self
                .labels
                .values()
                .filter(|l| l.counts.contains_key(w))
                .count() as f64;
            (n_labels / df.max(1.0)).ln().max(0.05)
        };
        let mut scores: Vec<(String, f64)> = self
            .labels
            .iter()
            .map(|(name, l)| {
                let prior = (l.docs as f64 / self.docs.max(1) as f64).ln();
                let like: f64 = known
                    .iter()
                    .map(|w| {
                        weight(w)
                            * ((*l.counts.get(*w).unwrap_or(&0) as f64 + 1.0)
                                / (l.total as f64 + v))
                                .ln()
                    })
                    .sum();
                (name.clone(), prior + like)
            })
            .collect();
        let max = scores.iter().map(|s| s.1).fold(f64::NEG_INFINITY, f64::max);
        let total: f64 = scores.iter().map(|s| (s.1 - max).exp()).sum();
        for s in scores.iter_mut() {
            s.1 = (s.1 - max).exp() / total;
        }
        scores.sort_by(|a, b| b.1.total_cmp(&a.1));
        // a request made of words the classifier has never seen gives no evidence at all
        if known.is_empty() {
            return scores.into_iter().map(|(n, _)| (n, 0.0)).collect();
        }
        scores.into_iter().map(|(n, p)| (n, p as f32)).collect()
    }
    /// The best label if it is clearly the best: probability at least `min`.
    pub fn decide(&self, text: &str, min: f32) -> Option<(String, f32)> {
        self.predict(text)
            .into_iter()
            .next()
            .filter(|(_, p)| *p >= min)
    }
}

// ---- details ---------------------------------------------------------------------------------

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Words people use for family, and the names they often save them under.
const RELATIONS: &[(&[&str], &[&str])] = &[
    (
        &["mother", "mom", "mum", "mummy", "mommy", "ma", "amma"],
        &["mum", "mom", "mother", "amma", "ma"],
    ),
    (
        &["father", "dad", "daddy", "papa", "appa"],
        &["dad", "father", "papa", "appa"],
    ),
];

/// Who the request is about: a saved contact (by name, a typo away, or a family word), else a
/// capitalised name that is not the first word, else a phone number.
pub fn person(text: &str, contacts: &[String]) -> Option<String> {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '+')
        .filter(|w| !w.is_empty())
        .collect();
    // whole contact names, longest first ("Ravi Kumar" before "Ravi")
    let mut by_len: Vec<&String> = contacts.iter().collect();
    by_len.sort_by_key(|c| std::cmp::Reverse(c.len()));
    for c in &by_len {
        let cl = c.to_lowercase();
        let parts: Vec<&str> = cl.split_whitespace().collect();
        if !parts.is_empty() && words.windows(parts.len()).any(|w| w == parts.as_slice()) {
            return Some((*c).clone());
        }
    }
    // a typo away (names of 4+ letters)
    for c in &by_len {
        let cl = c.to_lowercase();
        if cl.len() >= 4
            && !cl.contains(' ')
            && words
                .iter()
                .any(|w| w.len() >= 4 && edit_distance(w, &cl) == 1)
        {
            return Some((*c).clone());
        }
    }
    // family words
    for (said, saved) in RELATIONS {
        if words.iter().any(|w| said.contains(w)) {
            if let Some(c) = contacts
                .iter()
                .find(|c| saved.contains(&c.to_lowercase().as_str()))
            {
                return Some(c.clone());
            }
        }
    }
    // a number, possibly written in groups ("98765 43210", "+91-98765-43210")
    let mut run = String::new();
    let mut best = String::new();
    for ch in text.chars().chain(std::iter::once('x')) {
        if ch.is_ascii_digit() || (ch == '+' && run.is_empty()) {
            run.push(ch);
        } else if (ch == ' ' || ch == '-') && !run.is_empty() {
            continue;
        } else {
            if run.chars().filter(char::is_ascii_digit).count() >= 7 && best.is_empty() {
                best = run.clone();
            }
            run.clear();
        }
    }
    if !best.is_empty() {
        return Some(best);
    }
    // a capitalised name that does not start the sentence
    let orig: Vec<&str> = text.split_whitespace().collect();
    orig.iter()
        .skip(1)
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .find(|w| w.len() >= 2 && w.chars().next().is_some_and(char::is_uppercase) && *w != "I")
        .map(String::from)
}

pub fn day(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    for d in [
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
        "today",
        "tomorrow",
        "tonight",
    ] {
        if lower.split(|c: char| !c.is_alphanumeric()).any(|w| w == d) {
            return Some(d.to_string());
        }
    }
    None
}

/// The first whole number 0..=100 (a volume, a percent).
pub fn percent(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_digit())
        .filter(|w| !w.is_empty())
        .find_map(|w| w.parse::<u32>().ok().filter(|n| *n <= 100))
        .map(|n| n.to_string())
}

/// On or off: off if the request says so, otherwise on.
pub fn on_off(text: &str) -> &'static str {
    let l = format!(" {} ", text.to_lowercase());
    if [
        " off ",
        " disable",
        " stop ",
        " kill ",
        " no more ",
        "turn it off",
    ]
    .iter()
    .any(|w| l.contains(w))
    {
        "off"
    } else {
        "on"
    }
}

/// What to say in a message: the words after "saying", "that says", "to say", "that", or a colon.
pub fn message_text(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    for marker in [" saying ", " that says ", " to say ", ": ", " that "] {
        if let Some(i) = lower.find(marker) {
            let t = text[i + marker.len()..]
                .trim()
                .trim_matches('"')
                .to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn trained() -> Classifier {
        let mut c = Classifier::new();
        for t in [
            "call mum",
            "phone dad",
            "phone my sister",
            "ring Asha",
            "give Ravi a call",
        ] {
            c.train("call", t);
        }
        for t in [
            "torch on",
            "turn on the flashlight",
            "light please",
            "switch the torch off",
        ] {
            c.train("flashlight", t);
        }
        for t in [
            "how many texts did mum send me",
            "count messages from ravi",
            "number of sms from asha",
        ] {
            c.train("q.msgs_from", t);
        }
        c
    }
    #[test]
    fn it_classifies_unseen_phrasings_and_knows_when_it_does_not_know() {
        let c = trained();
        assert_eq!(
            c.decide("could you phone my brother", 0.6).unwrap().0,
            "call"
        );
        assert_eq!(
            c.decide("put the torch on, it's dark", 0.6).unwrap().0,
            "flashlight"
        );
        assert_eq!(
            c.decide("how many messages did Ravi send", 0.6).unwrap().0,
            "q.msgs_from"
        );
        assert!(
            c.decide("write me a poem about rain", 0.6).is_none(),
            "no evidence, no decision"
        );
    }
    #[test]
    fn it_learns_online() {
        let mut c = trained();
        assert_ne!(
            c.decide("blow the candle out", 0.6).map(|d| d.0),
            Some("flashlight".into())
        );
        c.train("flashlight", "blow the candle out");
        c.train("flashlight", "candle on");
        assert_eq!(
            c.decide("blow the candle out", 0.6).unwrap().0,
            "flashlight"
        );
    }
    #[test]
    fn people_are_found_by_name_typo_relation_number_or_capital() {
        let contacts = vec![
            "Mum".to_string(),
            "Ravi Kumar".to_string(),
            "Asha".to_string(),
        ];
        assert_eq!(
            person("text ravi kumar I'm late", &contacts).as_deref(),
            Some("Ravi Kumar")
        );
        assert_eq!(person("call Ashaa", &contacts).as_deref(), Some("Asha"));
        assert_eq!(
            person("could you ring my mother", &contacts).as_deref(),
            Some("Mum")
        );
        assert_eq!(
            person("phone my brother Arjun", &contacts).as_deref(),
            Some("Arjun")
        );
        assert_eq!(
            person("call 98765 43210", &contacts).as_deref(),
            Some("9876543210")
        );
        assert_eq!(
            person("call +919876543210 now", &contacts).as_deref(),
            Some("+919876543210")
        );
        assert!(person("switch the torch on", &contacts).is_none());
    }
    #[test]
    fn days_numbers_switches_and_message_text() {
        assert_eq!(
            day("put dentist on my calendar for Monday").as_deref(),
            Some("monday")
        );
        assert_eq!(percent("make it louder, 80").as_deref(), Some("80"));
        assert_eq!(percent("volume 250"), None);
        assert_eq!(on_off("switch the torch on"), "on");
        assert_eq!(on_off("turn the flashlight off"), "off");
        assert_eq!(
            message_text("drop Mum a text saying I'll be late").as_deref(),
            Some("I'll be late")
        );
        assert_eq!(
            message_text("tell Ravi: on my way").as_deref(),
            Some("on my way")
        );
        assert_eq!(message_text("text mum"), None);
    }
}
