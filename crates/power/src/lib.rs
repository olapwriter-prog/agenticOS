//! US-F05: choose a CPU governor and a model size from stage + battery.
use agentos_hal::PowerSource;
use agentos_stage::Stage;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Governor {
    Powersave,
    Schedutil,
    Performance,
}
impl Governor {
    pub fn as_str(self) -> &'static str {
        match self {
            Governor::Powersave => "powersave",
            Governor::Schedutil => "schedutil",
            Governor::Performance => "performance",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelBudget {
    None,
    Small,
    Large,
}

#[derive(Debug, Clone, Copy)]
pub struct PowerPolicy {
    pub low_battery_floor: u8,
    pub deep_min_battery: u8,
}
impl Default for PowerPolicy {
    fn default() -> Self {
        Self {
            low_battery_floor: 15,
            deep_min_battery: 30,
        }
    }
}

pub fn choose_governor(stage: Stage, p: &dyn PowerSource, pol: &PowerPolicy) -> Governor {
    if p.level_percent() <= pol.low_battery_floor && !p.on_charger() {
        return Governor::Powersave;
    }
    match stage {
        Stage::Asleep | Stage::WindingDown => Governor::Powersave,
        Stage::Active => Governor::Schedutil,
        Stage::Deep => {
            if p.on_charger() {
                Governor::Performance
            } else {
                Governor::Schedutil
            }
        }
    }
}

pub fn model_budget(stage: Stage, p: &dyn PowerSource, pol: &PowerPolicy) -> ModelBudget {
    match stage {
        Stage::Asleep | Stage::WindingDown => ModelBudget::None,
        Stage::Active => ModelBudget::Small,
        Stage::Deep => {
            if p.on_charger() && p.level_percent() >= pol.deep_min_battery {
                ModelBudget::Large
            } else {
                ModelBudget::Small
            }
        }
    }
}

/// Writes the governor to every cpufreq policy under `root` (normally /sys/devices/system/cpu/cpufreq).
pub fn apply_governor(root: &Path, g: Governor) -> io::Result<usize> {
    let mut n = 0;
    for entry in std::fs::read_dir(root)? {
        let dir: PathBuf = entry?.path();
        if dir
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("policy"))
        {
            std::fs::write(dir.join("scaling_governor"), g.as_str())?;
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_hal::FakePower;

    #[test]
    fn low_battery_forces_powersave_even_when_active() {
        let p = FakePower::new(10, false);
        assert_eq!(
            choose_governor(Stage::Active, &p, &PowerPolicy::default()),
            Governor::Powersave
        );
    }
    #[test]
    fn deep_on_charger_is_performance_large() {
        let p = FakePower::new(80, true);
        let pol = PowerPolicy::default();
        assert_eq!(
            choose_governor(Stage::Deep, &p, &pol),
            Governor::Performance
        );
        assert_eq!(model_budget(Stage::Deep, &p, &pol), ModelBudget::Large);
    }
    #[test]
    fn deep_with_nearly_empty_charging_battery_stays_small() {
        let p = FakePower::new(20, true);
        assert_eq!(
            model_budget(Stage::Deep, &p, &PowerPolicy::default()),
            ModelBudget::Small
        );
    }
    #[test]
    fn asleep_loads_no_model() {
        let p = FakePower::new(100, true);
        assert_eq!(
            model_budget(Stage::Asleep, &p, &PowerPolicy::default()),
            ModelBudget::None
        );
    }
    #[test]
    fn apply_governor_writes_every_policy() {
        let root = std::env::temp_dir().join(format!("cpufreq-{}", std::process::id()));
        for i in [0, 4, 7] {
            std::fs::create_dir_all(root.join(format!("policy{i}"))).unwrap();
        }
        std::fs::create_dir_all(root.join("boost")).unwrap();
        assert_eq!(apply_governor(&root, Governor::Powersave).unwrap(), 3);
        let got = std::fs::read_to_string(root.join("policy4/scaling_governor")).unwrap();
        assert_eq!(got, "powersave");
        std::fs::remove_dir_all(root).ok();
    }
}
