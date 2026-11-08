//! Byte-level BPE (GPT-2 style, as Qwen uses), read from the GGUF file: the vocabulary, the merge
//! rules, and which tokens are special (`<|im_start|>` and the like are matched whole, first).
use crate::gguf::{Gguf, Value};
use std::collections::HashMap;

pub struct Tokenizer {
    pub tokens: Vec<String>,
    ids: HashMap<String, u32>,
    merges: HashMap<(String, String), usize>,
    /// Control and user-defined tokens, longest first.
    special: Vec<(String, u32)>,
    is_special: Vec<bool>,
    pre: fancy_regex::Regex,
    byte_to_char: [char; 256],
    char_to_byte: HashMap<char, u8>,
    pub eos: u32,
    pub eot: Vec<u32>,
}

/// GPT-2's map from bytes to printable characters, so every byte string is a valid token string.
fn byte_chars() -> [char; 256] {
    let mut out = ['\0'; 256];
    let mut n = 0u32;
    for b in 0..256u32 {
        let printable =
            (33..=126).contains(&b) || (161..=172).contains(&b) || (174..=255).contains(&b);
        out[b as usize] = if printable {
            char::from_u32(b).unwrap()
        } else {
            n += 1;
            char::from_u32(255 + n).unwrap()
        };
    }
    out
}

/// The pre-split rule for each tokenizer family (from llama.cpp's llama-vocab.cpp).
fn pre_regex(pre: &str) -> &'static str {
    match pre {
        "qwen35" => {
            r"(?:'[sS]|'[tT]|'[rR][eE]|'[vV][eE]|'[mM]|'[lL][lL]|'[dD])|[^\r\n\p{L}\p{N}]?[\p{L}\p{M}]+|\p{N}| ?[^\s\p{L}\p{M}\p{N}]+[\r\n]*|\s*[\r\n]+|\s+(?!\S)|\s+"
        }
        // qwen2 and the GPT-4o-like default
        _ => {
            r"(?:'[sS]|'[tT]|'[rR][eE]|'[vV][eE]|'[mM]|'[lL][lL]|'[dD])|[^\r\n\p{L}\p{N}]?\p{L}+|\p{N}| ?[^\s\p{L}\p{N}]+[\r\n]*|\s*[\r\n]+|\s+(?!\S)|\s+"
        }
    }
}

impl Tokenizer {
    pub fn from_gguf(g: &Gguf) -> Result<Tokenizer, String> {
        let model = g
            .get("tokenizer.ggml.model")
            .and_then(Value::as_str)
            .unwrap_or("");
        if model != "gpt2" {
            return Err(format!(
                "tokenizer model {model:?} is not supported (only gpt2-style BPE)"
            ));
        }
        let tokens: Vec<String> = g
            .get("tokenizer.ggml.tokens")
            .and_then(Value::as_array)
            .ok_or("no tokenizer.ggml.tokens")?
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect();
        let types: Vec<u64> = g
            .get("tokenizer.ggml.token_type")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|v| v.as_u64().unwrap_or(1)).collect())
            .unwrap_or_else(|| vec![1; tokens.len()]);
        let mut merges = HashMap::new();
        if let Some(m) = g.get("tokenizer.ggml.merges").and_then(Value::as_array) {
            for (rank, v) in m.iter().enumerate() {
                if let Some((a, b)) = v.as_str().and_then(|s| s.split_once(' ')) {
                    merges.insert((a.to_string(), b.to_string()), rank);
                }
            }
        }
        let ids: HashMap<String, u32> = tokens
            .iter()
            .enumerate()
            .map(|(i, t)| (t.clone(), i as u32))
            .collect();
        let is_special: Vec<bool> = types.iter().map(|t| *t == 3 || *t == 4).collect();
        let mut special: Vec<(String, u32)> = tokens
            .iter()
            .enumerate()
            .filter(|(i, t)| is_special[*i] && !t.is_empty())
            .map(|(i, t)| (t.clone(), i as u32))
            .collect();
        special.sort_by_key(|s| std::cmp::Reverse(s.0.len()));
        let pre_name = g
            .get("tokenizer.ggml.pre")
            .and_then(Value::as_str)
            .unwrap_or("");
        let pre = fancy_regex::Regex::new(pre_regex(pre_name)).map_err(|e| e.to_string())?;
        let byte_to_char = byte_chars();
        let char_to_byte = byte_to_char
            .iter()
            .enumerate()
            .map(|(b, c)| (*c, b as u8))
            .collect();
        let eos = g
            .get("tokenizer.ggml.eos_token_id")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        // end-of-turn tokens a chat model may emit
        let eot = ["<|im_end|>", "<|endoftext|>", "<|eot_id|>"]
            .iter()
            .filter_map(|t| ids.get(*t).copied())
            .chain(std::iter::once(eos))
            .collect();
        Ok(Tokenizer {
            tokens,
            ids,
            merges,
            special,
            is_special,
            pre,
            byte_to_char,
            char_to_byte,
            eos,
            eot,
        })
    }

    pub fn vocab_size(&self) -> usize {
        self.tokens.len()
    }
    pub fn id(&self, s: &str) -> Option<u32> {
        self.ids.get(s).copied()
    }

    fn bpe(&self, piece: &str, out: &mut Vec<u32>) {
        let mapped: String = piece
            .bytes()
            .map(|b| self.byte_to_char[b as usize])
            .collect();
        if let Some(id) = self.ids.get(&mapped) {
            out.push(*id);
            return;
        }
        let mut syms: Vec<String> = mapped.chars().map(|c| c.to_string()).collect();
        while syms.len() > 1 {
            let mut best: Option<(usize, usize)> = None;
            for i in 0..syms.len() - 1 {
                if let Some(&r) = self.merges.get(&(syms[i].clone(), syms[i + 1].clone())) {
                    if best.is_none_or(|(_, br)| r < br) {
                        best = Some((i, r));
                    }
                }
            }
            let Some((i, _)) = best else { break };
            let merged = format!("{}{}", syms[i], syms[i + 1]);
            syms[i] = merged;
            syms.remove(i + 1);
        }
        for s in syms {
            match self.ids.get(&s) {
                Some(id) => out.push(*id),
                // should not happen with a byte-level vocabulary; fall back to single characters
                None => {
                    for c in s.chars() {
                        if let Some(id) = self.ids.get(&c.to_string()) {
                            out.push(*id);
                        }
                    }
                }
            }
        }
    }

    fn encode_plain(&self, text: &str, out: &mut Vec<u32>) {
        for m in self.pre.find_iter(text).flatten() {
            self.bpe(m.as_str(), out);
        }
    }

    /// Text to token ids. Special tokens written in the text (`<|im_start|>`) become themselves.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let mut out = Vec::new();
        let mut rest = text;
        'outer: while !rest.is_empty() {
            // the earliest special token, longest at a given position
            let mut first: Option<(usize, &str, u32)> = None;
            for (s, id) in &self.special {
                if let Some(at) = rest.find(s.as_str()) {
                    if first.is_none_or(|(f, fs, _)| at < f || (at == f && s.len() > fs.len())) {
                        first = Some((at, s, *id));
                    }
                }
            }
            match first {
                Some((at, s, id)) => {
                    self.encode_plain(&rest[..at], &mut out);
                    out.push(id);
                    rest = &rest[at + s.len()..];
                }
                None => {
                    self.encode_plain(rest, &mut out);
                    break 'outer;
                }
            }
        }
        out
    }

    /// The bytes a token stands for (special tokens as their text).
    pub fn token_bytes(&self, id: u32) -> Vec<u8> {
        let Some(t) = self.tokens.get(id as usize) else {
            return vec![];
        };
        if self.is_special[id as usize] {
            return t.as_bytes().to_vec();
        }
        t.chars()
            .map(|c| self.char_to_byte.get(&c).copied().unwrap_or(b'?'))
            .collect()
    }

    pub fn decode(&self, ids: &[u32]) -> String {
        let bytes: Vec<u8> = ids.iter().flat_map(|i| self.token_bytes(*i)).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub fn is_special(&self, id: u32) -> bool {
        self.is_special.get(id as usize).copied().unwrap_or(false)
    }
}
