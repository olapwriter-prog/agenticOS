//! Hardware abstraction: every device the OS touches is a trait.
//! Real phones implement them over /sys and /dev; SimPhone implements them with fakes.
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

pub trait Clock: Send + Sync {
    /// Monotonic milliseconds since boot.
    fn now_ms(&self) -> u64;
}

pub trait PowerSource: Send + Sync {
    fn level_percent(&self) -> u8;
    fn on_charger(&self) -> bool;
}

pub trait ApprovalKey: Send + Sync {
    /// Blocks until the human presses (true) or the key times out (false).
    fn confirm(&self, prompt: &str) -> bool;
}

/// A clock the tests can move by hand.
#[derive(Clone, Default)]
pub struct FakeClock(Arc<AtomicU64>);
impl FakeClock {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn advance_ms(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}
impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// The real monotonic clock. `scale` multiplies elapsed time (SimPhone only; 1 on a real phone).
pub struct ScaledClock {
    start: std::time::Instant,
    scale: u64,
}
impl ScaledClock {
    pub fn new(scale: u64) -> Self {
        Self {
            start: std::time::Instant::now(),
            scale: scale.max(1),
        }
    }
    /// Reads SIM_SCALE, but only honours it when `sim_image` is true.
    pub fn from_env(sim_image: bool) -> Self {
        let scale = if sim_image {
            std::env::var("SIM_SCALE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1)
        } else {
            1
        };
        Self::new(scale)
    }
}
impl Clock for ScaledClock {
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64 * self.scale
    }
}

/// A battery the tests can drain, fill and plug in.
#[derive(Clone)]
pub struct FakePower {
    level: Arc<AtomicU8>,
    charger: Arc<AtomicBool>,
}
impl FakePower {
    pub fn new(level: u8, charger: bool) -> Self {
        Self {
            level: Arc::new(AtomicU8::new(level.min(100))),
            charger: Arc::new(AtomicBool::new(charger)),
        }
    }
    pub fn set_level(&self, l: u8) {
        self.level.store(l.min(100), Ordering::SeqCst)
    }
    pub fn set_charger(&self, c: bool) {
        self.charger.store(c, Ordering::SeqCst)
    }
}
impl PowerSource for FakePower {
    fn level_percent(&self) -> u8 {
        self.level.load(Ordering::SeqCst)
    }
    fn on_charger(&self) -> bool {
        self.charger.load(Ordering::SeqCst)
    }
}

/// An approval key whose answer is scripted.
pub struct FakeKey {
    pub answer: bool,
}
impl ApprovalKey for FakeKey {
    fn confirm(&self, _prompt: &str) -> bool {
        self.answer
    }
}

/// Real battery, read from the Linux power_supply class.
pub struct SysfsPower {
    pub dir: std::path::PathBuf,
}
impl SysfsPower {
    fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join(name))
            .ok()
            .map(|s| s.trim().to_string())
    }
}
impl PowerSource for SysfsPower {
    fn level_percent(&self) -> u8 {
        self.read("capacity")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }
    fn on_charger(&self) -> bool {
        matches!(
            self.read("status").as_deref(),
            Some("Charging") | Some("Full")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fake_clock_moves_only_when_told() {
        let c = FakeClock::new();
        assert_eq!(c.now_ms(), 0);
        c.advance_ms(1500);
        assert_eq!(c.now_ms(), 1500);
    }
    #[test]
    fn scaled_clock_runs_faster_only_when_allowed() {
        let fast = ScaledClock::new(1000);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(fast.now_ms() >= 20_000);
        std::env::set_var("SIM_SCALE", "1000");
        let refused = ScaledClock::from_env(false);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(refused.now_ms() < 5_000);
        std::env::remove_var("SIM_SCALE");
    }
    #[test]
    fn fake_power_clamps_to_100() {
        let p = FakePower::new(250, false);
        assert_eq!(p.level_percent(), 100);
        p.set_charger(true);
        assert!(p.on_charger());
    }
    #[test]
    fn sysfs_power_reads_files() {
        let d = std::env::temp_dir().join(format!("hal-sysfs-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("capacity"), "73\n").unwrap();
        std::fs::write(d.join("status"), "Charging\n").unwrap();
        let p = SysfsPower { dir: d.clone() };
        assert_eq!(p.level_percent(), 73);
        assert!(p.on_charger());
        std::fs::remove_dir_all(d).ok();
    }
}
