//! Agent Skills on the phone.
//!
//! A skill (the open Agent Skills format: a folder with SKILL.md, YAML frontmatter with a name and
//! a description, a markdown guide, and often reference files and scripts) is written for an
//! agent with a large context. A 4B model on a phone has about 4,000 tokens for everything. So
//! skills here are not pasted into a prompt; they are a library the agent looks things up in:
//!
//!   import   a catalog's skills, only those whose license allows redistribution, each with its
//!            license, the evidence for it, and where it came from. Guides and reference
//!            markdown are kept, split into sections (never inside a code block); scripts are
//!            listed, never run.
//!   search   which skills fit a topic: BM25 over names, descriptions and section titles, from a
//!            small index loaded once.
//!   excerpt  the parts of one skill that answer a question, best sections first by BM25, put
//!            back in reading order, cut to a token budget a small model can hold.
//!
//! On disk: `index.json` (every skill's name, description, source, license, section titles) and
//! one file per skill with its sections, read only when that skill is opened.
pub mod tools;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Licenses that allow keeping and redistributing the text (with attribution).
pub const ALLOWED: &[&str] = &["MIT", "MIT-0", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "CC-BY-4.0", "CC0-1.0"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Section {
    pub title: String,
    /// the file it came from, relative to the skill folder
    pub file: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source: String,
    pub license: String,
    /// why we believe the license: "LICENSE.txt in the skill", "frontmatter", "repository LICENSE"
    pub license_evidence: String,
    /// where it came from: repository URL, commit, path
    pub origin: String,
    pub sections: Vec<Section>,
    /// scripts and other files the skill ships, listed for the reader, never run here
    pub other_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source: String,
    pub license: String,
    pub origin: String,
    pub titles: Vec<String>,
    /// approximate tokens of the whole skill
    pub tokens: usize,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Index {
    pub skills: Vec<Entry>,
    /// source -> (repository, commit, license note)
    pub sources: BTreeMap<String, String>,
}

/// A skill folder that was not kept, and why.
#[derive(Debug, Clone, Serialize)]
pub struct Skipped {
    pub path: String,
    pub reason: String,
}

// ---- reading SKILL.md --------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct Front {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    license: Option<serde_yaml::Value>,
}

/// Split "---\nyaml\n---\nbody" into the frontmatter and the body.
pub fn split_front(text: &str) -> (Option<&str>, &str) {
    let t = text.trim_start_matches('\u{feff}');
    let Some(rest) = t.strip_prefix("---") else { return (None, t) };
    let rest = rest.trim_start_matches(['\r', ' ']).strip_prefix('\n').unwrap_or(rest);
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return (Some(&rest[..at]), &rest[at + line.len()..]);
        }
        at += line.len();
    }
    (None, t)
}

/// The frontmatter: YAML, or, where hand-written YAML does not parse (an unquoted ": " in a
/// description is common), the top-level `name`, `description` and `license` lines, with
/// indented continuation lines and `>`/`|` blocks.
fn parse_front(y: &str) -> Front {
    if let Ok(f) = serde_yaml::from_str::<Front>(y) {
        return f;
    }
    let mut f = Front::default();
    let mut current: Option<&str> = None;
    let mut acc: Vec<String> = vec![];
    let put = |key: Option<&str>, acc: &mut Vec<String>, f: &mut Front| {
        let v = acc.join(" ").trim().trim_matches(['"', '\'']).to_string();
        match key {
            Some("name") => f.name = v,
            Some("description") => f.description = v,
            Some("license") => f.license = Some(serde_yaml::Value::String(v)),
            _ => {}
        }
        acc.clear();
    };
    for line in y.lines() {
        let top = !line.starts_with(' ') && !line.starts_with('\t');
        if top {
            put(current, &mut acc, &mut f);
            current = None;
            if let Some((k, v)) = line.split_once(':') {
                let k = k.trim();
                if ["name", "description", "license"].contains(&k) {
                    current = Some(match k { "name" => "name", "description" => "description", _ => "license" });
                    let v = v.trim();
                    if !matches!(v, ">" | ">-" | "|" | "|-" | "") {
                        acc.push(v.to_string());
                    }
                }
            }
        } else if current.is_some() {
            acc.push(line.trim().to_string());
        }
    }
    put(current, &mut acc, &mut f);
    f
}

/// An SPDX id from a license text or a frontmatter value, if it is one we know.
pub fn detect_license(s: &str) -> Option<&'static str> {
    let head: String = s.chars().take(1500).collect();
    let low = head.to_lowercase();
    let t = low.trim();
    let exact = [("mit-0", "MIT-0"), ("mit", "MIT"), ("apache-2.0", "Apache-2.0"), ("apache 2.0", "Apache-2.0"), ("bsd-3-clause", "BSD-3-Clause"), ("bsd-2-clause", "BSD-2-Clause"), ("isc", "ISC"), ("cc-by-4.0", "CC-BY-4.0"), ("cc0-1.0", "CC0-1.0")];
    for (k, v) in exact {
        if t == k {
            return Some(v);
        }
    }
    if low.contains("apache license") && low.contains("version 2.0") {
        return Some("Apache-2.0");
    }
    if low.contains("mit no attribution") || low.contains("mit-0") {
        return Some("MIT-0");
    }
    if low.contains("mit license") || (low.contains("permission is hereby granted, free of charge") && low.contains("the above copyright notice")) {
        return Some("MIT");
    }
    if low.contains("attribution 4.0 international") {
        return Some("CC-BY-4.0");
    }
    if low.contains("redistribution and use in source and binary forms") {
        return Some(if low.contains("neither the name") { "BSD-3-Clause" } else { "BSD-2-Clause" });
    }
    if low.contains("cc0 1.0") {
        return Some("CC0-1.0");
    }
    None
}

/// Markdown into sections by headings, never splitting inside a ``` block; tiny sections join
/// the one before, long ones are cut at paragraph breaks.
pub fn sections(file: &str, md: &str, top_title: &str) -> Vec<Section> {
    let mut out: Vec<Section> = vec![];
    let mut title = top_title.to_string();
    let mut buf = String::new();
    let mut fence = false;
    let flush = |out: &mut Vec<Section>, title: &str, buf: &mut String| {
        let text = buf.trim().to_string();
        buf.clear();
        if text.is_empty() {
            return;
        }
        out.push(Section { title: title.to_string(), file: file.to_string(), text });
    };
    for line in md.lines() {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fence = !fence;
        }
        let h = line.trim_start();
        if !fence && h.starts_with('#') && h.trim_start_matches('#').starts_with(' ') {
            flush(&mut out, &title, &mut buf);
            title = h.trim_start_matches('#').trim().to_string();
            continue;
        }
        buf.push_str(line);
        buf.push('\n');
    }
    flush(&mut out, &title, &mut buf);
    // join tiny sections into the previous one
    let mut joined: Vec<Section> = vec![];
    for s in out {
        match joined.last_mut() {
            Some(prev) if s.text.len() < 160 && prev.text.len() < 3000 => {
                prev.text.push_str(&format!("\n\n{}:\n{}", s.title, s.text));
            }
            _ => joined.push(s),
        }
    }
    // cut long ones at paragraph breaks (outside code blocks) into ~2,400-character parts
    let mut res = vec![];
    for s in joined {
        if s.text.len() <= 3200 {
            res.push(s);
            continue;
        }
        let mut part = String::new();
        let mut n = 1;
        let mut fence = false;
        for para in s.text.split_inclusive("\n\n") {
            fence ^= para.matches("```").count() % 2 == 1;
            part.push_str(para);
            if part.len() >= 2400 && !fence {
                res.push(Section { title: if n == 1 { s.title.clone() } else { format!("{} ({n})", s.title) }, file: s.file.clone(), text: part.trim().to_string() });
                part.clear();
                n += 1;
            }
        }
        if !part.trim().is_empty() {
            res.push(Section { title: if n == 1 { s.title.clone() } else { format!("{} ({n})", s.title) }, file: s.file.clone(), text: part.trim().to_string() });
        }
    }
    res
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
        if name == ".git" || name == "node_modules" {
            continue;
        }
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// How one catalog is imported.
pub struct Source {
    /// short name: "google", "microsoft", ...
    pub name: String,
    /// repository URL and commit, for the origin of every skill
    pub repo: String,
    pub commit: String,
    /// the repository's license, if one covers every skill without its own (SPDX), and why
    pub repo_license: Option<(String, String)>,
}

/// Read every skill under `root`. Kept: those with a license in ALLOWED.
pub fn import(root: &Path, src: &Source) -> (Vec<Skill>, Vec<Skipped>) {
    let mut files = vec![];
    walk(root, &mut files);
    let mut kept: Vec<Skill> = vec![];
    let mut skipped = vec![];
    let mut seen: HashSet<String> = HashSet::new();
    for f in files.iter().filter(|f| f.file_name().is_some_and(|n| n == "SKILL.md")) {
        let dir = f.parent().unwrap();
        let rel_dir = dir.strip_prefix(root).unwrap_or(dir).display().to_string();
        let Ok(text) = std::fs::read_to_string(f) else {
            skipped.push(Skipped { path: rel_dir, reason: "not UTF-8".into() });
            continue;
        };
        let (front, body) = split_front(&text);
        let fm: Front = front.map(parse_front).unwrap_or_default();
        let name = if fm.name.trim().is_empty() { dir.file_name().unwrap_or_default().to_string_lossy().to_string() } else { fm.name.trim().to_string() };
        if fm.description.trim().is_empty() {
            skipped.push(Skipped { path: rel_dir, reason: "no description".into() });
            continue;
        }
        // the license: the skill's own file, then its frontmatter, then the repository's
        let own_file = ["LICENSE.txt", "LICENSE", "LICENSE.md"].iter().map(|n| dir.join(n)).find(|p| p.exists());
        let fm_license = fm.license.as_ref().and_then(|v| v.as_str().map(String::from));
        let (license, evidence) = if let Some(p) = &own_file {
            match std::fs::read_to_string(p).ok().and_then(|t| detect_license(&t)) {
                Some(l) => (Some(l.to_string()), format!("{} in the skill", p.file_name().unwrap().to_string_lossy())),
                None => (None, format!("{} in the skill: not a license we can redistribute under", p.file_name().unwrap().to_string_lossy())),
            }
        } else if let Some(l) = fm_license.as_deref().and_then(detect_license) {
            (Some(l.to_string()), "license field of SKILL.md".into())
        } else if let Some((l, why)) = &src.repo_license {
            (Some(l.clone()), why.clone())
        } else {
            (None, "no license found".into())
        };
        let Some(license) = license.filter(|l| ALLOWED.contains(&l.as_str())) else {
            skipped.push(Skipped { path: rel_dir, reason: evidence });
            continue;
        };
        let id = format!("{}/{}", src.name, slug(&name));
        if !seen.insert(id.clone()) {
            skipped.push(Skipped { path: rel_dir, reason: format!("duplicate of {id}") });
            continue;
        }
        let mut secs = sections("SKILL.md", body, &name);
        let mut others = vec![];
        let mut sub = vec![];
        walk(dir, &mut sub);
        for p in sub {
            if p == *f {
                continue;
            }
            let rel = p.strip_prefix(dir).unwrap_or(&p).display().to_string();
            // a nested skill is its own skill
            if p.file_name().is_some_and(|n| n == "SKILL.md") {
                continue;
            }
            let is_md = matches!(p.extension().and_then(|e| e.to_str()), Some("md") | Some("mdx") | Some("txt")) && !rel.to_lowercase().starts_with("license");
            if is_md && files_under_other_skill(dir, &p) {
                continue;
            }
            if is_md {
                if let Ok(t) = std::fs::read_to_string(&p) {
                    let (_, b) = split_front(&t);
                    let title = rel.trim_end_matches(".md").trim_end_matches(".mdx").replace('/', " / ");
                    secs.extend(sections(&rel, b, &title));
                    continue;
                }
            }
            if !rel.to_lowercase().starts_with("license") {
                others.push(rel);
            }
        }
        if secs.is_empty() {
            skipped.push(Skipped { path: rel_dir, reason: "empty".into() });
            continue;
        }
        kept.push(Skill {
            id,
            name,
            description: squash(&fm.description),
            source: src.name.clone(),
            license,
            license_evidence: evidence,
            origin: format!("{}/tree/{}/{}", src.repo.trim_end_matches(".git"), src.commit, rel_dir),
            sections: secs,
            other_files: others,
        });
    }
    (kept, skipped)
}

/// Is `p` inside a deeper folder that has its own SKILL.md (a nested skill)?
fn files_under_other_skill(skill_dir: &Path, p: &Path) -> bool {
    let mut d = p.parent();
    while let Some(x) = d {
        if x == skill_dir {
            return false;
        }
        if x.join("SKILL.md").exists() {
            return true;
        }
        d = x.parent();
    }
    false
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

pub fn tokens_of(s: &str) -> usize {
    s.len().div_ceil(4)
}

// ---- the pack on disk --------------------------------------------------------------------------

/// Write skills into a pack directory (adding to what is there), and rewrite the index.
pub fn write_pack(dir: &Path, skills: &[Skill], source_note: (&str, &str)) -> Result<usize, String> {
    std::fs::create_dir_all(dir.join("skills")).map_err(|e| e.to_string())?;
    let mut idx: Index = std::fs::read_to_string(dir.join("index.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    idx.skills.retain(|e| e.source != source_note.0);
    for s in skills {
        let file = dir.join("skills").join(format!("{}.json", s.id.replace('/', "__")));
        std::fs::write(&file, serde_json::to_string(s).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        idx.skills.push(Entry {
            id: s.id.clone(),
            name: s.name.clone(),
            description: s.description.clone(),
            source: s.source.clone(),
            license: s.license.clone(),
            origin: s.origin.clone(),
            titles: s.sections.iter().map(|x| x.title.clone()).collect(),
            tokens: s.sections.iter().map(|x| tokens_of(&x.text)).sum(),
        });
    }
    idx.sources.insert(source_note.0.to_string(), source_note.1.to_string());
    idx.skills.sort_by(|a, b| a.id.cmp(&b.id));
    std::fs::write(dir.join("index.json"), serde_json::to_string(&idx).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(idx.skills.len())
}

// ---- search ------------------------------------------------------------------------------------

const STOP: &[&str] = &["the", "a", "an", "and", "or", "of", "to", "in", "on", "for", "with", "is", "are", "be", "how", "do", "i", "you", "it", "this", "that", "what", "when", "use", "using", "my", "can", "should", "from", "by", "as", "at", "about", "into", "your", "me", "we", "not", "if", "then", "than", "which", "will", "does", "there", "these", "those"];

/// Lowercase words, without stop words, with a light stemming (plural s, -ing, -ed).
pub fn terms(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1 && !STOP.contains(w))
        .map(|w| {
            let w = w.strip_suffix("ing").filter(|r| r.len() > 3).unwrap_or(w);
            let w = w.strip_suffix("ed").filter(|r| r.len() > 3).unwrap_or(w);
            let w = w.strip_suffix("es").filter(|r| r.len() > 3).unwrap_or(w);
            let w = w.strip_suffix('s').filter(|r| r.len() > 2).unwrap_or(w);
            w.to_string()
        })
        .collect()
}

/// BM25 over a set of documents (term lists).
pub struct Bm25 {
    docs: Vec<HashMap<String, f64>>,
    lens: Vec<f64>,
    avg: f64,
    df: HashMap<String, f64>,
}

impl Bm25 {
    pub fn new(docs: Vec<Vec<String>>) -> Bm25 {
        let mut df: HashMap<String, f64> = HashMap::new();
        let mut tf = vec![];
        let mut lens = vec![];
        for d in &docs {
            let mut m: HashMap<String, f64> = HashMap::new();
            for t in d {
                *m.entry(t.clone()).or_default() += 1.0;
            }
            for t in m.keys() {
                *df.entry(t.clone()).or_default() += 1.0;
            }
            lens.push(d.len() as f64);
            tf.push(m);
        }
        let avg = (lens.iter().sum::<f64>() / lens.len().max(1) as f64).max(1.0);
        Bm25 { docs: tf, lens, avg, df }
    }
    pub fn score(&self, i: usize, q: &[String]) -> f64 {
        let (k1, b) = (1.2, 0.75);
        let n = self.docs.len() as f64;
        let mut s = 0.0;
        let mut seen = HashSet::new();
        for t in q {
            if !seen.insert(t) {
                continue;
            }
            let Some(f) = self.docs[i].get(t) else { continue };
            let df = self.df.get(t).copied().unwrap_or(0.0);
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
            s += idf * f * (k1 + 1.0) / (f + k1 * (1.0 - b + b * self.lens[i] / self.avg));
        }
        s
    }
    pub fn top(&self, q: &[String], k: usize) -> Vec<(usize, f64)> {
        let mut v: Vec<(usize, f64)> = (0..self.docs.len()).map(|i| (i, self.score(i, q))).filter(|(_, s)| *s > 0.0).collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        v.truncate(k);
        v
    }
}

/// The library: the index in memory, skills read from disk when opened.
pub struct Library {
    pub dir: PathBuf,
    pub index: Index,
    bm: Bm25,
}

impl Library {
    pub fn open(dir: &Path) -> Result<Library, String> {
        let t = std::fs::read_to_string(dir.join("index.json")).map_err(|e| format!("{}: {e}", dir.join("index.json").display()))?;
        let index: Index = serde_json::from_str(&t).map_err(|e| e.to_string())?;
        Ok(Library::from_index(dir, index))
    }
    pub fn from_index(dir: &Path, index: Index) -> Library {
        let docs = index
            .skills
            .iter()
            .map(|e| {
                // the name and description count more than section titles
                let mut d = terms(&e.name.replace('-', " "));
                d.extend(d.clone());
                d.extend(d.clone());
                let desc = terms(&e.description);
                d.extend(desc.clone());
                d.extend(desc);
                d.extend(terms(&e.source));
                for t in &e.titles {
                    d.extend(terms(t));
                }
                d
            })
            .collect();
        Library { dir: dir.to_path_buf(), bm: Bm25::new(docs), index }
    }

    /// The skills that fit a topic, best first, with their scores.
    pub fn search(&self, topic: &str, k: usize) -> Vec<(&Entry, f64)> {
        self.bm.top(&terms(topic), k).into_iter().map(|(i, s)| (&self.index.skills[i], s)).collect()
    }

    /// A skill by id ("google/dpop-adoption"), by name, or the best match for the words.
    pub fn find(&self, name: &str) -> Option<&Entry> {
        let n = name.trim().to_lowercase();
        let s = slug(&n);
        self.index
            .skills
            .iter()
            .find(|e| e.id == n)
            .or_else(|| self.index.skills.iter().find(|e| slug(&e.name) == s))
            .or_else(|| self.search(name, 1).into_iter().next().map(|(e, _)| e))
    }

    pub fn load(&self, e: &Entry) -> Result<Skill, String> {
        let f = self.dir.join("skills").join(format!("{}.json", e.id.replace('/', "__")));
        let t = std::fs::read_to_string(&f).map_err(|err| format!("{}: {err}", f.display()))?;
        serde_json::from_str(&t).map_err(|err| err.to_string())
    }
}

/// The parts of a skill that answer a question, within `budget` tokens: the best sections by
/// BM25 (the first section always counts a little, it usually says what the skill is for), put
/// back in reading order; a section too long for what is left is cut at a paragraph.
pub fn excerpt(skill: &Skill, question: &str, budget: usize) -> (Vec<Section>, usize) {
    let q = terms(question);
    let docs: Vec<Vec<String>> = skill.sections.iter().map(|s| {
        let mut d = terms(&s.title);
        d.extend(d.clone());
        d.extend(terms(&s.text));
        d
    }).collect();
    let bm = Bm25::new(docs);
    let mut order: Vec<(usize, f64)> = (0..skill.sections.len()).map(|i| (i, bm.score(i, &q) + if i == 0 { 0.5 } else { 0.0 })).collect();
    order.sort_by(|a, b| b.1.total_cmp(&a.1));
    let header = tokens_of(&skill.description) + 20;
    let mut left = budget.saturating_sub(header);
    let mut chosen: Vec<(usize, Section)> = vec![];
    for (i, s) in order {
        if left < 8 {
            break;
        }
        if s <= 0.0 && !chosen.is_empty() {
            break;
        }
        let sec = &skill.sections[i];
        let t = tokens_of(&sec.text);
        if t <= left {
            chosen.push((i, sec.clone()));
            left -= t;
        } else if left >= 24 {
            // as many whole paragraphs as fit
            let mut text = String::new();
            for p in sec.text.split("\n\n") {
                if tokens_of(&text) + tokens_of(p) + 1 > left {
                    break;
                }
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(p);
            }
            if !text.is_empty() {
                left -= tokens_of(&text);
                chosen.push((i, Section { title: sec.title.clone(), file: sec.file.clone(), text: format!("{text}\n…") }));
            }
        }
    }
    chosen.sort_by_key(|(i, _)| *i);
    let used = budget - left;
    (chosen.into_iter().map(|(_, s)| s).collect(), used)
}

/// An excerpt as text for a model's prompt.
pub fn excerpt_text(skill: &Skill, secs: &[Section]) -> String {
    let mut out = format!("Skill {} ({}): {}\n", skill.name, skill.license, skill.description);
    for s in secs {
        out.push_str(&format!("\n## {}\n{}\n", s.title, s.text));
    }
    out
}

#[cfg(test)]
mod tests;
