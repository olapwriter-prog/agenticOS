//! A small, strict HTTP/1.1 front door for the MCP endpoint, using only the standard library.
//! One request per connection. It checks the bearer token (in constant time), the Origin header,
//! the method and path, and hard size and time limits, before the body ever reaches the gateway.
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub struct HttpConfig {
    pub token: String,
    /// Origins allowed to call. A request with no Origin header (a command-line client) is allowed;
    /// one with an Origin that is not listed is refused, which blocks web pages from calling the phone.
    pub allowed_origins: Vec<String>,
    pub max_header_bytes: usize,
    pub max_body_bytes: usize,
    pub read_timeout: Duration,
}

impl HttpConfig {
    pub fn new(token: &str) -> Self {
        HttpConfig {
            token: token.to_string(),
            allowed_origins: vec![],
            max_header_bytes: 8 * 1024,
            max_body_bytes: 64 * 1024,
            read_timeout: Duration::from_secs(5),
        }
    }
}

/// Compare two byte strings without stopping at the first difference.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

fn reply(s: &mut TcpStream, status: &str, extra: &str, body: &str) -> io::Result<()> {
    let head = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n", body.len());
    s.write_all(head.as_bytes())?;
    s.write_all(body.as_bytes())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Serve exactly one request on `stream`.
pub fn serve_one(
    stream: &mut TcpStream,
    cfg: &HttpConfig,
    handler: &mut dyn FnMut(&str) -> Option<String>,
) -> io::Result<()> {
    stream.set_read_timeout(Some(cfg.read_timeout))?;
    stream.set_write_timeout(Some(cfg.read_timeout))?;

    // read the head
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i;
        }
        if buf.len() > cfg.max_header_bytes {
            return reply(stream, "431 Request Header Fields Too Large", "", "{}");
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "closed before the headers ended",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let header = |name: &str| -> Option<String> {
        lines.clone().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.trim().eq_ignore_ascii_case(name) {
                Some(v.trim().to_string())
            } else {
                None
            }
        })
    };

    // order matters: cheap refusals first, and the token before anything is read from the body
    if path != "/mcp" {
        return reply(stream, "404 Not Found", "", "{}");
    }
    if method != "POST" {
        return reply(stream, "405 Method Not Allowed", "Allow: POST\r\n", "{}");
    }
    let presented = header("authorization")
        .and_then(|v| v.strip_prefix("Bearer ").map(|t| t.to_string()))
        .unwrap_or_default();
    if !ct_eq(presented.as_bytes(), cfg.token.as_bytes()) {
        return reply(
            stream,
            "401 Unauthorized",
            "WWW-Authenticate: Bearer\r\n",
            "{}",
        );
    }
    if let Some(origin) = header("origin") {
        if !cfg.allowed_origins.iter().any(|o| o == &origin) {
            return reply(stream, "403 Forbidden", "", "{}");
        }
    }
    let Some(len) = header("content-length").and_then(|v| v.parse::<usize>().ok()) else {
        return reply(stream, "411 Length Required", "", "{}");
    };
    if len > cfg.max_body_bytes {
        return reply(stream, "413 Payload Too Large", "", "{}");
    }

    // read the body
    let mut body: Vec<u8> = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "closed before the body ended",
            ));
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(len);
    let Ok(text) = String::from_utf8(body) else {
        return reply(stream, "400 Bad Request", "", "{}");
    };

    match handler(&text) {
        Some(resp) => reply(stream, "200 OK", "", &resp),
        None => reply(stream, "202 Accepted", "", ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Start a server for `n` connections. Returns its address.
    fn server(cfg: HttpConfig, n: usize) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for _ in 0..n {
                let (mut s, _) = l.accept().unwrap();
                let _ = serve_one(&mut s, &cfg, &mut |b| {
                    if b == "notify" {
                        None
                    } else {
                        Some(format!("echo:{b}"))
                    }
                });
            }
        });
        addr
    }
    /// Send raw bytes, return the whole response.
    fn send(addr: &str, raw: &str) -> String {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(raw.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }
    fn post(extra_headers: &str, body: &str) -> String {
        format!(
            "POST /mcp HTTP/1.1\r\nHost: x\r\n{extra_headers}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }
    fn auth() -> &'static str {
        "Authorization: Bearer s3cret\r\n"
    }
    fn cfg() -> HttpConfig {
        HttpConfig::new("s3cret")
    }

    #[test]
    fn a_good_request_reaches_the_handler() {
        let a = server(cfg(), 1);
        let r = send(&a, &post(auth(), "hello"));
        assert!(r.starts_with("HTTP/1.1 200 OK"), "{r}");
        assert!(r.ends_with("echo:hello"));
    }
    #[test]
    fn a_notification_gets_202_and_no_body() {
        let a = server(cfg(), 1);
        let r = send(&a, &post(auth(), "notify"));
        assert!(r.starts_with("HTTP/1.1 202 Accepted"), "{r}");
    }
    #[test]
    fn no_token_and_a_wrong_token_are_both_401() {
        let a = server(cfg(), 3);
        assert!(send(&a, &post("", "x")).starts_with("HTTP/1.1 401"));
        assert!(send(&a, &post("Authorization: Bearer nope\r\n", "x")).starts_with("HTTP/1.1 401"));
        assert!(send(&a, &post("Authorization: s3cret\r\n", "x")).starts_with("HTTP/1.1 401"));
        // no "Bearer"
    }
    #[test]
    fn the_handler_is_never_called_without_the_token() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let c2 = called.clone();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let _ = serve_one(&mut s, &cfg(), &mut |_| {
                c2.store(true, std::sync::atomic::Ordering::SeqCst);
                Some("x".into())
            });
        });
        send(&addr, &post("", "x"));
        t.join().unwrap();
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    }
    #[test]
    fn wrong_method_and_wrong_path() {
        let a = server(cfg(), 2);
        assert!(send(&a, "GET /mcp HTTP/1.1\r\nHost: x\r\n\r\n").starts_with("HTTP/1.1 405"));
        assert!(send(
            &a,
            "POST /admin HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n"
        )
        .starts_with("HTTP/1.1 404"));
    }
    #[test]
    fn a_browser_origin_is_refused_unless_listed() {
        let mut c = cfg();
        c.allowed_origins = vec!["https://claude.ai".into()];
        let a = server(c, 3);
        assert!(send(
            &a,
            &post(&format!("{}Origin: https://evil.example\r\n", auth()), "x")
        )
        .starts_with("HTTP/1.1 403"));
        assert!(send(
            &a,
            &post(&format!("{}Origin: https://claude.ai\r\n", auth()), "x")
        )
        .starts_with("HTTP/1.1 200"));
        assert!(
            send(&a, &post(auth(), "x")).starts_with("HTTP/1.1 200"),
            "no Origin means a command-line client"
        );
    }
    #[test]
    fn an_oversize_body_is_refused_before_it_is_read() {
        let mut c = cfg();
        c.max_body_bytes = 10;
        let a = server(c, 1);
        // claims 1000 bytes but sends none: if the server tried to read them it would hang until the timeout
        let r = send(
            &a,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: x\r\n{}Content-Length: 1000\r\n\r\n",
                auth()
            ),
        );
        assert!(r.starts_with("HTTP/1.1 413"), "{r}");
    }
    #[test]
    fn a_missing_length_is_411() {
        let a = server(cfg(), 1);
        let r = send(
            &a,
            &format!("POST /mcp HTTP/1.1\r\nHost: x\r\n{}\r\n", auth()),
        );
        assert!(r.starts_with("HTTP/1.1 411"), "{r}");
    }
    #[test]
    fn a_header_flood_is_431() {
        let mut c = cfg();
        c.max_header_bytes = 512;
        let a = server(c, 1);
        let r = send(
            &a,
            &format!("POST /mcp HTTP/1.1\r\nX-Junk: {}\r\n", "a".repeat(2000)),
        );
        assert!(r.starts_with("HTTP/1.1 431"), "{r}");
    }
    #[test]
    fn a_client_that_stalls_is_cut_off_by_the_timeout() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let mut c = cfg();
        c.read_timeout = Duration::from_millis(200);
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            serve_one(&mut s, &c, &mut |_| None)
        });
        let _slow = TcpStream::connect(addr).unwrap(); // connects, sends nothing
        let started = std::time::Instant::now();
        assert!(t.join().unwrap().is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
    }
    #[test]
    fn a_body_split_across_packets_is_reassembled() {
        let a = server(cfg(), 1);
        let mut s = TcpStream::connect(&a).unwrap();
        let body = "abcdefghij";
        s.write_all(
            format!(
                "POST /mcp HTTP/1.1\r\nHost: x\r\n{}Content-Length: {}\r\n\r\nabc",
                auth(),
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
        s.flush().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        s.write_all(b"defghij").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.ends_with("echo:abcdefghij"), "{out}");
    }
    #[test]
    fn constant_time_compare_behaves_like_equality() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(ct_eq(b"", b""));
    }
}
