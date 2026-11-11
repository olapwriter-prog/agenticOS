//! Connectors for services that have a plain HTTP API and need no app: each one talks to the
//! service over `Http`, so the tests run with a fake and never touch the network.
pub mod google;
pub mod oauth;
pub mod telegram;

/// Base64url without padding, decoded. None for anything outside the alphabet.
pub fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in text.trim_end_matches('=').bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Standard base64 with padding (for MIME encoded words).
pub fn b64_std(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The network, as the connectors need it. `Ureq` is the real one.
pub trait Http {
    fn get(&mut self, url: &str, bearer: Option<&str>) -> Result<String, String>;
    fn post_json(
        &mut self,
        url: &str,
        bearer: Option<&str>,
        body: &serde_json::Value,
    ) -> Result<String, String>;
    fn post_form(&mut self, url: &str, fields: &[(&str, &str)]) -> Result<String, String>;
}

pub struct Ureq {
    pub timeout: std::time::Duration,
}

impl Default for Ureq {
    fn default() -> Self {
        Ureq {
            timeout: std::time::Duration::from_secs(15),
        }
    }
}

fn answer(r: Result<ureq::Response, ureq::Error>) -> Result<String, String> {
    match r {
        Ok(resp) => resp.into_string().map_err(|e| e.to_string()),
        Err(ureq::Error::Status(code, _)) => Err(format!("HTTP {code}")),
        Err(e) => Err(e.to_string()),
    }
}

impl Ureq {
    fn agent(&self) -> ureq::Agent {
        ureq::AgentBuilder::new().timeout(self.timeout).build()
    }
}

impl Http for Ureq {
    fn get(&mut self, url: &str, bearer: Option<&str>) -> Result<String, String> {
        let mut req = self.agent().get(url);
        if let Some(t) = bearer {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        answer(req.call())
    }
    fn post_json(
        &mut self,
        url: &str,
        bearer: Option<&str>,
        body: &serde_json::Value,
    ) -> Result<String, String> {
        let mut req = self.agent().post(url);
        if let Some(t) = bearer {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        answer(req.send_json(body.clone()))
    }
    fn post_form(&mut self, url: &str, fields: &[(&str, &str)]) -> Result<String, String> {
        answer(self.agent().post(url).send_form(fields))
    }
}
