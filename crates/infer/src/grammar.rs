//! Constrained output: GBNF grammars (llama.cpp's format) and the JSON schemas agentd sends,
//! turned into a matcher the sampler consults, so the model can only write what the grammar
//! allows. agentd uses this for every tool call and for the code it asks the model to write.
//!
//! The matcher keeps a set of parse stacks and advances them one character at a time, like
//! llama.cpp's; `*`, `+`, `?` and groups are rewritten into helper rules.
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Elem {
    /// Matches one character in (or, if negated, not in) these inclusive ranges.
    Char {
        ranges: Vec<(u32, u32)>,
        negated: bool,
    },
    Ref(usize),
}

#[derive(Debug, Clone)]
pub struct Grammar {
    rules: Vec<Vec<Vec<Elem>>>,
    root: usize,
}

type Frame = (usize, usize, usize); // rule, alternative, position
type Stack = Vec<Frame>;

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
    names: BTreeMap<String, usize>,
    rules: Vec<Option<Vec<Vec<Elem>>>>,
}

impl<'a> Parser<'a> {
    fn skip(&mut self, newlines: bool) {
        while self.at < self.s.len() {
            match self.s[self.at] {
                b' ' | b'\t' | b'\r' => self.at += 1,
                b'\n' if newlines => self.at += 1,
                b'#' => {
                    while self.at < self.s.len() && self.s[self.at] != b'\n' {
                        self.at += 1;
                    }
                }
                _ => break,
            }
        }
    }
    fn rule_id(&mut self, name: &str) -> usize {
        if let Some(&i) = self.names.get(name) {
            return i;
        }
        self.rules.push(None);
        let i = self.rules.len() - 1;
        self.names.insert(name.to_string(), i);
        i
    }
    fn fresh(&mut self, body: Vec<Vec<Elem>>) -> usize {
        self.rules.push(Some(body));
        self.rules.len() - 1
    }
    fn name(&mut self) -> String {
        let st = self.at;
        while self.at < self.s.len()
            && (self.s[self.at].is_ascii_alphanumeric()
                || self.s[self.at] == b'-'
                || self.s[self.at] == b'_')
        {
            self.at += 1;
        }
        String::from_utf8_lossy(&self.s[st..self.at]).into_owned()
    }
    fn char_lit(&mut self) -> Result<u32, String> {
        let rest = std::str::from_utf8(&self.s[self.at..]).map_err(|_| "grammar is not UTF-8")?;
        let mut it = rest.chars();
        let c = it.next().ok_or("unexpected end of grammar")?;
        if c != '\\' {
            self.at += c.len_utf8();
            return Ok(c as u32);
        }
        let e = it.next().ok_or("bad escape")?;
        self.at += 2;
        Ok(match e {
            'n' => '\n' as u32,
            't' => '\t' as u32,
            'r' => '\r' as u32,
            'x' | 'u' | 'U' => {
                let n = match e {
                    'x' => 2,
                    'u' => 4,
                    _ => 8,
                };
                let hex = std::str::from_utf8(&self.s[self.at..self.at + n])
                    .map_err(|_| "bad hex escape")?;
                self.at += n;
                u32::from_str_radix(hex, 16).map_err(|_| "bad hex escape")?
            }
            other => other as u32,
        })
    }
    /// alternatives until `)` or end of rule
    fn alternatives(&mut self, nested: bool) -> Result<Vec<Vec<Elem>>, String> {
        let mut alts = vec![self.sequence(nested)?];
        loop {
            self.skip(nested);
            if self.at < self.s.len() && self.s[self.at] == b'|' {
                self.at += 1;
                self.skip(true);
                alts.push(self.sequence(nested)?);
            } else {
                return Ok(alts);
            }
        }
    }
    fn sequence(&mut self, nested: bool) -> Result<Vec<Elem>, String> {
        let mut seq: Vec<Elem> = Vec::new();
        loop {
            self.skip(nested);
            if self.at >= self.s.len() {
                break;
            }
            let start = seq.len();
            match self.s[self.at] {
                b'"' => {
                    self.at += 1;
                    while self.at < self.s.len() && self.s[self.at] != b'"' {
                        let c = self.char_lit()?;
                        seq.push(Elem::Char {
                            ranges: vec![(c, c)],
                            negated: false,
                        });
                    }
                    self.at += 1;
                }
                b'[' => {
                    self.at += 1;
                    let negated = self.s.get(self.at) == Some(&b'^');
                    if negated {
                        self.at += 1;
                    }
                    let mut ranges = Vec::new();
                    while self.at < self.s.len() && self.s[self.at] != b']' {
                        let a = self.char_lit()?;
                        let b = if self.s.get(self.at) == Some(&b'-')
                            && self.s.get(self.at + 1) != Some(&b']')
                        {
                            self.at += 1;
                            self.char_lit()?
                        } else {
                            a
                        };
                        ranges.push((a, b));
                    }
                    self.at += 1;
                    seq.push(Elem::Char { ranges, negated });
                }
                b'(' => {
                    self.at += 1;
                    self.skip(true);
                    let body = self.alternatives(true)?;
                    self.skip(true);
                    if self.s.get(self.at) != Some(&b')') {
                        return Err("missing )".into());
                    }
                    self.at += 1;
                    let id = self.fresh(body);
                    seq.push(Elem::Ref(id));
                }
                b'.' => {
                    self.at += 1;
                    seq.push(Elem::Char {
                        ranges: vec![],
                        negated: true,
                    });
                }
                c if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' => {
                    // a rule name, unless it begins the next rule (`name ::=`)
                    let save = self.at;
                    let name = self.name();
                    let mut look = self.at;
                    while look < self.s.len() && (self.s[look] == b' ' || self.s[look] == b'\t') {
                        look += 1;
                    }
                    if self.s[look..].starts_with(b"::=") {
                        self.at = save;
                        break;
                    }
                    let id = self.rule_id(&name);
                    seq.push(Elem::Ref(id));
                }
                _ => break,
            }
            // postfix operators apply to the last element just parsed
            if seq.len() > start {
                if let Some(&op) = self.s.get(self.at) {
                    if matches!(op, b'*' | b'+' | b'?') {
                        self.at += 1;
                        let item: Vec<Elem> = seq.drain(start..).collect();
                        let id = self.rules.len();
                        let body = match op {
                            // R ::= item R |
                            b'*' => vec![[item.clone(), vec![Elem::Ref(id)]].concat(), vec![]],
                            // R ::= item R | item
                            b'+' => vec![[item.clone(), vec![Elem::Ref(id)]].concat(), item],
                            _ => vec![item, vec![]],
                        };
                        self.rules.push(Some(body));
                        seq.push(Elem::Ref(id));
                    }
                }
            }
        }
        Ok(seq)
    }
}

impl Grammar {
    pub fn parse(text: &str) -> Result<Grammar, String> {
        let mut p = Parser {
            s: text.as_bytes(),
            at: 0,
            names: BTreeMap::new(),
            rules: Vec::new(),
        };
        loop {
            p.skip(true);
            if p.at >= p.s.len() {
                break;
            }
            let name = p.name();
            if name.is_empty() {
                return Err(format!("expected a rule name at byte {}", p.at));
            }
            p.skip(false);
            if !p.s[p.at..].starts_with(b"::=") {
                return Err(format!("expected ::= after {name}"));
            }
            p.at += 3;
            p.skip(true);
            let id = p.rule_id(&name);
            let body = p.alternatives(false)?;
            p.rules[id] = Some(body);
        }
        let root = *p.names.get("root").ok_or("grammar has no root rule")?;
        let mut rules = Vec::new();
        for (i, r) in p.rules.into_iter().enumerate() {
            let name = p
                .names
                .iter()
                .find(|(_, v)| **v == i)
                .map(|(k, _)| k.clone())
                .unwrap_or_default();
            rules.push(r.ok_or(format!("rule {name} is used but not defined"))?);
        }
        Ok(Grammar { rules, root })
    }

    /// A matcher at the start of the grammar.
    pub fn matcher(&self) -> Matcher<'_> {
        let mut m = Matcher {
            g: self,
            stacks: Vec::new(),
            partial: Vec::new(),
        };
        let mut out = HashSet::new();
        for a in 0..self.rules[self.root].len() {
            self.expand(vec![(self.root, a, 0)], &mut out, 0);
        }
        m.stacks = out.into_iter().collect();
        m
    }

    /// Normalise a stack so its top is a character element (or the stack is empty: complete).
    fn expand(&self, mut st: Stack, out: &mut HashSet<Stack>, depth: usize) {
        if depth > 200 {
            return; // left recursion or a runaway grammar
        }
        loop {
            let Some(&(r, a, p)) = st.last() else {
                out.insert(st);
                return;
            };
            let alt = &self.rules[r][a];
            if p >= alt.len() {
                st.pop();
                continue;
            }
            match &alt[p] {
                Elem::Char { .. } => {
                    out.insert(st);
                    return;
                }
                Elem::Ref(r2) => {
                    let r2 = *r2;
                    st.last_mut().unwrap().2 += 1;
                    for a2 in 0..self.rules[r2].len() {
                        let mut s2 = st.clone();
                        s2.push((r2, a2, 0));
                        self.expand(s2, out, depth + 1);
                    }
                    return;
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct Matcher<'g> {
    g: &'g Grammar,
    stacks: Vec<Stack>,
    /// bytes of a UTF-8 character not yet complete
    partial: Vec<u8>,
}

fn char_ok(e: &Elem, c: u32) -> bool {
    match e {
        Elem::Char { ranges, negated } => {
            ranges.iter().any(|(a, b)| *a <= c && c <= *b) != *negated
        }
        Elem::Ref(_) => false,
    }
}

impl Matcher<'_> {
    fn accept_char(&mut self, c: u32) -> bool {
        let mut out = HashSet::new();
        for st in &self.stacks {
            let Some(&(r, a, p)) = st.last() else {
                continue;
            };
            if char_ok(&self.g.rules[r][a][p], c) {
                let mut s2 = st.clone();
                s2.last_mut().unwrap().2 += 1;
                self.g.expand(s2, &mut out, 0);
            }
        }
        self.stacks = out.into_iter().collect();
        !self.stacks.is_empty()
    }

    /// Feed the bytes of one token. False (and the matcher unusable) if the grammar refuses them.
    pub fn accept_bytes(&mut self, bytes: &[u8]) -> bool {
        for &b in bytes {
            self.partial.push(b);
            let need = match self.partial[0] {
                x if x < 0x80 => 1,
                x if x >> 5 == 0b110 => 2,
                x if x >> 4 == 0b1110 => 3,
                _ => 4,
            };
            if self.partial.len() < need {
                continue;
            }
            let c = std::str::from_utf8(&self.partial)
                .ok()
                .and_then(|s| s.chars().next())
                .map(|c| c as u32)
                .unwrap_or(0xfffd);
            self.partial.clear();
            if !self.accept_char(c) {
                return false;
            }
        }
        true
    }

    /// Would these bytes be accepted? (Does not change the matcher.)
    pub fn allows(&self, bytes: &[u8]) -> bool {
        let mut m = self.clone();
        m.accept_bytes(bytes)
    }

    /// Has the grammar been completed (may the model stop here)?
    pub fn complete(&self) -> bool {
        self.partial.is_empty() && self.stacks.iter().any(|s| s.is_empty())
    }
    /// Is there any way to continue?
    pub fn can_continue(&self) -> bool {
        self.stacks.iter().any(|s| !s.is_empty())
    }
}

// ---- JSON schema -> GBNF -------------------------------------------------------------------

const JSON_STRING: &str = r#""\"" ( [^"\\\x00-\x1f] | "\\" ["\\/bfnrt] | "\\u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] )* "\"""#;

fn lit(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// The subset of JSON Schema agentd uses: objects (properties in order, all listed as required
/// are written), arrays with item schemas and min/max items, strings (optionally an enum),
/// numbers, integers, booleans, and string maps (`additionalProperties`). JSON is written
/// compactly, which also keeps the model from wandering into whitespace.
pub fn json_schema_to_gbnf(schema: &Value) -> Result<String, String> {
    let mut rules: Vec<(String, String)> = vec![("string".into(), JSON_STRING.into())];
    let root = schema_rule(schema, "root", &mut rules)?;
    let mut out = format!("root ::= {root}\n");
    for (name, body) in rules {
        out.push_str(&format!("{name} ::= {body}\n"));
    }
    Ok(out)
}

fn schema_rule(s: &Value, name: &str, rules: &mut Vec<(String, String)>) -> Result<String, String> {
    if let Some(e) = s.get("enum").and_then(Value::as_array) {
        let alts: Vec<String> = e.iter().map(|v| lit(&v.to_string())).collect();
        return Ok(format!("( {} )", alts.join(" | ")));
    }
    let ty = s.get("type").and_then(Value::as_str).unwrap_or("object");
    Ok(match ty {
        "string" => "string".into(),
        "integer" => r#"("-"? [0-9]+)"#.into(),
        "number" => r#"("-"? [0-9]+ ("." [0-9]+)?)"#.into(),
        "boolean" => r#"("true" | "false")"#.into(),
        "array" => {
            let item = match s.get("items") {
                Some(i) => schema_rule(i, &format!("{name}-item"), rules)?,
                None => "string".into(),
            };
            let min = s.get("minItems").and_then(Value::as_u64).unwrap_or(0) as usize;
            let max = s
                .get("maxItems")
                .and_then(Value::as_u64)
                .map(|m| m as usize);
            let r = format!("{name}-arr");
            let body = match (min, max) {
                (0, None) => format!(r#""[" ( {item} ( "," {item} )* )? "]""#),
                (m, None) => {
                    let firsts = vec![item.clone(); m].join(r#" "," "#);
                    format!(r#""[" {firsts} ( "," {item} )* "]""#)
                }
                (m, Some(x)) => {
                    // exactly m..=x items
                    let mut alts = Vec::new();
                    for k in m..=x {
                        if k == 0 {
                            alts.push(r#""[" "]""#.to_string());
                        } else {
                            alts.push(format!(
                                r#""[" {} "]""#,
                                vec![item.clone(); k].join(r#" "," "#)
                            ));
                        }
                    }
                    alts.join(" | ")
                }
            };
            rules.push((r.clone(), body));
            r
        }
        _ => {
            let props = s.get("properties").and_then(Value::as_object);
            match props {
                Some(props) if !props.is_empty() => {
                    let required: Vec<&str> = s
                        .get("required")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_else(|| props.keys().map(String::as_str).collect());
                    // `required` keeps the intended order (serde_json sorts object keys)
                    let mut parts = Vec::new();
                    for k in &required {
                        let Some(v) = props.get(*k) else { continue };
                        let vr = schema_rule(v, &format!("{name}-{k}"), rules)?;
                        parts.push(format!(r#"{} ":" {vr}"#, lit(&format!("\"{k}\""))));
                    }
                    let r = format!("{name}-obj");
                    rules.push((
                        r.clone(),
                        format!(r#""{{" {} "}}""#, parts.join(r#" "," "#)),
                    ));
                    r
                }
                _ => {
                    // a map: string keys, values of additionalProperties (strings by default)
                    let v = match s.get("additionalProperties") {
                        Some(ap) if ap.is_object() => {
                            schema_rule(ap, &format!("{name}-val"), rules)?
                        }
                        _ => "string".into(),
                    };
                    let r = format!("{name}-map");
                    rules.push((
                        r.clone(),
                        format!(r#""{{" ( string ":" {v} ( "," string ":" {v} )* )? "}}""#),
                    ));
                    r
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn accepts(g: &Grammar, s: &str) -> bool {
        let mut m = g.matcher();
        m.accept_bytes(s.as_bytes()) && m.complete()
    }
    #[test]
    fn the_tool_grammar_agentd_sends_is_enforced() {
        let g = Grammar::parse(
            "root ::= action\naction ::= act-call | act-hangup\n\
             act-call ::= \"{\\\"tool\\\":\\\"call\\\",\\\"args\\\":{\" \"\\\"to\\\":\" text \"}}\"\n\
             act-hangup ::= \"{\\\"tool\\\":\\\"hangup\\\",\\\"args\\\":{\" \"}}\"\n\
             text ::= \"\\\"\" [^\"\\\\]* \"\\\"\"\n",
        )
        .unwrap();
        assert!(accepts(&g, r#"{"tool":"call","args":{"to":"Mum"}}"#));
        assert!(accepts(&g, r#"{"tool":"hangup","args":{}}"#));
        assert!(!accepts(&g, r#"{"tool":"format_disk","args":{}}"#));
        assert!(
            !accepts(&g, r#"{"tool":"call","args":{"to":"Mum"}"#),
            "incomplete"
        );
        let mut m = g.matcher();
        assert!(m.allows(br#"{"tool":"c"#) && !m.allows(br#"{"tool":"x"#));
        assert!(m.accept_bytes(br#"{"tool":"call","args":{"to":"Ma"#));
        assert!(
            m.allows("é".as_bytes()),
            "any character inside the string, multi-byte too"
        );
        assert!(!m.complete() && m.can_continue());
    }
    #[test]
    fn operators_groups_and_classes() {
        let g = Grammar::parse("root ::= (\"a\" | \"b\")+ [0-9]? x*\nx ::= \"!\"").unwrap();
        assert!(accepts(&g, "abba7!!"));
        assert!(accepts(&g, "a"));
        assert!(!accepts(&g, ""));
        assert!(!accepts(&g, "ac"));
    }
    #[test]
    fn json_schemas_become_grammars() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "args": {"type": "array", "items": {"type": "string"}, "maxItems": 2},
                "tool": {"type": "string", "enum": ["a", "b"]},
                "kv": {"type": "object", "additionalProperties": {"type": "string"}},
                "n": {"type": "integer"}
            },
            "required": ["name", "args", "tool", "kv", "n"]
        });
        let g = Grammar::parse(&json_schema_to_gbnf(&schema).unwrap()).unwrap();
        assert!(accepts(
            &g,
            r#"{"name":"x \"q\"","args":["a","b"],"tool":"b","kv":{"k":"v"},"n":-12}"#
        ));
        assert!(
            !accepts(
                &g,
                r#"{"name":"x","args":["a","b","c"],"tool":"b","kv":{},"n":1}"#
            ),
            "too many items"
        );
        assert!(
            !accepts(&g, r#"{"name":"x","args":[],"tool":"c","kv":{},"n":1}"#),
            "not in the enum"
        );
    }
}
