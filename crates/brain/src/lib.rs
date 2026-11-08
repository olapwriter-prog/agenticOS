//! The thinking loop: route -> (firewall) -> model -> validate -> (gate) -> result.
//! Models are a trait, so tests use scripted ones and the real llama.cpp host plugs in later.
use agentos_action::{Action, Registry, Tier};
use agentos_firewall::{Firewall, Verdict};
use agentos_gate::Gate;
use agentos_hal::Clock;
use agentos_router::{route, Brain, Job, Policy};
use agentos_store::Store;
use std::sync::Arc;

pub trait Model {
    fn complete(&mut self, brain: Brain, prompt: &str) -> String;
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Run this action now.
    Run(Action),
    /// A High-tier action: show it to the human and wait for the approval key.
    NeedsApproval(Action),
    /// Gave up after trying every allowed brain.
    GaveUp,
    /// The firewall refused to send the prompt to the commercial model.
    Blocked(String),
}

pub struct Engine {
    pub registry: Registry,
    pub firewall: Firewall,
    pub policy: Policy,
    pub store: Store,
    pub clock: Arc<dyn Clock>,
    pub gate: Gate,
}

impl Engine {
    fn log(&self, kind: &str, msg: &str) {
        let _ = self.store.append(self.clock.now_ms(), kind, msg);
    }

    /// Handle one request. `job.local_failures` is advanced by this loop.
    pub fn handle(&mut self, mut job: Job, prompt: &str, model: &mut dyn Model) -> Outcome {
        // Never more attempts than: the local limit, plus one commercial try, plus slack.
        for _ in 0..(self.policy.max_local_failures + 3) {
            let brain = route(&job, &self.policy);
            self.log(
                "route",
                &format!(
                    "{:?} failures={} -> {:?}",
                    job.kind, job.local_failures, brain
                ),
            );

            let text = if brain == Brain::Commercial {
                match self.firewall.check(prompt) {
                    Verdict::Allowed { text, report } => {
                        self.log("firewall", &format!("redacted {} item(s)", report.total()));
                        text
                    }
                    Verdict::Blocked { reason } => {
                        self.log("firewall", &format!("blocked: {reason}"));
                        return Outcome::Blocked(reason);
                    }
                }
            } else {
                prompt.to_string()
            };

            let reply = model.complete(brain, &text);
            match self.registry.parse(&reply) {
                Ok(action) => {
                    let tier = self.registry.tier_of(&action.tool).unwrap_or(Tier::High);
                    self.log("action", &format!("{} tier={:?}", action.tool, tier));
                    return if tier == Tier::High {
                        Outcome::NeedsApproval(action)
                    } else {
                        Outcome::Run(action)
                    };
                }
                Err(e) => {
                    self.log("invalid", &format!("{:?} from {:?}", e, brain));
                    if brain != Brain::Commercial {
                        job.local_failures += 1;
                    } else {
                        return Outcome::GaveUp;
                    }
                }
            }
        }
        Outcome::GaveUp
    }

    /// The human pressed the key for this exact action.
    pub fn approve(&mut self, action: &Action) -> String {
        let canon = canonical(action);
        self.log("gate", &format!("minted for {}", action.tool));
        self.gate.mint(&canon)
    }

    /// Before running a High-tier action, redeem its token.
    pub fn release(&mut self, token: &str, action: &Action) -> bool {
        let ok = self.gate.redeem(token, &canonical(action)).is_ok();
        self.log("gate", &format!("redeem {} -> {}", action.tool, ok));
        ok
    }
}

pub fn canonical(a: &Action) -> String {
    let args: Vec<String> = a
        .args
        .iter()
        .map(|(k, v)| format!("\"{k}\":\"{v}\""))
        .collect();
    format!(
        "{{\"tool\":\"{}\",\"args\":{{{}}}}}",
        a.tool,
        args.join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_action::Tool;
    use agentos_hal::FakeClock;
    use agentos_router::{JobKind, Sensitivity};

    /// A model that returns scripted replies per brain and records what it was shown.
    struct Script {
        local: Vec<String>,
        commercial: Vec<String>,
        seen: Vec<(Brain, String)>,
    }
    impl Script {
        fn new(local: &[&str], commercial: &[&str]) -> Self {
            Self {
                local: local.iter().rev().map(|s| s.to_string()).collect(),
                commercial: commercial.iter().rev().map(|s| s.to_string()).collect(),
                seen: vec![],
            }
        }
    }
    impl Model for Script {
        fn complete(&mut self, brain: Brain, prompt: &str) -> String {
            self.seen.push((brain, prompt.to_string()));
            let q = if brain == Brain::Commercial {
                &mut self.commercial
            } else {
                &mut self.local
            };
            q.pop().unwrap_or_default()
        }
    }

    fn engine() -> (FakeClock, Engine) {
        let clock = FakeClock::new();
        let e = Engine {
            registry: Registry::new(vec![
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
            ]),
            firewall: Firewall::new(&["vault key"]),
            policy: Policy::default(),
            store: Store::in_memory().unwrap(),
            clock: Arc::new(clock.clone()),
            gate: Gate::new(b"secret", Arc::new(clock.clone()), 600_000),
        };
        (clock, e)
    }
    fn job(kind: JobKind) -> Job {
        Job {
            kind,
            sensitivity: Sensitivity::Normal,
            online: true,
            on_charger: false,
            local_failures: 0,
        }
    }
    const REMIND: &str = r#"{"tool":"set_reminder","args":{"text":"call mum","when":"6pm"}}"#;
    const PAY: &str = r#"{"tool":"pay","args":{"to":"asha","amount":"500"}}"#;

    #[test]
    fn chat_runs_locally_and_never_touches_the_commercial_model() {
        let (_c, mut e) = engine();
        let mut m = Script::new(&[REMIND], &[]);
        assert!(matches!(
            e.handle(job(JobKind::Chat), "remind me", &mut m),
            Outcome::Run(_)
        ));
        assert!(m.seen.iter().all(|(b, _)| *b != Brain::Commercial));
    }
    #[test]
    fn a_high_tier_action_waits_for_approval() {
        let (_c, mut e) = engine();
        let mut m = Script::new(&[PAY], &[]);
        assert!(matches!(
            e.handle(job(JobKind::Chat), "pay asha", &mut m),
            Outcome::NeedsApproval(_)
        ));
    }
    #[test]
    fn stuck_local_model_escalates_after_two_failures() {
        let (_c, mut e) = engine();
        let mut m = Script::new(&["blah", "still blah"], &[REMIND]);
        let out = e.handle(job(JobKind::Design), "design it", &mut m);
        assert!(matches!(out, Outcome::Run(_)));
        let brains: Vec<Brain> = m.seen.iter().map(|(b, _)| *b).collect();
        assert_eq!(
            brains,
            vec![Brain::LocalLarge, Brain::LocalLarge, Brain::Commercial]
        );
    }
    #[test]
    fn commercial_prompt_is_redacted_before_it_leaves() {
        let (_c, mut e) = engine();
        let mut m = Script::new(&[], &[REMIND]);
        e.handle(
            job(JobKind::Plan),
            "plan a surprise, ask asha@example.com or 9876543210",
            &mut m,
        );
        let (b, shown) = &m.seen[0];
        assert_eq!(*b, Brain::Commercial);
        assert!(!shown.contains("asha@example.com") && !shown.contains("9876543210"));
        assert!(shown.contains("[EMAIL]") && shown.contains("[PHONE]"));
    }
    #[test]
    fn never_send_stops_the_call() {
        let (_c, mut e) = engine();
        let mut m = Script::new(&[], &[REMIND]);
        let out = e.handle(job(JobKind::Plan), "plan using my vault key", &mut m);
        assert!(matches!(out, Outcome::Blocked(_)));
        assert!(m.seen.is_empty());
    }
    #[test]
    fn gave_up_when_everyone_fails() {
        let (_c, mut e) = engine();
        let mut m = Script::new(&["x", "y", "z"], &["w"]);
        assert_eq!(
            e.handle(job(JobKind::Design), "design", &mut m),
            Outcome::GaveUp
        );
    }
    #[test]
    fn approval_token_releases_exactly_that_action() {
        let (_c, mut e) = engine();
        let mut m = Script::new(&[PAY], &[]);
        let Outcome::NeedsApproval(a) = e.handle(job(JobKind::Chat), "pay asha", &mut m) else {
            panic!()
        };
        let tok = e.approve(&a);
        let mut other = a.clone();
        other.args[0].1 = "50000".into();
        assert!(!e.release(&tok, &other));
        assert!(e.release(&tok, &a));
        assert!(!e.release(&tok, &a), "single use");
    }
    #[test]
    fn every_step_is_in_the_store() {
        let (_c, mut e) = engine();
        let mut m = Script::new(&[REMIND], &[]);
        e.handle(job(JobKind::Chat), "remind me", &mut m);
        assert_eq!(e.store.by_kind("route").unwrap().len(), 1);
        assert_eq!(e.store.by_kind("action").unwrap().len(), 1);
    }
    #[test]
    fn human_only_jobs_never_reach_commercial_even_when_stuck() {
        let (_c, mut e) = engine();
        let mut j = job(JobKind::Plan);
        j.sensitivity = Sensitivity::HumanOnly;
        let mut m = Script::new(&["a", "b", "c", "d", "e"], &[REMIND]);
        e.handle(j, "plan", &mut m);
        assert!(m.seen.iter().all(|(b, _)| *b != Brain::Commercial));
    }
}
