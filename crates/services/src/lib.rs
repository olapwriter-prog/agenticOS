//! Connectors for services that have a plain HTTP API and need no app: each one talks to the
//! service over `Http`, so the tests run with a fake and never touch the network.
pub mod telegram;

/// The network, as the connectors need it. `Ureq` is the real one.
pub trait Http {
    fn get(&mut self, url: &str) -> Result<String, String>;
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

impl Http for Ureq {
    fn get(&mut self, url: &str) -> Result<String, String> {
        let agent = ureq::AgentBuilder::new().timeout(self.timeout).build();
        answer(agent.get(url).call())
    }
    fn post_form(&mut self, url: &str, fields: &[(&str, &str)]) -> Result<String, String> {
        let agent = ureq::AgentBuilder::new().timeout(self.timeout).build();
        answer(agent.post(url).send_form(fields))
    }
}
