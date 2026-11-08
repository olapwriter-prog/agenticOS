//! The interruption policy. The phone has no screen to buzz, so "interrupting" means
//! sending the human a message in Claude. Most things should wait for a digest.
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Low,
    Normal,
    Urgent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub app: String,
    pub sender: String,
    pub text: String,
    pub priority: Priority,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Tell the human now.
    Deliver,
    /// Hold it for the next digest.
    Hold,
}

pub struct Policy {
    /// Quiet hours as minutes since midnight. `start` may be larger than `end` (wraps midnight).
    pub quiet_start: u32,
    pub quiet_end: u32,
    /// How many non-urgent interruptions per day before everything waits for the digest.
    pub daily_budget: u32,
    /// Senders whose messages are always urgent, even in quiet hours.
    pub vip: HashSet<String>,
}

pub struct Notifier {
    policy: Policy,
    delivered_today: u32,
    held: Vec<Note>,
}

fn in_quiet(minute_of_day: u32, start: u32, end: u32) -> bool {
    if start == end {
        false
    } else if start < end {
        minute_of_day >= start && minute_of_day < end
    } else {
        minute_of_day >= start || minute_of_day < end
    }
}

impl Notifier {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            delivered_today: 0,
            held: vec![],
        }
    }

    /// Decide what to do with one note at this time of day. A held note is remembered.
    pub fn offer(&mut self, note: Note, minute_of_day: u32) -> Decision {
        let urgent = note.priority == Priority::Urgent || self.policy.vip.contains(&note.sender);
        let quiet = in_quiet(
            minute_of_day,
            self.policy.quiet_start,
            self.policy.quiet_end,
        );
        let decision = if urgent {
            Decision::Deliver
        } else if quiet
            || note.priority == Priority::Low
            || self.delivered_today >= self.policy.daily_budget
        {
            Decision::Hold
        } else {
            Decision::Deliver
        };
        match decision {
            Decision::Deliver => {
                if !urgent {
                    self.delivered_today += 1;
                }
            }
            Decision::Hold => self.held.push(note),
        }
        decision
    }

    pub fn held_count(&self) -> usize {
        self.held.len()
    }

    /// Take everything held and group it by app, in arrival order within each app.
    pub fn digest(&mut self) -> Vec<(String, Vec<Note>)> {
        let mut by_app: BTreeMap<String, Vec<Note>> = BTreeMap::new();
        for n in self.held.drain(..) {
            by_app.entry(n.app.clone()).or_default().push(n);
        }
        by_app.into_iter().collect()
    }

    /// Call at the start of each day.
    pub fn new_day(&mut self) {
        self.delivered_today = 0;
    }
}

/// Plain-text digest, as Claude would say it.
pub fn digest_text(groups: &[(String, Vec<Note>)]) -> String {
    if groups.is_empty() {
        return "Nothing new while you were away.".to_string();
    }
    let mut out = String::new();
    for (app, notes) in groups {
        out.push_str(&format!("{app} ({}):\n", notes.len()));
        for n in notes {
            out.push_str(&format!("  {}: {}\n", n.sender, n.text));
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            quiet_start: 22 * 60,
            quiet_end: 7 * 60,
            daily_budget: 2,
            vip: ["Mum".to_string()].into_iter().collect(),
        }
    }
    fn note(app: &str, sender: &str, p: Priority) -> Note {
        Note {
            app: app.into(),
            sender: sender.into(),
            text: format!("hello from {sender}"),
            priority: p,
        }
    }
    const NOON: u32 = 12 * 60;
    const MIDNIGHT: u32 = 0;

    #[test]
    fn a_normal_note_in_the_day_is_delivered() {
        let mut n = Notifier::new(policy());
        assert_eq!(
            n.offer(note("chat", "Ravi", Priority::Normal), NOON),
            Decision::Deliver
        );
    }
    #[test]
    fn quiet_hours_wrap_midnight() {
        let mut n = Notifier::new(policy());
        assert_eq!(
            n.offer(note("chat", "Ravi", Priority::Normal), 23 * 60),
            Decision::Hold
        );
        assert_eq!(
            n.offer(note("chat", "Ravi", Priority::Normal), MIDNIGHT),
            Decision::Hold
        );
        assert_eq!(
            n.offer(note("chat", "Ravi", Priority::Normal), 6 * 60 + 59),
            Decision::Hold
        );
        assert_eq!(
            n.offer(note("chat", "Ravi", Priority::Normal), 7 * 60),
            Decision::Deliver
        );
    }
    #[test]
    fn urgent_and_vip_break_through_quiet_hours() {
        let mut n = Notifier::new(policy());
        assert_eq!(
            n.offer(note("chat", "Ravi", Priority::Urgent), MIDNIGHT),
            Decision::Deliver
        );
        assert_eq!(
            n.offer(note("chat", "Mum", Priority::Normal), MIDNIGHT),
            Decision::Deliver
        );
    }
    #[test]
    fn the_daily_budget_runs_out_but_urgent_still_gets_through() {
        let mut n = Notifier::new(policy());
        assert_eq!(
            n.offer(note("a", "x", Priority::Normal), NOON),
            Decision::Deliver
        );
        assert_eq!(
            n.offer(note("a", "y", Priority::Normal), NOON),
            Decision::Deliver
        );
        assert_eq!(
            n.offer(note("a", "z", Priority::Normal), NOON),
            Decision::Hold
        );
        assert_eq!(
            n.offer(note("a", "w", Priority::Urgent), NOON),
            Decision::Deliver
        );
    }
    #[test]
    fn urgent_notes_do_not_spend_the_budget() {
        let mut n = Notifier::new(policy());
        for _ in 0..5 {
            n.offer(note("a", "x", Priority::Urgent), NOON);
        }
        assert_eq!(
            n.offer(note("a", "y", Priority::Normal), NOON),
            Decision::Deliver
        );
    }
    #[test]
    fn low_priority_always_waits() {
        let mut n = Notifier::new(policy());
        assert_eq!(
            n.offer(note("news", "feed", Priority::Low), NOON),
            Decision::Hold
        );
    }
    #[test]
    fn new_day_refills_the_budget() {
        let mut n = Notifier::new(policy());
        n.offer(note("a", "x", Priority::Normal), NOON);
        n.offer(note("a", "y", Priority::Normal), NOON);
        n.new_day();
        assert_eq!(
            n.offer(note("a", "z", Priority::Normal), NOON),
            Decision::Deliver
        );
    }
    #[test]
    fn digest_groups_by_app_and_empties_the_queue() {
        let mut n = Notifier::new(policy());
        n.offer(note("chat", "Ravi", Priority::Normal), MIDNIGHT);
        n.offer(note("mail", "Bank", Priority::Normal), MIDNIGHT);
        n.offer(note("chat", "Asha", Priority::Normal), MIDNIGHT);
        assert_eq!(n.held_count(), 3);
        let d = n.digest();
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].0, "chat");
        assert_eq!(d[0].1.len(), 2);
        assert_eq!(d[0].1[0].sender, "Ravi");
        assert_eq!(n.held_count(), 0);
    }
    #[test]
    fn digest_text_reads_naturally() {
        let mut n = Notifier::new(policy());
        n.offer(note("chat", "Ravi", Priority::Normal), MIDNIGHT);
        let t = digest_text(&n.digest());
        assert_eq!(t, "chat (1):\n  Ravi: hello from Ravi");
        assert_eq!(digest_text(&[]), "Nothing new while you were away.");
    }
    #[test]
    fn equal_start_and_end_means_no_quiet_hours() {
        let mut p = policy();
        p.quiet_start = 0;
        p.quiet_end = 0;
        let mut n = Notifier::new(p);
        assert_eq!(
            n.offer(note("a", "x", Priority::Normal), 3 * 60),
            Decision::Deliver
        );
    }
}
