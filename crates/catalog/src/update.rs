//! A catalogue from the network, only if it is signed by the owner's key and newer than the one
//! installed. The signature is checked over the exact bytes received, before anything is parsed.
use agentos_update::manifest::{verify, Manifest};
use ed25519_dalek::VerifyingKey;

/// Decode a hex string into bytes; None when it is not even hex.
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    let t = text.trim();
    if !t.len().is_multiple_of(2) {
        return None;
    }
    (0..t.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&t[i..i + 2], 16).ok())
        .collect()
}

/// Check a downloaded catalogue: the signature (hex), then the manifest's version, size and hash.
/// Returns the manifest when the catalogue may be saved.
pub fn check(
    pubkey: &VerifyingKey,
    installed_version: u64,
    manifest_json: &[u8],
    sig_hex: &str,
    payload: &[u8],
) -> Result<Manifest, String> {
    let sig = unhex(sig_hex).ok_or("the signature is not hex")?;
    verify(pubkey, installed_version, manifest_json, &sig, payload)
        .map_err(|e| format!("catalogue refused: {e:?}"))
}

/// The public key from its hex form (32 bytes).
pub fn pubkey_from_hex(hex: &str) -> Result<VerifyingKey, String> {
    let bytes = unhex(hex).ok_or("the catalogue key is not hex")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "the catalogue key is not 32 bytes".to_string())?;
    VerifyingKey::from_bytes(&arr).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_update::manifest::sign;
    use ed25519_dalek::SigningKey;

    const CATALOG: &[u8] = b"[[item]]\nid = \"x\"\nname = \"X\"\nkind = \"native\"\n";

    fn signed(version: u64, payload: &[u8]) -> (VerifyingKey, Vec<u8>, String) {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let (json, sig) = sign(&key, version, "catalog", payload);
        let hex: String = sig.iter().map(|b| format!("{b:02x}")).collect();
        (key.verifying_key(), json, hex)
    }

    #[test]
    fn a_good_signature_is_accepted() {
        let (pk, json, sig) = signed(2, CATALOG);
        let m = check(&pk, 1, &json, &sig, CATALOG).unwrap();
        assert_eq!(m.version, 2);
    }

    #[test]
    fn a_changed_byte_is_refused() {
        let (pk, json, sig) = signed(2, CATALOG);
        let mut changed = CATALOG.to_vec();
        changed[10] = b'Y';
        assert!(check(&pk, 1, &json, &sig, &changed).is_err());
    }

    #[test]
    fn an_older_version_is_refused() {
        let (pk, json, sig) = signed(2, CATALOG);
        let e = check(&pk, 2, &json, &sig, CATALOG).unwrap_err();
        assert!(e.contains("Rollback"), "{e}");
    }
}
