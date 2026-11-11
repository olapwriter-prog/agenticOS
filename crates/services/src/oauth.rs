//! Sign-in by pasted link. `begin` prints a sign-in URL (PKCE, S256). The person opens it in any
//! browser and approves; the browser lands on the redirect address, which fails to load, which is
//! expected. They copy that address and paste it back, and `finish` exchanges the code for tokens.
//! No web server is needed. Tokens are kept by the caller, in the vault.
use crate::Http;
use sha2::{Digest, Sha256};

pub struct Provider {
    pub name: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub auth_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    pub redirect: String,
}

pub struct Pending {
    pub state: String,
    pub verifier: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokens {
    pub access: String,
    pub refresh: Option<String>,
    pub expires_at_ms: u64,
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Base64url without padding (RFC 4648 section 5).
pub fn b64url(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        for i in 0..=chunk.len() {
            out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// The PKCE challenge for a verifier: base64url(SHA-256(verifier)), unpadded.
pub fn challenge(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The sign-in URL, and what to keep until the paste comes back. `random` is 48 bytes from the OS.
pub fn begin(p: &Provider, random: &[u8; 48]) -> (String, Pending) {
    let verifier = b64url(random);
    let state = b64url(&Sha256::digest([random.as_slice(), b"state"].concat()))[..22].to_string();
    let url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        p.auth_url,
        enc(&p.client_id),
        enc(&p.redirect),
        enc(&p.scopes.join(" ")),
        enc(&state),
        challenge(&verifier)
    );
    (url, Pending { state, verifier })
}

/// The value of one query parameter in a URL or a query string.
fn query_param<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let q = text.split_once('?').map_or(text, |(_, q)| q);
    q.split('&')
        .find_map(|kv| kv.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
}

fn token_answer(raw: &str, now_ms: u64, keep_refresh: Option<&str>) -> Result<Tokens, String> {
    let v: serde_json::Value = serde_json::from_str(raw)
        .map_err(|_| "sign-in: bad answer from the token server".to_string())?;
    let access = v["access_token"].as_str().ok_or_else(|| {
        format!(
            "sign-in refused: {}",
            v["error_description"]
                .as_str()
                .or(v["error"].as_str())
                .unwrap_or("no access token")
        )
    })?;
    let expires_in = v["expires_in"].as_u64().unwrap_or(3600);
    Ok(Tokens {
        access: access.to_string(),
        refresh: v["refresh_token"]
            .as_str()
            .map(str::to_string)
            .or_else(|| keep_refresh.map(str::to_string)),
        expires_at_ms: now_ms + expires_in * 1000,
    })
}

/// Exchange the pasted redirect address (or a bare code) for tokens. A pasted address must carry
/// the state we sent: a different state means someone else's sign-in, and it is refused.
pub fn finish(
    p: &Provider,
    pend: &Pending,
    pasted: &str,
    http: &mut dyn Http,
    now_ms: u64,
) -> Result<Tokens, String> {
    let pasted = pasted.trim();
    let code = if pasted.contains('?') || pasted.contains("code=") {
        if query_param(pasted, "state") != Some(pend.state.as_str()) {
            return Err(
                "sign-in refused: the state does not match this sign-in; run oauth begin again"
                    .into(),
            );
        }
        query_param(pasted, "code")
            .ok_or("sign-in refused: no code in the pasted address")?
            .to_string()
    } else {
        pasted.to_string()
    };
    let mut fields = vec![
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", p.redirect.as_str()),
        ("client_id", p.client_id.as_str()),
        ("code_verifier", pend.verifier.as_str()),
    ];
    if let Some(s) = &p.client_secret {
        fields.push(("client_secret", s.as_str()));
    }
    let raw = http
        .post_form(&p.token_url, &fields)
        .map_err(|e| e.replace(&code, "<code>"))?;
    token_answer(&raw, now_ms, None)
}

/// Get a new access token with the refresh token. The refresh token stays unless the server
/// sends a new one.
pub fn refresh(
    p: &Provider,
    t: &Tokens,
    http: &mut dyn Http,
    now_ms: u64,
) -> Result<Tokens, String> {
    let refresh = t
        .refresh
        .as_deref()
        .ok_or("sign-in: no refresh token; sign in again")?;
    let mut fields = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
        ("client_id", p.client_id.as_str()),
    ];
    if let Some(s) = &p.client_secret {
        fields.push(("client_secret", s.as_str()));
    }
    let raw = http.post_form(&p.token_url, &fields)?;
    token_answer(&raw, now_ms, Some(refresh))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Fake {
        answers: Rc<RefCell<Vec<String>>>,
        bodies: Rc<RefCell<Vec<Vec<(String, String)>>>>,
    }
    impl Http for Fake {
        fn get(&mut self, _url: &str, _bearer: Option<&str>) -> Result<String, String> {
            Err("not used".into())
        }
        fn post_json(
            &mut self,
            _url: &str,
            _bearer: Option<&str>,
            _body: &serde_json::Value,
        ) -> Result<String, String> {
            Err("not used".into())
        }
        fn post_form(&mut self, _url: &str, fields: &[(&str, &str)]) -> Result<String, String> {
            self.bodies.borrow_mut().push(
                fields
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            );
            Ok(self.answers.borrow_mut().remove(0))
        }
    }

    fn provider() -> Provider {
        Provider {
            name: "google".into(),
            client_id: "client.id".into(),
            client_secret: None,
            auth_url: "https://accounts.example/auth".into(),
            token_url: "https://accounts.example/token".into(),
            scopes: vec!["calendar.events".into(), "gmail.readonly".into()],
            redirect: "http://127.0.0.1:8765/oauth/callback".into(),
        }
    }

    #[test]
    fn the_challenge_matches_the_rfc_7636_example() {
        // RFC 7636, appendix B: the verifier and its S256 challenge
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_pasted_address_with_the_wrong_state_is_refused() {
        let p = provider();
        let (_url, pend) = begin(&p, &[7u8; 48]);
        let answers = Rc::new(RefCell::new(vec![
            r#"{"access_token":"a","expires_in":3600}"#.to_string(),
        ]));
        let mut http = Fake {
            answers: answers.clone(),
            bodies: Rc::new(RefCell::new(vec![])),
        };
        let bad = format!("{}?code=abc&state=not-it", p.redirect);
        assert!(finish(&p, &pend, &bad, &mut http, 0)
            .unwrap_err()
            .contains("state does not match"));
        let good = format!("{}?code=abc&state={}", p.redirect, pend.state);
        assert_eq!(
            finish(&p, &pend, &good, &mut http, 1000).unwrap().access,
            "a"
        );
    }

    #[test]
    fn refresh_replaces_the_access_token_and_keeps_the_refresh_token() {
        let p = provider();
        let old = Tokens {
            access: "old".into(),
            refresh: Some("r1".into()),
            expires_at_ms: 0,
        };
        let answers = Rc::new(RefCell::new(vec![
            r#"{"access_token":"new","expires_in":60}"#.to_string(),
        ]));
        let bodies = Rc::new(RefCell::new(vec![]));
        let mut http = Fake {
            answers,
            bodies: bodies.clone(),
        };
        let t = refresh(&p, &old, &mut http, 5000).unwrap();
        assert_eq!(t.access, "new");
        assert_eq!(t.refresh.as_deref(), Some("r1"));
        assert_eq!(t.expires_at_ms, 65_000);
        assert!(bodies.borrow()[0].contains(&("refresh_token".to_string(), "r1".to_string())));
    }

    #[test]
    fn begin_builds_a_pkce_sign_in_url() {
        let p = provider();
        let (url, pend) = begin(&p, &[9u8; 48]);
        assert!(
            url.starts_with("https://accounts.example/auth?response_type=code&client_id=client.id")
        );
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&format!("state={}", pend.state)));
        assert!(url.contains(&format!("code_challenge={}", challenge(&pend.verifier))));
    }
}
