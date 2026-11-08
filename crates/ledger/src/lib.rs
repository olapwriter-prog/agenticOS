//! A tamper-evident, append-only ledger. Each line carries the hash of the line before it.
//! Format per line:  seq \t prev_hash \t hash \t payload
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

pub struct Ledger {
    path: PathBuf,
    next_seq: u64,
    last_hash: String,
}

const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn hash_of(seq: u64, prev: &str, payload: &str) -> String {
    let mut h = Sha256::new();
    h.update(seq.to_be_bytes());
    h.update(prev.as_bytes());
    h.update(payload.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

impl Ledger {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut l = Ledger {
            path: path.to_path_buf(),
            next_seq: 0,
            last_hash: GENESIS.into(),
        };
        if path.exists() {
            l.verify()?; // refuse to open a ledger that does not verify
            let f = std::fs::File::open(path)?;
            for line in io::BufReader::new(f).lines() {
                let line = line?;
                let p: Vec<&str> = line.splitn(4, '\t').collect();
                l.next_seq = p[0].parse::<u64>().unwrap() + 1;
                l.last_hash = p[2].to_string();
            }
        }
        Ok(l)
    }

    pub fn append(&mut self, payload: &str) -> io::Result<u64> {
        if payload.contains('\n') || payload.contains('\t') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "payload must not contain tab or newline",
            ));
        }
        let seq = self.next_seq;
        let hash = hash_of(seq, &self.last_hash, payload);
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{seq}\t{}\t{hash}\t{payload}", self.last_hash)?;
        f.sync_all()?;
        self.next_seq += 1;
        self.last_hash = hash;
        Ok(seq)
    }

    /// Re-hash the whole file. Err names the first bad line.
    pub fn verify(&self) -> io::Result<u64> {
        let bad = |n: u64| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("ledger broken at seq {n}"),
            )
        };
        // Nothing written yet is an empty, valid ledger. (A deleted ledger is caught by the anchor.)
        let f = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let mut prev = GENESIS.to_string();
        let mut count = 0u64;
        for line in io::BufReader::new(f).lines() {
            let line = line?;
            let p: Vec<&str> = line.splitn(4, '\t').collect();
            if p.len() != 4 {
                return Err(bad(count));
            }
            let seq: u64 = p[0].parse().map_err(|_| bad(count))?;
            if seq != count || p[1] != prev || hash_of(seq, p[1], p[3]) != p[2] {
                return Err(bad(count));
            }
            prev = p[2].to_string();
            count += 1;
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ledger-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }
    #[test]
    fn append_and_verify() {
        let p = tmp("ok");
        let mut l = Ledger::open(&p).unwrap();
        l.append("stage Asleep->Active").unwrap();
        l.append("approval granted a1").unwrap();
        assert_eq!(l.verify().unwrap(), 2);
        std::fs::remove_file(p).ok();
    }
    #[test]
    fn reopen_continues_the_chain() {
        let p = tmp("reopen");
        {
            let mut l = Ledger::open(&p).unwrap();
            l.append("one").unwrap();
        }
        let mut l = Ledger::open(&p).unwrap();
        assert_eq!(l.append("two").unwrap(), 1);
        assert_eq!(l.verify().unwrap(), 2);
        std::fs::remove_file(p).ok();
    }
    #[test]
    fn tampering_is_detected() {
        let p = tmp("tamper");
        let mut l = Ledger::open(&p).unwrap();
        l.append("pay 10 rupees").unwrap();
        l.append("pay 20 rupees").unwrap();
        let text = std::fs::read_to_string(&p)
            .unwrap()
            .replace("pay 10", "pay 99");
        std::fs::write(&p, text).unwrap();
        let err = l.verify().unwrap_err();
        assert!(err.to_string().contains("seq 0"));
        assert!(Ledger::open(&p).is_err());
        std::fs::remove_file(p).ok();
    }
    #[test]
    fn deleting_a_middle_line_is_detected() {
        let p = tmp("delete");
        let mut l = Ledger::open(&p).unwrap();
        for i in 0..3 {
            l.append(&format!("e{i}")).unwrap();
        }
        let lines: Vec<String> = std::fs::read_to_string(&p)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        std::fs::write(&p, format!("{}\n{}\n", lines[0], lines[2])).unwrap();
        assert!(l.verify().is_err());
        std::fs::remove_file(p).ok();
    }
    #[test]
    fn rejects_newlines_in_payload() {
        let p = tmp("nl");
        let mut l = Ledger::open(&p).unwrap();
        assert!(l.append("a\nb").is_err());
        std::fs::remove_file(p).ok();
    }
}
