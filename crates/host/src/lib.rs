//! The local model host: talks to llama.cpp's llama-server over HTTP.
//! One server per local brain (small and large). The grammar travels with every request.
use agentos_brain::Model;
use agentos_router::Brain;
use serde_json::json;
use std::time::Duration;

pub struct LlamaHost {
    pub small_url: String,
    pub large_url: String,
    pub grammar: String,
    pub max_tokens: u32,
    pub timeout: Duration,
    /// What the model is told before the request: who it is and what each tool does.
    /// With it, requests go to the chat endpoint so each model's own chat template is applied;
    /// without it, the raw request goes to /completion.
    pub system: Option<String>,
}

impl LlamaHost {
    fn url_for(&self, brain: Brain) -> Option<&str> {
        match brain {
            Brain::LocalSmall => Some(&self.small_url),
            Brain::LocalLarge => Some(&self.large_url),
            Brain::Commercial => None,
        }
    }

    fn call(&self, url: &str, prompt: &str) -> Result<String, String> {
        if let Some(system) = &self.system {
            return self.chat(url, system, prompt);
        }
        let body = json!({
            "prompt": prompt,
            "grammar": self.grammar,
            "n_predict": self.max_tokens,
            "temperature": 0.2,
            "cache_prompt": true
        });
        let agent = ureq::AgentBuilder::new().timeout(self.timeout).build();
        let resp = agent
            .post(&format!("{url}/completion"))
            .send_json(body)
            .map_err(|e| e.to_string())?;
        let v: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
        v.get("content")
            .and_then(|c| c.as_str())
            .map(String::from)
            .ok_or_else(|| "no content field".to_string())
    }
}

impl LlamaHost {
    fn chat(&self, url: &str, system: &str, prompt: &str) -> Result<String, String> {
        let body = json!({
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": prompt}
            ],
            "grammar": self.grammar,
            "max_tokens": self.max_tokens,
            "temperature": 0.2,
            "cache_prompt": true
        });
        let agent = ureq::AgentBuilder::new().timeout(self.timeout).build();
        let resp = agent
            .post(&format!("{url}/v1/chat/completions"))
            .send_json(body)
            .map_err(|e| e.to_string())?;
        let v: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
        v["choices"][0]["message"]["content"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| "no message content".to_string())
    }
}

impl Model for LlamaHost {
    /// A failed call returns an empty string. Empty fails the registry, so the engine counts
    /// it as a local failure and escalates, which is the behaviour we want from a dead server.
    fn complete(&mut self, brain: Brain, prompt: &str) -> String {
        match self.url_for(brain) {
            Some(url) => self.call(url, prompt).unwrap_or_default(),
            // The commercial client is added in Volume 7, behind the same trait.
            None => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// A one-shot fake llama-server. Returns (base_url, receiver of the raw request text).
    fn fake_server(reply_body: &'static str) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 65536];
            let mut got = 0;
            // read until we have headers plus the full body
            loop {
                let n = s.read(&mut buf[got..]).unwrap();
                got += n;
                let text = String::from_utf8_lossy(&buf[..got]).to_string();
                if let Some(i) = text.find("\r\n\r\n") {
                    let len = text[..i]
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if got >= i + 4 + len || n == 0 {
                        tx.send(text).unwrap();
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", reply_body.len(), reply_body);
            s.write_all(resp.as_bytes()).unwrap();
        });
        (url, rx)
    }

    fn host(url: &str) -> LlamaHost {
        LlamaHost {
            small_url: url.into(),
            large_url: url.into(),
            grammar: "root ::= action".into(),
            max_tokens: 128,
            timeout: Duration::from_secs(5),
            system: None,
        }
    }

    #[test]
    fn with_a_system_prompt_it_uses_the_chat_endpoint() {
        let (url, rx) = fake_server(
            r#"{"choices":[{"message":{"role":"assistant","content":"{\"tool\":\"call\"}"}}]}"#,
        );
        let mut h = host(&url);
        h.system = Some("You run a phone. Tools: call(to)".into());
        assert_eq!(
            h.complete(Brain::LocalSmall, "ring mum"),
            r#"{"tool":"call"}"#
        );
        let req = rx.recv().unwrap();
        assert!(req.starts_with("POST /v1/chat/completions"));
        assert!(req.contains("You run a phone") && req.contains("ring mum"));
        assert!(
            req.contains("root ::= action"),
            "the grammar still constrains the answer"
        );
    }

    #[test]
    fn sends_prompt_and_grammar_and_returns_content() {
        let (url, rx) = fake_server(r#"{"content":"{\"tool\":\"pay\"}"}"#);
        let mut h = host(&url);
        let out = h.complete(Brain::LocalSmall, "remind me at 6");
        assert_eq!(out, r#"{"tool":"pay"}"#);
        let req = rx.recv().unwrap();
        assert!(req.starts_with("POST /completion"));
        assert!(req.contains("remind me at 6"));
        assert!(req.contains("root ::= action"));
    }
    #[test]
    fn a_dead_server_returns_empty_not_a_panic() {
        let mut h = host("http://127.0.0.1:9");
        assert_eq!(h.complete(Brain::LocalLarge, "x"), "");
    }
    #[test]
    fn missing_content_field_returns_empty() {
        let (url, _rx) = fake_server(r#"{"oops":1}"#);
        let mut h = host(&url);
        assert_eq!(h.complete(Brain::LocalSmall, "x"), "");
    }
    #[test]
    fn the_host_never_answers_for_the_commercial_brain() {
        let mut h = host("http://127.0.0.1:9");
        assert_eq!(h.complete(Brain::Commercial, "x"), "");
    }
}
