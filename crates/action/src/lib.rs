//! The canonical action: what a model is allowed to ask for.
//! A registry lists every tool, its argument names and its risk tier.
//! The registry also produces the GBNF grammar that constrains the local model's output.
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Read-only or harmless. Runs without asking.
    Low,
    /// Changes something reversible. Asks once, shown in chat.
    Medium,
    /// Money, messages to others, deletion. Needs the approval key.
    High,
}

#[derive(Debug, Clone)]
pub struct Tool {
    pub name: &'static str,
    pub args: &'static [&'static str],
    pub tier: Tier,
}

/// Tools made at run time (remote MCP servers, the store). The names are leaked once, at load,
/// because a tool lives as long as the daemon does.
fn leak(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

impl Tool {
    pub fn dynamic(name: &str, args: &[String], tier: Tier) -> Tool {
        let a: Vec<&'static str> = args.iter().map(|s| leak(s)).collect();
        Tool {
            name: leak(name),
            args: Box::leak(a.into_boxed_slice()),
            tier,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub tool: String,
    pub args: Vec<(String, String)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ActionError {
    NotJson,
    NotAnObject,
    UnknownTool(String),
    MissingArg(String),
    ExtraArg(String),
    ArgNotText(String),
}

pub struct Registry {
    tools: Vec<Tool>,
}

impl Registry {
    pub fn new(tools: Vec<Tool>) -> Self {
        Self { tools }
    }

    pub fn tier_of(&self, tool: &str) -> Option<Tier> {
        self.tools.iter().find(|t| t.name == tool).map(|t| t.tier)
    }

    /// Parse and check a model's output. Anything not exactly a registered action is an error.
    pub fn parse(&self, text: &str) -> Result<Action, ActionError> {
        let v: Value = serde_json::from_str(text.trim()).map_err(|_| ActionError::NotJson)?;
        let obj = v.as_object().ok_or(ActionError::NotAnObject)?;
        let name = obj
            .get("tool")
            .and_then(Value::as_str)
            .ok_or(ActionError::NotAnObject)?;
        let tool = self
            .tools
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| ActionError::UnknownTool(name.to_string()))?;
        let args = obj
            .get("args")
            .and_then(Value::as_object)
            .ok_or(ActionError::NotAnObject)?;
        for want in tool.args {
            if !args.contains_key(*want) {
                return Err(ActionError::MissingArg(want.to_string()));
            }
        }
        let mut out = Vec::new();
        for (k, val) in args {
            if !tool.args.contains(&k.as_str()) {
                return Err(ActionError::ExtraArg(k.clone()));
            }
            let s = val
                .as_str()
                .ok_or_else(|| ActionError::ArgNotText(k.clone()))?;
            out.push((k.clone(), s.to_string()));
        }
        out.sort();
        Ok(Action {
            tool: name.to_string(),
            args: out,
        })
    }

    /// A GBNF grammar (llama.cpp format) that only lets the model write registered actions.
    pub fn gbnf(&self) -> String {
        let mut g = String::from("root ::= action\naction ::= ");
        let alts: Vec<String> = self
            .tools
            .iter()
            .map(|t| format!("act-{}", t.name.replace('_', "-")))
            .collect();
        g.push_str(&alts.join(" | "));
        g.push('\n');
        for t in &self.tools {
            let rule = format!("act-{}", t.name.replace('_', "-"));
            let mut body = format!("\"{{\\\"tool\\\":\\\"{}\\\",\\\"args\\\":{{\"", t.name);
            for (i, a) in t.args.iter().enumerate() {
                if i > 0 {
                    body.push_str(" \",\" ");
                }
                body.push_str(&format!(" \"\\\"{a}\\\":\" text"));
            }
            body.push_str(" \"}}\"");
            g.push_str(&format!("{rule} ::= {body}\n"));
        }
        g.push_str("text ::= \"\\\"\" [^\"\\\\]* \"\\\"\"\n");
        g
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> Registry {
        Registry::new(vec![
            Tool {
                name: "read_calendar",
                args: &["day"],
                tier: Tier::Low,
            },
            Tool {
                name: "set_reminder",
                args: &["text", "when"],
                tier: Tier::Medium,
            },
            Tool {
                name: "pay",
                args: &["to", "amount"],
                tier: Tier::High,
            },
        ])
    }

    #[test]
    fn a_dynamic_tool_parses_and_is_in_the_grammar() {
        let t = Tool::dynamic("zomato_search", &["query".to_string()], Tier::Low);
        let r = Registry::new(vec![t]);
        let a = r
            .parse(r#"{"tool":"zomato_search","args":{"query":"biryani"}}"#)
            .unwrap();
        assert_eq!(a.tool, "zomato_search");
        assert!(r.gbnf().contains("zomato-search"));
        assert!(r.gbnf().contains("\\\"query\\\""));
    }

    #[test]
    fn a_valid_action_parses() {
        let a = reg()
            .parse(r#"{"tool":"set_reminder","args":{"when":"6pm","text":"call mum"}}"#)
            .unwrap();
        assert_eq!(a.tool, "set_reminder");
        assert_eq!(a.args[0].0, "text");
    }
    #[test]
    fn unknown_tool_is_rejected() {
        let e = reg()
            .parse(r#"{"tool":"format_disk","args":{}}"#)
            .unwrap_err();
        assert_eq!(e, ActionError::UnknownTool("format_disk".into()));
    }
    #[test]
    fn missing_argument_is_rejected() {
        let e = reg()
            .parse(r#"{"tool":"pay","args":{"to":"asha"}}"#)
            .unwrap_err();
        assert_eq!(e, ActionError::MissingArg("amount".into()));
    }
    #[test]
    fn extra_argument_is_rejected() {
        let e = reg()
            .parse(r#"{"tool":"read_calendar","args":{"day":"today","secret":"x"}}"#)
            .unwrap_err();
        assert_eq!(e, ActionError::ExtraArg("secret".into()));
    }
    #[test]
    fn non_text_argument_is_rejected() {
        let e = reg()
            .parse(r#"{"tool":"pay","args":{"to":"asha","amount":500}}"#)
            .unwrap_err();
        assert_eq!(e, ActionError::ArgNotText("amount".into()));
    }
    #[test]
    fn chatter_is_not_an_action() {
        assert_eq!(
            reg().parse("Sure! I'll pay Asha now.").unwrap_err(),
            ActionError::NotJson
        );
    }
    #[test]
    fn tiers_are_ordered() {
        assert!(Tier::High > Tier::Medium && Tier::Medium > Tier::Low);
        assert_eq!(reg().tier_of("pay"), Some(Tier::High));
        assert_eq!(reg().tier_of("nope"), None);
    }
    #[test]
    fn grammar_names_every_tool_and_nothing_else() {
        let g = reg().gbnf();
        for t in ["read_calendar", "set_reminder", "pay"] {
            assert!(g.contains(t), "missing {t}");
        }
        assert!(!g.contains("format_disk"));
        assert!(g.starts_with("root ::= action"));
    }
}
