//! First-boot setup as one state machine: the steps of Android's setup wizard, each a question
//! from agentd and an answer that becomes an effect for the daemon to run. The module does no
//! input or output except `load` and `save`, and it never runs a tool itself, so the same flow
//! works from the screen, from `agentd ctl` and from an MCP client.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Step {
    Welcome,
    Wifi,
    Sim,
    Location,
    Owner,
    Import,
    Pin,
    Privacy,
    Models,
    Services,
    Apps,
    Android,
    Done,
}

impl Step {
    const ORDER: [Step; 13] = [
        Step::Welcome,
        Step::Wifi,
        Step::Sim,
        Step::Location,
        Step::Owner,
        Step::Import,
        Step::Pin,
        Step::Privacy,
        Step::Models,
        Step::Services,
        Step::Apps,
        Step::Android,
        Step::Done,
    ];
    /// 1-based position, as the screen counts it.
    pub fn index(self) -> usize {
        Step::ORDER.iter().position(|s| *s == self).unwrap_or(0) + 1
    }
    fn next(self) -> Step {
        let i = Step::ORDER.iter().position(|s| *s == self).unwrap_or(0);
        Step::ORDER[(i + 1).min(Step::ORDER.len() - 1)]
    }
    fn prev(self) -> Step {
        let i = Step::ORDER.iter().position(|s| *s == self).unwrap_or(0);
        Step::ORDER[i.saturating_sub(1)]
    }
    fn can_skip(self) -> bool {
        !matches!(
            self,
            Step::Owner | Step::Pin | Step::Privacy | Step::Android | Step::Done
        )
    }
}

/// What the daemon knows that the screens show: networks, apps from the catalogue, and whether
/// the Android sandbox is installed.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// (name, connected)
    pub networks: Vec<(String, bool)>,
    /// (id, name, mode) from the catalogue; may be empty
    pub known_apps: Vec<(String, String, String)>,
    pub android_available: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Screen {
    pub id: &'static str,
    pub index: usize,
    pub total: usize,
    pub title: String,
    pub body: String,
    /// buttons; empty means free text
    pub options: Vec<String>,
    /// "text", "password" or None
    pub input: Option<String>,
    pub can_skip: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    RunTool {
        tool: String,
        args: Vec<(String, String)>,
    },
    SetSecret {
        name: String,
        value: String,
    },
    SetPin(String),
    PullModels,
    ImportContacts(String),
    Finish,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Setup {
    pub step: Step,
    /// Answers that are safe to keep. Secrets (the PIN, tokens) never go here.
    pub answers: BTreeMap<String, String>,
    #[serde(skip)]
    error: Option<String>,
}

impl Default for Setup {
    fn default() -> Self {
        Setup {
            step: Step::Welcome,
            answers: BTreeMap::new(),
            error: None,
        }
    }
}

const TOTAL: usize = 13;

fn tool(name: &str, args: &[(&str, &str)]) -> Effect {
    Effect::RunTool {
        tool: name.into(),
        args: args
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

impl Setup {
    /// The saved state, or the first step when there is none (or it cannot be read).
    pub fn load(path: &Path) -> Setup {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    fn fail(&mut self, msg: &str) -> Vec<Effect> {
        self.error = Some(msg.to_string());
        Vec::new()
    }

    fn go(&mut self, to: Step) {
        self.error = None;
        self.step = to;
    }

    pub fn screen(&self, f: &Facts) -> Screen {
        let (id, title, body, options, input, can_skip): (&'static str, &str, String, Vec<String>, Option<&str>, bool) = match self.step {
            Step::Welcome => (
                "welcome",
                "Welcome",
                "This phone is set for India, time zone UTC+5:30. English only for now.".into(),
                vec!["yes".into()],
                None,
                true,
            ),
            Step::Wifi => (
                "wifi",
                "Connect to Wi-Fi",
                f.networks.iter().map(|(n, c)| format!("{n}{}", if *c { " (connected)" } else { "" })).collect::<Vec<_>>().join("\n"),
                vec!["skip".into()],
                Some("text"),
                true,
            ),
            Step::Sim => ("sim", "Insert a SIM", "Checking the mobile network.".into(), vec!["continue".into()], None, true),
            Step::Location => (
                "location",
                "Location",
                "Allow where_am_i to use your location? Answer yes or no.".into(),
                vec!["yes".into(), "no".into()],
                None,
                true,
            ),
            Step::Owner => ("owner", "Your account", "Your name and number, saved as \"Me\". Write it as Name|number.".into(), vec![], Some("text"), false),
            Step::Import => ("import", "Copy contacts", "A vCard file path to import, or skip.".into(), vec!["skip".into()], Some("text"), true),
            Step::Pin => ("pin", "Screen lock", "Set the approval PIN (4 to 8 digits). Every payment and message asks for it.".into(), vec![], Some("password"), false),
            Step::Privacy => (
                "privacy",
                "Privacy",
                "On-device by default. The cloud model stays off unless you add a key. Answer yes to accept.".into(),
                vec!["yes".into()],
                None,
                false,
            ),
            Step::Models => ("models", "Assistant", "Download the on-device model now?".into(), vec!["download".into(), "later".into()], None, true),
            Step::Services => (
                "services",
                "Services",
                "Paste a Telegram bot token as: telegram <token>, or skip.".into(),
                vec!["skip".into()],
                Some("text"),
                true,
            ),
            Step::Apps => (
                "apps",
                "Your apps",
                f.known_apps.iter().map(|(id, name, mode)| format!("{id}: {name} ({mode})")).collect::<Vec<_>>().join("\n"),
                vec!["skip".into()],
                Some("text"),
                true,
            ),
            Step::Android => ("android", "Play Store", "Open the Android sandbox so you can sign in to the Play Store by touch.".into(), vec!["open".into(), "skip".into()], None, true),
            Step::Done => ("done", "All set", "Setup is finished.".into(), vec![], None, false),
        };
        Screen {
            id,
            index: self.step.index(),
            total: TOTAL,
            title: title.into(),
            body,
            options,
            input: input.map(str::to_string),
            can_skip,
            error: self.error.clone(),
        }
    }

    /// An answer to the current step. Returns the effects to run, in order. A bad answer keeps
    /// the step, sets the error the screen shows, and returns no effects.
    pub fn answer(&mut self, text: &str, f: &Facts) -> Vec<Effect> {
        let text = text.trim();
        match self.step {
            Step::Welcome => {
                self.answers.insert("welcome".into(), text.into());
                self.go(Step::Wifi);
                vec![]
            }
            Step::Wifi => {
                if text == "skip" {
                    self.go(Step::Sim);
                    return vec![];
                }
                let Some((ssid, pw)) = text.split_once('|') else {
                    return self.fail("write it as SSID|password, or skip");
                };
                self.answers.insert("wifi".into(), ssid.trim().into());
                self.go(Step::Sim);
                vec![tool(
                    "wifi_connect",
                    &[("ssid", ssid.trim()), ("password", pw)],
                )]
            }
            Step::Sim => {
                self.go(Step::Location);
                vec![]
            }
            Step::Location => {
                if text != "yes" && text != "no" {
                    return self.fail("answer yes or no");
                }
                self.answers.insert("location".into(), text.into());
                self.go(Step::Owner);
                vec![]
            }
            Step::Owner => {
                let Some((name, number)) = text.split_once('|') else {
                    return self.fail("write it as Name|number");
                };
                let (name, number) = (name.trim(), number.trim());
                if name.is_empty() || number.is_empty() {
                    return self.fail("both a name and a number are needed");
                }
                self.answers.insert("owner".into(), name.into());
                self.go(Step::Import);
                vec![tool("add_contact", &[("name", "Me"), ("number", number)])]
            }
            Step::Import => {
                if text == "skip" {
                    self.go(Step::Pin);
                    return vec![];
                }
                self.answers.insert("import".into(), text.into());
                self.go(Step::Pin);
                vec![Effect::ImportContacts(text.into())]
            }
            Step::Pin => {
                if !(4..=8).contains(&text.len()) || !text.bytes().all(|b| b.is_ascii_digit()) {
                    return self.fail("a PIN is 4 to 8 digits");
                }
                // the PIN goes to the vault only; it is not in the answers, so not in the file
                self.go(Step::Privacy);
                vec![Effect::SetPin(text.into())]
            }
            Step::Privacy => {
                if text != "yes" {
                    return self.fail("answer yes to accept");
                }
                self.answers.insert("privacy".into(), "on-device".into());
                self.go(Step::Models);
                vec![]
            }
            Step::Models => {
                self.answers.insert("models".into(), text.into());
                self.go(Step::Services);
                if text == "download" {
                    vec![Effect::PullModels]
                } else {
                    vec![]
                }
            }
            Step::Services => {
                if text == "skip" {
                    self.go(Step::Apps);
                    return vec![];
                }
                let Some(token) = text.strip_prefix("telegram ") else {
                    return self.fail("write it as: telegram <token>, or skip");
                };
                // the token is a secret: stored in the vault, never in the answers
                self.answers.insert("services".into(), "telegram".into());
                self.go(Step::Apps);
                vec![Effect::SetSecret {
                    name: "conn.telegram".into(),
                    value: token.trim().into(),
                }]
            }
            Step::Apps => {
                // "id=mode,id=mode" with mode headless, sandbox, builtin or drop
                let mut any_sandbox = false;
                if text != "skip" {
                    for pair in text.split(',') {
                        let Some((id, mode)) = pair.split_once('=') else {
                            return self.fail("write each app as id=mode, with mode headless, sandbox, builtin or drop");
                        };
                        if !["headless", "sandbox", "builtin", "drop"].contains(&mode.trim()) {
                            return self.fail("the mode is headless, sandbox, builtin or drop");
                        }
                        any_sandbox |= mode.trim() == "sandbox";
                        let _ = id;
                    }
                    self.answers.insert("apps".into(), text.into());
                }
                let _ = f;
                if any_sandbox && f.android_available {
                    self.go(Step::Android);
                } else {
                    self.go(Step::Done);
                    return vec![Effect::Finish];
                }
                vec![]
            }
            Step::Android => {
                let effects = if text == "open" {
                    vec![tool("android_open", &[("app", "com.android.vending")])]
                } else {
                    vec![]
                };
                self.go(Step::Done);
                let mut out = effects;
                out.push(Effect::Finish);
                out
            }
            Step::Done => vec![],
        }
    }

    /// Skip the current step with no effect. Refused where skipping is not allowed.
    pub fn skip(&mut self) -> bool {
        if !self.step.can_skip() {
            return false;
        }
        self.error = None;
        self.step = self.step.next();
        true
    }

    /// Back one step (not before the first).
    pub fn back(&mut self) {
        self.error = None;
        self.step = self.step.prev();
    }

    pub fn is_done(&self) -> bool {
        self.step == Step::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            networks: vec![("Home".into(), false)],
            known_apps: vec![("zomato".into(), "Zomato".into(), "headless".into())],
            android_available: true,
        }
    }

    #[test]
    fn walking_every_step_with_valid_answers_ends_at_done_with_the_effects_in_order() {
        let f = facts();
        let mut s = Setup::default();
        let mut effects = vec![];
        for a in [
            "yes",
            "home|pw1",
            "continue",
            "yes",
            "Asha|+91 98765 43210",
            "skip",
            "1234",
            "yes",
            "download",
            "skip",
            "zomato=headless",
        ] {
            effects.extend(s.answer(a, &f));
        }
        assert!(s.is_done());
        assert_eq!(
            effects,
            vec![
                tool("wifi_connect", &[("ssid", "home"), ("password", "pw1")]),
                tool(
                    "add_contact",
                    &[("name", "Me"), ("number", "+91 98765 43210")]
                ),
                Effect::SetPin("1234".into()),
                Effect::PullModels,
                Effect::Finish,
            ]
        );
    }

    #[test]
    fn back_from_wifi_returns_to_welcome() {
        let mut s = Setup::default();
        s.answer("yes", &facts());
        assert_eq!(s.step, Step::Wifi);
        s.back();
        assert_eq!(s.step, Step::Welcome);
    }

    #[test]
    fn skip_is_refused_on_pin() {
        let mut s = Setup {
            step: Step::Pin,
            ..Setup::default()
        };
        assert!(!s.skip());
        assert_eq!(s.step, Step::Pin);
    }

    #[test]
    fn a_wifi_answer_without_a_bar_gives_no_effect_and_an_error() {
        let mut s = Setup {
            step: Step::Wifi,
            ..Setup::default()
        };
        assert!(s.answer("homenet", &facts()).is_empty());
        assert_eq!(s.step, Step::Wifi);
        assert!(s.screen(&facts()).error.unwrap().contains("SSID|password"));
    }

    #[test]
    fn saving_and_loading_restores_the_step_and_the_answers() {
        let dir = std::env::temp_dir().join(format!("setup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("setup.json");
        let mut s = Setup::default();
        s.answer("yes", &facts());
        s.answer("skip", &facts());
        s.save(&path).unwrap();
        let back = Setup::load(&path);
        assert_eq!(back.step, Step::Sim);
        assert_eq!(back.answers.get("welcome").map(String::as_str), Some("yes"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pin_never_appears_in_the_saved_file() {
        let dir = std::env::temp_dir().join(format!("setup-pin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("setup.json");
        let mut s = Setup {
            step: Step::Pin,
            ..Setup::default()
        };
        let effects = s.answer("987654", &facts());
        assert_eq!(effects, vec![Effect::SetPin("987654".into())]);
        s.save(&path).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("987654"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
