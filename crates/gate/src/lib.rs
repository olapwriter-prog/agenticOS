//! The Gate: approval tokens. A token is signed, bound to the exact action (by hash),
//! expires after a short time, and can be used once.
use agentos_hal::Clock;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::Arc;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, PartialEq, Eq)]
pub enum GateError {
    Malformed,
    BadSignature,
    Expired,
    AlreadyUsed,
}

pub struct Gate {
    secret: Vec<u8>,
    clock: Arc<dyn Clock>,
    ttl_ms: u64,
    used: HashSet<u64>,
    next_id: u64,
}

pub fn action_hash(action: &str) -> String {
    Sha256::digest(action.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

impl Gate {
    pub fn new(secret: &[u8], clock: Arc<dyn Clock>, ttl_ms: u64) -> Self {
        Self {
            secret: secret.to_vec(),
            clock,
            ttl_ms,
            used: HashSet::new(),
            next_id: 1,
        }
    }

    fn mac(&self, id: u64, expiry: u64, hash: &str) -> HmacSha256 {
        let mut m = HmacSha256::new_from_slice(&self.secret).expect("any key length works");
        m.update(format!("{id}|{expiry}|{hash}").as_bytes());
        m
    }

    /// Called only after the human approved this exact action.
    pub fn mint(&mut self, action: &str) -> String {
        let id = self.next_id;
        self.next_id += 1;
        let expiry = self.clock.now_ms() + self.ttl_ms;
        let hash = action_hash(action);
        let sig = self.mac(id, expiry, &hash).finalize().into_bytes();
        format!("{id}.{expiry}.{}", hex(&sig))
    }

    /// Checks the token against the action about to run, then burns it.
    pub fn redeem(&mut self, token: &str, action: &str) -> Result<(), GateError> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Err(GateError::Malformed);
        }
        let id: u64 = parts[0].parse().map_err(|_| GateError::Malformed)?;
        let expiry: u64 = parts[1].parse().map_err(|_| GateError::Malformed)?;
        let sig = unhex(parts[2]).ok_or(GateError::Malformed)?;
        let hash = action_hash(action);
        // The signature is checked against the action actually about to run, so a token
        // minted for a different action fails here. verify_slice compares in constant time.
        if self.mac(id, expiry, &hash).verify_slice(&sig).is_err() {
            return Err(GateError::BadSignature);
        }
        if self.clock.now_ms() > expiry {
            return Err(GateError::Expired);
        }
        if !self.used.insert(id) {
            return Err(GateError::AlreadyUsed);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_hal::FakeClock;

    fn gate() -> (FakeClock, Gate) {
        let c = FakeClock::new();
        (
            c.clone(),
            Gate::new(b"test-secret", Arc::new(c), 10 * 60_000),
        )
    }
    const PAY: &str = r#"{"tool":"pay","args":{"to":"asha","amount":"500"}}"#;

    #[test]
    fn a_fresh_token_works_once() {
        let (_c, mut g) = gate();
        let t = g.mint(PAY);
        assert_eq!(g.redeem(&t, PAY), Ok(()));
        assert_eq!(g.redeem(&t, PAY), Err(GateError::AlreadyUsed));
    }
    #[test]
    fn a_token_cannot_approve_a_different_action() {
        let (_c, mut g) = gate();
        let t = g.mint(PAY);
        let other = r#"{"tool":"pay","args":{"to":"asha","amount":"50000"}}"#;
        assert_eq!(g.redeem(&t, other), Err(GateError::BadSignature));
        // and the real action still works, because the failed attempt did not burn it
        assert_eq!(g.redeem(&t, PAY), Ok(()));
    }
    #[test]
    fn tokens_expire() {
        let (c, mut g) = gate();
        let t = g.mint(PAY);
        c.advance_ms(10 * 60_000 + 1);
        assert_eq!(g.redeem(&t, PAY), Err(GateError::Expired));
    }
    #[test]
    fn a_token_just_before_expiry_still_works() {
        let (c, mut g) = gate();
        let t = g.mint(PAY);
        c.advance_ms(10 * 60_000);
        assert_eq!(g.redeem(&t, PAY), Ok(()));
    }
    #[test]
    fn forged_expiry_is_rejected() {
        let (c, mut g) = gate();
        let t = g.mint(PAY);
        c.advance_ms(10 * 60_000 + 5);
        let mut p: Vec<String> = t.split('.').map(String::from).collect();
        p[1] = "99999999999".into();
        assert_eq!(g.redeem(&p.join("."), PAY), Err(GateError::BadSignature));
    }
    #[test]
    fn a_token_from_another_phone_is_rejected() {
        let (c, mut g) = gate();
        let mut other = Gate::new(b"someone-elses-secret", Arc::new(c), 60_000);
        let t = other.mint(PAY);
        assert_eq!(g.redeem(&t, PAY), Err(GateError::BadSignature));
    }
    #[test]
    fn garbage_is_malformed() {
        let (_c, mut g) = gate();
        assert_eq!(g.redeem("hello", PAY), Err(GateError::Malformed));
        assert_eq!(g.redeem("1.2.zz", PAY), Err(GateError::Malformed));
    }
}
