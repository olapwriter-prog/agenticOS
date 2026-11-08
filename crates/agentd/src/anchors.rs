//! Ledger anchors, wired to the real ledger file. Saves a keyed checkpoint now and then and
//! checks it at boot, so a rewritten or truncated ledger stops the daemon instead of passing.
use agentos_update::anchor::{check, make, Anchor, Verdict};
use serde_json::{json, Value};
use std::path::Path;

const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Hashes of a ledger file: element i is the head after i entries (element 0 is the genesis hash).
pub fn heads(ledger: &Path) -> Vec<String> {
    let mut v = vec![GENESIS.to_string()];
    if let Ok(t) = std::fs::read_to_string(ledger) {
        for line in t.lines() {
            if let Some(h) = line.split('\t').nth(2) {
                v.push(h.to_string());
            }
        }
    }
    v
}

pub fn anchor_path(dir: &Path) -> std::path::PathBuf {
    dir.join("anchor.json")
}

/// Save an anchor for the ledger as it is now.
pub fn save(dir: &Path, ledger: &Path, key: &[u8], now_secs: u64) -> std::io::Result<Anchor> {
    let h = heads(ledger);
    let count = (h.len() - 1) as u64;
    let a = make(key, &h[count as usize], count, now_secs);
    let j = json!({"head": a.head, "count": a.count, "at": a.at, "mac": a.mac});
    let tmp = dir.join("anchor.json.tmp");
    std::fs::write(&tmp, j.to_string())?;
    std::fs::rename(tmp, anchor_path(dir))?;
    Ok(a)
}

pub fn load(dir: &Path) -> Option<Anchor> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(anchor_path(dir)).ok()?).ok()?;
    Some(Anchor {
        head: v["head"].as_str()?.to_string(),
        count: v["count"].as_u64()?,
        at: v["at"].as_u64()?,
        mac: v["mac"].as_str()?.to_string(),
    })
}

/// Check the saved anchor (if any) against the ledger. No anchor yet is fine on a first boot.
pub fn verify(dir: &Path, ledger: &Path, key: &[u8]) -> Verdict {
    let Some(a) = load(dir) else {
        return Verdict::Ok;
    };
    let h = heads(ledger);
    check(key, &a, (h.len() - 1) as u64, |n| {
        h.get(n as usize).cloned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_ledger::Ledger;
    fn rig(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let d = std::env::temp_dir().join(format!("agentd-anc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let l = d.join("ledger.log");
        let mut ledger = Ledger::open(&l).unwrap();
        for i in 0..5 {
            ledger.append(&format!("event {i}")).unwrap();
        }
        (d, l)
    }

    #[test]
    fn heads_has_one_more_entry_than_the_ledger() {
        let (d, l) = rig("h");
        assert_eq!(heads(&l).len(), 6);
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn a_saved_anchor_verifies_and_a_grown_ledger_still_passes() {
        let (d, l) = rig("g");
        save(&d, &l, b"k", 100).unwrap();
        assert_eq!(verify(&d, &l, b"k"), Verdict::Ok);
        Ledger::open(&l).unwrap().append("more").unwrap();
        assert_eq!(verify(&d, &l, b"k"), Verdict::Ok);
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn a_truncated_ledger_is_noticed() {
        let (d, l) = rig("t");
        save(&d, &l, b"k", 100).unwrap();
        let t = std::fs::read_to_string(&l).unwrap();
        let kept: Vec<&str> = t.lines().take(3).collect();
        std::fs::write(&l, kept.join("\n") + "\n").unwrap();
        assert_eq!(verify(&d, &l, b"k"), Verdict::Shrunk);
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn a_fully_rechained_forgery_is_noticed() {
        // The attacker rewrites history and recomputes every hash, so the chain verifies on its own.
        let (d, l) = rig("r");
        save(&d, &l, b"k", 100).unwrap();
        std::fs::remove_file(&l).unwrap();
        let mut forged = Ledger::open(&l).unwrap();
        for i in 0..5 {
            forged.append(&format!("harmless {i}")).unwrap();
        }
        assert!(
            Ledger::open(&l).unwrap().verify().is_ok(),
            "the forged chain is internally valid"
        );
        assert_eq!(verify(&d, &l, b"k"), Verdict::Rewritten);
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn no_anchor_yet_is_fine() {
        let (d, l) = rig("n");
        assert_eq!(verify(&d, &l, b"k"), Verdict::Ok);
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn the_wrong_key_fails_the_mac() {
        let (d, l) = rig("w");
        save(&d, &l, b"k", 100).unwrap();
        assert_eq!(verify(&d, &l, b"other"), Verdict::BadMac);
        let _ = std::fs::remove_dir_all(&d);
    }
}
