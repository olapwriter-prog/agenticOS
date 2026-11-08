//! Wake locks with leases. A lock that is not renewed expires on its own,
//! so a crashed agent can never keep the phone awake forever.
use agentos_hal::Clock;
use std::collections::HashMap;
use std::sync::Arc;

pub struct WakeLocks {
    clock: Arc<dyn Clock>,
    max_lease_ms: u64,
    held: HashMap<String, u64>, // name -> expiry
}

impl WakeLocks {
    pub fn new(clock: Arc<dyn Clock>, max_lease_ms: u64) -> Self {
        Self {
            clock,
            max_lease_ms,
            held: HashMap::new(),
        }
    }
    /// Take or renew a lock. The lease is capped at max_lease_ms.
    pub fn acquire(&mut self, name: &str, lease_ms: u64) {
        let lease = lease_ms.min(self.max_lease_ms);
        self.held
            .insert(name.to_string(), self.clock.now_ms() + lease);
    }
    pub fn release(&mut self, name: &str) {
        self.held.remove(name);
    }
    fn sweep(&mut self) {
        let now = self.clock.now_ms();
        self.held.retain(|_, exp| *exp > now);
    }
    /// May the phone suspend right now?
    pub fn can_suspend(&mut self) -> bool {
        self.sweep();
        self.held.is_empty()
    }
    pub fn holders(&mut self) -> Vec<String> {
        self.sweep();
        let mut v: Vec<_> = self.held.keys().cloned().collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_hal::FakeClock;

    fn setup() -> (FakeClock, WakeLocks) {
        let c = FakeClock::new();
        (c.clone(), WakeLocks::new(Arc::new(c), 60_000))
    }
    #[test]
    fn holding_a_lock_blocks_suspend() {
        let (_c, mut w) = setup();
        w.acquire("chat", 5_000);
        assert!(!w.can_suspend());
    }
    #[test]
    fn lease_expires_by_itself() {
        let (c, mut w) = setup();
        w.acquire("crashed-agent", 5_000);
        c.advance_ms(5_001);
        assert!(w.can_suspend());
    }
    #[test]
    fn lease_is_capped() {
        let (c, mut w) = setup();
        w.acquire("greedy", u64::MAX / 4);
        c.advance_ms(60_001);
        assert!(w.can_suspend());
    }
    #[test]
    fn renew_extends() {
        let (c, mut w) = setup();
        w.acquire("a", 1_000);
        c.advance_ms(900);
        w.acquire("a", 1_000);
        c.advance_ms(900);
        assert_eq!(w.holders(), vec!["a".to_string()]);
    }
    #[test]
    fn release_frees() {
        let (_c, mut w) = setup();
        w.acquire("a", 1_000);
        w.release("a");
        assert!(w.can_suspend());
    }
}
