//! Boot secrets, kept in the vault. First run creates them; later runs read them back.
//! This closes the Volume 3 and Volume 7 open items "the Gate secret is a plain string" and
//! "the anchor key must come from the vault".
use agentos_vault::{KdfParams, Vault};
use std::path::Path;

pub struct Secrets {
    pub gateway_token: String,
    pub gate_secret: Vec<u8>,
    pub anchor_key: Vec<u8>,
    pub commercial_key: Option<String>,
}

fn random(n: usize) -> Result<Vec<u8>, String> {
    let mut b = vec![0u8; n];
    getrandom::getrandom(&mut b).map_err(|e| e.to_string())?;
    Ok(b)
}
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn vault_path(dir: &Path) -> std::path::PathBuf {
    dir.join("vault.bin")
}

/// Open the vault, creating it and the three secrets on first run.
pub fn open_or_create(dir: &Path, pass: &str, kdf_mib: u32) -> Result<Secrets, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = vault_path(dir);
    let mut v = if path.exists() {
        Vault::open(&path, pass).map_err(|e| format!("cannot open vault: {e:?}"))?
    } else {
        let params = KdfParams {
            m_cost: kdf_mib.max(1) * 1024,
            t_cost: 3,
            p_cost: 1,
        };
        Vault::create(&path, pass, params).map_err(|e| format!("cannot create vault: {e:?}"))?
    };
    for (name, len) in [
        ("gateway_token", 32),
        ("gate_secret", 32),
        ("anchor_key", 32),
    ] {
        if v.get(name).is_none() {
            let val = random(len)?;
            let stored = if name == "gateway_token" {
                hex(&val).into_bytes()
            } else {
                val
            };
            v.put(name, &stored).map_err(|e| format!("{e:?}"))?;
        }
    }
    Ok(Secrets {
        gateway_token: String::from_utf8(v.get("gateway_token").unwrap().to_vec())
            .map_err(|e| e.to_string())?,
        gate_secret: v.get("gate_secret").unwrap().to_vec(),
        anchor_key: v.get("anchor_key").unwrap().to_vec(),
        commercial_key: v
            .get("commercial_key")
            .and_then(|b| String::from_utf8(b.to_vec()).ok()),
    })
}

/// Store the commercial API key. The key never goes in the config file.
pub fn set_commercial_key(dir: &Path, pass: &str, key: &str) -> Result<(), String> {
    let mut v =
        Vault::open(&vault_path(dir), pass).map_err(|e| format!("cannot open vault: {e:?}"))?;
    v.put("commercial_key", key.as_bytes())
        .map_err(|e| format!("{e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("agentd-sec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }
    #[test]
    fn first_run_creates_and_second_run_reads_the_same_secrets() {
        let d = tmp("a");
        let a = open_or_create(&d, "pw", 1).unwrap();
        let b = open_or_create(&d, "pw", 1).unwrap();
        assert_eq!(a.gateway_token, b.gateway_token);
        assert_eq!(a.gate_secret, b.gate_secret);
        assert_eq!(a.gateway_token.len(), 64);
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn the_token_is_not_in_the_vault_file_in_the_clear() {
        let d = tmp("b");
        let a = open_or_create(&d, "pw", 1).unwrap();
        let raw = std::fs::read(vault_path(&d)).unwrap();
        assert!(!raw
            .windows(a.gateway_token.len())
            .any(|w| w == a.gateway_token.as_bytes()));
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn a_wrong_passphrase_is_refused() {
        let d = tmp("c");
        open_or_create(&d, "pw", 1).unwrap();
        assert!(open_or_create(&d, "nope", 1).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn the_commercial_key_round_trips() {
        let d = tmp("d");
        open_or_create(&d, "pw", 1).unwrap();
        set_commercial_key(&d, "pw", "sk-test").unwrap();
        assert_eq!(
            open_or_create(&d, "pw", 1)
                .unwrap()
                .commercial_key
                .as_deref(),
            Some("sk-test")
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
