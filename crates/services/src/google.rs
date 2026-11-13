//! Google Calendar and Gmail as connectors. Signed in once with the pasted-link flow (oauth.rs),
//! with a client the owner made in Google Cloud. Access tokens are refreshed when they expire
//! within a minute; the new tokens go back to the vault through `save`.
use crate::oauth::{self, Provider, Tokens};
use crate::{b64_std, b64url_decode, Http};
use agentos_action::{Action, Tier, Tool};
use agentos_executor::Connector;
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta};
use serde_json::{json, Value};

pub const CALENDAR_EVENTS: &str = "https://www.googleapis.com/calendar/v3/calendars/primary/events";
pub const GMAIL: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
pub const SCOPES: [&str; 3] = [
    "https://www.googleapis.com/auth/calendar.events",
    "https://www.googleapis.com/auth/gmail.readonly",
    "https://www.googleapis.com/auth/gmail.send",
];
const REFRESH_WITHIN_MS: u64 = 60_000;
const MAX_BODY_CHARS: usize = 4000;

/// Google's endpoints for a client the owner made ("client id:secret", as stored in the vault).
pub fn provider(client: &str) -> Provider {
    let (id, secret) = client.split_once(':').unwrap_or((client, ""));
    Provider {
        name: "google".into(),
        client_id: id.to_string(),
        client_secret: (!secret.is_empty()).then(|| secret.to_string()),
        auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
        token_url: "https://oauth2.googleapis.com/token".into(),
        scopes: SCOPES.iter().map(|s| s.to_string()).collect(),
        redirect: "http://127.0.0.1:8765/oauth/callback".into(),
    }
}

pub struct Google {
    provider: Provider,
    tokens: Tokens,
    http: Box<dyn Http>,
    /// The clock, in milliseconds since the epoch.
    now: Box<dyn Fn() -> u64>,
    /// The local time zone, in minutes east of UTC.
    offset_min: i32,
    /// Called with new tokens after a refresh, so they reach the vault.
    save: Box<dyn FnMut(&Tokens)>,
}

impl Google {
    pub fn new(
        provider: Provider,
        tokens: Tokens,
        http: Box<dyn Http>,
        now: Box<dyn Fn() -> u64>,
        offset_min: i32,
        save: Box<dyn FnMut(&Tokens)>,
    ) -> Self {
        Google {
            provider,
            tokens,
            http,
            now,
            offset_min,
            save,
        }
    }

    fn zone(&self) -> FixedOffset {
        FixedOffset::east_opt(self.offset_min * 60).unwrap_or(FixedOffset::east_opt(0).unwrap())
    }

    /// today, tomorrow or YYYY-MM-DD in the local time zone
    fn day(&self, day: &str) -> Result<NaiveDate, String> {
        let now = DateTime::from_timestamp_millis((self.now)() as i64)
            .ok_or("no clock")?
            .with_timezone(&self.zone());
        match day.trim() {
            "today" => Ok(now.date_naive()),
            "tomorrow" => Ok(now.date_naive() + TimeDelta::days(1)),
            d => NaiveDate::parse_from_str(d, "%Y-%m-%d")
                .map_err(|_| format!("not a day: {d} (say today, tomorrow or YYYY-MM-DD)")),
        }
    }

    fn at(&self, date: NaiveDate, time: NaiveTime) -> String {
        let dt = NaiveDateTime::new(date, time)
            .and_local_timezone(self.zone())
            .single()
            .unwrap_or_default();
        dt.to_rfc3339()
    }

    /// A token that is good for at least a minute, refreshed (and saved) when needed.
    fn token(&mut self) -> Result<String, String> {
        let now = (self.now)();
        if self.tokens.expires_at_ms <= now + REFRESH_WITHIN_MS {
            let t = oauth::refresh(&self.provider, &self.tokens, self.http.as_mut(), now)?;
            (self.save)(&t);
            self.tokens = t;
        }
        Ok(self.tokens.access.clone())
    }

    fn get(&mut self, url: &str) -> Result<Value, String> {
        let token = self.token()?;
        let raw = self
            .http
            .get(url, Some(&token))
            .map_err(|e| e.replace(&token, "<token>"))?;
        serde_json::from_str(&raw).map_err(|_| "google: bad answer".to_string())
    }

    fn post(&mut self, url: &str, body: &Value) -> Result<Value, String> {
        let token = self.token()?;
        let raw = self
            .http
            .post_json(url, Some(&token), body)
            .map_err(|e| e.replace(&token, "<token>"))?;
        serde_json::from_str(&raw).map_err(|_| "google: bad answer".to_string())
    }

    fn gcal_list(&mut self, day: &str) -> Result<String, String> {
        let d = self.day(day)?;
        let start = self.at(d, NaiveTime::MIN);
        let end = self.at(d + TimeDelta::days(1), NaiveTime::MIN);
        let url = format!(
            "{CALENDAR_EVENTS}?timeMin={}&timeMax={}&singleEvents=true&orderBy=startTime",
            enc(&start),
            enc(&end)
        );
        let v = self.get(&url)?;
        let lines: Vec<String> = v["items"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|e| {
                        let when = e["start"]["dateTime"]
                            .as_str()
                            .map(|t| t.get(11..16).unwrap_or(t).to_string())
                            .unwrap_or_else(|| "all day".into());
                        format!("{when} {}", e["summary"].as_str().unwrap_or("(no title)"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(if lines.is_empty() {
            "nothing".into()
        } else {
            lines.join("\n")
        })
    }

    fn gcal_add(&mut self, day: &str, time: &str, title: &str) -> Result<String, String> {
        let d = self.day(day)?;
        let t = NaiveTime::parse_from_str(time, "%H:%M")
            .map_err(|_| format!("not a time: {time} (say HH:MM)"))?;
        let body = json!({
            "summary": title,
            "start": {"dateTime": self.at(d, t)},
            "end": {"dateTime": self.at(d, t + TimeDelta::hours(1))},
        });
        self.post(CALENDAR_EVENTS, &body)?;
        Ok(format!("added {title} on {d} at {time}"))
    }

    fn gmail_unread(&mut self) -> Result<String, String> {
        let list = self.get(&format!("{GMAIL}/messages?q=is:unread&maxResults=10"))?;
        let mut lines = Vec::new();
        for m in list["messages"].as_array().cloned().unwrap_or_default() {
            let Some(id) = m["id"].as_str() else { continue };
            let meta = self.get(&format!("{GMAIL}/messages/{id}?format=metadata"))?;
            let header = |name: &str| -> String {
                meta["payload"]["headers"]
                    .as_array()
                    .and_then(|hs| {
                        hs.iter().find(|h| {
                            h["name"]
                                .as_str()
                                .is_some_and(|n| n.eq_ignore_ascii_case(name))
                        })
                    })
                    .and_then(|h| h["value"].as_str())
                    .unwrap_or("")
                    .to_string()
            };
            lines.push(format!(
                "{id}: from {} — {}",
                header("From"),
                header("Subject")
            ));
        }
        Ok(if lines.is_empty() {
            "no unread mail".into()
        } else {
            lines.join("\n")
        })
    }

    fn gmail_read(&mut self, id: &str) -> Result<String, String> {
        let msg = self.get(&format!("{GMAIL}/messages/{id}?format=full"))?;
        let body = plain_text(&msg["payload"]).ok_or("this message has no plain-text part")?;
        Ok(body.chars().take(MAX_BODY_CHARS).collect())
    }

    fn gmail_send(&mut self, to: &str, subject: &str, text: &str) -> Result<String, String> {
        let subject = if subject.is_ascii() {
            subject.to_string()
        } else {
            format!("=?UTF-8?B?{}?=", b64_std(subject.as_bytes()))
        };
        let message = format!(
            "To: {to}\r\nSubject: {subject}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{text}\r\n"
        );
        let raw = crate::oauth::b64url(message.as_bytes());
        self.post(&format!("{GMAIL}/messages/send"), &json!({"raw": raw}))?;
        Ok(format!("sent to {to}"))
    }
}

/// The first text/plain part, searching nested parts, decoded.
fn plain_text(part: &Value) -> Option<String> {
    if part["mimeType"].as_str() == Some("text/plain") {
        let data = part["body"]["data"].as_str()?;
        return String::from_utf8(b64url_decode(data)?).ok();
    }
    part["parts"].as_array()?.iter().find_map(plain_text)
}

/// Percent-encoding for a query value.
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn arg<'a>(a: &'a Action, name: &str) -> Result<&'a str, String> {
    a.args
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| format!("missing {name}"))
}

impl Connector for Google {
    fn name(&self) -> &str {
        "google"
    }
    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "gcal_list",
                args: &["day"],
                tier: Tier::Low,
            },
            Tool {
                name: "gcal_add",
                args: &["day", "time", "title"],
                tier: Tier::Medium,
            },
            Tool {
                name: "gmail_unread",
                args: &[],
                tier: Tier::Low,
            },
            Tool {
                name: "gmail_read",
                args: &["id"],
                tier: Tier::Low,
            },
            Tool {
                name: "gmail_send",
                args: &["to", "subject", "text"],
                tier: Tier::High,
            },
        ]
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        match a.tool.as_str() {
            "gcal_list" => self.gcal_list(arg(a, "day")?),
            "gcal_add" => {
                let (day, time, title) = (
                    arg(a, "day")?.to_string(),
                    arg(a, "time")?.to_string(),
                    arg(a, "title")?.to_string(),
                );
                self.gcal_add(&day, &time, &title)
            }
            "gmail_unread" => self.gmail_unread(),
            "gmail_read" => self.gmail_read(arg(a, "id")?),
            "gmail_send" => {
                let (to, subject, text) = (
                    arg(a, "to")?.to_string(),
                    arg(a, "subject")?.to_string(),
                    arg(a, "text")?.to_string(),
                );
                self.gmail_send(&to, &subject, &text)
            }
            other => Err(format!("google cannot {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// One recorded request: method, url, bearer token, JSON body.
    type Sent = (String, String, Option<String>, Option<Value>);

    struct Fake {
        sent: Rc<RefCell<Vec<Sent>>>,
        /// answers by url substring, first match wins
        answers: Vec<(&'static str, String)>,
    }
    impl Fake {
        fn answer(&self, url: &str) -> Result<String, String> {
            self.answers
                .iter()
                .find(|(k, _)| url.contains(k))
                .map(|(_, v)| v.clone())
                .ok_or_else(|| format!("no answer for {url}"))
        }
    }
    impl Http for Fake {
        fn get(&mut self, url: &str, bearer: Option<&str>) -> Result<String, String> {
            self.sent.borrow_mut().push((
                "GET".into(),
                url.into(),
                bearer.map(str::to_string),
                None,
            ));
            self.answer(url)
        }
        fn post_json(
            &mut self,
            url: &str,
            bearer: Option<&str>,
            body: &Value,
        ) -> Result<String, String> {
            self.sent.borrow_mut().push((
                "POST".into(),
                url.into(),
                bearer.map(str::to_string),
                Some(body.clone()),
            ));
            self.answer(url)
        }
        fn post_form(&mut self, url: &str, fields: &[(&str, &str)]) -> Result<String, String> {
            let body = Value::Object(
                fields
                    .iter()
                    .map(|(k, v)| (k.to_string(), json!(v)))
                    .collect(),
            );
            self.sent
                .borrow_mut()
                .push(("POST".into(), url.into(), None, Some(body)));
            self.answer(url)
        }
    }

    /// 2026-11-11 10:00 UTC, which is 15:30 in India
    const NOW: u64 = 1_794_391_200_000;

    fn google(
        sent: &Rc<RefCell<Vec<Sent>>>,
        answers: Vec<(&'static str, String)>,
        expires_at_ms: u64,
    ) -> (Google, Rc<RefCell<Vec<Tokens>>>) {
        let saved = Rc::new(RefCell::new(vec![]));
        let s2 = saved.clone();
        let tokens = Tokens {
            access: "acc".into(),
            refresh: Some("ref".into()),
            expires_at_ms,
        };
        let g = Google::new(
            provider("client.id:secret"),
            tokens,
            Box::new(Fake {
                sent: sent.clone(),
                answers,
            }),
            Box::new(|| NOW),
            330,
            Box::new(move |t: &Tokens| s2.borrow_mut().push(t.clone())),
        );
        (g, saved)
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

    const FAR: u64 = u64::MAX / 2;

    #[test]
    fn gcal_list_asks_for_the_local_day() {
        let sent = Rc::new(RefCell::new(vec![]));
        let (mut g, _) = google(&sent, vec![("calendar", r#"{"items":[{"summary":"Dentist","start":{"dateTime":"2026-11-12T10:00:00+05:30"}}]}"#.into())], FAR);
        assert_eq!(
            g.call(&act("gcal_list", &[("day", "tomorrow")])).unwrap(),
            "10:00 Dentist"
        );
        let s = sent.borrow();
        assert_eq!(s[0].1, "https://www.googleapis.com/calendar/v3/calendars/primary/events?timeMin=2026-11-12T00%3A00%3A00%2B05%3A30&timeMax=2026-11-13T00%3A00%3A00%2B05%3A30&singleEvents=true&orderBy=startTime");
        assert_eq!(s[0].2.as_deref(), Some("acc"));
    }

    #[test]
    fn gcal_add_posts_a_one_hour_event() {
        let sent = Rc::new(RefCell::new(vec![]));
        let (mut g, _) = google(&sent, vec![("calendar", "{}".into())], FAR);
        g.call(&act(
            "gcal_add",
            &[
                ("day", "2026-11-12"),
                ("time", "10:00"),
                ("title", "Dentist"),
            ],
        ))
        .unwrap();
        let s = sent.borrow();
        assert_eq!(s[0].0, "POST");
        assert_eq!(s[0].1, CALENDAR_EVENTS);
        assert_eq!(
            s[0].3.clone().unwrap(),
            json!({"summary": "Dentist", "start": {"dateTime": "2026-11-12T10:00:00+05:30"}, "end": {"dateTime": "2026-11-12T11:00:00+05:30"}})
        );
    }

    #[test]
    fn gmail_unread_reads_the_headers_of_each_message() {
        let sent = Rc::new(RefCell::new(vec![]));
        let answers = vec![
            ("messages?q=is:unread", r#"{"messages":[{"id":"m1"},{"id":"m2"}]}"#.into()),
            ("messages/m1?format=metadata", r#"{"payload":{"headers":[{"name":"From","value":"Asha"},{"name":"Subject","value":"Lunch"}]}}"#.into()),
            ("messages/m2?format=metadata", r#"{"payload":{"headers":[{"name":"from","value":"Ravi"},{"name":"Subject","value":"Rent"}]}}"#.into()),
        ];
        let (mut g, _) = google(&sent, answers, FAR);
        assert_eq!(
            g.call(&act("gmail_unread", &[])).unwrap(),
            "m1: from Asha — Lunch\nm2: from Ravi — Rent"
        );
        assert_eq!(
            sent.borrow()[0].1,
            "https://gmail.googleapis.com/gmail/v1/users/me/messages?q=is:unread&maxResults=10"
        );
    }

    #[test]
    fn gmail_read_returns_the_plain_text_part() {
        let sent = Rc::new(RefCell::new(vec![]));
        // "Hello there" in base64url
        let body = crate::oauth::b64url(b"Hello there");
        let msg = json!({"payload": {"mimeType": "multipart/alternative", "parts": [
            {"mimeType": "text/html", "body": {"data": "PGI-aGk8L2I-"}},
            {"mimeType": "text/plain", "body": {"data": body}}]}});
        let (mut g, _) = google(
            &sent,
            vec![("messages/m1?format=full", msg.to_string())],
            FAR,
        );
        assert_eq!(
            g.call(&act("gmail_read", &[("id", "m1")])).unwrap(),
            "Hello there"
        );
        assert_eq!(
            sent.borrow()[0].1,
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/m1?format=full"
        );
    }

    #[test]
    fn gmail_send_posts_a_base64url_message() {
        let sent = Rc::new(RefCell::new(vec![]));
        let (mut g, _) = google(&sent, vec![("messages/send", r#"{"id":"x"}"#.into())], FAR);
        g.call(&act(
            "gmail_send",
            &[
                ("to", "asha@example.com"),
                ("subject", "Lunch"),
                ("text", "Noon?"),
            ],
        ))
        .unwrap();
        let s = sent.borrow();
        assert_eq!(
            s[0].1,
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/send"
        );
        let raw = s[0].3.as_ref().unwrap()["raw"]
            .as_str()
            .unwrap()
            .to_string();
        let text = String::from_utf8(crate::b64url_decode(&raw).unwrap()).unwrap();
        assert!(
            text.starts_with("To: asha@example.com\r\nSubject: Lunch\r\n"),
            "{text}"
        );
        assert!(text.ends_with("\r\n\r\nNoon?\r\n"));
    }

    #[test]
    fn an_expiring_token_is_refreshed_and_saved() {
        let sent = Rc::new(RefCell::new(vec![]));
        let answers = vec![
            (
                "oauth2.googleapis.com/token",
                r#"{"access_token":"new","expires_in":3600}"#.into(),
            ),
            ("calendar", r#"{"items":[]}"#.into()),
        ];
        // expires in 30 seconds: inside the one-minute margin
        let (mut g, saved) = google(&sent, answers, NOW + 30_000);
        assert_eq!(
            g.call(&act("gcal_list", &[("day", "today")])).unwrap(),
            "nothing"
        );
        let s = sent.borrow();
        assert_eq!(s[0].1, "https://oauth2.googleapis.com/token");
        assert_eq!(
            s[1].2.as_deref(),
            Some("new"),
            "the calendar call uses the new token"
        );
        assert_eq!(saved.borrow()[0].access, "new");
        assert_eq!(saved.borrow()[0].refresh.as_deref(), Some("ref"));
    }
}
