//! The one-line status. The phone has no screen, so this sentence is how it says how it is.
//! It is built only from facts the supervisor holds, never from the model.
use agentos_hal::PowerSource;
use agentos_stage::Stage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub stage: Stage,
    pub battery: u8,
    pub charging: bool,
    pub background_jobs: usize,
    pub pending_approvals: usize,
    pub offline: bool,
}

impl Facts {
    pub fn from_power(
        stage: Stage,
        p: &dyn PowerSource,
        background_jobs: usize,
        pending_approvals: usize,
        offline: bool,
    ) -> Self {
        Facts {
            stage,
            battery: p.level_percent(),
            charging: p.on_charger(),
            background_jobs,
            pending_approvals,
            offline,
        }
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// One sentence. Things that need the human come first.
pub fn line(f: &Facts) -> String {
    let mut parts: Vec<String> = Vec::new();
    if f.pending_approvals > 0 {
        parts.push(format!(
            "{} waiting for you",
            plural(f.pending_approvals, "approval", "approvals")
        ));
    }
    parts.push(
        match f.stage {
            Stage::Asleep => "Asleep",
            Stage::Active => "Active, background work paused",
            Stage::Deep if f.background_jobs > 0 => "Working in the background",
            Stage::Deep => "Charging and idle, ready for background work",
            Stage::WindingDown => "Finishing up",
        }
        .to_string(),
    );
    if f.stage == Stage::Deep && f.background_jobs > 0 {
        parts.push(plural(f.background_jobs, "job running", "jobs running"));
    }
    let battery = if f.charging {
        format!("battery {}% and charging", f.battery)
    } else {
        format!("battery {}%", f.battery)
    };
    parts.push(battery);
    if f.offline {
        parts.push("offline".to_string());
    }
    if !f.charging && f.battery <= 15 {
        parts.push("power saving on".to_string());
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_hal::FakePower;

    fn facts(stage: Stage) -> Facts {
        Facts {
            stage,
            battery: 80,
            charging: false,
            background_jobs: 0,
            pending_approvals: 0,
            offline: false,
        }
    }

    #[test]
    fn asleep_on_battery() {
        assert_eq!(line(&facts(Stage::Asleep)), "Asleep, battery 80%");
    }
    #[test]
    fn active_says_background_is_paused() {
        assert_eq!(
            line(&facts(Stage::Active)),
            "Active, background work paused, battery 80%"
        );
    }
    #[test]
    fn deep_with_jobs_counts_them_and_pluralises() {
        let mut f = facts(Stage::Deep);
        f.charging = true;
        f.background_jobs = 1;
        assert_eq!(
            line(&f),
            "Working in the background, 1 job running, battery 80% and charging"
        );
        f.background_jobs = 3;
        assert!(line(&f).contains("3 jobs running"));
    }
    #[test]
    fn deep_without_jobs_is_idle() {
        let mut f = facts(Stage::Deep);
        f.charging = true;
        assert!(line(&f).starts_with("Charging and idle"));
    }
    #[test]
    fn approvals_come_first() {
        let mut f = facts(Stage::Active);
        f.pending_approvals = 2;
        assert!(line(&f).starts_with("2 approvals waiting for you"));
        f.pending_approvals = 1;
        assert!(line(&f).starts_with("1 approval waiting for you"));
    }
    #[test]
    fn low_battery_and_offline_are_said_plainly() {
        let mut f = facts(Stage::Asleep);
        f.battery = 12;
        f.offline = true;
        assert_eq!(line(&f), "Asleep, battery 12%, offline, power saving on");
    }
    #[test]
    fn low_battery_on_a_charger_is_not_power_saving() {
        let mut f = facts(Stage::Asleep);
        f.battery = 12;
        f.charging = true;
        assert!(!line(&f).contains("power saving"));
    }
    #[test]
    fn facts_come_from_the_power_trait() {
        let p = FakePower::new(55, true);
        let f = Facts::from_power(Stage::Deep, &p, 2, 0, false);
        assert_eq!((f.battery, f.charging), (55, true));
    }
}
