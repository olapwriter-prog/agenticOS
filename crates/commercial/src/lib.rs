//! The commercial brain: a client for the Anthropic Messages API, behind the same `Model` trait
//! the local host uses. Local brains are delegated to an inner model; only `Brain::Commercial`
//! goes out over the network. The context firewall runs before this (Vol 3), so the prompt is
//! already redacted. The key goes in a header and never in the body.
use agentos_brain::Model;
use agentos_router::Brain;
use serde_json::{json, Value};
use std::time::Duration;

pub struct CommercialClient<M: Model> {
    pub local: M,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub timeout: Duration,
}

impl<M: Model> CommercialClient<M> {
    pub fn new(local: M, api_key: &str, model: &str) -> Self {
        CommercialClient {
            local,
            base_url: "https://api.anthropic.com".into(),
            api_key: api_key.into(),
            model: model.into(),
            max_tokens: 1024,
            timeout: Duration::from_secs(30),
        }
    }

    fn call(&self, prompt: &str) -> Result<String, String> {
        let body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": [{"role": "user", "content": prompt}]
        });
        let agent = ureq::AgentBuilder::new().timeout(self.timeout).build();
        let resp = agent
            .post(&format!("{}/v1/messages", self.base_url))
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", "2023-06-01")
            .send_json(body)
            .map_err(|e| e.to_string())?;
        let v: Value = resp.into_json().map_err(|e| e.to_string())?;
        extract_text(&v).ok_or_else(|| "no text block in reply".to_string())
    }
}

/// The first block of type "text" in `content`.
pub fn extract_text(v: &Value) -> Option<String> {
    v.get("content")?
        .as_array()?
        .iter()
        .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))?
        .get("text")?
        .as_str()
        .map(String::from)
}

impl<M: Model> Model for CommercialClient<M> {
    /// Any failure (network, status, shape) returns an empty string, which the engine treats as a
    /// failed attempt. The error text is deliberately not returned: it could echo request details.
    fn complete(&mut self, brain: Brain, prompt: &str) -> String {
        match brain {
            Brain::Commercial => self.call(prompt).unwrap_or_default(),
            other => self.local.complete(other, prompt),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    struct Local;
    impl Model for Local {
        fn complete(&mut self, _b: Brain, _p: &str) -> String {
            "from-local".into()
        }
    }

    /// One-shot fake server: replies with `status` and `body`, sends the raw request back on the channel.
    fn fake(status: &str, body: &str) -> (String, mpsc::Receiver<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        let (status, body) = (status.to_string(), body.to_string());
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = vec![0u8; 16384];
            let mut got = Vec::new();
            loop {
                let n = s.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
                let t = String::from_utf8_lossy(&got).to_string();
                if let Some(i) = t.find("\r\n\r\n") {
                    let len = t[..i]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if got.len() >= i + 4 + len {
                        break;
                    }
                }
            }
            tx.send(String::from_utf8_lossy(&got).to_string()).unwrap();
            let r = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            s.write_all(r.as_bytes()).unwrap();
        });
        (url, rx)
    }
    fn client(url: String) -> CommercialClient<Local> {
        let mut c = CommercialClient::new(Local, "sk-test-KEY", "claude-sonnet-5-5");
        c.base_url = url;
        c.timeout = Duration::from_secs(3);
        c
    }

    #[test]
    fn a_good_reply_returns_the_first_text_block() {
        let (url, rx) = fake(
            "200 OK",
            r#"{"content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"{\"tool\":\"x\"}"}]}"#,
        );
        let mut c = client(url);
        assert_eq!(
            c.complete(Brain::Commercial, "plan this"),
            r#"{"tool":"x"}"#
        );
        let req = rx.recv().unwrap();
        assert!(req.starts_with("POST /v1/messages"));
        let lower = req.to_ascii_lowercase();
        assert!(lower.contains("x-api-key: sk-test-key"));
        assert!(lower.contains("anthropic-version: 2023-06-01"));
    }
    #[test]
    fn the_body_carries_model_and_prompt_but_never_the_key() {
        let (url, rx) = fake("200 OK", r#"{"content":[{"type":"text","text":"ok"}]}"#);
        let mut c = client(url);
        c.complete(Brain::Commercial, "hello there");
        let req = rx.recv().unwrap();
        let body = req.split("\r\n\r\n").nth(1).unwrap();
        let v: Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["model"], "claude-sonnet-5-5");
        assert_eq!(v["max_tokens"], 1024);
        assert_eq!(v["messages"][0]["role"], "user");
        assert_eq!(v["messages"][0]["content"], "hello there");
        assert!(!body.contains("sk-test-KEY"));
    }
    #[test]
    fn http_errors_give_an_empty_string() {
        let (url, _rx) = fake("529 Overloaded", r#"{"error":{"type":"overloaded_error"}}"#);
        assert_eq!(client(url).complete(Brain::Commercial, "x"), "");
        let (url, _rx) = fake("401 Unauthorized", r#"{"error":{}}"#);
        assert_eq!(client(url).complete(Brain::Commercial, "x"), "");
    }
    #[test]
    fn a_reply_with_no_text_block_gives_an_empty_string() {
        let (url, _rx) = fake("200 OK", r#"{"content":[{"type":"tool_use"}]}"#);
        assert_eq!(client(url).complete(Brain::Commercial, "x"), "");
        let (url, _rx) = fake("200 OK", "not json");
        assert_eq!(client(url).complete(Brain::Commercial, "x"), "");
    }
    #[test]
    fn an_unreachable_server_gives_an_empty_string() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        drop(l);
        assert_eq!(client(url).complete(Brain::Commercial, "x"), "");
    }
    #[test]
    fn local_brains_never_touch_the_network() {
        let mut c = client("http://127.0.0.1:1".into());
        assert_eq!(c.complete(Brain::LocalSmall, "x"), "from-local");
        assert_eq!(c.complete(Brain::LocalLarge, "x"), "from-local");
    }
    #[test]
    fn extract_text_handles_odd_shapes() {
        assert_eq!(extract_text(&json!({})), None);
        assert_eq!(extract_text(&json!({"content": "str"})), None);
        assert_eq!(extract_text(&json!({"content":[{"type":"text"}]})), None);
        assert_eq!(
            extract_text(
                &json!({"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]})
            ),
            Some("a".into())
        );
    }
}
