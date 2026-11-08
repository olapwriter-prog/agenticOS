//! The vault: secrets at rest. A passphrase becomes a key with Argon2id; the key seals the whole
//! file with ChaCha20-Poly1305. A wrong passphrase and a tampered file look the same: it will not open.
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"AGV1";
const HEADER_LEN: usize = 4 + 4 + 4 + 4 + 16; // magic, m_cost, t_cost, p_cost, salt

#[derive(Debug, Clone, Copy)]
pub struct KdfParams {
    /// Memory in KiB.
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Default for KdfParams {
    /// 32 MiB, 3 passes. Tune on the phone so unlocking takes about a second.
    fn default() -> Self {
        KdfParams {
            m_cost: 32 * 1024,
            t_cost: 3,
            p_cost: 1,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum VaultError {
    Io(String),
    /// Wrong passphrase, or the file was changed. These cannot be told apart, on purpose.
    WrongPassphraseOrTampered,
    BadFormat,
    Kdf(String),
    Random,
}

impl From<io::Error> for VaultError {
    fn from(e: io::Error) -> Self {
        VaultError::Io(e.to_string())
    }
}

pub struct Vault {
    path: PathBuf,
    params: KdfParams,
    salt: [u8; 16],
    key: [u8; 32],
    items: BTreeMap<String, Vec<u8>>,
}

fn derive(pass: &[u8], salt: &[u8; 16], p: KdfParams) -> Result<[u8; 32], VaultError> {
    let params = Params::new(p.m_cost, p.t_cost, p.p_cost, Some(32))
        .map_err(|e| VaultError::Kdf(e.to_string()))?;
    let mut key = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(pass, salt, &mut key)
        .map_err(|e| VaultError::Kdf(e.to_string()))?;
    Ok(key)
}

fn header(p: KdfParams, salt: &[u8; 16]) -> Vec<u8> {
    let mut h = Vec::with_capacity(HEADER_LEN);
    h.extend_from_slice(MAGIC);
    h.extend_from_slice(&p.m_cost.to_le_bytes());
    h.extend_from_slice(&p.t_cost.to_le_bytes());
    h.extend_from_slice(&p.p_cost.to_le_bytes());
    h.extend_from_slice(salt);
    h
}

fn encode(items: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::new();
    for (k, v) in items {
        out.extend_from_slice(&(k.len() as u32).to_le_bytes());
        out.extend_from_slice(k.as_bytes());
        out.extend_from_slice(&(v.len() as u32).to_le_bytes());
        out.extend_from_slice(v);
    }
    out
}

fn decode(mut b: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, VaultError> {
    let mut m = BTreeMap::new();
    let take = |b: &mut &[u8], n: usize| -> Result<Vec<u8>, VaultError> {
        if b.len() < n {
            return Err(VaultError::BadFormat);
        }
        let (a, rest) = b.split_at(n);
        *b = rest;
        Ok(a.to_vec())
    };
    while !b.is_empty() {
        let kl = u32::from_le_bytes(take(&mut b, 4)?.try_into().unwrap()) as usize;
        let k = String::from_utf8(take(&mut b, kl)?).map_err(|_| VaultError::BadFormat)?;
        let vl = u32::from_le_bytes(take(&mut b, 4)?.try_into().unwrap()) as usize;
        m.insert(k, take(&mut b, vl)?);
    }
    Ok(m)
}

impl Vault {
    /// Make a new, empty vault and write it.
    pub fn create(path: &Path, pass: &str, params: KdfParams) -> Result<Vault, VaultError> {
        let mut salt = [0u8; 16];
        getrandom::getrandom(&mut salt).map_err(|_| VaultError::Random)?;
        let key = derive(pass.as_bytes(), &salt, params)?;
        let v = Vault {
            path: path.to_path_buf(),
            params,
            salt,
            key,
            items: BTreeMap::new(),
        };
        v.save()?;
        Ok(v)
    }

    pub fn open(path: &Path, pass: &str) -> Result<Vault, VaultError> {
        let b = fs::read(path)?;
        if b.len() < HEADER_LEN + 12 + 16 || &b[..4] != MAGIC {
            return Err(VaultError::BadFormat);
        }
        let u = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let params = KdfParams {
            m_cost: u(4),
            t_cost: u(8),
            p_cost: u(12),
        };
        // Refuse absurd parameters from a hostile file before spending memory on them.
        if params.m_cost > 1024 * 1024 || params.t_cost > 20 || params.p_cost > 8 {
            return Err(VaultError::BadFormat);
        }
        let salt: [u8; 16] = b[16..32].try_into().unwrap();
        let nonce = &b[HEADER_LEN..HEADER_LEN + 12];
        let ct = &b[HEADER_LEN + 12..];
        let key = derive(pass.as_bytes(), &salt, params)?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        let plain = cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: ct,
                    aad: &b[..HEADER_LEN],
                },
            )
            .map_err(|_| VaultError::WrongPassphraseOrTampered)?;
        Ok(Vault {
            path: path.to_path_buf(),
            params,
            salt,
            key,
            items: decode(&plain)?,
        })
    }

    fn save(&self) -> Result<(), VaultError> {
        let mut nonce = [0u8; 12];
        getrandom::getrandom(&mut nonce).map_err(|_| VaultError::Random)?; // a fresh nonce on every save
        let h = header(self.params, &self.salt);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.key));
        let ct = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &encode(&self.items),
                    aad: &h,
                },
            )
            .map_err(|_| VaultError::BadFormat)?;
        let mut out = h;
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        // write beside the file, then rename, so a crash never leaves half a vault
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, out)?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn put(&mut self, name: &str, value: &[u8]) -> Result<(), VaultError> {
        self.items.insert(name.to_string(), value.to_vec());
        self.save()
    }
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.items.get(name).map(|v| v.as_slice())
    }
    pub fn delete(&mut self, name: &str) -> Result<bool, VaultError> {
        let had = self.items.remove(name).is_some();
        self.save()?;
        Ok(had)
    }
    pub fn names(&self) -> Vec<&str> {
        self.items.keys().map(|k| k.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cheap parameters so tests run in milliseconds. Never use these on a phone.
    const FAST: KdfParams = KdfParams {
        m_cost: 8,
        t_cost: 1,
        p_cost: 1,
    };

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("vault-{}-{}", tag, std::process::id()));
        let _ = fs::remove_file(&p);
        p
    }

    #[test]
    fn what_goes_in_comes_out_after_reopening() {
        let p = tmp("roundtrip");
        {
            let mut v = Vault::create(&p, "correct horse", FAST).unwrap();
            v.put("gate_secret", b"\x01\x02\x03").unwrap();
            v.put("api_key", b"sk-test-123").unwrap();
        }
        let v = Vault::open(&p, "correct horse").unwrap();
        assert_eq!(v.get("gate_secret"), Some(&b"\x01\x02\x03"[..]));
        assert_eq!(v.get("api_key"), Some(&b"sk-test-123"[..]));
        assert_eq!(v.names(), vec!["api_key", "gate_secret"]);
        fs::remove_file(p).ok();
    }
    #[test]
    fn the_file_does_not_contain_the_secret_in_the_clear() {
        let p = tmp("clear");
        let mut v = Vault::create(&p, "pw", FAST).unwrap();
        v.put("api_key", b"sk-very-secret-value").unwrap();
        let raw = fs::read(&p).unwrap();
        assert!(!raw.windows(14).any(|w| w == b"very-secret-va"));
        assert!(!raw.windows(7).any(|w| w == b"api_key"));
        fs::remove_file(p).ok();
    }
    #[test]
    fn a_wrong_passphrase_does_not_open_it() {
        let p = tmp("wrong");
        Vault::create(&p, "right", FAST).unwrap();
        assert_eq!(
            Vault::open(&p, "wrong").err(),
            Some(VaultError::WrongPassphraseOrTampered)
        );
        fs::remove_file(p).ok();
    }
    #[test]
    fn flipping_any_byte_stops_it_opening() {
        let p = tmp("tamper");
        let mut v = Vault::create(&p, "pw", FAST).unwrap();
        v.put("k", b"value").unwrap();
        let good = fs::read(&p).unwrap();
        for i in [20usize, 33, good.len() - 1] {
            // header salt, nonce, last ciphertext byte
            let mut bad = good.clone();
            bad[i] ^= 1;
            fs::write(&p, &bad).unwrap();
            assert!(Vault::open(&p, "pw").is_err(), "byte {i}");
        }
        fs::remove_file(p).ok();
    }
    #[test]
    fn changing_the_kdf_cost_in_the_header_is_detected() {
        let p = tmp("hdr");
        Vault::create(&p, "pw", FAST).unwrap();
        let mut b = fs::read(&p).unwrap();
        b[8] = 2; // t_cost 1 -> 2
        fs::write(&p, &b).unwrap();
        assert!(Vault::open(&p, "pw").is_err());
        fs::remove_file(p).ok();
    }
    #[test]
    fn a_hostile_file_cannot_demand_gigabytes_of_memory() {
        let p = tmp("dos");
        Vault::create(&p, "pw", FAST).unwrap();
        let mut b = fs::read(&p).unwrap();
        b[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        fs::write(&p, &b).unwrap();
        assert_eq!(Vault::open(&p, "pw").err(), Some(VaultError::BadFormat));
        fs::remove_file(p).ok();
    }
    #[test]
    fn every_save_uses_a_new_nonce() {
        let p = tmp("nonce");
        let mut v = Vault::create(&p, "pw", FAST).unwrap();
        v.put("a", b"1").unwrap();
        let n1 = fs::read(&p).unwrap()[HEADER_LEN..HEADER_LEN + 12].to_vec();
        v.put("a", b"1").unwrap(); // same content again
        let n2 = fs::read(&p).unwrap()[HEADER_LEN..HEADER_LEN + 12].to_vec();
        assert_ne!(n1, n2);
        fs::remove_file(p).ok();
    }
    #[test]
    fn delete_removes_and_reports() {
        let p = tmp("del");
        let mut v = Vault::create(&p, "pw", FAST).unwrap();
        v.put("a", b"1").unwrap();
        assert!(v.delete("a").unwrap());
        assert!(!v.delete("a").unwrap());
        assert!(Vault::open(&p, "pw").unwrap().get("a").is_none());
        fs::remove_file(p).ok();
    }
    #[test]
    fn a_short_or_foreign_file_is_a_bad_format() {
        let p = tmp("junk");
        fs::write(&p, b"hello").unwrap();
        assert_eq!(Vault::open(&p, "pw").err(), Some(VaultError::BadFormat));
        fs::remove_file(p).ok();
    }
    #[test]
    fn two_vaults_with_the_same_passphrase_have_different_keys() {
        let (p1, p2) = (tmp("s1"), tmp("s2"));
        Vault::create(&p1, "same", FAST).unwrap();
        Vault::create(&p2, "same", FAST).unwrap();
        assert_ne!(
            fs::read(&p1).unwrap()[16..32],
            fs::read(&p2).unwrap()[16..32]
        );
        fs::remove_file(p1).ok();
        fs::remove_file(p2).ok();
    }
}
