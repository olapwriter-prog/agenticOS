//! A keyed checkpoint of the ledger. The phone saves an anchor now and then (and sends a copy off
//! the device). Later, the current ledger head and store count are checked against it: the count
//! may only grow, and the hash recorded at that count must still match.
use hmac::{Hmac, Mac};
use sha2::Sha256;

type H = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    /// ledger head hash (hex) at the time
    pub head: String,
    /// number of ledger entries at the time
    pub count: u64,
    /// unix seconds
    pub at: u64,
    pub mac: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    BadMac,
    /// fewer entries than the anchor recorded: entries were removed
    Shrunk,
    /// same count but a different head: entries were rewritten
    Rewritten,
}

fn mac_of(key: &[u8], head: &str, count: u64, at: u64) -> String {
    let mut m = H::new_from_slice(key).expect("hmac takes any key length");
    m.update(head.as_bytes());
    m.update(&[0]);
    m.update(&count.to_be_bytes());
    m.update(&at.to_be_bytes());
    crate::manifest::hex(&m.finalize().into_bytes())
}

pub fn make(key: &[u8], head: &str, count: u64, at: u64) -> Anchor {
    Anchor {
        head: head.into(),
        count,
        at,
        mac: mac_of(key, head, count, at),
    }
}

/// Check an anchor against the ledger as it is now. `head_at(count)` returns the hash the current
/// ledger has at that entry count, or None if it has fewer entries.
pub fn check(
    key: &[u8],
    a: &Anchor,
    current_count: u64,
    head_at: impl Fn(u64) -> Option<String>,
) -> Verdict {
    let want = mac_of(key, &a.head, a.count, a.at);
    // compare without early exit
    let ok = want.len() == a.mac.len()
        && want
            .bytes()
            .zip(a.mac.bytes())
            .fold(0u8, |d, (x, y)| d | (x ^ y))
            == 0;
    if !ok {
        return Verdict::BadMac;
    }
    if current_count < a.count {
        return Verdict::Shrunk;
    }
    match head_at(a.count) {
        Some(h) if h == a.head => Verdict::Ok,
        _ => Verdict::Rewritten,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const KEY: &[u8] = b"anchor-key";
    fn chain(n: u64) -> Vec<String> {
        (0..=n).map(|i| format!("h{i}")).collect()
    }

    #[test]
    fn an_untouched_ledger_that_grew_passes() {
        let c = chain(10);
        let a = make(KEY, &c[5], 5, 1000);
        assert_eq!(
            check(KEY, &a, 10, |n| c.get(n as usize).cloned()),
            Verdict::Ok
        );
    }
    #[test]
    fn removed_entries_are_noticed() {
        let c = chain(10);
        let a = make(KEY, &c[8], 8, 1000);
        assert_eq!(
            check(KEY, &a, 6, |n| c.get(n as usize).cloned()),
            Verdict::Shrunk
        );
    }
    #[test]
    fn rewritten_history_is_noticed_even_if_the_chain_is_internally_consistent() {
        let a = make(KEY, "h5", 5, 1000);
        let forged: Vec<String> = (0..=10).map(|i| format!("forged{i}")).collect();
        assert_eq!(
            check(KEY, &a, 10, |n| forged.get(n as usize).cloned()),
            Verdict::Rewritten
        );
    }
    #[test]
    fn an_edited_anchor_fails_its_mac() {
        let c = chain(10);
        let mut a = make(KEY, &c[5], 5, 1000);
        a.count = 3;
        assert_eq!(
            check(KEY, &a, 10, |n| c.get(n as usize).cloned()),
            Verdict::BadMac
        );
        let mut b = make(KEY, &c[5], 5, 1000);
        b.head = "h99".into();
        assert_eq!(check(KEY, &b, 10, |_| None), Verdict::BadMac);
    }
    #[test]
    fn an_attacker_without_the_key_cannot_forge_a_new_anchor() {
        let forged = make(b"guess", "forged5", 5, 1000);
        assert_eq!(
            check(KEY, &forged, 10, |_| Some("forged5".into())),
            Verdict::BadMac
        );
    }
    #[test]
    fn the_time_is_covered_by_the_mac() {
        let mut a = make(KEY, "h1", 1, 1000);
        a.at = 2000;
        assert_eq!(check(KEY, &a, 1, |_| Some("h1".into())), Verdict::BadMac);
    }
}
