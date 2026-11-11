//! A Telegram bot as a connector. A bot can write only to chats that have messaged it first;
//! a personal client would need TDLib and is out of scope.
use crate::Http;
use agentos_action::{Action, Tier, Tool};
use agentos_executor::Connector;
use serde_json::Value;

pub struct Telegram {
    token: String,
    http: Box<dyn Http>,
    /// The next update to ask for: the last one seen plus one.
    offset: i64,
}

impl Telegram {
    pub fn new(token: String, http: Box<dyn Http>) -> Self {
        Telegram {
            token,
            http,
            offset: 0,
        }
    }

    fn url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{method}", self.token)
    }

    /// An error text never carries the token: it is the bot's secret.
    fn clean(&self, e: String) -> String {
        e.replace(&self.token, "<token>")
    }

    fn send(&mut self, chat: &str, text: &str) -> Result<String, String> {
        let url = self.url("sendMessage");
        let raw = self
            .http
            .post_form(&url, &[("chat_id", chat), ("text", text)])
            .map_err(|e| self.clean(e))?;
        let v: Value =
            serde_json::from_str(&raw).map_err(|_| "telegram: bad answer".to_string())?;
        if v["ok"] != Value::Bool(true) {
            return Err(format!(
                "telegram refused: {}",
                v["description"].as_str().unwrap_or("unknown")
            ));
        }
        Ok(format!("sent to {chat}"))
    }

    fn read(&mut self) -> Result<String, String> {
        let url = format!("{}?offset={}", self.url("getUpdates"), self.offset);
        let raw = self.http.get(&url, None).map_err(|e| self.clean(e))?;
        let v: Value =
            serde_json::from_str(&raw).map_err(|_| "telegram: bad answer".to_string())?;
        if v["ok"] != Value::Bool(true) {
            return Err(format!(
                "telegram refused: {}",
                v["description"].as_str().unwrap_or("unknown")
            ));
        }
        let mut lines = Vec::new();
        for u in v["result"].as_array().cloned().unwrap_or_default() {
            if let Some(id) = u["update_id"].as_i64() {
                self.offset = self.offset.max(id + 1);
            }
            let m = &u["message"];
            let Some(text) = m["text"].as_str() else {
                continue;
            };
            let name = m["from"]["first_name"].as_str().unwrap_or("someone");
            let chat = m["chat"]["id"].to_string();
            lines.push(format!("from {name} ({chat}): {text}"));
        }
        Ok(if lines.is_empty() {
            "no new messages".into()
        } else {
            lines.join("\n")
        })
    }
}

fn arg<'a>(a: &'a Action, name: &str) -> Result<&'a str, String> {
    a.args
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| format!("missing {name}"))
}

impl Connector for Telegram {
    fn name(&self) -> &str {
        "telegram"
    }
    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "telegram_send",
                args: &["chat", "text"],
                tier: Tier::High,
            },
            Tool {
                name: "telegram_read",
                args: &[],
                tier: Tier::Low,
            },
        ]
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        match a.tool.as_str() {
            "telegram_send" => {
                let chat = arg(a, "chat")?.to_string();
                let text = arg(a, "text")?.to_string();
                self.send(&chat, &text)
            }
            "telegram_read" => self.read(),
            other => Err(format!("telegram cannot {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct Log {
        posts: Vec<(String, Vec<(String, String)>)>,
        gets: Vec<String>,
        answers: Vec<String>,
        fail: Option<String>,
    }
    struct Fake(Rc<RefCell<Log>>);
    impl Http for Fake {
        fn get(&mut self, url: &str, _bearer: Option<&str>) -> Result<String, String> {
            let mut l = self.0.borrow_mut();
            l.gets.push(url.to_string());
            if let Some(e) = l.fail.clone() {
                return Err(e);
            }
            Ok(l.answers.remove(0))
        }
        fn post_json(
            &mut self,
            _url: &str,
            _bearer: Option<&str>,
            _body: &serde_json::Value,
        ) -> Result<String, String> {
            Err("not used".into())
        }
        fn post_form(&mut self, url: &str, fields: &[(&str, &str)]) -> Result<String, String> {
            let mut l = self.0.borrow_mut();
            l.posts.push((
                url.to_string(),
                fields
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ));
            if let Some(e) = l.fail.clone() {
                return Err(e);
            }
            Ok(l.answers.remove(0))
        }
    }

    const TOKEN: &str = "123:SECRET";

    fn bot(log: &Rc<RefCell<Log>>) -> Telegram {
        Telegram::new(TOKEN.into(), Box::new(Fake(log.clone())))
    }
    fn act(tool: &str, args: &[(&str, &str)]) -> Action {
        Action {
            tool: tool.into(),
            args: args
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn send_posts_the_exact_request() {
        let log = Rc::new(RefCell::new(Log {
            answers: vec![r#"{"ok":true}"#.into()],
            ..Log::default()
        }));
        let mut t = bot(&log);
        assert_eq!(
            t.call(&act(
                "telegram_send",
                &[("chat", "42"), ("text", "hi there")]
            ))
            .unwrap(),
            "sent to 42"
        );
        let l = log.borrow();
        assert_eq!(
            l.posts[0].0,
            "https://api.telegram.org/bot123:SECRET/sendMessage"
        );
        assert_eq!(
            l.posts[0].1,
            vec![
                ("chat_id".to_string(), "42".to_string()),
                ("text".to_string(), "hi there".to_string())
            ]
        );
    }

    #[test]
    fn the_token_never_appears_in_an_error() {
        let log = Rc::new(RefCell::new(Log {
            fail: Some("dns: https://api.telegram.org/bot123:SECRET/getUpdates".into()),
            ..Log::default()
        }));
        let mut t = bot(&log);
        let e = t.call(&act("telegram_read", &[])).unwrap_err();
        assert!(!e.contains("SECRET"), "{e}");
        assert!(e.contains("<token>"));
    }

    #[test]
    fn the_offset_advances_past_the_last_update() {
        let updates = r#"{"ok":true,"result":[
            {"update_id":10,"message":{"from":{"first_name":"Asha"},"chat":{"id":7},"text":"hello"}},
            {"update_id":11,"message":{"from":{"first_name":"Ravi"},"chat":{"id":8},"text":"see you"}}]}"#;
        let log = Rc::new(RefCell::new(Log {
            answers: vec![updates.into(), r#"{"ok":true,"result":[]}"#.into()],
            ..Log::default()
        }));
        let mut t = bot(&log);
        let text = t.call(&act("telegram_read", &[])).unwrap();
        assert_eq!(text, "from Asha (7): hello\nfrom Ravi (8): see you");
        assert_eq!(
            t.call(&act("telegram_read", &[])).unwrap(),
            "no new messages"
        );
        let l = log.borrow();
        assert!(l.gets[0].ends_with("getUpdates?offset=0"));
        assert!(l.gets[1].ends_with("getUpdates?offset=12"));
    }
}
