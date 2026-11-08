//! os-kernel-d: the supervisor. It owns the stage, and on every change it
//! (1) writes the ledger, (2) freezes or thaws background slices, (3) sets the governor.
use agentos_freeze::Freezer;
use agentos_hal::{Clock, PowerSource};
use agentos_ledger::Ledger;
use agentos_power::{apply_governor, choose_governor, model_budget, ModelBudget, PowerPolicy};
use agentos_stage::{background_allowed, next, Event, Stage};
use agentos_wake::WakeLocks;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

pub struct Paths {
    pub cpufreq: PathBuf,
    pub ledger: PathBuf,
}

pub struct Kernel {
    pub stage: Stage,
    pub locks: WakeLocks,
    power: Arc<dyn PowerSource>,
    policy: PowerPolicy,
    freezer: Freezer,
    ledger: Ledger,
    cpufreq: PathBuf,
}

impl Kernel {
    pub fn new(
        clock: Arc<dyn Clock>,
        power: Arc<dyn PowerSource>,
        freezer: Freezer,
        paths: Paths,
    ) -> io::Result<Self> {
        Ok(Self {
            stage: Stage::Asleep,
            locks: WakeLocks::new(clock, 10 * 60_000),
            power,
            policy: PowerPolicy::default(),
            freezer,
            ledger: Ledger::open(&paths.ledger)?,
            cpufreq: paths.cpufreq,
        })
    }

    /// Write one line to the ledger, for things that happen outside the stage machine.
    pub fn record(&mut self, line: &str) -> io::Result<u64> {
        self.ledger.append(line)
    }

    /// Feed one event. Returns the new stage.
    pub fn handle(&mut self, ev: Event) -> io::Result<Stage> {
        let before = self.stage;
        let after = next(before, ev);
        if after != before {
            self.ledger
                .append(&format!("stage {:?} -> {:?} on {:?}", before, after, ev))?;
            self.stage = after;
            self.freezer.set_frozen(!background_allowed(after))?;
            let g = choose_governor(after, self.power.as_ref(), &self.policy);
            apply_governor(&self.cpufreq, g)?;
        }
        Ok(self.stage)
    }

    pub fn model_budget(&self) -> ModelBudget {
        model_budget(self.stage, self.power.as_ref(), &self.policy)
    }
    pub fn may_suspend(&mut self) -> bool {
        self.stage == Stage::Asleep && self.locks.can_suspend()
    }
}

/// A crash or clean exit of the supervisor must never leave agents frozen.
impl Drop for Kernel {
    fn drop(&mut self) {
        let _ = self.freezer.set_frozen(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_freeze::FREEZABLE;
    use agentos_hal::{FakeClock, FakePower};

    struct Rig {
        k: Kernel,
        power: FakePower,
        clock: FakeClock,
        root: PathBuf,
    }

    fn rig(tag: &str) -> Rig {
        let root = std::env::temp_dir().join(format!("kd-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for s in FREEZABLE {
            std::fs::create_dir_all(root.join("cg").join(s)).unwrap();
        }
        for i in [0, 4] {
            std::fs::create_dir_all(root.join("cpu").join(format!("policy{i}"))).unwrap();
        }
        let clock = FakeClock::new();
        let power = FakePower::new(80, false);
        let k = Kernel::new(
            Arc::new(clock.clone()),
            Arc::new(power.clone()),
            Freezer {
                root: root.join("cg"),
            },
            Paths {
                cpufreq: root.join("cpu"),
                ledger: root.join("ledger.log"),
            },
        )
        .unwrap();
        Rig {
            k,
            power,
            clock,
            root,
        }
    }
    fn gov(r: &Rig) -> String {
        std::fs::read_to_string(r.root.join("cpu/policy0/scaling_governor")).unwrap()
    }

    #[test]
    fn chat_thaws_and_goes_schedutil() {
        let mut r = rig("chat");
        r.k.handle(Event::ChatArrived).unwrap();
        assert_eq!(r.k.stage, Stage::Active);
        assert_eq!(gov(&r), "schedutil");
        assert_eq!(r.k.model_budget(), ModelBudget::Small);
    }
    #[test]
    fn charger_enters_deep_and_unfreezes_background() {
        let mut r = rig("deep");
        r.power.set_charger(true);
        r.k.handle(Event::ChargerReady).unwrap();
        assert_eq!(r.k.stage, Stage::Deep);
        assert_eq!(gov(&r), "performance");
        assert!(!r.k.freezer.is_frozen().unwrap());
        assert_eq!(r.k.model_budget(), ModelBudget::Large);
    }
    #[test]
    fn chat_during_deep_freezes_background_at_once() {
        let mut r = rig("preempt");
        r.power.set_charger(true);
        r.k.handle(Event::ChargerReady).unwrap();
        r.k.handle(Event::ChatArrived).unwrap();
        assert!(r.k.freezer.is_frozen().unwrap());
        assert_eq!(r.k.stage, Stage::Active);
    }
    #[test]
    fn every_change_is_in_the_ledger() {
        let mut r = rig("ledger");
        r.k.handle(Event::ChatArrived).unwrap();
        r.k.handle(Event::ChatIdle).unwrap();
        assert_eq!(r.k.ledger.verify().unwrap(), 2);
    }
    #[test]
    fn cannot_suspend_while_a_lock_is_held() {
        let mut r = rig("suspend");
        assert!(r.k.may_suspend());
        r.k.locks.acquire("download", 30_000);
        assert!(!r.k.may_suspend());
        r.clock.advance_ms(30_001);
        assert!(r.k.may_suspend());
    }
    #[test]
    fn dropping_the_supervisor_thaws_everything() {
        let mut r = rig("drop");
        r.k.handle(Event::ChatArrived).unwrap();
        let f = Freezer {
            root: r.root.join("cg"),
        };
        assert!(f.is_frozen().unwrap());
        drop(r.k);
        assert!(!f.is_frozen().unwrap());
    }
    #[test]
    fn same_event_twice_writes_once() {
        let mut r = rig("idem");
        r.k.handle(Event::ChatArrived).unwrap();
        r.k.handle(Event::ChatArrived).unwrap();
        assert_eq!(r.k.ledger.verify().unwrap(), 1);
    }
}
