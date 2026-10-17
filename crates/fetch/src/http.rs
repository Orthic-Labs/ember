//! A small HTTP/1.1 client for model downloads & bounded JSON requests, native only.
//!
//! The transfer is `rightkit-http` (the Right Suite HTTP owner, M17): blocking `ureq` with rustls and the
//! operating system's trust store. This module keeps Ember's own surface ([`Url`], [`Limits`], [`Response`],
//! [`get`], [`post_json`], [`HttpError`]) so the downloader, map tiles and AI callers do not change.
//!
//! The policy is Ember's: no redirects (the caller follows them, one hop at a time), no retries (the caller
//! owns those), no proxies, `Accept-Encoding: identity`. Each request builds its owner client, so the TLS roots
//! and the address policy are the owner's public-web defaults. A response body streams in on a helper thread,
//! so every read checks the cancel flag and the stall limit at least four times a second.

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use rightkit_http::{Client, ClientConfig, Phase, Request, RetryPolicy};

/// Why a request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// The URL can't be used (scheme, host, characters).
    BadUrl(String),
    /// Name resolution or connecting failed (or took longer than the connect timeout).
    Connect(String),
    /// No data arrived for the stall timeout.
    Stalled,
    /// The cancel flag was raised.
    Cancelled,
    /// TLS failed (certificate, handshake).
    Tls(String),
    /// The server's response could not be understood.
    Protocol(String),
    /// Any other I/O error.
    Io(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::BadUrl(e) => write!(f, "unusable URL: {e}"),
            HttpError::Connect(e) => write!(f, "could not connect: {e}"),
            HttpError::Stalled => write!(f, "the server stopped sending data"),
            HttpError::Cancelled => write!(f, "cancelled"),
            HttpError::Tls(e) => write!(f, "secure connection failed: {e}"),
            HttpError::Protocol(e) => write!(f, "unexpected response: {e}"),
            HttpError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// A parsed `http://` or `https://` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    /// Path and query, starting with `/`.
    pub path: String,
}

impl Url {
    pub fn parse(s: &str) -> Result<Url, HttpError> {
        let bad = |why: &str| HttpError::BadUrl(why.to_string());
        if s.len() > 16 * 1024 {
            return Err(bad("URL exceeds 16 KiB"));
        }
        let s = s.trim();
        if s.chars().any(|c| c.is_control() || c == ' ') {
            return Err(bad("contains spaces or control characters"));
        }
        let (tls, rest) = if let Some(r) = s.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = s.strip_prefix("http://") {
            (false, r)
        } else {
            return Err(bad("only http:// and https:// are supported"));
        };
        let rest = rest.split('#').next().unwrap_or_default();
        let (authority, path) = match rest.find(['/', '?']) {
            Some(i) => (rest.get(..i).unwrap_or_default(), rest.get(i..).unwrap_or_default()),
            None => (rest, ""),
        };
        let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
        if authority.contains('@') {
            return Err(bad("user names and passwords in URLs are not supported"));
        }
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let end = v6.find(']').ok_or_else(|| bad("unterminated IPv6 address"))?;
            let host = v6.get(..end).unwrap_or_default();
            let after = v6.get(end + 1..).unwrap_or_default();
            (host.to_string(), after.strip_prefix(':'))
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), Some(p)),
                None => (authority.to_string(), None),
            }
        };
        let port = match port {
            Some(p) => p.parse::<u16>().ok().filter(|p| *p != 0).ok_or_else(|| bad("bad port"))?,
            None if tls => 443,
            None => 80,
        };
        if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':' | '_')) {
            return Err(bad("bad host name"));
        }
        Ok(Url { tls, host: host.to_ascii_lowercase(), port, path })
    }

    /// `Location` of a redirect, resolved against this URL.
    pub fn join(&self, location: &str) -> Result<Url, HttpError> {
        let l = location.trim();
        if l.starts_with("http://") || l.starts_with("https://") {
            let url = Url::parse(l)?;
            if self.tls && !url.tls {
                return Err(HttpError::BadUrl("redirect from https to http refused".into()));
            }
            return Ok(url);
        }
        // another scheme (`ftp:`, `file:`, `data:`…)
        if let Some((scheme, _)) = l.split_once(':')
            && !scheme.is_empty()
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            return Err(HttpError::BadUrl(format!("redirect to an unsupported location `{scheme}:`")));
        }
        let scheme = if self.tls { "https" } else { "http" };
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        let base = format!("{scheme}://{host}:{}", self.port);
        if let Some(rest) = l.strip_prefix("//") {
            return Url::parse(&format!("{scheme}://{rest}"));
        }
        if l.starts_with('/') {
            return Url::parse(&format!("{base}{l}"));
        }
        // relative to the current directory
        let dir = self.path.split('?').next().unwrap_or("/");
        let dir = dir.rsplit_once('/').map_or("", |(d, _)| d);
        Url::parse(&format!("{base}{dir}/{l}"))
    }

    fn host_header(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        let default = if self.tls { 443 } else { 80 };
        if self.port == default { host } else { format!("{host}:{}", self.port) }
    }

    /// The URL without its query (for messages: a query may hold a signed token).
    pub fn display(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        format!("{scheme}://{}{}", self.host_header(), self.path.split('?').next().unwrap_or("/"))
    }
}

impl Url {
    /// The full URL with its query, as the owner client takes it (credentials are never present).
    fn request_url(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        format!("{scheme}://{}{}", self.host_header(), self.path)
    }
}

/// Timeouts and the cancel flag for one request.
pub struct Limits<'a> {
    pub connect: Duration,
    pub stall: Duration,
    pub cancel: &'a AtomicBool,
    /// Optional whole-operation deadline, including reads that keep making progress.
    pub deadline: Option<Instant>,
}

impl Limits<'_> {
    fn check(&self) -> Result<(), HttpError> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(HttpError::Cancelled);
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(HttpError::Stalled);
        }
        Ok(())
    }
}

/// How often a blocked read wakes up to check the cancel flag.
const TICK: Duration = Duration::from_millis(250);

/// A response body arrives as chunks from the helper thread: data, a read error, or an empty chunk at the end.
type Chunk = io::Result<Vec<u8>>;

/// A response: its status and headers are known, its body is read with [`Response::read`].
pub struct Response {
    pub status: u16,
    headers: Vec<(String, String)>,
    chunks: mpsc::Receiver<Chunk>,
    pending: Vec<u8>,
    at: usize,
    /// The `Content-Length` the server announced (absent for chunked and read-to-close bodies).
    expected: Option<u64>,
    received: u64,
    done: bool,
    last_data: Instant,
}

impl Response {
    /// The first header named `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        find(&self.headers, name)
    }

    /// The body's length when the server announced it.
    pub fn content_length(&self) -> Option<u64> {
        self.expected
    }

    /// Read body bytes into `out` (0 at the end of the body).
    pub fn read(&mut self, out: &mut [u8], limits: &Limits) -> Result<usize, HttpError> {
        limits.check()?;
        let rest = self.pending.get(self.at..).unwrap_or_default();
        if !rest.is_empty() {
            let n = rest.len().min(out.len());
            if let (Some(dst), Some(src)) = (out.get_mut(..n), rest.get(..n)) {
                dst.copy_from_slice(src);
            }
            self.at = self.at.saturating_add(n);
            return Ok(n);
        }
        if self.done || out.is_empty() {
            return Ok(0);
        }
        loop {
            limits.check()?;
            match self.chunks.recv_timeout(TICK) {
                Ok(Ok(chunk)) if chunk.is_empty() => {
                    self.done = true;
                    if self.expected.is_some_and(|n| self.received < n) {
                        return Err(HttpError::Protocol("the connection closed before the end of the file".into()));
                    }
                    return Ok(0);
                }
                Ok(Ok(chunk)) => {
                    self.last_data = Instant::now();
                    self.received = self.received.saturating_add(chunk.len() as u64);
                    self.pending = chunk;
                    self.at = 0;
                    return self.read(out, limits);
                }
                Ok(Err(error)) => {
                    self.done = true;
                    return Err(body_error(error));
                }
                Err(RecvTimeoutError::Timeout) => {
                    if self.last_data.elapsed() >= limits.stall {
                        return Err(HttpError::Stalled);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.done = true;
                    return Err(HttpError::Protocol("the response stream ended unexpectedly".into()));
                }
            }
        }
    }
}

fn find<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

fn body_error(error: io::Error) -> HttpError {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => HttpError::Stalled,
        io::ErrorKind::UnexpectedEof => HttpError::Protocol("the connection closed in the middle of the body".into()),
        _ => HttpError::Io(error.to_string()),
    }
}

/// Read `body` on a helper thread, handing chunks over through a small bounded channel. The thread ends at the
/// end of the body, on a read error, or when the response is dropped (the next send fails).
fn stream_body(mut body: Box<dyn Read + Send>) -> Result<mpsc::Receiver<Chunk>, HttpError> {
    let (sender, receiver) = mpsc::sync_channel::<Chunk>(2);
    std::thread::Builder::new()
        .name("fetch-body".into())
        .spawn(move || {
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                match body.read(&mut buffer) {
                    Ok(0) => {
                        let _ = sender.send(Ok(Vec::new()));
                        break;
                    }
                    Ok(n) => {
                        let data = buffer.get(..n).unwrap_or_default().to_vec();
                        if sender.send(Ok(data)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
            }
        })
        .map_err(|e| HttpError::Io(format!("could not start the download thread: {e}")))?;
    Ok(receiver)
}

/// Owner errors to Ember's error classes; callers match on these.
fn map_error(error: rightkit_http::HttpError) -> HttpError {
    use rightkit_http::HttpError as Owner;
    let text = error.to_string();
    match error {
        Owner::Cancelled => HttpError::Cancelled,
        Owner::Timeout { phase: Phase::Connect, .. } => HttpError::Connect(text),
        Owner::Timeout { .. } => HttpError::Stalled,
        Owner::Transport { connect_phase: true, message, .. } => {
            let lower = message.to_ascii_lowercase();
            if lower.contains("tls") || lower.contains("certificate") || lower.contains("handshake") {
                HttpError::Tls(message)
            } else {
                HttpError::Connect(message)
            }
        }
        Owner::Transport { message, .. } => HttpError::Io(message),
        Owner::AddressDenied { .. } | Owner::HttpsRequired { .. } | Owner::InvalidRequest(_) => HttpError::BadUrl(text),
        Owner::Stream(message) | Owner::Json(message) => HttpError::Protocol(message),
        _ => HttpError::Io(text),
    }
}

/// The owner client's configuration for one Ember request: no retries, no redirects, and the caller's connect
/// and stall limits (a response head or a silent body is bounded by the stall limit).
fn client_config(user_agent: Option<String>, limits: &Limits) -> ClientConfig {
    let mut config = base_config();
    config.user_agent = user_agent.unwrap_or_else(|| format!("Ember/{}", env!("CARGO_PKG_VERSION")));
    config.connect_timeout = Some(limits.connect);
    config.response_timeout = Some(limits.stall);
    config.retry = RetryPolicy::none();
    config.max_redirects = 0;
    config
}

#[cfg(not(test))]
fn base_config() -> ClientConfig {
    ClientConfig::default()
}

/// Tests talk to a loopback server; the owner's public-web address policy would refuse it.
#[cfg(test)]
fn base_config() -> ClientConfig {
    rightkit_http::testing::loopback_config()
}

/// Send `GET url` with extra `headers` and read the status line and headers.
pub fn get(url: &Url, headers: &[(&str, String)], limits: &Limits) -> Result<Response, HttpError> {
    request(url, headers, limits, "GET", &[])
}

/// One POST, no redirects/retries. Caller caps response bytes & supplies a total deadline.
pub fn post_json(url: &Url, headers: &[(&str, String)], body: &[u8], limits: &Limits) -> Result<Response, HttpError> {
    if body.len() > 4 * 1024 * 1024 || limits.deadline.is_none() {
        return Err(HttpError::Protocol("POST requires bounded body & deadline".into()));
    }
    request(url, headers, limits, "POST", body)
}

fn request(url: &Url, headers: &[(&str, String)], limits: &Limits, method: &str, body: &[u8]) -> Result<Response, HttpError> {
    // A caller that names itself (a tile server's usage policy asks for a contact address) replaces the default.
    let mut user_agent = None;
    let mut extra: Vec<(String, String)> = Vec::new();
    for (k, v) in headers {
        if v.chars().any(|c| c == '\r' || c == '\n') || k.chars().any(|c| c == '\r' || c == '\n' || c == ':') {
            return Err(HttpError::BadUrl("header with a line break".into()));
        }
        if k.eq_ignore_ascii_case("user-agent") {
            user_agent = Some(v.clone());
        } else {
            extra.push((k.to_string(), v.clone()));
        }
    }
    limits.check()?;
    let client = Client::try_new(client_config(user_agent, limits)).map_err(map_error)?;
    let mut req = if method == "POST" { Request::post(url.request_url()).body(body.to_vec()) } else { Request::get(url.request_url()) };
    req = req.header("Accept", "*/*").header("Accept-Encoding", "identity");
    if method == "POST" {
        req = req.header("Content-Type", "application/json");
    }
    for (k, v) in extra {
        req = req.header(k, v);
    }
    if let Some(deadline) = limits.deadline {
        req = req.deadline(deadline.saturating_duration_since(Instant::now()));
    }
    let response = client.stream_any(&req).map_err(map_error)?;
    let status = response.status;
    let headers = response.headers.clone();
    let chunked = find(&headers, "transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    let bodyless = status == 204 || status == 304 || (100..200).contains(&status);
    let expected = match find(&headers, "content-length") {
        Some(n) if !chunked && !bodyless => Some(n.trim().parse::<u64>().map_err(|_| HttpError::Protocol(format!("bad Content-Length `{n}`")))?),
        _ => None,
    };
    let chunks = stream_body(response.into_reader())?;
    Ok(Response { status, headers, chunks, pending: Vec::new(), at: 0, expected, received: 0, done: false, last_data: Instant::now() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    #[test]
    fn rejects_oversized_and_control_character_urls() {
        assert!(Url::parse(&format!("https://example.com/{}", "x".repeat(16 * 1024))).is_err());
        assert!(Url::parse("https://example.com/x?secret=abc\r\nInjected: 1").is_err());
    }

    #[test]
    fn parses_urls() {
        let u = Url::parse("https://cdn.example.com/models/sam3/model.safetensors?sig=1#x").unwrap();
        assert_eq!((u.tls, u.host.as_str(), u.port, u.path.as_str()), (true, "cdn.example.com", 443, "/models/sam3/model.safetensors?sig=1"));
        assert_eq!(u.display(), "https://cdn.example.com/models/sam3/model.safetensors");
        assert_eq!(u.request_url(), "https://cdn.example.com/models/sam3/model.safetensors?sig=1");
        let u = Url::parse("http://127.0.0.1:8080").unwrap();
        assert_eq!((u.tls, u.port, u.path.as_str()), (false, 8080, "/"));
        let u = Url::parse("http://[::1]:9/a").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("::1", 9));
        for bad in ["ftp://x/y", "https://", "https://a b/", "https://u:p@h/", "http://h:0/", "http://h:99999/", "https://h/\r\nX: y", "file:///etc"]
        {
            assert!(Url::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn resolves_redirects() {
        let u = Url::parse("https://a.example/x/y/file?q").unwrap();
        assert_eq!(u.join("https://b.example/z").unwrap().host, "b.example");
        assert_eq!(u.join("/root").unwrap().path, "/root");
        assert_eq!(u.join("other").unwrap().path, "/x/y/other");
        assert_eq!(u.join("//c.example/p").unwrap().host, "c.example");
        assert!(u.join("ftp://nope").is_err());
        // no downgrade from https to http, however the redirect is written
        assert!(u.join("http://a.example/z").is_err());
        let plain = Url::parse("http://127.0.0.1:8080/a").unwrap();
        assert!(!plain.join("http://127.0.0.1:9/b").unwrap().tls);
        assert!(plain.join("https://b.example/z").unwrap().tls);
    }

    #[test]
    fn post_sends_exact_bounded_body_once() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut reader = BufReader::new(&socket);
            let mut head = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                head.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            assert!(head.starts_with("POST /test HTTP/1.1\r\n"));
            // header names are case-insensitive and the client sends them lower-case
            let lower = head.to_ascii_lowercase();
            assert!(lower.contains("content-length: 2\r\n"));
            assert!(lower.contains("content-type: application/json\r\n"));
            let mut body = [0; 2];
            reader.read_exact(&mut body).unwrap();
            assert_eq!(&body, b"{}");
            drop(reader);
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").unwrap();
        });
        let cancel = AtomicBool::new(false);
        let limits = Limits {
            connect: Duration::from_secs(2),
            stall: Duration::from_secs(2),
            cancel: &cancel,
            deadline: Instant::now().checked_add(Duration::from_secs(3)),
        };
        let mut response = post_json(&Url::parse(&format!("http://{addr}/test")).unwrap(), &[], b"{}", &limits).unwrap();
        assert_eq!(response.status, 200);
        let mut bytes = [0; 2];
        assert_eq!(response.read(&mut bytes, &limits).unwrap(), 2);
        assert_eq!(&bytes, b"{}");
        worker.join().unwrap();
    }

    #[test]
    fn post_rejects_missing_deadline_oversize_and_cancel_before_connect() {
        let cancel = AtomicBool::new(true);
        let url = Url::parse("http://127.0.0.1:1/test").unwrap();
        let mut limits = Limits { connect: Duration::from_secs(1), stall: Duration::from_secs(1), cancel: &cancel, deadline: None };
        assert!(matches!(post_json(&url, &[], b"{}", &limits), Err(HttpError::Protocol(_))));
        limits.deadline = Instant::now().checked_add(Duration::from_secs(1));
        assert!(matches!(post_json(&url, &[], b"{}", &limits), Err(HttpError::Cancelled)));
        assert!(matches!(post_json(&url, &[], &vec![0; 4 * 1024 * 1024 + 1], &limits), Err(HttpError::Protocol(_))));
        assert!(matches!(post_json(&url, &[("Authorization", "key\r\nInjected: 1".into())], b"{}", &limits), Err(HttpError::BadUrl(_))));
        cancel.store(false, Ordering::Relaxed);
        limits.deadline = Some(Instant::now());
        assert!(matches!(post_json(&url, &[], b"{}", &limits), Err(HttpError::Stalled)));
    }
}
