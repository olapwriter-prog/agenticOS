//! Documents on the phone: Word, Excel, PowerPoint and PDF files read natively, as headless tools.
//!
//! The skill guides for these formats (Anthropic's and OpenAI's pdf, docx, xlsx, pptx) work by
//! running Python scripts. The phone does not run vendor scripts; it reads the formats itself:
//!   .docx  paragraphs, headings (by style) and tables, in order (word/document.xml)
//!   .xlsx  every sheet as rows, with shared strings, inline strings, numbers, booleans and the
//!          values formulas last computed to (xl/workbook.xml, its relationships, the sheets)
//!   .pptx  slides in presentation order, paragraph by paragraph (ppt/presentation.xml)
//!   .pdf   text page by page (no OCR: a scanned page has no text to read)
//! A document becomes parts (a heading's section, a sheet, a slide, a page); a question picks the
//! parts that answer it, cut to the token budget of a small model (BM25, as for skills).
//!
//! Files come from one folder; names cannot leave it, and every zip entry is read with a size
//! cap (a crafted file cannot expand to fill the memory).
use agentos_action::{Action, Tier, Tool};
use agentos_executor::Connector;
use agentos_screen::{Card, Ui};
use agentos_skillpack::{terms, tokens_of, Bm25};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The most an entry of a zip may expand to.
const MAX_ENTRY: u64 = 64 << 20;

#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub title: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Doc {
    pub kind: &'static str,
    pub parts: Vec<Part>,
}

fn zip_text(z: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Result<String, String> {
    let f = z.by_name(name).map_err(|e| format!("{name}: {e}"))?;
    if f.size() > MAX_ENTRY {
        return Err(format!("{name} is too large ({} bytes)", f.size()));
    }
    let mut s = String::new();
    f.take(MAX_ENTRY).read_to_string(&mut s).map_err(|e| format!("{name}: {e}"))?;
    Ok(s)
}

fn local(n: &[u8]) -> &[u8] {
    match n.iter().position(|b| *b == b':') {
        Some(i) => &n[i + 1..],
        None => n,
    }
}

/// The relationship id of an element: the namespaced `r:id`, not a plain `id` beside it.
fn rel_id(e: &quick_xml::events::BytesStart) -> Option<String> {
    e.attributes().flatten().find(|a| {
        let k = a.key.as_ref();
        k.contains(&b':') && local(k) == b"id"
    }).map(|a| a.unescape_value().map(|v| v.to_string()).unwrap_or_default())
}

fn attr(e: &quick_xml::events::BytesStart, name: &[u8]) -> Option<String> {
    e.attributes().flatten().find(|a| local(a.key.as_ref()) == name).map(|a| a.unescape_value().map(|v| v.to_string()).unwrap_or_default())
}

// ---- .docx -------------------------------------------------------------------------------------

pub fn read_docx(path: &Path) -> Result<Doc, String> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).map_err(|e| e.to_string())?).map_err(|e| format!("not a .docx: {e}"))?;
    let xml = zip_text(&mut z, "word/document.xml")?;
    let mut r = Reader::from_str(&xml);
    let mut parts: Vec<Part> = vec![Part { title: "Start".into(), text: String::new() }];
    let mut para = String::new();
    let mut style = String::new();
    let mut in_t = false;
    // tables: rows of cells; nested tables flatten into their cell
    let mut tables: Vec<Vec<Vec<String>>> = vec![];
    let mut cell = String::new();
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => match local(e.name().as_ref()) {
                b"t" => in_t = true,
                b"tbl" => tables.push(vec![]),
                b"tr" => {
                    if let Some(t) = tables.last_mut() {
                        t.push(vec![]);
                    }
                }
                b"tc" => cell.clear(),
                b"p" => {
                    para.clear();
                    style.clear();
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => match local(e.name().as_ref()) {
                b"pStyle" => style = attr(&e, b"val").unwrap_or_default(),
                b"tab" => para.push('\t'),
                b"br" | b"cr" => para.push('\n'),
                _ => {}
            },
            Ok(Event::Text(t)) if in_t => para.push_str(&t.unescape().unwrap_or_default()),
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                b"t" => in_t = false,
                b"p" => {
                    let text = para.trim().to_string();
                    if !tables.is_empty() {
                        if !text.is_empty() {
                            if !cell.is_empty() {
                                cell.push(' ');
                            }
                            cell.push_str(&text);
                        }
                    } else if style.to_lowercase().starts_with("heading") || style == "Title" {
                        if !text.is_empty() {
                            parts.push(Part { title: text, text: String::new() });
                        }
                    } else if !text.is_empty() {
                        let p = parts.last_mut().unwrap();
                        p.text.push_str(&text);
                        p.text.push_str("\n\n");
                    }
                }
                b"tc" => {
                    if let Some(row) = tables.last_mut().and_then(|t| t.last_mut()) {
                        row.push(cell.trim().to_string());
                    }
                    cell.clear();
                }
                b"tbl" => {
                    let t = tables.pop().unwrap_or_default();
                    let text = t.iter().map(|r| r.join(" | ")).collect::<Vec<_>>().join("\n");
                    match tables.last_mut().and_then(|t| t.last_mut()) {
                        Some(outer) => outer.push(text),
                        None => {
                            let p = parts.last_mut().unwrap();
                            p.text.push_str(&text);
                            p.text.push_str("\n\n");
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("word/document.xml: {e}")),
            _ => {}
        }
    }
    parts.retain(|p| !p.text.trim().is_empty() || p.title != "Start");
    for p in parts.iter_mut() {
        p.text = p.text.trim().to_string();
    }
    Ok(Doc { kind: "Word", parts })
}

// ---- .xlsx -------------------------------------------------------------------------------------

/// "AB12" -> column 27 (0-based)
fn col_of(r: &str) -> usize {
    r.chars().take_while(|c| c.is_ascii_alphabetic()).fold(0, |n, c| n * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1)).saturating_sub(1)
}

/// Relationship id -> target, from a .rels file.
fn rels(xml: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut r = Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if local(e.name().as_ref()) == b"Relationship" => {
                if let (Some(id), Some(t)) = (attr(&e, b"Id"), attr(&e, b"Target")) {
                    out.insert(id, t);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// A relationship target, relative to `base` ("xl/") or absolute ("/xl/...").
fn resolve(base: &str, target: &str) -> String {
    match target.strip_prefix('/') {
        Some(t) => t.to_string(),
        None => format!("{base}{target}"),
    }
}

pub fn read_xlsx(path: &Path) -> Result<(Doc, Vec<(String, Vec<Vec<String>>)>), String> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).map_err(|e| e.to_string())?).map_err(|e| format!("not a .xlsx: {e}"))?;
    // shared strings (rich text runs joined)
    let mut shared: Vec<String> = vec![];
    if let Ok(xml) = zip_text(&mut z, "xl/sharedStrings.xml") {
        let mut r = Reader::from_str(&xml);
        let (mut cur, mut in_t) = (String::new(), false);
        loop {
            match r.read_event() {
                Ok(Event::Start(e)) if local(e.name().as_ref()) == b"t" => in_t = true,
                Ok(Event::Text(t)) if in_t => cur.push_str(&t.unescape().unwrap_or_default()),
                Ok(Event::End(e)) => match local(e.name().as_ref()) {
                    b"t" => in_t = false,
                    b"si" => shared.push(std::mem::take(&mut cur)),
                    _ => {}
                },
                Ok(Event::Eof) => break,
                Err(e) => return Err(format!("sharedStrings: {e}")),
                _ => {}
            }
        }
    }
    let wb_rels = rels(&zip_text(&mut z, "xl/_rels/workbook.xml.rels").unwrap_or_default());
    let wb = zip_text(&mut z, "xl/workbook.xml")?;
    let mut sheets: Vec<(String, String)> = vec![];
    let mut r = Reader::from_str(&wb);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if local(e.name().as_ref()) == b"sheet" => {
                let name = attr(&e, b"name").unwrap_or_default();
                let file = rel_id(&e).and_then(|id| wb_rels.get(&id).cloned()).map(|t| resolve("xl/", &t));
                if let Some(f) = file {
                    sheets.push((name, f));
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("workbook.xml: {e}")),
            _ => {}
        }
    }
    let mut parts = vec![];
    let mut tables = vec![];
    for (name, file) in sheets {
        let xml = zip_text(&mut z, &file)?;
        let mut r = Reader::from_str(&xml);
        let mut rows: Vec<Vec<String>> = vec![];
        let (mut col, mut typ, mut val, mut in_v) = (0usize, String::new(), String::new(), false);
        loop {
            match r.read_event() {
                Ok(Event::Start(e)) => match local(e.name().as_ref()) {
                    b"row" => rows.push(vec![]),
                    b"c" => {
                        col = attr(&e, b"r").map(|r| col_of(&r)).unwrap_or_else(|| rows.last().map_or(0, |x| x.len()));
                        typ = attr(&e, b"t").unwrap_or_default();
                        val.clear();
                    }
                    b"v" | b"t" => in_v = true,
                    _ => {}
                },
                Ok(Event::Text(t)) if in_v => val.push_str(&t.unescape().unwrap_or_default()),
                Ok(Event::End(e)) => match local(e.name().as_ref()) {
                    b"v" | b"t" => in_v = false,
                    b"c" => {
                        let shown = match typ.as_str() {
                            "s" => val.trim().parse::<usize>().ok().and_then(|i| shared.get(i).cloned()).unwrap_or_default(),
                            "b" => if val.trim() == "1" { "TRUE".into() } else { "FALSE".into() },
                            "e" => format!("#{}", val.trim_start_matches('#')),
                            _ => num_text(&val),
                        };
                        if let Some(row) = rows.last_mut() {
                            while row.len() < col {
                                row.push(String::new());
                            }
                            row.push(shown);
                        }
                    }
                    _ => {}
                },
                Ok(Event::Eof) => break,
                Err(e) => return Err(format!("{file}: {e}")),
                _ => {}
            }
        }
        rows.retain(|r| r.iter().any(|c| !c.is_empty()));
        let text = rows.iter().map(|r| r.join(" | ")).collect::<Vec<_>>().join("\n");
        parts.push(Part { title: format!("Sheet {name}"), text });
        tables.push((name, rows));
    }
    Ok((Doc { kind: "Excel", parts }, tables))
}

/// "4720.5" stays; "60000" stays; "1.0E-2" -> "0.01"; float noise ("0.30000000000000004") trimmed.
fn num_text(v: &str) -> String {
    let v = v.trim();
    match v.parse::<f64>() {
        Ok(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
        Ok(f) => {
            let s = format!("{:.10}", f);
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        }
        Err(_) => v.to_string(),
    }
}

// ---- .pptx -------------------------------------------------------------------------------------

pub fn read_pptx(path: &Path) -> Result<Doc, String> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).map_err(|e| e.to_string())?).map_err(|e| format!("not a .pptx: {e}"))?;
    let prels = rels(&zip_text(&mut z, "ppt/_rels/presentation.xml.rels").unwrap_or_default());
    let pres = zip_text(&mut z, "ppt/presentation.xml")?;
    let mut order = vec![];
    let mut r = Reader::from_str(&pres);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if local(e.name().as_ref()) == b"sldId" => {
                if let Some(t) = rel_id(&e).and_then(|id| prels.get(&id).cloned()) {
                    order.push(resolve("ppt/", &t));
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("presentation.xml: {e}")),
            _ => {}
        }
    }
    let mut parts = vec![];
    for (i, file) in order.iter().enumerate() {
        let xml = zip_text(&mut z, file)?;
        let mut r = Reader::from_str(&xml);
        let (mut paras, mut cur, mut in_t) = (vec![], String::new(), false);
        loop {
            match r.read_event() {
                Ok(Event::Start(e)) if local(e.name().as_ref()) == b"t" => in_t = true,
                Ok(Event::Text(t)) if in_t => cur.push_str(&t.unescape().unwrap_or_default()),
                Ok(Event::End(e)) => match local(e.name().as_ref()) {
                    b"t" => in_t = false,
                    b"p" => {
                        if !cur.trim().is_empty() {
                            paras.push(std::mem::take(&mut cur).trim().to_string());
                        }
                        cur.clear();
                    }
                    _ => {}
                },
                Ok(Event::Eof) => break,
                Err(e) => return Err(format!("{file}: {e}")),
                _ => {}
            }
        }
        let title = paras.first().cloned().unwrap_or_default();
        parts.push(Part { title: format!("Slide {}: {title}", i + 1), text: paras.iter().skip(1).cloned().collect::<Vec<_>>().join("\n") });
    }
    Ok(Doc { kind: "PowerPoint", parts })
}

// ---- .pdf --------------------------------------------------------------------------------------

pub fn read_pdf(path: &Path) -> Result<Doc, String> {
    let d = lopdf::Document::load(path).map_err(|e| format!("not a readable PDF: {e}"))?;
    if d.is_encrypted() {
        return Err("the PDF is encrypted".into());
    }
    let mut parts = vec![];
    for (n, _) in d.get_pages() {
        let text = d.extract_text(&[n]).unwrap_or_default();
        let text = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join("\n");
        parts.push(Part { title: format!("Page {n}"), text });
    }
    Ok(Doc { kind: "PDF", parts })
}

pub fn read(path: &Path) -> Result<Doc, String> {
    match path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref() {
        Some("docx") => read_docx(path),
        Some("xlsx") => read_xlsx(path).map(|(d, _)| d),
        Some("pptx") => read_pptx(path),
        Some("pdf") => read_pdf(path),
        Some(e) => Err(format!(".{e} files are not read here (docx, xlsx, pptx, pdf)")),
        None => Err("a file without an extension".into()),
    }
}

/// The parts that answer a question, in document order, within `budget` tokens. No question: the
/// document from the start.
pub fn pick(doc: &Doc, question: &str, budget: usize) -> Vec<Part> {
    let q = terms(question);
    let mut order: Vec<usize> = (0..doc.parts.len()).collect();
    if !q.is_empty() {
        let bm = Bm25::new(doc.parts.iter().map(|p| {
            let mut d = terms(&p.title);
            d.extend(d.clone());
            d.extend(terms(&p.text));
            d
        }).collect());
        let mut scored: Vec<(usize, f64)> = order.iter().map(|&i| (i, bm.score(i, &q))).collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        order = scored.into_iter().filter(|(_, s)| *s > 0.0).map(|(i, _)| i).collect();
        if order.is_empty() {
            order = (0..doc.parts.len()).collect();
        }
    }
    let mut left = budget;
    let mut chosen: Vec<(usize, Part)> = vec![];
    for i in order {
        let p = &doc.parts[i];
        let t = tokens_of(&p.title) + tokens_of(&p.text);
        if t <= left {
            chosen.push((i, p.clone()));
            left -= t;
        } else if left > 30 {
            let keep: String = p.text.chars().take(left.saturating_sub(tokens_of(&p.title) + 2) * 4).collect();
            chosen.push((i, Part { title: p.title.clone(), text: format!("{keep}…") }));
            break;
        }
    }
    chosen.sort_by_key(|(i, _)| *i);
    chosen.into_iter().map(|(_, p)| p).collect()
}

// ---- tools -------------------------------------------------------------------------------------

pub const TOOLS: &[Tool] = &[
    Tool { name: "documents", args: &[], tier: Tier::Low },
    Tool { name: "read_document", args: &["name", "question"], tier: Tier::Low },
];

pub fn describe(tool: &str) -> &'static str {
    match tool {
        "documents" => "the Word, Excel, PowerPoint and PDF files on the phone",
        "read_document" => "read a document, or the part of it that answers a question",
        _ => "",
    }
}

pub struct Docs {
    pub dir: PathBuf,
    pub budget: usize,
}

impl Docs {
    fn files(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(&self.dir).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect()).unwrap_or_default();
        // one level down: the files that came from Android apps
        if let Ok(rd) = std::fs::read_dir(self.dir.join("android")) {
            v.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_file()));
        }
        v.sort();
        v
    }
    fn find(&self, name: &str) -> Result<PathBuf, String> {
        let n = name.trim();
        if n.contains('/') || n.contains('\\') || n.contains("..") {
            return Err("a document name, not a path".into());
        }
        let low = n.to_lowercase();
        let files = self.files();
        files
            .iter()
            .find(|p| p.file_name().is_some_and(|f| f.to_string_lossy().to_lowercase() == low))
            .or_else(|| files.iter().find(|p| p.file_stem().is_some_and(|f| f.to_string_lossy().to_lowercase() == low)))
            .or_else(|| files.iter().find(|p| p.file_name().is_some_and(|f| f.to_string_lossy().to_lowercase().contains(&low))))
            .cloned()
            .ok_or(format!("no document called {name}"))
    }
}

impl Connector for Docs {
    fn name(&self) -> &str {
        "documents"
    }
    fn tools(&self) -> Vec<Tool> {
        TOOLS.to_vec()
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        let get = |k: &str| a.args.iter().find(|(n, _)| n == k).map(|(_, v)| v.trim().to_string()).unwrap_or_default();
        match a.tool.as_str() {
            "documents" => {
                let mut ui = Ui::new("Documents").subtitle(self.dir.display().to_string());
                for f in self.files() {
                    let name = f.file_name().unwrap().to_string_lossy().to_string();
                    let size = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
                    let c = match read(&f) {
                        Ok(d) => {
                            let unit = match d.kind { "Excel" => "sheet", "PowerPoint" => "slide", "PDF" => "page", _ => "section" };
                            let n = d.parts.len();
                            Card::new(&name).badge(d.kind).subtitle(format!("{n} {unit}{} · {} KB", if n == 1 { "" } else { "s" }, size.div_ceil(1024))).button("Read", format!("read document {name}"))
                        }
                        Err(e) => Card::new(&name).subtitle(e).tone("warn"),
                    };
                    ui = ui.card(c);
                }
                if ui.cards.is_empty() {
                    ui = ui.card(Card::new("No documents"));
                }
                Ok(ui.to_reply())
            }
            "read_document" => {
                let f = self.find(&get("name"))?;
                let d = read(&f)?;
                let q = get("question");
                let parts = pick(&d, &q, self.budget);
                let total: usize = d.parts.iter().map(|p| tokens_of(&p.text)).sum();
                let mut ui = Ui::new(f.file_name().unwrap().to_string_lossy().to_string()).layout("reader").accent("lime").subtitle(format!("{} · {} parts · {}", d.kind, d.parts.len(), if q.is_empty() { format!("from the start, {total} tokens in all") } else { format!("the parts about: {q}") }));
                for p in parts {
                    let mut c = Card::new(&p.title);
                    for l in p.text.lines().filter(|l| !l.trim().is_empty()) {
                        c = c.line(l.to_string());
                    }
                    if p.text.trim().is_empty() {
                        c = c.line(if d.kind == "PDF" { "no text on this page (a scan?)" } else { "(empty)" });
                    }
                    ui = ui.card(c);
                }
                Ok(ui.to_reply())
            }
            other => Err(format!("documents cannot {other}")),
        }
    }
}

#[cfg(test)]
mod tests;
