//! The stage machine: Asleep -> Active -> Deep -> Winding down.
//! A pure function. No clocks, no files, no threads, so it is trivial to test.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Asleep,
    Active,
    Deep,
    WindingDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The human sent a chat message or tapped the key.
    ChatArrived,
    /// The human has been silent past the idle limit.
    ChatIdle,
    /// Phone plugged in with enough battery for heavy work.
    ChargerReady,
    /// Charger removed or battery fell below the floor.
    PowerLost,
    /// Background work queue is empty.
    QueueEmpty,
    /// Something is waiting in the background queue.
    WorkQueued,
    /// Wind-down finished (agents saved, models unloaded).
    WindDownDone,
}

pub fn next(stage: Stage, ev: Event) -> Stage {
    use Event::*;
    use Stage::*;
    match (stage, ev) {
        // The human always wins: chat pulls us to Active from anywhere except mid wind-down.
        (WindingDown, ChatArrived) => Active,
        (_, ChatArrived) => Active,
        (Active, ChatIdle) => Asleep,
        (Asleep, ChargerReady) | (Active, ChargerReady) => Deep,
        (Deep, PowerLost) => WindingDown,
        (Deep, QueueEmpty) => WindingDown,
        (Asleep, WorkQueued) => Asleep, // wake only when conditions allow, see ChargerReady
        (WindingDown, WindDownDone) => Asleep,
        (s, _) => s,
    }
}

/// What the supervisor must hold running in each stage.
pub fn background_allowed(stage: Stage) -> bool {
    matches!(stage, Stage::Deep)
}
pub fn model_loaded(stage: Stage) -> bool {
    matches!(stage, Stage::Active | Stage::Deep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Event::*;
    use Stage::*;

    #[test]
    fn chat_always_wakes() {
        for s in [Asleep, Active, Deep, WindingDown] {
            assert_eq!(next(s, ChatArrived), Active, "from {:?}", s);
        }
    }
    #[test]
    fn deep_needs_charger() {
        assert_eq!(next(Asleep, WorkQueued), Asleep);
        assert_eq!(next(Asleep, ChargerReady), Deep);
    }
    #[test]
    fn power_loss_winds_down_then_sleeps() {
        let s = next(Deep, PowerLost);
        assert_eq!(s, WindingDown);
        assert_eq!(next(s, WindDownDone), Asleep);
    }
    #[test]
    fn idle_only_affects_active() {
        assert_eq!(next(Active, ChatIdle), Asleep);
        assert_eq!(next(Deep, ChatIdle), Deep);
    }
    #[test]
    fn background_only_in_deep() {
        assert!(background_allowed(Deep));
        assert!(!background_allowed(Active));
        assert!(!background_allowed(Asleep));
    }
    /// Property-style: no event sequence can reach Deep without passing ChargerReady.
    #[test]
    fn deep_unreachable_without_charger() {
        let evs = [
            ChatArrived,
            ChatIdle,
            PowerLost,
            QueueEmpty,
            WorkQueued,
            WindDownDone,
        ];
        let mut frontier = vec![Asleep];
        let mut seen = std::collections::HashSet::new();
        while let Some(s) = frontier.pop() {
            if !seen.insert(s) {
                continue;
            }
            for e in evs {
                frontier.push(next(s, e));
            }
        }
        assert!(!seen.contains(&Deep));
    }
}
