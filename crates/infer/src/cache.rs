//! The prompt-state cache: model states saved after shared prefixes (the system prompt and tool
//! list, one per adapter), so a request that starts the same way skips reading them again.
//!
//! Memory is the cost: a 4B model holds roughly 150 KB per token, so a 2,000-token prefix is about
//! 300 MB. The cache therefore has a byte budget and evicts by value. The value of an entry is a
//! priority set from outside (agentd's cache manager predicts which prefixes will be used next),
//! with recency and past hits as tie-breakers, per byte. Entries the manager no longer wants can
//! be dropped outright. Nothing here decides *what* to warm; it only keeps what it is told is
//! worth keeping within the budget.

/// Anything whose memory can be counted.
pub trait Weigh {
    fn bytes(&self) -> usize;
}

#[derive(Debug, Clone)]
pub struct Entry<S> {
    pub id: u64,
    /// the adapter the state was computed with ("" = the base model)
    pub adapter: String,
    pub tokens: Vec<u32>,
    pub label: String,
    pub snap: S,
    pub bytes: usize,
    /// what building it cost, so a hit saves about this much
    pub build_ms: f64,
    pub hits: u32,
    /// the clock (requests seen) when it was last used or made
    pub last_used: u64,
    pub made: u64,
    /// 0..1, how likely it is to be used soon; set by the manager, 0.5 until it says otherwise
    pub priority: f32,
    /// made ahead of time (warmed) rather than by a request
    pub warmed: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub dropped: u64,
    pub warmed: u64,
    /// warmed entries that a request later used
    pub warm_used: u64,
    /// prefill time saved by hits, ms
    pub saved_ms: f64,
    pub peak_bytes: usize,
}

/// A summary of one entry, without the state itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    pub id: u64,
    pub adapter: String,
    pub label: String,
    pub tokens: usize,
    pub bytes: usize,
    pub hits: u32,
    /// requests since it was last used
    pub idle: u64,
    pub priority: f32,
    pub warmed: bool,
    pub build_ms: f64,
}

pub struct PrefixCache<S> {
    entries: Vec<Entry<S>>,
    /// bytes the cache may hold; 0 keeps nothing
    pub budget: usize,
    /// at most this many entries, whatever their size
    pub max_entries: usize,
    clock: u64,
    next_id: u64,
    pub stats: Stats,
}

fn starts_with(a: &[u32], b: &[u32]) -> bool {
    a.len() >= b.len() && a[..b.len()] == *b
}

impl<S: Weigh> PrefixCache<S> {
    pub fn new(budget: usize) -> Self {
        PrefixCache {
            entries: vec![],
            budget,
            max_entries: 8,
            clock: 0,
            next_id: 1,
            stats: Stats::default(),
        }
    }

    /// One request went by: the clock that ages entries.
    pub fn tick(&mut self) {
        self.clock += 1;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn used(&self) -> usize {
        self.entries.iter().map(|e| e.bytes).sum()
    }

    pub fn contains(&self, adapter: &str, tokens: &[u32]) -> bool {
        self.entries
            .iter()
            .any(|e| e.adapter == adapter && e.tokens == tokens)
    }

    /// The longest cached prefix of `prompt` for this adapter that leaves at least one token to
    /// read (the model needs logits for the last one): its id and length. Does not count a hit.
    pub fn best(&self, adapter: &str, prompt: &[u32]) -> Option<(u64, usize)> {
        self.entries
            .iter()
            .filter(|e| {
                e.adapter == adapter
                    && e.tokens.len() < prompt.len()
                    && starts_with(prompt, &e.tokens)
            })
            .max_by_key(|e| e.tokens.len())
            .map(|e| (e.id, e.tokens.len()))
    }

    /// Use an entry: counts the hit and returns the state to restore.
    pub fn hit(&mut self, id: u64) -> Option<&S> {
        let clock = self.clock;
        let e = self.entries.iter_mut().find(|e| e.id == id)?;
        e.hits += 1;
        e.last_used = clock;
        self.stats.hits += 1;
        self.stats.saved_ms += e.build_ms;
        if e.warmed && e.hits == 1 {
            self.stats.warm_used += 1;
        }
        Some(&e.snap)
    }

    pub fn miss(&mut self) {
        self.stats.misses += 1;
    }

    /// Keep a state. Replaces an entry for the same adapter and tokens. Returns its id, or None
    /// when it can never fit (bigger than the whole budget).
    pub fn insert(
        &mut self,
        adapter: &str,
        tokens: Vec<u32>,
        label: &str,
        snap: S,
        build_ms: f64,
        warmed: bool,
    ) -> Option<u64> {
        let bytes = snap.bytes();
        if bytes > self.budget {
            return None;
        }
        self.entries
            .retain(|e| !(e.adapter == adapter && e.tokens == tokens));
        let id = self.next_id;
        self.next_id += 1;
        if warmed {
            self.stats.warmed += 1;
        }
        self.entries.push(Entry {
            id,
            adapter: adapter.to_string(),
            tokens,
            label: label.to_string(),
            snap,
            bytes,
            build_ms,
            hits: 0,
            last_used: self.clock,
            made: self.clock,
            priority: 0.5,
            warmed,
        });
        self.fit(Some(id));
        self.stats.peak_bytes = self.stats.peak_bytes.max(self.used());
        self.entries.iter().any(|e| e.id == id).then_some(id)
    }

    /// What an entry is worth keeping, per megabyte: the manager's priority first, then whether
    /// it has been used lately and how often, scaled by what rebuilding it would cost.
    fn value(&self, e: &Entry<S>) -> f64 {
        let idle = self.clock.saturating_sub(e.last_used) as f64;
        let recency = 1.0 / (1.0 + idle / 4.0);
        let used = (e.hits.min(10) as f64) / 10.0;
        let worth = 0.6 * e.priority as f64 + 0.25 * recency + 0.15 * used;
        let cost = 1.0 + (e.build_ms / 1000.0).ln_1p();
        worth * cost / (1.0 + e.bytes as f64 / (1 << 20) as f64).sqrt()
    }

    /// Evict the least valuable entries until the cache is within budget and entry count.
    /// `keep` is spared if anything else can go.
    fn fit(&mut self, keep: Option<u64>) {
        while !self.entries.is_empty()
            && (self.used() > self.budget || self.entries.len() > self.max_entries)
        {
            let victim = self
                .entries
                .iter()
                .filter(|e| Some(e.id) != keep || self.entries.len() == 1)
                .min_by(|a, b| {
                    self.value(a)
                        .partial_cmp(&self.value(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|e| e.id);
            let Some(v) = victim else { break };
            self.entries.retain(|e| e.id != v);
            self.stats.evictions += 1;
        }
    }

    /// A new budget: shrinking evicts at once.
    pub fn set_budget(&mut self, bytes: usize) {
        self.budget = bytes;
        self.fit(None);
    }

    pub fn set_priority(&mut self, id: u64, p: f32) -> bool {
        match self.entries.iter_mut().find(|e| e.id == id) {
            Some(e) => {
                e.priority = p.clamp(0.0, 1.0);
                true
            }
            None => false,
        }
    }

    pub fn drop_entry(&mut self, id: u64) -> bool {
        let n = self.entries.len();
        self.entries.retain(|e| e.id != id);
        let gone = n != self.entries.len();
        if gone {
            self.stats.dropped += 1;
        }
        gone
    }

    /// Forget every state computed with an adapter (it changed, or is no longer served).
    pub fn drop_adapter(&mut self, adapter: &str) -> usize {
        let n = self.entries.len();
        self.entries.retain(|e| e.adapter != adapter);
        let gone = n - self.entries.len();
        self.stats.dropped += gone as u64;
        gone
    }

    pub fn clear(&mut self) {
        self.stats.dropped += self.entries.len() as u64;
        self.entries.clear();
    }

    pub fn infos(&self) -> Vec<Info> {
        self.entries
            .iter()
            .map(|e| Info {
                id: e.id,
                adapter: e.adapter.clone(),
                label: e.label.clone(),
                tokens: e.tokens.len(),
                bytes: e.bytes,
                hits: e.hits,
                idle: self.clock.saturating_sub(e.last_used),
                priority: e.priority,
                warmed: e.warmed,
                build_ms: e.build_ms,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Blob(usize);
    impl Weigh for Blob {
        fn bytes(&self) -> usize {
            self.0
        }
    }

    const MB: usize = 1 << 20;

    #[test]
    fn the_longest_prefix_for_the_same_adapter_wins_and_counts_a_hit() {
        let mut c = PrefixCache::new(100 * MB);
        let a = c
            .insert("", vec![1, 2], "short", Blob(MB), 50.0, false)
            .unwrap();
        let b = c
            .insert("", vec![1, 2, 3], "long", Blob(MB), 80.0, false)
            .unwrap();
        c.insert(
            "messages",
            vec![1, 2, 3, 4],
            "expert",
            Blob(MB),
            80.0,
            false,
        );
        assert_eq!(c.best("", &[1, 2, 3, 4, 5]), Some((b, 3)));
        // a prefix must leave a token to read
        assert_eq!(c.best("", &[1, 2, 3]), Some((a, 2)));
        assert_eq!(c.best("", &[9, 9, 9]), None);
        assert_eq!(c.best("messages", &[1, 2, 3, 4, 5]).map(|x| x.1), Some(4));
        assert!(c.hit(b).is_some());
        assert_eq!((c.stats.hits, c.stats.saved_ms), (1, 80.0));
    }

    #[test]
    fn it_stays_within_the_budget_and_evicts_what_the_manager_values_least() {
        let mut c = PrefixCache::new(250 * MB);
        let sys = c
            .insert("", vec![1], "system", Blob(100 * MB), 4000.0, false)
            .unwrap();
        let money = c
            .insert("money", vec![1], "money", Blob(100 * MB), 4000.0, true)
            .unwrap();
        c.set_priority(sys, 0.9);
        c.set_priority(money, 0.05);
        // a third does not fit: the low-priority one goes, not the system prompt
        let msg = c
            .insert(
                "messages",
                vec![1],
                "messages",
                Blob(100 * MB),
                4000.0,
                true,
            )
            .unwrap();
        assert!(c.used() <= 250 * MB);
        let ids: Vec<u64> = c.infos().iter().map(|i| i.id).collect();
        assert!(ids.contains(&sys) && ids.contains(&msg) && !ids.contains(&money));
        assert_eq!(c.stats.evictions, 1);
        // shrinking the budget (low battery, memory pressure) evicts at once
        c.set_budget(120 * MB);
        assert_eq!(c.infos().len(), 1);
        assert_eq!(c.infos()[0].id, sys);
        // something bigger than the whole budget is never kept
        assert!(c
            .insert("", vec![2], "huge", Blob(500 * MB), 1.0, false)
            .is_none());
    }

    #[test]
    fn idle_unused_entries_lose_to_fresh_used_ones() {
        let mut c = PrefixCache::new(150 * MB);
        let old = c
            .insert("", vec![1], "old", Blob(70 * MB), 1000.0, false)
            .unwrap();
        let used = c
            .insert("", vec![2], "used", Blob(70 * MB), 1000.0, false)
            .unwrap();
        for _ in 0..20 {
            c.tick();
            c.hit(used);
        }
        c.insert("", vec![3], "new", Blob(70 * MB), 1000.0, false);
        let ids: Vec<u64> = c.infos().iter().map(|i| i.id).collect();
        assert!(!ids.contains(&old) && ids.contains(&used));
        assert_eq!(c.infos().iter().find(|i| i.id == used).unwrap().hits, 20);
    }

    #[test]
    fn warmed_entries_count_when_first_used_and_drops_are_counted() {
        let mut c = PrefixCache::new(100 * MB);
        let w = c
            .insert("phone", vec![1, 2], "phone", Blob(MB), 10.0, true)
            .unwrap();
        c.hit(w);
        c.hit(w);
        assert_eq!((c.stats.warmed, c.stats.warm_used), (1, 1));
        c.insert("phone", vec![3], "phone", Blob(MB), 10.0, false);
        assert_eq!(c.drop_adapter("phone"), 2);
        assert!(c.is_empty());
        c.max_entries = 2;
        for i in 0..5 {
            c.insert("", vec![i], "x", Blob(1), 1.0, false);
        }
        assert_eq!(c.len(), 2);
    }
}
