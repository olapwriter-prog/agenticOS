use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub version: u64,
    pub size: u64,
    /// lowercase hex sha256 of the payload
    pub sha256: String,
    /// which slot this image is for, "a" or "b"
    pub slot: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Refuse {
    BadManifest,
    BadSignature,
    Rollback { installed: u64, offered: u64 },
    WrongSize,
    WrongHash,
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// Publisher side: sign the manifest bytes. Returns (manifest_json, signature_bytes).
pub fn sign(key: &SigningKey, version: u64, slot: &str, payload: &[u8]) -> (Vec<u8>, [u8; 64]) {
    let m = Manifest {
        version,
        size: payload.len() as u64,
        sha256: sha256_hex(payload),
        slot: slot.into(),
    };
    let json = serde_json::to_vec(&m).expect("manifest serialises");
    let sig = key.sign(&json).to_bytes();
    (json, sig)
}

/// Phone side. The signature is checked over the exact bytes received, before they are parsed.
pub fn verify(
    pubkey: &VerifyingKey,
    installed_version: u64,
    manifest_json: &[u8],
    sig: &[u8],
    payload: &[u8],
) -> Result<Manifest, Refuse> {
    let sig = Signature::from_slice(sig).map_err(|_| Refuse::BadSignature)?;
    pubkey
        .verify(manifest_json, &sig)
        .map_err(|_| Refuse::BadSignature)?;
    let m: Manifest = serde_json::from_slice(manifest_json).map_err(|_| Refuse::BadManifest)?;
    if m.version <= installed_version {
        return Err(Refuse::Rollback {
            installed: installed_version,
            offered: m.version,
        });
    }
    if m.size != payload.len() as u64 {
        return Err(Refuse::WrongSize);
    }
    if m.sha256 != sha256_hex(payload) {
        return Err(Refuse::WrongHash);
    }
    Ok(m)
}

/// A/B slots: the update is written to the inactive slot and only becomes the boot choice after it
/// passes a health check. A failed check leaves the old slot as the choice.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Slot {
    A,
    B,
}
pub struct Slots {
    pub active: Slot,
    pub version_a: u64,
    pub version_b: u64,
}
impl Slots {
    pub fn inactive(&self) -> Slot {
        if self.active == Slot::A {
            Slot::B
        } else {
            Slot::A
        }
    }
    pub fn installed_version(&self) -> u64 {
        if self.active == Slot::A {
            self.version_a
        } else {
            self.version_b
        }
    }
    /// Record a verified image in the inactive slot and switch to it only if `healthy`.
    pub fn apply(&mut self, m: &Manifest, healthy: impl FnOnce() -> bool) -> bool {
        let target = self.inactive();
        let old = match target {
            Slot::A => self.version_a,
            Slot::B => self.version_b,
        };
        match target {
            Slot::A => self.version_a = m.version,
            Slot::B => self.version_b = m.version,
        }
        if healthy() {
            self.active = target;
            true
        } else {
            match target {
                Slot::A => self.version_a = old,
                Slot::B => self.version_b = old,
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }
    const PAYLOAD: &[u8] = b"rootfs image bytes";

    #[test]
    fn a_good_update_verifies() {
        let k = key(1);
        let (m, s) = sign(&k, 5, "b", PAYLOAD);
        let got = verify(&k.verifying_key(), 4, &m, &s, PAYLOAD).unwrap();
        assert_eq!(got.version, 5);
        assert_eq!(got.slot, "b");
    }
    #[test]
    fn a_signature_from_another_key_is_refused() {
        let (m, s) = sign(&key(2), 5, "b", PAYLOAD);
        assert_eq!(
            verify(&key(1).verifying_key(), 4, &m, &s, PAYLOAD),
            Err(Refuse::BadSignature)
        );
    }
    #[test]
    fn a_tampered_manifest_is_refused() {
        let k = key(1);
        let (m, s) = sign(&k, 5, "b", PAYLOAD);
        let tampered = String::from_utf8(m)
            .unwrap()
            .replace("\"version\":5", "\"version\":9")
            .into_bytes();
        assert_eq!(
            verify(&k.verifying_key(), 4, &tampered, &s, PAYLOAD),
            Err(Refuse::BadSignature)
        );
    }
    #[test]
    fn a_short_or_garbled_signature_is_refused() {
        let k = key(1);
        let (m, _) = sign(&k, 5, "b", PAYLOAD);
        assert_eq!(
            verify(&k.verifying_key(), 4, &m, &[0u8; 10], PAYLOAD),
            Err(Refuse::BadSignature)
        );
        assert_eq!(
            verify(&k.verifying_key(), 4, &m, &[0u8; 64], PAYLOAD),
            Err(Refuse::BadSignature)
        );
    }
    #[test]
    fn same_or_older_versions_are_rollbacks() {
        let k = key(1);
        let (m, s) = sign(&k, 5, "b", PAYLOAD);
        assert_eq!(
            verify(&k.verifying_key(), 5, &m, &s, PAYLOAD),
            Err(Refuse::Rollback {
                installed: 5,
                offered: 5
            })
        );
        assert_eq!(
            verify(&k.verifying_key(), 9, &m, &s, PAYLOAD),
            Err(Refuse::Rollback {
                installed: 9,
                offered: 5
            })
        );
    }
    #[test]
    fn a_swapped_payload_is_caught_by_size_or_hash() {
        let k = key(1);
        let (m, s) = sign(&k, 5, "b", PAYLOAD);
        assert_eq!(
            verify(&k.verifying_key(), 4, &m, &s, b"short"),
            Err(Refuse::WrongSize)
        );
        let same_len = vec![b'x'; PAYLOAD.len()];
        assert_eq!(
            verify(&k.verifying_key(), 4, &m, &s, &same_len),
            Err(Refuse::WrongHash)
        );
    }
    #[test]
    fn a_signed_non_manifest_is_a_bad_manifest() {
        let k = key(1);
        let junk = b"not json".to_vec();
        let s = k.sign(&junk).to_bytes();
        assert_eq!(
            verify(&k.verifying_key(), 0, &junk, &s, PAYLOAD),
            Err(Refuse::BadManifest)
        );
    }
    #[test]
    fn a_failed_health_check_keeps_the_old_slot() {
        let k = key(1);
        let (mj, s) = sign(&k, 6, "b", PAYLOAD);
        let m = verify(&k.verifying_key(), 5, &mj, &s, PAYLOAD).unwrap();
        let mut slots = Slots {
            active: Slot::A,
            version_a: 5,
            version_b: 3,
        };
        assert!(!slots.apply(&m, || false));
        assert_eq!(slots.active, Slot::A);
        assert_eq!(slots.version_b, 3, "the failed image is not recorded");
        assert_eq!(slots.installed_version(), 5);
    }
    #[test]
    fn a_healthy_update_switches_slots_and_raises_the_floor() {
        let k = key(1);
        let (mj, s) = sign(&k, 6, "b", PAYLOAD);
        let m = verify(&k.verifying_key(), 5, &mj, &s, PAYLOAD).unwrap();
        let mut slots = Slots {
            active: Slot::A,
            version_a: 5,
            version_b: 3,
        };
        assert!(slots.apply(&m, || true));
        assert_eq!(slots.active, Slot::B);
        assert_eq!(slots.installed_version(), 6);
        assert_eq!(slots.inactive(), Slot::A);
    }
}
