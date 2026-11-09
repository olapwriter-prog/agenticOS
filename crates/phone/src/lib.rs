//! Headless phone services: calls, SMS, contacts, USSD, Wi-Fi, mobile data, airplane mode,
//! location, torch, volume and feedback, exposed as tools behind the executor.
//!
//! Nothing here has a screen. The agent (local intents, the local model, or a Claude client over
//! MCP) asks for an action; the executor checks its tier; this crate does it on the hardware.
//! Calling someone and sending a text are High tier, so they need the approval key.
pub mod backend;
pub mod linux;
pub mod watch;

use agentos_action::{Action, Tier, Tool};
use agentos_executor::Connector;
use backend::{Backend, CallState};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

pub use backend::{Call, NetStatus, Sim, Sms, WifiNet};
pub use linux::Linux;

// ---- contacts ------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub name: String,
    pub number: String,
}

/// The address book, saved as JSON in the data directory.
#[derive(Debug, Default)]
pub struct Contacts {
    pub list: Vec<Contact>,
    path: Option<PathBuf>,
}

/// Strip spaces, dashes, dots and brackets; accept `+` then 3 to 15 digits.
pub fn normalize_number(s: &str) -> Option<String> {
    let n: String = s
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '.' | '(' | ')'))
        .collect();
    let digits = n.strip_prefix('+').unwrap_or(&n);
    let ok = (3..=15).contains(&digits.len()) && digits.chars().all(|c| c.is_ascii_digit());
    ok.then_some(n)
}

impl Contacts {
    pub fn load(path: PathBuf) -> Result<Contacts, String> {
        let list = match std::fs::read_to_string(&path) {
            Ok(t) => serde_json::from_str(&t).map_err(|e| format!("{}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Ok(Contacts {
            list,
            path: Some(path),
        })
    }
    fn save(&self) -> Result<(), String> {
        let Some(p) = &self.path else { return Ok(()) };
        let tmp = p.with_extension("tmp");
        let t = serde_json::to_string_pretty(&self.list).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, t).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, p).map_err(|e| e.to_string())
    }
    pub fn add(&mut self, name: &str, number: &str) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("a contact needs a name".into());
        }
        let number =
            normalize_number(number).ok_or_else(|| format!("{number:?} is not a phone number"))?;
        match self
            .list
            .iter_mut()
            .find(|c| c.name.eq_ignore_ascii_case(name))
        {
            Some(c) => c.number = number.clone(),
            None => self.list.push(Contact {
                name: name.to_string(),
                number: number.clone(),
            }),
        }
        self.save()?;
        Ok(format!("saved {name} as {number}"))
    }
    /// Exact name first (ignoring case), then a unique prefix.
    pub fn find(&self, name: &str) -> Option<&Contact> {
        let name = name.trim();
        if let Some(c) = self.list.iter().find(|c| c.name.eq_ignore_ascii_case(name)) {
            return Some(c);
        }
        let lower = name.to_lowercase();
        let mut hits = self
            .list
            .iter()
            .filter(|c| c.name.to_lowercase().starts_with(&lower));
        match (hits.next(), hits.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        }
    }
    pub fn name_for(&self, number: &str) -> Option<&str> {
        let n = normalize_number(number)?;
        self.list
            .iter()
            .find(|c| c.number == n || (n.len() >= 10 && c.number.ends_with(&n[n.len() - 10..])))
            .map(|c| c.name.as_str())
    }
    /// A contact name or a number, turned into a number to dial.
    pub fn resolve(&self, who: &str) -> Result<String, String> {
        if let Some(n) = normalize_number(who) {
            return Ok(n);
        }
        self.find(who)
            .map(|c| c.number.clone())
            .ok_or_else(|| format!("no number for {who}; save it first"))
    }
}

// ---- the phone -----------------------------------------------------------------------------

/// Something the network did that the human may want to know about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhoneEvent {
    IncomingCall { from: String },
    MissedCall { from: String },
    NewSms { from: String, text: String },
}

pub struct Phone {
    pub backend: Box<dyn Backend>,
    pub contacts: Contacts,
    seen_calls: HashSet<String>,
    ringing: HashSet<String>,
    answered: HashSet<String>,
    seen_sms: HashSet<String>,
    primed: bool,
}

pub type Shared = Rc<RefCell<Phone>>;

impl Phone {
    pub fn new(backend: Box<dyn Backend>, contacts: Contacts) -> Self {
        Phone {
            backend,
            contacts,
            seen_calls: HashSet::new(),
            ringing: HashSet::new(),
            answered: HashSet::new(),
            seen_sms: HashSet::new(),
            primed: false,
        }
    }
    pub fn shared(self) -> Shared {
        Rc::new(RefCell::new(self))
    }

    fn who(&self, number: &str) -> String {
        match self.contacts.name_for(number) {
            Some(n) => format!("{n} ({number})"),
            None => number.to_string(),
        }
    }

    /// Look for new calls and texts. The first poll only learns what is already there, so a
    /// restart does not replay the whole inbox.
    pub fn poll(&mut self) -> Vec<PhoneEvent> {
        let mut ev = Vec::new();
        if let Ok(calls) = self.backend.calls() {
            for c in calls {
                let new = self.seen_calls.insert(c.id.clone());
                match c.state {
                    CallState::Ringing if c.incoming && new && self.primed => {
                        self.ringing.insert(c.id.clone());
                        ev.push(PhoneEvent::IncomingCall {
                            from: self.who(&c.number),
                        });
                    }
                    CallState::Active | CallState::Held => {
                        self.ringing.remove(&c.id);
                        self.answered.insert(c.id.clone());
                    }
                    CallState::Ended
                        if self.ringing.remove(&c.id) && !self.answered.contains(&c.id) =>
                    {
                        ev.push(PhoneEvent::MissedCall {
                            from: self.who(&c.number),
                        });
                    }
                    _ => {}
                }
            }
        }
        if let Ok(msgs) = self.backend.messages() {
            for m in msgs {
                if self.seen_sms.insert(m.id.clone()) && m.incoming && self.primed {
                    ev.push(PhoneEvent::NewSms {
                        from: self.who(&m.number),
                        text: m.text,
                    });
                }
            }
        }
        self.primed = true;
        ev
    }

    fn live_call(&mut self, want: impl Fn(&Call) -> bool) -> Result<Call, String> {
        self.backend
            .calls()?
            .into_iter()
            .filter(|c| c.state != CallState::Ended)
            .find(want)
            .ok_or_else(|| "no such call".into())
    }

    pub fn status_text(&mut self) -> Result<String, String> {
        let s = self.backend.status()?;
        let mut parts = vec![];
        if s.airplane {
            parts.push("airplane mode".to_string());
        } else if s.registered {
            parts.push(format!("{} {} signal {}%", s.operator, s.tech, s.signal));
        } else {
            parts.push("no service".into());
        }
        parts.push(format!(
            "mobile data {}",
            if s.mobile_data { "on" } else { "off" }
        ));
        parts.push(match s.wifi {
            Some(w) => format!("wifi {w}"),
            None => "wifi off".into(),
        });
        Ok(parts.join(", "))
    }
}

// ---- tools ---------------------------------------------------------------------------------

/// Every phone tool, for the executor.
pub struct PhoneTools(pub Shared);

const TOOLS: &[Tool] = &[
    Tool {
        name: "phone_status",
        args: &[],
        tier: Tier::Low,
    },
    Tool {
        name: "find_contact",
        args: &["name"],
        tier: Tier::Low,
    },
    Tool {
        name: "list_contacts",
        args: &[],
        tier: Tier::Low,
    },
    Tool {
        name: "add_contact",
        args: &["name", "number"],
        tier: Tier::Medium,
    },
    Tool {
        name: "call",
        args: &["to"],
        tier: Tier::High,
    },
    Tool {
        name: "answer_call",
        args: &[],
        tier: Tier::Medium,
    },
    Tool {
        name: "hangup",
        args: &[],
        tier: Tier::Low,
    },
    Tool {
        name: "list_calls",
        args: &[],
        tier: Tier::Low,
    },
    Tool {
        name: "send_sms",
        args: &["to", "text"],
        tier: Tier::High,
    },
    Tool {
        name: "read_messages",
        args: &[],
        tier: Tier::Low,
    },
    Tool {
        name: "all_messages",
        args: &[],
        tier: Tier::Low,
    },
    Tool {
        name: "call_log",
        args: &[],
        tier: Tier::Low,
    },
    Tool {
        name: "messages_from",
        args: &["from"],
        tier: Tier::Low,
    },
    Tool {
        name: "ussd",
        args: &["code"],
        tier: Tier::Medium,
    },
    Tool {
        name: "wifi_scan",
        args: &[],
        tier: Tier::Low,
    },
    Tool {
        name: "wifi_connect",
        args: &["ssid", "password"],
        tier: Tier::Medium,
    },
    Tool {
        name: "airplane_mode",
        args: &["on"],
        tier: Tier::Medium,
    },
    Tool {
        name: "mobile_data",
        args: &["on"],
        tier: Tier::Medium,
    },
    Tool {
        name: "where_am_i",
        args: &[],
        tier: Tier::Medium,
    },
    Tool {
        name: "flashlight",
        args: &["on"],
        tier: Tier::Low,
    },
    Tool {
        name: "set_volume",
        args: &["percent"],
        tier: Tier::Low,
    },
    Tool {
        name: "ring_phone",
        args: &[],
        tier: Tier::Low,
    },
];

fn arg<'a>(a: &'a Action, name: &str) -> Result<&'a str, String> {
    a.args
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| format!("missing {name}"))
}
fn flag(a: &Action, name: &str) -> Result<bool, String> {
    match arg(a, name)?.trim().to_lowercase().as_str() {
        "on" | "true" | "yes" | "1" | "enable" => Ok(true),
        "off" | "false" | "no" | "0" | "disable" => Ok(false),
        other => Err(format!("{name} must be on or off, not {other:?}")),
    }
}
fn sms_lines(p: &Phone, msgs: &[Sms]) -> String {
    if msgs.is_empty() {
        return "no messages".into();
    }
    msgs.iter()
        .map(|m| {
            let dir = if m.incoming { "from" } else { "to" };
            format!("{dir} {}: {}", p.who(&m.number), m.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Connector for PhoneTools {
    fn name(&self) -> &str {
        "phone"
    }
    fn tools(&self) -> Vec<Tool> {
        TOOLS.to_vec()
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        let mut p = self.0.borrow_mut();
        match a.tool.as_str() {
            "phone_status" => p.status_text(),
            "find_contact" => {
                let name = arg(a, "name")?;
                p.contacts
                    .find(name)
                    .map(|c| format!("{}: {}", c.name, c.number))
                    .ok_or_else(|| format!("no contact called {name}"))
            }
            "list_contacts" => Ok(if p.contacts.list.is_empty() {
                "no contacts".into()
            } else {
                p.contacts
                    .list
                    .iter()
                    .map(|c| format!("{}: {}", c.name, c.number))
                    .collect::<Vec<_>>()
                    .join("\n")
            }),
            "add_contact" => p.contacts.add(arg(a, "name")?, arg(a, "number")?),
            "call" => {
                let to = arg(a, "to")?;
                let number = p.contacts.resolve(to)?;
                p.backend.dial(&number)?;
                Ok(format!("calling {}", p.who(&number)))
            }
            "answer_call" => {
                let c = p
                    .live_call(|c| c.incoming && c.state == CallState::Ringing)
                    .map_err(|_| "nothing is ringing".to_string())?;
                p.backend.answer(&c.id)?;
                p.ringing.remove(&c.id);
                p.answered.insert(c.id.clone());
                Ok(format!("answered {}", p.who(&c.number)))
            }
            "hangup" => {
                let c = p
                    .live_call(|_| true)
                    .map_err(|_| "no call to end".to_string())?;
                p.backend.hangup(&c.id)?;
                // Declining a ringing call is a choice, not a missed call.
                p.ringing.remove(&c.id);
                Ok(format!("ended call with {}", p.who(&c.number)))
            }
            "list_calls" => {
                let calls = p.backend.calls()?;
                if calls.is_empty() {
                    return Ok("no calls".into());
                }
                Ok(calls
                    .iter()
                    .rev()
                    .take(10)
                    .map(|c| {
                        let dir = if c.incoming { "from" } else { "to" };
                        format!("{dir} {} {}", p.who(&c.number), format!("{:?}", c.state).to_lowercase())
                    })
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            "send_sms" => {
                let to = arg(a, "to")?;
                let text = arg(a, "text")?;
                if text.trim().is_empty() {
                    return Err("empty message".into());
                }
                let number = p.contacts.resolve(to)?;
                p.backend.send_sms(&number, text)?;
                Ok(format!("sms sent to {}", p.who(&number)))
            }
            "read_messages" => {
                let msgs = p.backend.messages()?;
                let recent: Vec<Sms> = msgs.iter().rev().take(10).rev().cloned().collect();
                Ok(sms_lines(&p, &recent))
            }
            // every message and call, for analysis code in the sandbox rather than the model
            "all_messages" => {
                let msgs = p.backend.messages()?;
                Ok(sms_lines(&p, &msgs))
            }
            "call_log" => {
                let calls = p.backend.calls()?;
                Ok(if calls.is_empty() {
                    "no calls".into()
                } else {
                    calls
                        .iter()
                        .map(|c| {
                            let dir = if c.incoming { "from" } else { "to" };
                            format!("{dir} {} {}", p.who(&c.number), format!("{:?}", c.state).to_lowercase())
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            }
            "messages_from" => {
                let who = arg(a, "from")?;
                let number = p.contacts.resolve(who)?;
                let tail = &number[number.len().saturating_sub(10)..];
                let msgs: Vec<Sms> = p
                    .backend
                    .messages()?
                    .into_iter()
                    .filter(|m| normalize_number(&m.number).is_some_and(|n| n.ends_with(tail)))
                    .collect();
                Ok(sms_lines(&p, &msgs))
            }
            "ussd" => {
                let code = arg(a, "code")?.trim();
                let ok = !code.is_empty()
                    && code
                        .chars()
                        .all(|c| c.is_ascii_digit() || c == '*' || c == '#');
                if !ok {
                    return Err("a USSD code is digits, * and #, like *123#".into());
                }
                p.backend.ussd(code)
            }
            "wifi_scan" => {
                let nets = p.backend.wifi_scan()?;
                Ok(if nets.is_empty() {
                    "no networks found".into()
                } else {
                    nets.iter()
                        .map(|n| format!("{} ({}%)", n.ssid, n.signal))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            }
            "wifi_connect" => {
                let ssid = arg(a, "ssid")?;
                p.backend.wifi_connect(ssid, arg(a, "password")?)?;
                Ok(format!("connected to {ssid}"))
            }
            "airplane_mode" => {
                let on = flag(a, "on")?;
                p.backend.set_airplane(on)?;
                Ok(format!("airplane mode {}", if on { "on" } else { "off" }))
            }
            "mobile_data" => {
                let on = flag(a, "on")?;
                p.backend.set_mobile_data(on)?;
                Ok(format!("mobile data {}", if on { "on" } else { "off" }))
            }
            "where_am_i" => p.backend.location(),
            "flashlight" => {
                let on = flag(a, "on")?;
                p.backend.torch(on)?;
                Ok(format!("flashlight {}", if on { "on" } else { "off" }))
            }
            "set_volume" => {
                let v: u8 = arg(a, "percent")?
                    .trim()
                    .trim_end_matches('%')
                    .parse()
                    .ok()
                    .filter(|v| *v <= 100)
                    .ok_or("volume is 0 to 100")?;
                p.backend.set_volume(v)?;
                Ok(format!("volume {v}%"))
            }
            "ring_phone" => {
                p.backend.feedback("phone-incoming-call")?;
                Ok("ringing".into())
            }
            other => Err(format!("phone cannot {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(tool: &str, args: &[(&str, &str)]) -> Action {
        Action {
            tool: tool.into(),
            args: args
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
    fn phone() -> (PhoneTools, Shared) {
        let p = Phone::new(Box::new(Sim::new()), Contacts::default()).shared();
        (PhoneTools(p.clone()), p)
    }

    #[test]
    fn numbers_are_normalized_and_junk_is_refused() {
        assert_eq!(
            normalize_number("+91 98765-43210").as_deref(),
            Some("+919876543210")
        );
        assert_eq!(
            normalize_number("(080) 1234.5678").as_deref(),
            Some("08012345678")
        );
        assert!(normalize_number("mum").is_none());
        assert!(normalize_number("12").is_none());
        assert!(normalize_number("+91;rm -rf").is_none());
    }
    #[test]
    fn contacts_resolve_by_name_prefix_and_number() {
        let mut c = Contacts::default();
        c.add("Mum", "+91 98765 43210").unwrap();
        c.add("Asha", "+919999988888").unwrap();
        c.add("Ashok", "+919999977777").unwrap();
        assert_eq!(c.resolve("mum").unwrap(), "+919876543210");
        assert_eq!(c.resolve("Ashok").unwrap(), "+919999977777");
        assert!(c.resolve("Ash").is_err(), "ambiguous prefix");
        assert_eq!(c.resolve("98765 43210").unwrap(), "9876543210");
        assert_eq!(c.name_for("9876543210"), Some("Mum"));
    }
    #[test]
    fn contacts_survive_a_restart() {
        let p = std::env::temp_dir().join(format!("contacts-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&p);
        Contacts::load(p.clone())
            .unwrap()
            .add("Mum", "+919876543210")
            .unwrap();
        assert_eq!(
            Contacts::load(p.clone()).unwrap().resolve("mum").unwrap(),
            "+919876543210"
        );
        std::fs::remove_file(p).ok();
    }
    #[test]
    fn calling_and_texting_are_high_tier() {
        let (t, _) = phone();
        let tier = |n| t.tools().iter().find(|x| x.name == n).unwrap().tier;
        assert_eq!(tier("call"), Tier::High);
        assert_eq!(tier("send_sms"), Tier::High);
        assert_eq!(tier("hangup"), Tier::Low);
    }
    #[test]
    fn call_a_contact_then_hang_up() {
        let (mut t, _) = phone();
        t.call(&act(
            "add_contact",
            &[("name", "Mum"), ("number", "+919876543210")],
        ))
        .unwrap();
        assert_eq!(
            t.call(&act("call", &[("to", "mum")])).unwrap(),
            "calling Mum (+919876543210)"
        );
        assert!(t.call(&act("hangup", &[])).unwrap().contains("Mum"));
        assert!(t.call(&act("hangup", &[])).is_err());
    }
    #[test]
    fn an_unknown_name_is_not_dialled() {
        let (mut t, _) = phone();
        assert!(t.call(&act("call", &[("to", "the bank")])).is_err());
    }
    #[test]
    fn incoming_call_and_sms_become_events_once() {
        let (mut t, p) = phone();
        assert!(p.borrow_mut().poll().is_empty());
        p.borrow_mut().backend.inject_call("+919876543210").unwrap();
        p.borrow_mut()
            .backend
            .inject_sms("+919876543210", "where are you")
            .unwrap();
        let ev = p.borrow_mut().poll();
        assert_eq!(ev.len(), 2);
        assert!(matches!(&ev[0], PhoneEvent::IncomingCall { .. }));
        assert!(matches!(&ev[1], PhoneEvent::NewSms { text, .. } if text == "where are you"));
        assert!(
            p.borrow_mut().poll().is_empty(),
            "nothing new the second time"
        );
        t.call(&act("answer_call", &[])).unwrap();
        assert!(p.borrow_mut().poll().is_empty());
        t.call(&act("hangup", &[])).unwrap();
        assert!(
            p.borrow_mut().poll().is_empty(),
            "answered calls are not missed"
        );
    }
    #[test]
    fn answered_then_ended_between_polls_is_not_missed() {
        let (mut t, p) = phone();
        p.borrow_mut().poll();
        p.borrow_mut().backend.inject_call("+919876543210").unwrap();
        p.borrow_mut().poll();
        t.call(&act("answer_call", &[])).unwrap();
        t.call(&act("hangup", &[])).unwrap();
        assert!(p.borrow_mut().poll().is_empty());
    }
    #[test]
    fn declining_a_ringing_call_is_not_missed() {
        let (mut t, p) = phone();
        p.borrow_mut().poll();
        p.borrow_mut().backend.inject_call("+919876543210").unwrap();
        p.borrow_mut().poll();
        t.call(&act("hangup", &[])).unwrap();
        assert!(p.borrow_mut().poll().is_empty());
    }
    #[test]
    fn an_unanswered_call_that_stops_is_missed() {
        let (_t, p) = phone();
        p.borrow_mut().poll();
        p.borrow_mut().backend.inject_call("+919876543210").unwrap();
        p.borrow_mut().poll();
        // the caller gives up: the network ends the call, not us
        let id = p.borrow_mut().backend.calls().unwrap()[0].id.clone();
        p.borrow_mut().backend.hangup(&id).unwrap();
        assert!(matches!(
            &p.borrow_mut().poll()[..],
            [PhoneEvent::MissedCall { .. }]
        ));
    }
    #[test]
    fn the_inbox_from_before_boot_is_not_replayed() {
        let mut sim = Sim::new();
        sim.inject_sms("+919876543210", "old").unwrap();
        let mut p = Phone::new(Box::new(sim), Contacts::default());
        assert!(p.poll().is_empty());
    }
    #[test]
    fn call_listings_keep_names_as_saved() {
        let (mut t, p) = phone();
        t.call(&act("add_contact", &[("name", "Ravi"), ("number", "+919812345678")])).unwrap();
        p.borrow_mut().backend.inject_call("+919812345678").unwrap();
        let log = t.call(&act("call_log", &[])).unwrap();
        assert_eq!(log, "from Ravi (+919812345678) ringing", "only the state is lowercase");
    }
    #[test]
    fn switches_and_bad_values() {
        let (mut t, p) = phone();
        t.call(&act("flashlight", &[("on", "on")])).unwrap();
        assert!(t.call(&act("flashlight", &[("on", "maybe")])).is_err());
        assert!(t.call(&act("set_volume", &[("percent", "150")])).is_err());
        t.call(&act("set_volume", &[("percent", "30%")])).unwrap();
        assert!(t.call(&act("ussd", &[("code", "*123#; reboot")])).is_err());
        assert!(t
            .call(&act("ussd", &[("code", "*123#")]))
            .unwrap()
            .contains("balance"));
        t.call(&act("airplane_mode", &[("on", "on")])).unwrap();
        assert!(p.borrow_mut().status_text().unwrap().contains("airplane"));
        assert!(t
            .call(&act("send_sms", &[("to", "+919876543210"), ("text", "hi")]))
            .is_err());
    }
    #[test]
    fn messages_from_matches_on_the_last_ten_digits() {
        let (mut t, p) = phone();
        p.borrow_mut()
            .backend
            .inject_sms("+919876543210", "hello")
            .unwrap();
        p.borrow_mut()
            .backend
            .inject_sms("+910000000000", "spam")
            .unwrap();
        t.call(&act(
            "add_contact",
            &[("name", "Mum"), ("number", "09876543210")],
        ))
        .unwrap();
        let out = t.call(&act("messages_from", &[("from", "Mum")])).unwrap();
        assert!(out.contains("hello") && !out.contains("spam"), "{out}");
    }
}
