//! Loopback JSON-lines control server: one request per line, one reply per line.
//! This is the transport the MCP server (`lightcraft-cli mcp --connect`) wraps.
//!
//! **Authentication.** Every connection must open with `{"hello":{"token":"<secret>"}}` before
//! any request is read as a request. The secret is 256 random bits from the OS CSPRNG, made when
//! the server starts; it is written to an owner-only file (`<config dir>/control/<port>.token`,
//! rewritten every launch, removed by [`shutdown`] on a clean exit) and is never logged or sent
//! anywhere else. A connection whose first line is anything else, or whose token differs
//! (constant-time compare), gets one error reply and is closed; nothing is executed. An
//! unauthenticated connection has [`AUTH_TIMEOUT`] to send its hello line, and the line is
//! limited to [`MAX_HELLO_LINE`] bytes. There is no peer check: the OS offers none that is
//! practical for loopback TCP (no `SO_PEERCRED` for TCP; mapping the client port to a process
//! needs unsafe, racy platform calls), so the secret is the only gate. See `docs/control-protocol.md`.
//!
//! After authentication the server only ever reads requests: a line that is not a JSON object with
//! a string `method` (an HTTP request line from a browser's cross-origin `fetch`, a stray `nc`,
//! binary junk) gets one error reply and the connection is closed, so nothing after it is
//! executed. Lines are bounded ([`MAX_LINE`]) and so is the number of open connections
//! ([`MAX_CONNECTIONS`]); junk never reaches the UI thread.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lightcraft_mcp::control_auth;
use lightcraft_ui_egui::ControlRequest;
use serde_json::{Value, json};

/// Longest accepted request line (bytes, without the newline). Requests are small JSON objects;
/// anything longer is refused and the connection closed.
pub const MAX_LINE: usize = 4 * 1024 * 1024;
/// Longest accepted first (hello) line of a connection that has not authenticated yet.
pub const MAX_HELLO_LINE: usize = 1024;
/// How long a connection may stay silent before it authenticates.
pub const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
/// Connections served at once; further ones get an error line and are closed.
pub const MAX_CONNECTIONS: usize = 16;

/// The token file this launch wrote, for [`shutdown`].
static TOKEN_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Start the control server on `127.0.0.1:port`. Fails closed: if the secret cannot be made or its
/// owner-only file cannot be written, no server runs (no client could authenticate anyway).
pub fn start(port: u16, ctx: egui::Context) -> Receiver<ControlRequest> {
    let (tx, rx) = channel::<ControlRequest>();
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            log::error!("control server failed to bind 127.0.0.1:{port}: {e}");
            return rx;
        }
    };
    let Some(path) = control_auth::default_token_path(port) else {
        log::error!("control server not started: Ember has no settings folder to put the control token in");
        return rx;
    };
    let secret = match install_token(&path) {
        Ok(s) => s,
        Err(e) => {
            log::error!("control server not started: {e}");
            return rx;
        }
    };
    *TOKEN_FILE.lock().unwrap_or_else(PoisonError::into_inner) = Some(path.clone());
    log::info!("control server listening on 127.0.0.1:{port}; token file {}", path.display());
    let secret = Arc::new(secret);
    std::thread::spawn(move || accept_loop(listener, tx, ctx, secret));
    rx
}

/// Remove this launch's token file (clean exit). Safe to call when no server runs.
pub fn shutdown() {
    let path = TOKEN_FILE.lock().unwrap_or_else(PoisonError::into_inner).take();
    if let Some(path) = path {
        control_auth::remove_token_file(&path);
    }
}

/// Make a fresh secret and write it, owner-only, to `path` (replacing any earlier file, so a file
/// left by a previous launch or widened by someone is never reused). Returns the secret.
fn install_token(path: &Path) -> Result<String, String> {
    let secret = new_secret()?;
    control_auth::write_token_file(path, &secret)?;
    Ok(secret)
}

/// 256 random bits from the OS CSPRNG as 64 hex digits.
fn new_secret() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("no secure randomness for the control token: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn accept_loop(listener: TcpListener, tx: Sender<ControlRequest>, ctx: egui::Context, secret: Arc<String>) {
    let open = Arc::new(AtomicUsize::new(0));
    for mut stream in listener.incoming().flatten() {
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            let _ = writeln!(stream, "{}", json!({"ok": false, "error": "too many control connections"}));
            continue;
        }
        let tx = tx.clone();
        let ctx = ctx.clone();
        let open = Arc::clone(&open);
        let secret = Arc::clone(&secret);
        std::thread::spawn(move || {
            serve(stream, &secret, &tx, &ctx);
            open.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

fn serve(stream: TcpStream, secret: &str, tx: &Sender<ControlRequest>, ctx: &egui::Context) {
    let Ok(read) = stream.try_clone() else { return };
    let Ok(timeouts) = stream.try_clone() else { return };
    if timeouts.set_read_timeout(Some(AUTH_TIMEOUT)).is_err() {
        return;
    }
    serve_lines(
        read,
        stream,
        secret,
        || {
            let _ = timeouts.set_read_timeout(None);
        },
        |method, params| {
            let (req, rrx) = ControlRequest::new(method, params);
            tx.send(req).ok()?;
            ctx.request_repaint();
            Some(rrx.recv_timeout(Duration::from_secs(60)).unwrap_or_else(|_| json!({"ok": false, "error": "timeout"})))
        },
    );
}

/// One parsed line.
enum Line {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Blank,
    /// Not a request: reply with this error and close the connection.
    Reject(String),
}

fn parse_line(bytes: &[u8]) -> Line {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Line::Reject("not a control request (invalid UTF-8); closing the connection".into());
    };
    let text = text.trim();
    if text.is_empty() {
        return Line::Blank;
    }
    let msg = match serde_json::from_str::<Value>(text) {
        Ok(v) => v,
        Err(e) => return Line::Reject(format!("bad JSON ({e}); expected one JSON request object per line; closing the connection")),
    };
    let Some(obj) = msg.as_object() else {
        return Line::Reject("not a control request (expected a JSON object); closing the connection".into());
    };
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return Line::Reject("not a control request (missing string `method`); closing the connection".into());
    };
    Line::Request {
        id: obj.get("id").cloned().unwrap_or(Value::Null),
        method: method.to_string(),
        params: obj.get("params").cloned().unwrap_or(json!({})),
    }
}

/// The token of a `{"hello":{"token":"…"}}` line, `None` when the line is anything else.
fn parse_hello(bytes: &[u8]) -> Option<String> {
    let msg: Value = serde_json::from_slice(bytes).ok()?;
    Some(msg.as_object()?.get("hello")?.as_object()?.get("token")?.as_str()?.to_string())
}

/// Why an unauthenticated connection is refused (never contains a secret).
const AUTH_REQUIRED: &str = "authentication required: the first line must be {\"hello\":{\"token\":\"<secret>\"}} with the secret from the owner-only \
    control/<port>.token file in Ember's settings folder (or LIGHTCRAFT_CONTROL_TOKEN); closing the connection";
const AUTH_FAILED: &str = "authentication failed: wrong control token; closing the connection";

/// Read one `\n`-terminated line of at most `max` bytes into `buf` (newline stripped).
/// `Ok(false)` at end of stream; `Err` when the line is too long or the read fails.
fn read_bounded_line(reader: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> Result<bool, String> {
    buf.clear();
    // `take` bounds the bytes buffered for one line: a peer can't make us allocate more than `max`.
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    let n = reader.take(limit).read_until(b'\n', buf).map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(false);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        return Ok(true);
    }
    if buf.len() > max {
        return Err(format!("request line longer than {max} bytes; closing the connection"));
    }
    // Last line without a newline, then end of stream.
    Ok(true)
}

/// Serve requests from `read`, answering on `out`, until the peer closes, sends something that is
/// not a request, or `handle` returns `None` (app gone). The first non-blank line must be the hello
/// line carrying `secret`; until then no request is parsed or run. `on_auth` runs once, right after
/// a successful hello. `handle` gets `(method, params)` and returns the reply object (the request's
/// `id` is added here).
fn serve_lines<R: Read, W: Write>(read: R, mut out: W, secret: &str, on_auth: impl FnOnce(), mut handle: impl FnMut(String, Value) -> Option<Value>) {
    let mut reader = BufReader::new(read);
    let mut buf = Vec::new();
    let mut on_auth = Some(on_auth);
    let mut authenticated = false;
    loop {
        let max = if authenticated { MAX_LINE } else { MAX_HELLO_LINE };
        let reject = match read_bounded_line(&mut reader, &mut buf, max) {
            Ok(false) => return,
            Err(e) => e,
            Ok(true) if !authenticated => {
                if buf.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                match parse_hello(&buf) {
                    Some(token) if control_auth::ct_eq(secret.as_bytes(), token.as_bytes()) => {
                        authenticated = true;
                        if let Some(f) = on_auth.take() {
                            f();
                        }
                        let ok = json!({"ok": true, "result": {"hello": "ember-control", "protocol": 1}});
                        if writeln!(out, "{ok}").and_then(|()| out.flush()).is_err() {
                            return;
                        }
                        continue;
                    }
                    Some(_) => AUTH_FAILED.to_string(),
                    None => AUTH_REQUIRED.to_string(),
                }
            }
            Ok(true) => match parse_line(&buf) {
                Line::Blank => continue,
                Line::Reject(e) => e,
                Line::Request { id, method, params } => {
                    let Some(mut r) = handle(method, params) else { return };
                    if let Some(o) = r.as_object_mut() {
                        o.insert("id".into(), id);
                    }
                    if writeln!(out, "{r}").and_then(|()| out.flush()).is_err() {
                        return;
                    }
                    continue;
                }
            },
        };
        let _ = writeln!(out, "{}", json!({"ok": false, "error": reject})).and_then(|()| out.flush());
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn hello(token: &str) -> String {
        control_auth::hello_line(token)
    }

    /// Serve `input` as it is (no hello added). Returns the replies and the methods that ran.
    fn run_raw(input: &[u8]) -> (Vec<Value>, Vec<String>) {
        let mut out = Vec::new();
        let mut calls = Vec::new();
        serve_lines(
            input,
            &mut out,
            SECRET,
            || {},
            |m, _p| {
                calls.push(m);
                Some(json!({"ok": true, "result": null}))
            },
        );
        let replies = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).collect();
        (replies, calls)
    }

    /// Serve `input` after a valid hello; the hello's own reply is checked and dropped.
    fn run(input: &[u8]) -> (Vec<Value>, Vec<String>) {
        let mut full = hello(SECRET).into_bytes();
        full.push(b'\n');
        full.extend_from_slice(input);
        let (mut replies, calls) = run_raw(&full);
        let first = replies.remove(0);
        assert_eq!(first["ok"], true, "{first}");
        (replies, calls)
    }

    #[test]
    fn requests_are_answered_in_order_with_ids() {
        let (r, calls) = run(b"{\"id\":1,\"method\":\"a\"}\n\n{\"id\":2,\"method\":\"b\",\"params\":{}}\r\n{\"method\":\"c\"}");
        assert_eq!(calls, ["a", "b", "c"]);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0]["id"], 1);
        assert_eq!(r[1]["id"], 2);
        assert_eq!(r[2]["id"], Value::Null);
    }

    #[test]
    fn connecting_without_the_secret_is_refused_and_nothing_runs() {
        for input in [
            &b"{\"id\":1,\"method\":\"engine.execute\",\"params\":{\"command\":\"folder.move\"}}\n{\"method\":\"b\"}\n"[..],
            b"{\"hello\":{}}\n{\"method\":\"b\"}\n",
            b"{\"hello\":{\"token\":5}}\n{\"method\":\"b\"}\n",
            b"{\"hello\":\"x\"}\n{\"method\":\"b\"}\n",
            b"{\"token\":\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\",\"method\":\"b\"}\n",
            b"\xff\xfe\n{\"method\":\"b\"}\n",
        ] {
            let (r, calls) = run_raw(input);
            assert!(calls.is_empty(), "nothing may run: {calls:?}");
            assert_eq!(r.len(), 1, "one error reply, then close");
            assert_eq!(r[0]["ok"], false);
            let e = r[0]["error"].as_str().unwrap();
            assert!(e.contains("authentication required") && e.contains("closing the connection"), "{e}");
            assert!(!e.contains(SECRET));
        }
    }

    #[test]
    fn a_wrong_secret_is_refused_and_nothing_runs() {
        let wrong_secrets = ["f".repeat(64), SECRET[..63].to_string(), String::new(), format!("{SECRET}0")];
        for wrong in &wrong_secrets {
            let input = format!("{}\n{{\"id\":1,\"method\":\"engine.execute\"}}\n", hello(wrong));
            let (r, calls) = run_raw(input.as_bytes());
            assert!(calls.is_empty(), "nothing may run: {calls:?}");
            assert_eq!(r.len(), 1);
            assert_eq!(r[0]["ok"], false);
            let e = r[0]["error"].as_str().unwrap();
            assert!(e.contains("authentication failed"), "{e}");
            assert!(!e.contains(SECRET));
        }
    }

    #[test]
    fn the_right_secret_runs_requests() {
        let (r, calls) = run(b"{\"id\":9,\"method\":\"ui.inspect\"}\n");
        assert_eq!(calls, ["ui.inspect"]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["id"], 9);
        assert_eq!(r[0]["ok"], true);
    }

    #[test]
    fn the_hello_line_is_bounded_before_authentication() {
        let mut input = vec![b'x'; MAX_HELLO_LINE + 1];
        input.push(b'\n');
        let (r, calls) = run_raw(&input);
        assert!(calls.is_empty());
        assert_eq!(r.len(), 1);
        assert!(r[0]["error"].as_str().unwrap().contains("longer than"));
        // The same length is fine once authenticated (up to MAX_LINE).
        let big = format!("{{\"id\":1,\"method\":\"a\",\"params\":{{\"pad\":\"{}\"}}}}\n", "y".repeat(MAX_HELLO_LINE * 2));
        let (r, calls) = run(big.as_bytes());
        assert_eq!(calls, ["a"]);
        assert_eq!(r[0]["ok"], true);
    }

    #[test]
    fn on_auth_runs_only_after_a_good_hello() {
        for (input, expected) in [(hello(SECRET), true), (hello("nope"), false), (String::from("{\"method\":\"a\"}"), false)] {
            let mut ran = false;
            serve_lines(input.as_bytes(), Vec::new(), SECRET, || ran = true, |_, _| None);
            assert_eq!(ran, expected, "{input}");
        }
    }

    #[test]
    fn http_request_closes_before_the_body_runs() {
        // A browser's cross-origin `fetch` POST: the request line is rejected and the connection
        // closed, so the JSON body is never executed (before authentication, and after it).
        let http = b"POST / HTTP/1.1\r\nHost: 127.0.0.1:7980\r\nContent-Type: text/plain\r\n\r\n\
            {\"id\":1,\"method\":\"engine.execute\",\"params\":{\"command\":\"folder.move\"}}\n";
        let (r, calls) = run_raw(http);
        assert!(calls.is_empty(), "nothing may run: {calls:?}");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["ok"], false);
        assert!(r[0]["error"].as_str().unwrap().contains("closing the connection"));
        let (r, calls) = run(http);
        assert!(calls.is_empty(), "nothing may run: {calls:?}");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["ok"], false);
        assert!(r[0]["error"].as_str().unwrap().contains("closing the connection"));
    }

    #[test]
    fn non_request_json_closes_the_connection() {
        for junk in [
            &b"[1,2]\n{\"method\":\"a\"}\n"[..],
            b"{\"id\":1}\n{\"method\":\"a\"}\n",
            b"{\"method\":5}\n{\"method\":\"a\"}\n",
            b"\xff\xfe\n{\"method\":\"a\"}\n",
        ] {
            let (r, calls) = run(junk);
            assert!(calls.is_empty());
            assert_eq!(r.len(), 1);
            assert_eq!(r[0]["ok"], false);
        }
    }

    #[test]
    fn a_request_before_junk_still_runs() {
        let (r, calls) = run(b"{\"method\":\"a\"}\nGET / HTTP/1.1\n{\"method\":\"b\"}\n");
        assert_eq!(calls, ["a"]);
        assert_eq!(r.len(), 2);
        assert_eq!(r[1]["ok"], false);
    }

    #[test]
    fn overlong_line_is_refused_without_buffering_it_all() {
        // An endless line without a newline: reading stops after MAX_LINE + 1 bytes.
        let mut input = hello(SECRET).into_bytes();
        input.push(b'\n');
        let endless = std::io::Cursor::new(input).chain(std::io::repeat(b'x'));
        let mut out = Vec::new();
        let mut ran = false;
        serve_lines(
            endless,
            &mut out,
            SECRET,
            || {},
            |_, _| {
                ran = true;
                None
            },
        );
        assert!(!ran);
        let last = String::from_utf8(out).unwrap().lines().last().unwrap().to_string();
        let reply: Value = serde_json::from_str(&last).unwrap();
        assert!(reply["error"].as_str().unwrap().contains("longer than"));
    }

    #[test]
    fn line_at_the_limit_is_accepted() {
        let mut buf = Vec::new();
        let line = vec![b'a'; 10];
        let mut input = line.clone();
        input.push(b'\n');
        assert_eq!(read_bounded_line(&mut &input[..], &mut buf, 10), Ok(true));
        assert_eq!(buf, line);
        assert!(read_bounded_line(&mut &[b'a'; 11][..], &mut buf, 10).is_err());
    }

    /// One real loopback connection served by `serve_lines`; returns the address and the methods run.
    fn tcp_server() -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let log = calls.clone();
        std::thread::spawn(move || {
            while let Ok((s, _)) = listener.accept() {
                let log = log.clone();
                std::thread::spawn(move || {
                    let read = s.try_clone().unwrap();
                    serve_lines(
                        read,
                        s,
                        SECRET,
                        || {},
                        |m, _| {
                            log.lock().unwrap().push(m);
                            Some(json!({"ok": true, "result": "ran"}))
                        },
                    );
                });
            }
        });
        (addr, calls)
    }

    #[test]
    fn tcp_connection_is_closed_after_junk() {
        // End to end over a real loopback socket (the transport the app uses).
        let (addr, calls) = tcp_server();
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        c.write_all(format!("{}\n{{\"id\":7,\"method\":\"x\"}}\nhello\n{{\"method\":\"y\"}}\n", hello(SECRET)).as_bytes()).unwrap();
        let mut text = String::new();
        c.read_to_string(&mut text).unwrap(); // returns at EOF: the server closed the connection
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines[1].contains("\"id\":7"));
        assert!(lines[2].contains("closing the connection"));
        assert_eq!(*calls.lock().unwrap(), ["x"]);
    }

    #[test]
    fn tcp_client_without_or_with_a_wrong_secret_is_refused_and_the_right_one_works() {
        let (addr, calls) = tcp_server();
        let addr_text = addr.to_string();
        // No secret: the first request is refused and the connection closed.
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        c.write_all(b"{\"id\":1,\"method\":\"engine.execute\"}\n").unwrap();
        let mut text = String::new();
        c.read_to_string(&mut text).unwrap();
        assert!(text.contains("authentication required"), "{text}");
        assert!(calls.lock().unwrap().is_empty());
        // The shared client with the wrong secret.
        use lightcraft_mcp::Backend;
        let mut wrong = lightcraft_mcp::Remote::lazy(&addr_text).with_token(&"e".repeat(64));
        let err = wrong.call("engine.execute", json!({})).unwrap_err();
        assert!(err.contains("refused the control token"), "{err}");
        assert!(calls.lock().unwrap().is_empty());
        // The shared client with the right one.
        let mut right = lightcraft_mcp::Remote::lazy(&addr_text).with_token(SECRET);
        assert_eq!(right.call("engine.execute", json!({})).unwrap(), json!("ran"));
        assert_eq!(*calls.lock().unwrap(), ["engine.execute"]);
    }

    #[test]
    fn secrets_are_random_hex_of_256_bits() {
        let a = new_secret().unwrap();
        let b = new_secret().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert!(control_auth::plausible_token(&a));
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ember-ctl-srv-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn each_launch_writes_a_fresh_secret_to_the_token_file() {
        let dir = scratch("fresh");
        let path = control_auth::token_path(&dir, 7980);
        let first = install_token(&path).unwrap();
        assert_eq!(control_auth::read_token_file(&path).unwrap(), first);
        let second = install_token(&path).unwrap();
        assert_ne!(first, second);
        assert_eq!(control_auth::read_token_file(&path).unwrap(), second);
        control_auth::remove_token_file(&path);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_widened_token_file_is_repaired_to_0600_when_the_server_starts() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("widened");
        let path = control_auth::token_path(&dir, 7980);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "stale-secret-from-an-earlier-launch\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        // A client refuses the widened file ...
        assert!(control_auth::read_token_file(&path).is_err());
        // ... and the server replaces it with a fresh owner-only one.
        let secret = install_token(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(control_auth::read_token_file(&path).unwrap(), secret);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
