//! The tool executor. Connectors register tools; the executor is the only thing that runs them.
//! A High-tier action cannot be run without an `Approved` value, and the only way to get
//! one is to redeem a valid Gate token for that exact action.
use agentos_action::{Action, Registry, Tier, Tool};
use agentos_brain::canonical;
use agentos_gate::{Gate, GateError};
use agentos_hal::Clock;
use agentos_store::Store;
use std::sync::Arc;

pub trait Connector {
    fn name(&self) -> &str;
    fn tools(&self) -> Vec<Tool>;
    fn call(&mut self, action: &Action) -> Result<String, String>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExecError {
    UnknownTool(String),
    /// The action is High tier. Show it to the human and get a token.
    NeedsApproval,
    /// `run` was given a tool of a lower tier than the caller claimed, or vice versa.
    WrongEntryPoint,
    ConnectorFailed(String),
}

/// Proof that the human approved exactly this action.
///
/// Its field is private, so code outside this module cannot build one:
///
/// ```compile_fail
/// use agentos_executor::Approved;
/// use agentos_action::Action;
/// let a = Approved { action: Action { tool: "pay".into(), args: vec![] } };
/// ```
pub struct Approved {
    action: Action,
}

impl Approved {
    pub fn action(&self) -> &Action {
        &self.action
    }
}

pub struct Executor {
    tools: Vec<Tool>,
    registry: Registry,
    connectors: Vec<Box<dyn Connector>>,
    store: Store,
    clock: Arc<dyn Clock>,
    gate: Gate,
}

impl Executor {
    pub fn new(
        connectors: Vec<Box<dyn Connector>>,
        store: Store,
        clock: Arc<dyn Clock>,
        gate: Gate,
    ) -> Self {
        let tools: Vec<Tool> = connectors.iter().flat_map(|c| c.tools()).collect();
        let registry = Registry::new(tools.clone());
        Self {
            tools,
            registry,
            connectors,
            store,
            clock,
            gate,
        }
    }

    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }
    pub fn registry(&self) -> &Registry {
        &self.registry
    }
    pub fn store(&self) -> &Store {
        &self.store
    }

    fn log(&self, msg: &str) {
        let _ = self.store.append(self.clock.now_ms(), "tool", msg);
    }

    fn dispatch(&mut self, action: &Action) -> Result<String, ExecError> {
        let idx = self
            .connectors
            .iter()
            .position(|c| c.tools().iter().any(|t| t.name == action.tool))
            .ok_or_else(|| ExecError::UnknownTool(action.tool.clone()))?;
        match self.connectors[idx].call(action) {
            Ok(out) => {
                self.log(&format!("{} ok", action.tool));
                Ok(out)
            }
            Err(e) => {
                self.log(&format!("{} failed", action.tool));
                Err(ExecError::ConnectorFailed(e))
            }
        }
    }

    /// Run a Low or Medium action. A High action is refused here, always.
    pub fn run(&mut self, action: &Action) -> Result<String, ExecError> {
        match self.registry.tier_of(&action.tool) {
            None => Err(ExecError::UnknownTool(action.tool.clone())),
            Some(Tier::High) => {
                self.log(&format!("{} refused: needs approval", action.tool));
                Err(ExecError::NeedsApproval)
            }
            Some(_) => self.dispatch(action),
        }
    }

    /// Turn a token into proof of approval. Fails if the token is not valid for this exact action.
    pub fn approve(&mut self, token: &str, action: &Action) -> Result<Approved, GateError> {
        self.gate.redeem(token, &canonical(action))?;
        self.log(&format!("{} approved", action.tool));
        Ok(Approved {
            action: action.clone(),
        })
    }

    /// Run a High action. Consumes the proof, so one approval runs one action once.
    pub fn run_approved(&mut self, approved: Approved) -> Result<String, ExecError> {
        match self.registry.tier_of(&approved.action.tool) {
            None => Err(ExecError::UnknownTool(approved.action.tool.clone())),
            Some(Tier::High) => self.dispatch(&approved.action),
            Some(_) => Err(ExecError::WrongEntryPoint),
        }
    }

    /// For the chat gateway: mint a token after the human pressed the key.
    pub fn mint_after_key(&mut self, action: &Action) -> String {
        self.gate.mint(&canonical(action))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_hal::FakeClock;

    struct Fake {
        calls: Vec<String>,
        fail: bool,
    }
    impl Connector for Fake {
        fn name(&self) -> &str {
            "fake"
        }
        fn tools(&self) -> Vec<Tool> {
            vec![
                Tool {
                    name: "read",
                    args: &["what"],
                    tier: Tier::Low,
                },
                Tool {
                    name: "note",
                    args: &["text"],
                    tier: Tier::Medium,
                },
                Tool {
                    name: "pay",
                    args: &["to", "amount"],
                    tier: Tier::High,
                },
            ]
        }
        fn call(&mut self, a: &Action) -> Result<String, String> {
            self.calls.push(a.tool.clone());
            if self.fail {
                Err("boom".into())
            } else {
                Ok(format!("did {}", a.tool))
            }
        }
    }

    fn exec(fail: bool) -> Executor {
        let clock = FakeClock::new();
        Executor::new(
            vec![Box::new(Fake {
                calls: vec![],
                fail,
            })],
            Store::in_memory().unwrap(),
            Arc::new(clock.clone()),
            Gate::new(b"k", Arc::new(clock), 600_000),
        )
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
    fn low_and_medium_run_directly() {
        let mut e = exec(false);
        assert_eq!(e.run(&act("read", &[("what", "x")])).unwrap(), "did read");
        assert_eq!(e.run(&act("note", &[("text", "x")])).unwrap(), "did note");
    }
    #[test]
    fn high_is_refused_by_run_and_never_reaches_the_connector() {
        let mut e = exec(false);
        let r = e.run(&act("pay", &[("amount", "5"), ("to", "a")]));
        assert_eq!(r, Err(ExecError::NeedsApproval));
        assert_eq!(e.store().by_kind("tool").unwrap().len(), 1);
        assert!(e.store().by_kind("tool").unwrap()[0]
            .payload
            .contains("refused"));
    }
    #[test]
    fn a_valid_token_runs_the_high_action_once() {
        let mut e = exec(false);
        let a = act("pay", &[("amount", "5"), ("to", "a")]);
        let tok = e.mint_after_key(&a);
        let ap = e.approve(&tok, &a).unwrap();
        assert_eq!(e.run_approved(ap).unwrap(), "did pay");
        assert!(e.approve(&tok, &a).is_err(), "token is single use");
    }
    #[test]
    fn a_token_for_another_amount_gives_no_proof() {
        let mut e = exec(false);
        let a = act("pay", &[("amount", "5"), ("to", "a")]);
        let tok = e.mint_after_key(&a);
        let other = act("pay", &[("amount", "5000"), ("to", "a")]);
        assert!(e.approve(&tok, &other).is_err());
    }
    #[test]
    fn unknown_tools_are_unknown_on_both_paths() {
        let mut e = exec(false);
        assert_eq!(
            e.run(&act("rm", &[])),
            Err(ExecError::UnknownTool("rm".into()))
        );
    }
    #[test]
    fn approval_for_a_low_tool_cannot_be_used_on_the_high_path() {
        let mut e = exec(false);
        let a = act("note", &[("text", "x")]);
        let tok = e.mint_after_key(&a);
        let ap = e.approve(&tok, &a).unwrap();
        assert_eq!(e.run_approved(ap), Err(ExecError::WrongEntryPoint));
    }
    #[test]
    fn connector_errors_are_reported_and_logged() {
        let mut e = exec(true);
        let r = e.run(&act("read", &[("what", "x")]));
        assert_eq!(r, Err(ExecError::ConnectorFailed("boom".into())));
        assert!(e.store().by_kind("tool").unwrap()[0]
            .payload
            .contains("failed"));
    }
    #[test]
    fn tools_from_all_connectors_are_listed() {
        assert_eq!(exec(false).tools().len(), 3);
    }
}
