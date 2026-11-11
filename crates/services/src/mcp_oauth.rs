//! OAuth for a remote MCP server that answers 401 (B6). The server's own origin publishes where
//! to sign in (RFC 8414), where to register a client (RFC 7591), and the sign-in itself is the
//! pasted-link flow of oauth.rs. Each server may still need its own exception (the plan notes
//! that one server accepts only a fixed redirect address); those are checked by a person.
use crate::oauth::Provider;
use crate::Http;
use serde_json::{json, Value};

/// The part of a URL that names the server: scheme and host (and port).
pub fn origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split('/').next()?;
    if host.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{host}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
}

/// Parse the RFC 8414 document. Both endpoints must be present.
pub fn parse_metadata(text: &str) -> Result<Metadata, String> {
    let v: Value =
        serde_json::from_str(text).map_err(|_| "the sign-in details are not JSON".to_string())?;
    let authorization_endpoint = v["authorization_endpoint"]
        .as_str()
        .ok_or("the server publishes no authorization endpoint")?
        .to_string();
    let token_endpoint = v["token_endpoint"]
        .as_str()
        .ok_or("the server publishes no token endpoint")?
        .to_string();
    Ok(Metadata {
        authorization_endpoint,
        token_endpoint,
        registration_endpoint: v["registration_endpoint"].as_str().map(str::to_string),
    })
}

/// Fetch the server's sign-in details from its origin.
pub fn discover(http: &mut dyn Http, server_url: &str) -> Result<Metadata, String> {
    let base = origin(server_url).ok_or("the server address has no host")?;
    let text = http.get(
        &format!("{base}/.well-known/oauth-authorization-server"),
        None,
    )?;
    parse_metadata(&text)
}

/// Register a client with the server (RFC 7591): a public client that signs in with PKCE and
/// returns to the redirect address. Returns the client id the server gave.
pub fn register(http: &mut dyn Http, endpoint: &str, redirect: &str) -> Result<String, String> {
    let body = json!({
        "client_name": "agentd",
        "redirect_uris": [redirect],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none"
    });
    let text = http.post_json(endpoint, None, &body)?;
    let v: Value = serde_json::from_str(&text)
        .map_err(|_| "the registration answer is not JSON".to_string())?;
    v["client_id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "the server gave no client id".to_string())
}

/// The sign-in provider for a server, once its client is registered.
pub fn provider(name: &str, meta: &Metadata, client_id: &str, redirect: &str) -> Provider {
    Provider {
        name: name.to_string(),
        client_id: client_id.to_string(),
        client_secret: None,
        auth_url: meta.authorization_endpoint.clone(),
        token_url: meta.token_endpoint.clone(),
        scopes: vec![],
        redirect: redirect.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct Fake {
        gets: Rc<RefCell<Vec<String>>>,
        posts: Rc<RefCell<Vec<(String, Value)>>>,
        answer: String,
    }
    impl Http for Fake {
        fn get(&mut self, url: &str, _b: Option<&str>) -> Result<String, String> {
            self.gets.borrow_mut().push(url.into());
            Ok(self.answer.clone())
        }
        fn post_json(
            &mut self,
            url: &str,
            _b: Option<&str>,
            body: &Value,
        ) -> Result<String, String> {
            self.posts.borrow_mut().push((url.into(), body.clone()));
            Ok(self.answer.clone())
        }
        fn post_form(&mut self, _u: &str, _f: &[(&str, &str)]) -> Result<String, String> {
            Err("not used".into())
        }
    }

    const META: &str = r#"{"authorization_endpoint":"https://auth.example/authorize","token_endpoint":"https://auth.example/token","registration_endpoint":"https://auth.example/register"}"#;

    #[test]
    fn the_origin_is_the_scheme_and_host() {
        assert_eq!(
            origin("https://mcp.example.com/v1/mcp").as_deref(),
            Some("https://mcp.example.com")
        );
        assert_eq!(
            origin("http://127.0.0.1:9000/mcp").as_deref(),
            Some("http://127.0.0.1:9000")
        );
        assert_eq!(origin("nonsense"), None);
    }

    #[test]
    fn discovery_reads_the_endpoints_from_the_origin() {
        let gets = Rc::new(RefCell::new(vec![]));
        let mut http = Fake {
            gets: gets.clone(),
            answer: META.into(),
            ..Fake::default()
        };
        let m = discover(&mut http, "https://mcp.example.com/v1/mcp").unwrap();
        assert_eq!(m.token_endpoint, "https://auth.example/token");
        assert_eq!(
            gets.borrow()[0],
            "https://mcp.example.com/.well-known/oauth-authorization-server"
        );
    }

    #[test]
    fn metadata_without_endpoints_is_refused() {
        assert!(parse_metadata(r#"{"token_endpoint":"x"}"#).is_err());
    }

    #[test]
    fn registration_sends_a_public_client_and_returns_its_id() {
        let posts = Rc::new(RefCell::new(vec![]));
        let mut http = Fake {
            posts: posts.clone(),
            answer: r#"{"client_id":"abc123"}"#.into(),
            ..Fake::default()
        };
        let id = register(
            &mut http,
            "https://auth.example/register",
            "http://127.0.0.1:8765/oauth/callback",
        )
        .unwrap();
        assert_eq!(id, "abc123");
        let (url, body) = posts.borrow()[0].clone();
        assert_eq!(url, "https://auth.example/register");
        assert_eq!(
            body["redirect_uris"],
            json!(["http://127.0.0.1:8765/oauth/callback"])
        );
        assert_eq!(body["token_endpoint_auth_method"], json!("none"));
    }

    #[test]
    fn the_provider_uses_the_discovered_endpoints() {
        let m = parse_metadata(META).unwrap();
        let p = provider(
            "zomato",
            &m,
            "abc123",
            "http://127.0.0.1:8765/oauth/callback",
        );
        assert_eq!(p.auth_url, "https://auth.example/authorize");
        assert_eq!(p.client_id, "abc123");
    }
}
