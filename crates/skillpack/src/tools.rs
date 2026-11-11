//! Skills as headless tools: find the skills for a topic, read what one says about a question
//! (cut to what a small model can hold), and see where every skill came from and under what
//! license. All Low tier: reading only.
use crate::{excerpt, excerpt_text, tokens_of, Library};
use agentos_action::{Action, Tier, Tool};
use agentos_executor::Connector;
use agentos_screen::{Card, Ui};
use std::path::Path;

pub const TOOLS: &[Tool] = &[
    Tool { name: "skills", args: &["topic"], tier: Tier::Low },
    Tool { name: "skill", args: &["name", "question"], tier: Tier::Low },
    Tool { name: "skill_sources", args: &[], tier: Tier::Low },
];

pub fn describe(tool: &str) -> &'static str {
    match tool {
        "skills" => "find skill guides (from Google, Microsoft, OpenAI, Anthropic, Vercel) about a topic",
        "skill" => "read what one skill guide says about a question (name from 'skills')",
        "skill_sources" => "where the skill guides came from, and their licenses",
        _ => "",
    }
}

pub struct SkillTools {
    pub lib: Library,
    /// tokens an excerpt may take: about a sixth of a 4B model's 4,096-token context
    pub budget: usize,
}

impl SkillTools {
    pub fn open(dir: &Path, budget: usize) -> Result<SkillTools, String> {
        Ok(SkillTools { lib: Library::open(dir)?, budget })
    }

    /// For a model's prompt: the best skill's excerpt for a request, if one fits it well enough.
    pub fn context_for(&self, request: &str, budget: usize, min_score: f64) -> Option<String> {
        let (e, score) = self.lib.search(request, 1).into_iter().next()?;
        if score < min_score {
            return None;
        }
        let s = self.lib.load(e).ok()?;
        let (secs, _) = excerpt(&s, request, budget);
        Some(excerpt_text(&s, &secs))
    }

    fn search(&self, topic: &str) -> Result<Ui, String> {
        let hits = self.lib.search(topic, 6);
        let mut ui = Ui::new(format!("Skills: {topic}")).subtitle(format!("{} skill guides on the phone", self.lib.index.skills.len()));
        for (e, _) in &hits {
            let d: String = e.description.chars().take(180).collect();
            ui = ui.card(
                Card::new(&e.name)
                    .subtitle(format!("{} · {} · {} sections", e.source, e.license, e.titles.len()))
                    .line(if e.description.chars().count() > 180 { format!("{d}…") } else { d })
                    .button("Read", format!("skill {}: {topic}", e.id)),
            );
        }
        if hits.is_empty() {
            ui = ui.card(Card::new("No skill matches").subtitle("try other words"));
        }
        Ok(ui)
    }

    fn read(&self, name: &str, question: &str) -> Result<Ui, String> {
        let e = self.lib.find(name).ok_or(format!("no skill called {name}"))?;
        let s = self.lib.load(e)?;
        let q = if question.trim().is_empty() { s.description.clone() } else { question.to_string() };
        let (secs, used) = excerpt(&s, &q, self.budget);
        let total: usize = s.sections.iter().map(|x| tokens_of(&x.text)).sum();
        let mut ui = Ui::new(&s.name).subtitle(format!("{} · {} · {used} of {total} tokens, the parts about: {}", s.source, s.license, q.chars().take(60).collect::<String>()));
        ui = ui.card(Card::new("What it is for").line(s.description.clone()).tone("info"));
        for sec in &secs {
            let mut c = Card::new(&sec.title).subtitle(sec.file.clone());
            for l in sec.text.lines().filter(|l| !l.trim().is_empty()) {
                c = c.line(l.to_string());
            }
            ui = ui.card(c);
        }
        if !s.other_files.is_empty() {
            let shown: Vec<&str> = s.other_files.iter().take(6).map(String::as_str).collect();
            ui = ui.card(Card::new("Files not run here").line(format!("{}{}", shown.join(", "), if s.other_files.len() > 6 { format!(" and {} more", s.other_files.len() - 6) } else { String::new() })).line(format!("source: {}", s.origin)));
        } else {
            ui = ui.card(Card::new("Source").line(s.origin.clone()).line(format!("license {} ({})", s.license, s.license_evidence)));
        }
        Ok(ui)
    }

    fn sources(&self) -> Result<Ui, String> {
        let mut ui = Ui::new("Skill sources").subtitle(format!("{} skills", self.lib.index.skills.len()));
        for (src, note) in &self.lib.index.sources {
            let n = self.lib.index.skills.iter().filter(|e| &e.source == src).count();
            let mut lic: Vec<&str> = self.lib.index.skills.iter().filter(|e| &e.source == src).map(|e| e.license.as_str()).collect();
            lic.sort();
            lic.dedup();
            ui = ui.card(Card::new(src).badge(format!("{n}")).line(note.clone()).line(format!("licenses: {}", lic.join(", "))));
        }
        Ok(ui)
    }
}

impl Connector for SkillTools {
    fn name(&self) -> &str {
        "skills"
    }
    fn tools(&self) -> Vec<Tool> {
        TOOLS.to_vec()
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        let get = |k: &str| a.args.iter().find(|(n, _)| n == k).map(|(_, v)| v.trim().to_string()).unwrap_or_default();
        let ui = match a.tool.as_str() {
            "skills" => self.search(&get("topic"))?,
            "skill" => self.read(&get("name"), &get("question"))?,
            "skill_sources" => self.sources()?,
            other => return Err(format!("skills cannot {other}")),
        };
        Ok(ui.to_reply())
    }
}
