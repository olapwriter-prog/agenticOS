//! Dedicated agents: a goal, a budget, and a ceiling on what they may do alone.
//! An agent runs only in the Deep stage, goes through the executor for every action, and stops
//! the moment it runs out of budget or needs the human.
use agentos_action::{Action, Registry, Tier};
use agentos_executor::{Approved, ExecError, Executor};
use agentos_hal::Clock;
use agentos_stage::{background_allowed, Stage};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct Step {
    pub action: Action,
    pub result: Result<String, String>,
}

/// Decides the next action from what has happened so far. `None` means the goal is reached.
/// In production this is a model; in tests it is a script.
pub trait Planner {
    fn next(&mut self, history: &[Step]) -> Option<Action>;
}

#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub max_steps: u32,
    pub max_runtime_ms: u64,
    /// The highest tier the agent may run without asking. Above it, the agent pauses.
    pub ceiling: Tier,
}

#[derive(Debug, PartialEq, Eq)]
pub enum State {
    /// Ran one step; call `tick` again.
    Running,
    /// Not in the Deep stage. Nothing ran and no budget was spent.
    Frozen,
    /// Waiting for the human. The action has not run.
    Paused(Action),
    Done,
    OutOfSteps,
    OutOfTime,
    /// Three failures in a row.
    Failed(String),
}

pub struct Agent {
    pub name: String,
    planner: Box<dyn Planner>,
    budget: Budget,
    clock: Arc<dyn Clock>,
    history: Vec<Step>,
    used_ms: u64,
    consecutive_failures: u32,
    paused: Option<Action>,
    finished: Option<State>,
}

impl Agent {
    pub fn new(
        name: &str,
        planner: Box<dyn Planner>,
        budget: Budget,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Agent {
            name: name.into(),
            planner,
            budget,
            clock,
            history: vec![],
            used_ms: 0,
            consecutive_failures: 0,
            paused: None,
            finished: None,
        }
    }

    pub fn history(&self) -> &[Step] {
        &self.history
    }
    pub fn steps_used(&self) -> usize {
        self.history.len()
    }

    fn record(&mut self, action: Action, result: Result<String, String>) {
        if result.is_ok() {
            self.consecutive_failures = 0;
        } else {
            self.consecutive_failures += 1;
        }
        self.history.push(Step { action, result });
    }

    /// Run at most one step.
    pub fn tick(&mut self, stage: Stage, exec: &mut Executor) -> State {
        if let Some(done) = &self.finished {
            return clone_state(done);
        }
        if let Some(a) = &self.paused {
            return State::Paused(a.clone());
        }
        if !background_allowed(stage) {
            return State::Frozen;
        }
        if self.history.len() as u32 >= self.budget.max_steps {
            return self.finish(State::OutOfSteps);
        }
        if self.used_ms >= self.budget.max_runtime_ms {
            return self.finish(State::OutOfTime);
        }
        let Some(action) = self.planner.next(&self.history) else {
            return self.finish(State::Done);
        };

        let tier = tier_of(exec.registry(), &action);
        if tier > self.budget.ceiling {
            self.paused = Some(action.clone());
            return State::Paused(action);
        }
        let started = self.clock.now_ms();
        let result = exec.run(&action);
        self.used_ms += self.clock.now_ms().saturating_sub(started);
        match result {
            Err(ExecError::NeedsApproval) => {
                self.paused = Some(action.clone());
                State::Paused(action)
            }
            other => {
                self.record(action, other.map_err(|e| format!("{e:?}")));
                if self.consecutive_failures >= 3 {
                    let msg = self
                        .history
                        .last()
                        .and_then(|s| s.result.clone().err())
                        .unwrap_or_default();
                    return self.finish(State::Failed(msg));
                }
                State::Running
            }
        }
    }

    /// The human approved a High action with the key. Run it and carry on.
    pub fn resume_with_approval(&mut self, exec: &mut Executor, approved: Approved) -> State {
        match &self.paused {
            Some(a) if a == approved.action() => {}
            _ => return State::Failed("approval is not for the paused action".into()),
        }
        let action = self.paused.take().expect("checked above");
        let started = self.clock.now_ms();
        let result = exec.run_approved(approved);
        self.used_ms += self.clock.now_ms().saturating_sub(started);
        self.record(action, result.map_err(|e| format!("{e:?}")));
        State::Running
    }

    /// The human allows one action that is above the agent's ceiling but below High.
    pub fn allow_once(&mut self, exec: &mut Executor) -> State {
        let Some(action) = self.paused.take() else {
            return State::Running;
        };
        if tier_of(exec.registry(), &action) == Tier::High {
            self.paused = Some(action.clone());
            return State::Paused(action); // High needs the key, not a nod
        }
        let started = self.clock.now_ms();
        let result = exec.run(&action);
        self.used_ms += self.clock.now_ms().saturating_sub(started);
        self.record(action, result.map_err(|e| format!("{e:?}")));
        State::Running
    }

    /// The human declined. The agent stops.
    pub fn decline(&mut self) -> State {
        self.paused = None;
        self.finish(State::Failed("the human declined".into()))
    }

    fn finish(&mut self, s: State) -> State {
        let out = clone_state(&s);
        self.finished = Some(s);
        out
    }
}

fn clone_state(s: &State) -> State {
    match s {
        State::Running => State::Running,
        State::Frozen => State::Frozen,
        State::Paused(a) => State::Paused(a.clone()),
        State::Done => State::Done,
        State::OutOfSteps => State::OutOfSteps,
        State::OutOfTime => State::OutOfTime,
        State::Failed(m) => State::Failed(m.clone()),
    }
}

/// Unknown tools count as High, so an agent can never quietly run something the registry does not know.
fn tier_of(reg: &Registry, a: &Action) -> Tier {
    reg.tier_of(&a.tool).unwrap_or(Tier::High)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_connectors::{Calendar, Messenger, Reminders};
    use agentos_gate::Gate;
    use agentos_hal::FakeClock;
    use agentos_store::Store;

    struct Script(Vec<Action>);
    impl Planner for Script {
        fn next(&mut self, _h: &[Step]) -> Option<Action> {
            if self.0.is_empty() {
                None
            } else {
                Some(self.0.remove(0))
            }
        }
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
    fn read(day: &str) -> Action {
        act("read_calendar", &[("day", day)])
    }
    fn exec(clock: &FakeClock) -> Executor {
        Executor::new(
            vec![
                Box::new(Calendar::default()),
                Box::new(Reminders::default()),
                Box::new(Messenger::default()),
            ],
            Store::in_memory().unwrap(),
            Arc::new(clock.clone()),
            Gate::new(b"k", Arc::new(clock.clone()), 600_000),
        )
    }
    fn budget() -> Budget {
        Budget {
            max_steps: 10,
            max_runtime_ms: 10_000,
            ceiling: Tier::Medium,
        }
    }
    fn agent(clock: &FakeClock, script: Vec<Action>, b: Budget) -> Agent {
        Agent::new("test", Box::new(Script(script)), b, Arc::new(clock.clone()))
    }
    fn drive(a: &mut Agent, e: &mut Executor) -> State {
        loop {
            match a.tick(Stage::Deep, e) {
                State::Running => continue,
                other => return other,
            }
        }
    }

    #[test]
    fn an_agent_runs_its_plan_to_the_end() {
        let c = FakeClock::new();
        let mut e = exec(&c);
        let mut a = agent(&c, vec![read("mon"), read("tue")], budget());
        assert_eq!(drive(&mut a, &mut e), State::Done);
        assert_eq!(a.steps_used(), 2);
    }
    #[test]
    fn outside_the_deep_stage_nothing_runs_and_nothing_is_spent() {
        let c = FakeClock::new();
        let mut e = exec(&c);
        let mut a = agent(&c, vec![read("mon")], budget());
        for stage in [Stage::Asleep, Stage::Active, Stage::WindingDown] {
            assert_eq!(a.tick(stage, &mut e), State::Frozen);
        }
        assert_eq!(a.steps_used(), 0);
        assert_eq!(drive(&mut a, &mut e), State::Done);
    }
    #[test]
    fn the_step_budget_stops_an_endless_plan() {
        let c = FakeClock::new();
        let mut e = exec(&c);
        let forever = (0..1000).map(|_| read("mon")).collect();
        let mut a = agent(
            &c,
            forever,
            Budget {
                max_steps: 5,
                ..budget()
            },
        );
        assert_eq!(drive(&mut a, &mut e), State::OutOfSteps);
        assert_eq!(a.steps_used(), 5);
    }
    /// A connector that burns fake time, so the runtime budget can be tested.
    struct Slow(FakeClock);
    impl agentos_executor::Connector for Slow {
        fn name(&self) -> &str {
            "slow"
        }
        fn tools(&self) -> Vec<agentos_action::Tool> {
            vec![agentos_action::Tool {
                name: "slow_read",
                args: &[],
                tier: Tier::Low,
            }]
        }
        fn call(&mut self, _a: &Action) -> Result<String, String> {
            self.0.advance_ms(400);
            Ok("ok".into())
        }
    }
    #[test]
    fn the_runtime_budget_counts_time_spent_in_actions() {
        let c = FakeClock::new();
        let mut e = Executor::new(
            vec![Box::new(Slow(c.clone()))],
            Store::in_memory().unwrap(),
            Arc::new(c.clone()),
            Gate::new(b"k", Arc::new(c.clone()), 1000),
        );
        let plan = (0..100).map(|_| act("slow_read", &[])).collect();
        let mut a = agent(
            &c,
            plan,
            Budget {
                max_runtime_ms: 1000,
                ..budget()
            },
        );
        assert_eq!(drive(&mut a, &mut e), State::OutOfTime);
        assert_eq!(a.steps_used(), 3); // 400 + 400 + 400 >= 1000
    }
    #[test]
    fn time_spent_frozen_is_not_charged() {
        let c = FakeClock::new();
        let mut e = exec(&c);
        let mut a = agent(
            &c,
            vec![read("mon")],
            Budget {
                max_runtime_ms: 100,
                ..budget()
            },
        );
        assert_eq!(a.tick(Stage::Active, &mut e), State::Frozen);
        c.advance_ms(3_600_000); // an hour of chatting
        assert_eq!(drive(&mut a, &mut e), State::Done);
    }
    #[test]
    fn a_high_action_pauses_and_is_not_run() {
        let c = FakeClock::new();
        let mut e = exec(&c);
        let msg = act("send_message", &[("text", "hi"), ("to", "asha")]);
        let mut a = agent(&c, vec![read("mon"), msg.clone()], budget());
        assert_eq!(drive(&mut a, &mut e), State::Paused(msg));
        assert_eq!(a.steps_used(), 1);
        // ticking again does not sneak past the pause
        assert!(matches!(a.tick(Stage::Deep, &mut e), State::Paused(_)));
    }
    #[test]
    fn approval_runs_the_paused_action_and_the_plan_continues() {
        let c = FakeClock::new();
        let mut e = exec(&c);
        let msg = act("send_message", &[("text", "hi"), ("to", "asha")]);
        let mut a = agent(&c, vec![msg.clone(), read("mon")], budget());
        assert!(matches!(drive(&mut a, &mut e), State::Paused(_)));
        let tok = e.mint_after_key(&msg);
        let ap = e.approve(&tok, &msg).unwrap();
        assert_eq!(a.resume_with_approval(&mut e, ap), State::Running);
        assert_eq!(drive(&mut a, &mut e), State::Done);
        assert_eq!(a.steps_used(), 2);
    }
    #[test]
    fn an_approval_for_another_action_does_not_release_the_pause() {
        let c = FakeClock::new();
        let mut e = exec(&c);
        let msg = act("send_message", &[("text", "hi"), ("to", "asha")]);
        let other = act("send_message", &[("text", "different"), ("to", "asha")]);
        let mut a = agent(&c, vec![msg], budget());
        drive(&mut a, &mut e);
        let tok = e.mint_after_key(&other);
        let ap = e.approve(&tok, &other).unwrap();
        assert!(matches!(
            a.resume_with_approval(&mut e, ap),
            State::Failed(_)
        ));
    }
    #[test]
    fn a_ceiling_of_low_pauses_a_medium_action_until_allowed_once() {
        let c = FakeClock::new();
        let mut e = exec(&c);
        let rem = act("set_reminder", &[("text", "x"), ("when", "6pm")]);
        let mut a = agent(
            &c,
            vec![rem.clone()],
            Budget {
                ceiling: Tier::Low,
                ..budget()
            },
        );
        assert_eq!(drive(&mut a, &mut e), State::Paused(rem));
        assert_eq!(a.allow_once(&mut e), State::Running);
        assert_eq!(drive(&mut a, &mut e), State::Done);
    }
    #[test]
    fn allow_once_is_not_enough_for_a_high_action() {
        let c = FakeClock::new();
        let mut e = exec(&c);
        let msg = act("send_message", &[("text", "hi"), ("to", "asha")]);
        let mut a = agent(
            &c,
            vec![msg],
            Budget {
                ceiling: Tier::High,
                ..budget()
            },
        );
        assert!(matches!(drive(&mut a, &mut e), State::Paused(_)));
        assert!(matches!(a.allow_once(&mut e), State::Paused(_)));
    }
    #[test]
    fn declining_stops_the_agent_for_good() {
        let c = FakeClock::new();
        let mut e = exec(&c);
        let msg = act("send_message", &[("text", "hi"), ("to", "asha")]);
        let mut a = agent(&c, vec![msg, read("mon")], budget());
        drive(&mut a, &mut e);
        assert!(matches!(a.decline(), State::Failed(_)));
        assert!(matches!(a.tick(Stage::Deep, &mut e), State::Failed(_)));
    }
    #[test]
    fn three_failures_in_a_row_end_the_agent() {
        let c = FakeClock::new();
        let mut e = exec(&c);
        let bad = act("read_calendar", &[]); // missing its argument, so the connector errors
        let mut a = agent(
            &c,
            vec![bad.clone(), bad.clone(), bad.clone(), read("mon")],
            budget(),
        );
        assert!(matches!(drive(&mut a, &mut e), State::Failed(_)));
        assert_eq!(a.steps_used(), 3);
    }
    #[test]
    fn a_success_resets_the_failure_count() {
        let c = FakeClock::new();
        let mut e = exec(&c);
        let bad = act("read_calendar", &[]);
        let plan = vec![
            bad.clone(),
            bad.clone(),
            read("mon"),
            bad.clone(),
            bad,
            read("tue"),
        ];
        let mut a = agent(&c, plan, budget());
        assert_eq!(drive(&mut a, &mut e), State::Done);
    }
    #[test]
    fn an_unknown_tool_is_treated_as_high_and_pauses() {
        let c = FakeClock::new();
        let mut e = exec(&c);
        let mut a = agent(
            &c,
            vec![act("format_disk", &[])],
            Budget {
                ceiling: Tier::Medium,
                ..budget()
            },
        );
        assert!(matches!(drive(&mut a, &mut e), State::Paused(_)));
    }
    #[test]
    fn the_planner_sees_the_history() {
        struct Peek(std::rc::Rc<std::cell::RefCell<Vec<usize>>>);
        impl Planner for Peek {
            fn next(&mut self, h: &[Step]) -> Option<Action> {
                self.0.borrow_mut().push(h.len());
                if h.len() < 2 {
                    Some(act("read_calendar", &[("day", "mon")]))
                } else {
                    None
                }
            }
        }
        let seen = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
        let c = FakeClock::new();
        let mut e = exec(&c);
        let mut a = Agent::new(
            "p",
            Box::new(Peek(seen.clone())),
            budget(),
            Arc::new(c.clone()),
        );
        drive(&mut a, &mut e);
        assert_eq!(*seen.borrow(), vec![0, 1, 2]);
    }
}
