//! Skills: named, multi-step recipes written as data (TOML), not code.
//! A skill is a list of tool calls with {parameters}. Running one goes through the executor,
//! so every step obeys the same tiers and the same Gate as a single request does.
use agentos_action::Action;
use agentos_executor::{Approved, ExecError, Executor};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Deserialize, Clone)]
pub struct Step {
    pub tool: String,
    #[serde(default)]
    pub args: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub params: Vec<String>,
    pub steps: Vec<Step>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SkillError {
    BadFile(String),
    MissingParam(String),
    UnknownParam(String),
    NoSteps,
}

impl Skill {
    pub fn from_toml(text: &str) -> Result<Skill, SkillError> {
        let s: Skill =
            toml::from_str(text).map_err(|e| SkillError::BadFile(e.message().to_string()))?;
        if s.steps.is_empty() {
            return Err(SkillError::NoSteps);
        }
        Ok(s)
    }

    /// Fill in {placeholders}. Every declared parameter must be given, and every placeholder
    /// used in a step must be a declared parameter, so a typo cannot slip through quietly.
    pub fn instantiate(&self, given: &HashMap<String, String>) -> Result<Vec<Action>, SkillError> {
        for p in &self.params {
            if !given.contains_key(p) {
                return Err(SkillError::MissingParam(p.clone()));
            }
        }
        let mut out = Vec::new();
        for step in &self.steps {
            let mut args = Vec::new();
            for (k, v) in &step.args {
                let mut text = v.clone();
                while let Some(open) = text.find('{') {
                    let close = text[open..]
                        .find('}')
                        .map(|c| c + open)
                        .ok_or_else(|| SkillError::BadFile("unclosed {".into()))?;
                    let name = text[open + 1..close].to_string();
                    if !self.params.contains(&name) {
                        return Err(SkillError::UnknownParam(name));
                    }
                    text.replace_range(open..=close, &given[&name]);
                }
                args.push((k.clone(), text));
            }
            args.sort();
            out.push(Action {
                tool: step.tool.clone(),
                args,
            });
        }
        Ok(out)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    /// Every step ran. The strings are each step's output.
    Done(Vec<String>),
    /// Stopped before step `index`, which is High tier and needs the human.
    Paused {
        index: usize,
        action: Action,
    },
    Failed {
        index: usize,
        error: String,
    },
}

pub struct SkillRun {
    actions: Vec<Action>,
    next: usize,
    outputs: Vec<String>,
}

impl SkillRun {
    pub fn new(actions: Vec<Action>) -> Self {
        Self {
            actions,
            next: 0,
            outputs: vec![],
        }
    }

    /// Run until the end, a High step, or a failure.
    pub fn advance(&mut self, exec: &mut Executor) -> Progress {
        while self.next < self.actions.len() {
            let a = self.actions[self.next].clone();
            match exec.run(&a) {
                Ok(out) => {
                    self.outputs.push(out);
                    self.next += 1;
                }
                Err(ExecError::NeedsApproval) => {
                    return Progress::Paused {
                        index: self.next,
                        action: a,
                    }
                }
                Err(e) => {
                    return Progress::Failed {
                        index: self.next,
                        error: format!("{e:?}"),
                    }
                }
            }
        }
        Progress::Done(self.outputs.clone())
    }

    /// Run the paused High step with proof of approval, then carry on.
    pub fn resume(&mut self, exec: &mut Executor, approved: Approved) -> Progress {
        if approved.action() != &self.actions[self.next] {
            return Progress::Failed {
                index: self.next,
                error: "approval is for a different action".into(),
            };
        }
        match exec.run_approved(approved) {
            Ok(out) => {
                self.outputs.push(out);
                self.next += 1;
                self.advance(exec)
            }
            Err(e) => Progress::Failed {
                index: self.next,
                error: format!("{e:?}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_connectors::{Calendar, Messenger, Reminders};
    use agentos_gate::Gate;
    use agentos_hal::FakeClock;
    use agentos_store::Store;
    use std::sync::Arc;

    fn test_executor() -> Executor {
        let clock = FakeClock::new();
        Executor::new(
            vec![
                Box::new(Calendar::default()),
                Box::new(Reminders::default()),
                Box::new(Messenger::default()),
            ],
            Store::in_memory().unwrap(),
            Arc::new(clock.clone()),
            Gate::new(b"k", Arc::new(clock), 600_000),
        )
    }

    const TRIP: &str = include_str!("../skills/tell-family-trip.toml");

    fn given() -> HashMap<String, String> {
        [("day", "Friday"), ("who", "Asha"), ("place", "Goa")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_skill_file_loads() {
        let s = Skill::from_toml(TRIP).unwrap();
        assert_eq!(s.steps.len(), 3);
        assert_eq!(s.params, vec!["day", "who", "place"]);
    }
    #[test]
    fn placeholders_are_filled() {
        let acts = Skill::from_toml(TRIP)
            .unwrap()
            .instantiate(&given())
            .unwrap();
        assert!(acts[0]
            .args
            .contains(&("title".to_string(), "Trip to Goa".to_string())));
        assert!(acts[2]
            .args
            .contains(&("to".to_string(), "Asha".to_string())));
    }
    #[test]
    fn a_missing_parameter_is_an_error() {
        let mut g = given();
        g.remove("who");
        assert_eq!(
            Skill::from_toml(TRIP).unwrap().instantiate(&g),
            Err(SkillError::MissingParam("who".into()))
        );
    }
    #[test]
    fn an_undeclared_placeholder_is_an_error() {
        let bad = TRIP.replace(
            "{place}\" }\n\n[[steps]]\ntool = \"set_reminder\"",
            "{plac}\" }\n\n[[steps]]\ntool = \"set_reminder\"",
        );
        let s = Skill::from_toml(&bad).unwrap();
        assert_eq!(
            s.instantiate(&given()),
            Err(SkillError::UnknownParam("plac".into()))
        );
    }
    #[test]
    fn a_skill_with_no_steps_is_rejected() {
        assert_eq!(
            Skill::from_toml("name='x'\ndescription='y'\nsteps=[]").err(),
            Some(SkillError::NoSteps)
        );
    }
    #[test]
    fn garbage_is_a_bad_file() {
        assert!(matches!(
            Skill::from_toml("this is not toml ["),
            Err(SkillError::BadFile(_))
        ));
    }
    #[test]
    fn the_run_pauses_at_the_high_step_and_resumes_with_proof() {
        let mut ex = test_executor();
        let acts = Skill::from_toml(TRIP)
            .unwrap()
            .instantiate(&given())
            .unwrap();
        let mut run = SkillRun::new(acts);
        let Progress::Paused { index, action } = run.advance(&mut ex) else {
            panic!("should pause")
        };
        assert_eq!(index, 2);
        let tok = ex.mint_after_key(&action);
        let ap = ex.approve(&tok, &action).unwrap();
        match run.resume(&mut ex, ap) {
            Progress::Done(out) => assert_eq!(out, vec!["added", "reminder set", "sent to Asha"]),
            other => panic!("{other:?}"),
        }
    }
    #[test]
    fn approval_for_another_step_cannot_resume_this_one() {
        let mut ex = test_executor();
        let acts = Skill::from_toml(TRIP)
            .unwrap()
            .instantiate(&given())
            .unwrap();
        let mut run = SkillRun::new(acts);
        run.advance(&mut ex);
        let other = Action {
            tool: "send_message".into(),
            args: vec![
                ("text".into(), "different".into()),
                ("to".into(), "Asha".into()),
            ],
        };
        let tok = ex.mint_after_key(&other);
        let ap = ex.approve(&tok, &other).unwrap();
        assert!(matches!(run.resume(&mut ex, ap), Progress::Failed { .. }));
    }
}
