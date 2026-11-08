//! Reference connectors. They keep their state in memory so tests can look inside.
//! A real connector (WhatsApp, mail) implements the same trait over a network client.
use agentos_action::{Action, Tier, Tool};
use agentos_executor::Connector;

fn arg<'a>(a: &'a Action, name: &str) -> Result<&'a str, String> {
    a.args
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| format!("missing {name}"))
}

#[derive(Default)]
pub struct Calendar {
    pub events: Vec<(String, String)>, // (day, title)
}
impl Connector for Calendar {
    fn name(&self) -> &str {
        "calendar"
    }
    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "read_calendar",
                args: &["day"],
                tier: Tier::Low,
            },
            Tool {
                name: "add_event",
                args: &["day", "title"],
                tier: Tier::Medium,
            },
        ]
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        match a.tool.as_str() {
            "read_calendar" => {
                let day = arg(a, "day")?;
                let v: Vec<&str> = self
                    .events
                    .iter()
                    .filter(|(d, _)| d == day)
                    .map(|(_, t)| t.as_str())
                    .collect();
                Ok(if v.is_empty() {
                    "nothing".into()
                } else {
                    v.join("; ")
                })
            }
            "add_event" => {
                self.events
                    .push((arg(a, "day")?.to_string(), arg(a, "title")?.to_string()));
                Ok("added".into())
            }
            other => Err(format!("calendar cannot {other}")),
        }
    }
}

#[derive(Default)]
pub struct Reminders {
    pub items: Vec<(String, String)>, // (when, text)
}
impl Connector for Reminders {
    fn name(&self) -> &str {
        "reminders"
    }
    fn tools(&self) -> Vec<Tool> {
        vec![Tool {
            name: "set_reminder",
            args: &["text", "when"],
            tier: Tier::Medium,
        }]
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        self.items
            .push((arg(a, "when")?.to_string(), arg(a, "text")?.to_string()));
        Ok("reminder set".into())
    }
}

#[derive(Default)]
pub struct Messenger {
    pub sent: Vec<(String, String)>, // (to, text)
}
impl Connector for Messenger {
    fn name(&self) -> &str {
        "messenger"
    }
    fn tools(&self) -> Vec<Tool> {
        vec![Tool {
            name: "send_message",
            args: &["to", "text"],
            tier: Tier::High,
        }]
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        let to = arg(a, "to")?;
        if to.trim().is_empty() {
            return Err("empty recipient".into());
        }
        self.sent
            .push((to.to_string(), arg(a, "text")?.to_string()));
        Ok(format!("sent to {to}"))
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

    #[test]
    fn calendar_add_then_read() {
        let mut c = Calendar::default();
        c.call(&act("add_event", &[("day", "mon"), ("title", "dentist")]))
            .unwrap();
        assert_eq!(
            c.call(&act("read_calendar", &[("day", "mon")])).unwrap(),
            "dentist"
        );
        assert_eq!(
            c.call(&act("read_calendar", &[("day", "tue")])).unwrap(),
            "nothing"
        );
    }
    #[test]
    fn reminder_is_stored() {
        let mut r = Reminders::default();
        r.call(&act(
            "set_reminder",
            &[("text", "call mum"), ("when", "6pm")],
        ))
        .unwrap();
        assert_eq!(r.items, vec![("6pm".to_string(), "call mum".to_string())]);
    }
    #[test]
    fn messenger_rejects_an_empty_recipient() {
        let mut m = Messenger::default();
        assert!(m
            .call(&act("send_message", &[("to", " "), ("text", "hi")]))
            .is_err());
        assert!(m.sent.is_empty());
    }
    #[test]
    fn messenger_is_high_tier() {
        assert_eq!(Messenger::default().tools()[0].tier, Tier::High);
    }
    #[test]
    fn a_connector_refuses_a_tool_it_does_not_own() {
        assert!(Calendar::default().call(&act("send_message", &[])).is_err());
    }
}
