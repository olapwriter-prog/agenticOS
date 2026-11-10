//! Screens instead of sentences. A tool answers with a small UI document: a title, cards (each a
//! title, a few lines, a badge, buttons), and buttons for the whole screen. A button carries the
//! words to say back ("send invoice INV/2026-27/0003"), so pressing it is the same request as
//! saying it: tiers and the approval key apply unchanged. The document carries its own text
//! form, so a phone without a screen reads or speaks the same answer.
//!
//! Wire form (one JSON object): {"ui": {"title", "subtitle"?, "cards": [...], "actions": [...]}, "text": "..."}
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Button {
    pub label: String,
    /// what pressing it says to the agent
    pub say: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Card {
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub subtitle: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub lines: Vec<String>,
    /// a short tag shown on the card: "₹4,720", "due in 3 days", "best"
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub badge: String,
    /// "ok", "warn", "info" or "" (plain)
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub tone: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub actions: Vec<Button>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Ui {
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub subtitle: String,
    #[serde(default)]
    pub cards: Vec<Card>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub actions: Vec<Button>,
}

impl Ui {
    pub fn new(title: impl Into<String>) -> Ui {
        Ui { title: title.into(), ..Default::default() }
    }
    pub fn subtitle(mut self, s: impl Into<String>) -> Ui {
        self.subtitle = s.into();
        self
    }
    pub fn card(mut self, c: Card) -> Ui {
        self.cards.push(c);
        self
    }
    pub fn action(mut self, label: impl Into<String>, say: impl Into<String>) -> Ui {
        self.actions.push(Button { label: label.into(), say: say.into() });
        self
    }

    /// The same answer as text: title, then each card on a line or two, then what can be said.
    pub fn text(&self) -> String {
        let mut out = vec![self.title.clone()];
        if !self.subtitle.is_empty() {
            out[0] = format!("{} ({})", self.title, self.subtitle);
        }
        for c in &self.cards {
            let mut head = format!("- {}", c.title);
            if !c.badge.is_empty() {
                head.push_str(&format!(" [{}]", c.badge));
            }
            if !c.subtitle.is_empty() {
                head.push_str(&format!(": {}", c.subtitle));
            }
            out.push(head);
            for l in &c.lines {
                out.push(format!("    {l}"));
            }
            for b in &c.actions {
                out.push(format!("    say \"{}\" to {}", b.say, b.label.to_lowercase()));
            }
        }
        for b in &self.actions {
            out.push(format!("say \"{}\" to {}", b.say, b.label.to_lowercase()));
        }
        out.join("\n")
    }

    /// The reply a tool returns: the document and its text.
    pub fn to_reply(&self) -> String {
        serde_json::json!({"ui": self, "text": self.text()}).to_string()
    }

    /// A tool reply back into a document, if it is one.
    pub fn from_reply(s: &str) -> Option<(Ui, String)> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        let ui: Ui = serde_json::from_value(v.get("ui")?.clone()).ok()?;
        Some((ui, v["text"].as_str().unwrap_or_default().to_string()))
    }
}

impl Card {
    pub fn new(title: impl Into<String>) -> Card {
        Card { title: title.into(), ..Default::default() }
    }
    pub fn subtitle(mut self, s: impl Into<String>) -> Card {
        self.subtitle = s.into();
        self
    }
    pub fn line(mut self, l: impl Into<String>) -> Card {
        self.lines.push(l.into());
        self
    }
    pub fn badge(mut self, b: impl Into<String>) -> Card {
        self.badge = b.into();
        self
    }
    pub fn tone(mut self, t: &str) -> Card {
        self.tone = t.into();
        self
    }
    pub fn button(mut self, label: impl Into<String>, say: impl Into<String>) -> Card {
        self.actions.push(Button { label: label.into(), say: say.into() });
        self
    }
}
