//! A minimal, synchronous, dependency-light JSON-RPC client for one language
//! server subprocess, framed as the Language Server Protocol's
//! `Content-Length`-delimited JSON on the server's stdin/stdout. It is the Rust
//! port of the Python adapter's `lspclient.py`: a background reader thread
//! demultiplexes id-matched responses from server-initiated notifications
//! (`$/progress`, diagnostics) and requests (answered with `null` so nothing
//! blocks the server), while the calling thread sends framed requests and blocks
//! for the matching reply.
//!
//! It is intentionally small and blocking — hinzu-core is synchronous and the
//! generic extractor drives one server to completion, so a plain worklist over a
//! blocking client is the right shape (the entl async discipline is a
//! per-binding concern this crate does not take on). The only non-Rust artifact
//! in the whole Python path is now the external `ty` binary this client spawns —
//! which hinzu does not write.

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

/// Shared state between the calling thread and the background reader thread.
#[derive(Default)]
struct Shared {
    /// Responses keyed by request id, drained by [`LspClient::wait`].
    responses: HashMap<i64, Value>,
    /// Whether the server has published diagnostics for at least one document —
    /// a signal that a check pass has run and resolution requests will land.
    got_diagnostics: bool,
    /// Whether a `$/progress` `end` has arrived — the workspace-load settle
    /// signal advertised via `window/workDoneProgress`.
    progress_ended: bool,
    /// Whether the server process's stdout closed (it exited or crashed).
    alive: bool,
    /// The server's stderr lines, surfaced in errors so a failure carries the
    /// server's own diagnostics rather than a bare "it exited".
    stderr: Vec<String>,
}

/// A stdio JSON-RPC client for one language-server subprocess.
pub struct LspClient {
    child: Child,
    /// Guarded because both the calling thread (requests/notifications) and the
    /// reader thread (null replies to server→client requests) write frames.
    stdin: Arc<Mutex<ChildStdin>>,
    state: Arc<(Mutex<Shared>, Condvar)>,
    next_id: i64,
    reader: Option<JoinHandle<()>>,
    errhandle: Option<JoinHandle<()>>,
}

impl LspClient {
    /// Spawn `cmd` (argv) with `cwd` and start the reader threads. The server is
    /// not yet initialized; the caller drives the LSP lifecycle.
    pub fn spawn(cmd: &[String], cwd: &std::path::Path) -> Result<Self> {
        let (program, args) = cmd
            .split_first()
            .ok_or_else(|| anyhow!("empty language-server command"))?;
        let mut child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning language server `{program}`"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("language server has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("language server has no stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("language server has no stderr"))?;

        let state = Arc::new((Mutex::new(Shared::new()), Condvar::new()));
        let stdin = Arc::new(Mutex::new(stdin));

        let reader = {
            let state = Arc::clone(&state);
            let stdin = Arc::clone(&stdin);
            std::thread::spawn(move || read_loop(stdout, state, stdin))
        };
        let errhandle = {
            let state = Arc::clone(&state);
            std::thread::spawn(move || err_loop(stderr, state))
        };

        Ok(LspClient {
            child,
            stdin,
            state,
            next_id: 0,
            reader: Some(reader),
            errhandle: Some(errhandle),
        })
    }

    /// Send a request and block for its response, up to `timeout`.
    pub fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.request_async(method, params)?;
        self.wait(id, timeout)
    }

    /// Send a request without waiting; returns its id for a later [`Self::wait`].
    pub fn request_async(&mut self, method: &str, params: Value) -> Result<i64> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        Ok(id)
    }

    /// Send a notification (no id, no reply expected).
    pub fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    /// Block until the response for `id` arrives, or `timeout` elapses, or the
    /// server exits. Returns the response's `result` (or an error for a
    /// JSON-RPC `error` reply).
    pub fn wait(&mut self, id: i64, timeout: Duration) -> Result<Value> {
        let (lock, cv) = &*self.state;
        let deadline = Instant::now() + timeout;
        let mut guard = lock.lock().unwrap();
        loop {
            if let Some(msg) = guard.responses.remove(&id) {
                if let Some(err) = msg.get("error") {
                    bail!("language server error for request {id}: {err}");
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
            if !guard.alive {
                bail!("the language server exited{}", tail(&guard.stderr));
            }
            let now = Instant::now();
            if now >= deadline {
                bail!("language-server request {id} timed out");
            }
            let (g, _) = cv.wait_timeout(guard, deadline - now).unwrap();
            guard = g;
        }
    }

    /// Block until the server publishes diagnostics or reports a `$/progress`
    /// end — the "workspace has been checked once" settle signal — or `timeout`
    /// elapses. Returns whether a settle signal arrived.
    pub fn wait_until_settled(&self, timeout: Duration) -> bool {
        let (lock, cv) = &*self.state;
        let deadline = Instant::now() + timeout;
        let mut guard = lock.lock().unwrap();
        loop {
            if guard.progress_ended || guard.got_diagnostics {
                return true;
            }
            if !guard.alive {
                return false;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (g, _) = cv.wait_timeout(guard, deadline - now).unwrap();
            guard = g;
        }
    }

    /// A copy of the server's stderr lines so far, for diagnostics.
    pub fn stderr_lines(&self) -> Vec<String> {
        self.state.0.lock().unwrap().stderr.clone()
    }

    /// Politely ask the server to `shutdown`/`exit`, then reap the child. Best
    /// effort — a server that is already gone is not an error.
    pub fn shutdown(&mut self) {
        let _ = self.request("shutdown", Value::Null, Duration::from_secs(5));
        let _ = self.notify("exit", Value::Null);
        let _ = self.child.wait();
    }

    fn send(&self, obj: Value) -> Result<()> {
        let data = serde_json::to_vec(&obj)?;
        let mut stdin = self.stdin.lock().unwrap();
        write!(stdin, "Content-Length: {}\r\n\r\n", data.len())?;
        stdin.write_all(&data)?;
        stdin.flush()?;
        Ok(())
    }
}

impl Shared {
    fn new() -> Self {
        Shared {
            alive: true,
            ..Shared::default()
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // Ensure the child cannot outlive the client even if `shutdown` was not
        // called (e.g. an error path unwound past it).
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
        if let Some(h) = self.errhandle.take() {
            let _ = h.join();
        }
    }
}

/// The last few server stderr lines, formatted for an error suffix.
fn tail(lines: &[String]) -> String {
    let n = lines.len().min(8);
    if n == 0 {
        return String::new();
    }
    format!(":\n{}", lines[lines.len() - n..].join("\n"))
}

/// The reader thread: parse `Content-Length` frames off the server's stdout and
/// route each message — id-matched responses into the shared map, diagnostics /
/// progress into their settle flags, and server→client requests answered with a
/// `null` result so the server never blocks on us.
fn read_loop(
    stdout: std::process::ChildStdout,
    state: Arc<(Mutex<Shared>, Condvar)>,
    stdin: Arc<Mutex<ChildStdin>>,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let body = match read_frame(&mut reader) {
            Some(b) => b,
            None => {
                let (lock, cv) = &*state;
                lock.lock().unwrap().alive = false;
                cv.notify_all();
                return;
            }
        };
        let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
            continue;
        };

        let has_id = msg.get("id").map(|v| !v.is_null()).unwrap_or(false);
        let is_response = has_id && (msg.get("result").is_some() || msg.get("error").is_some());
        if is_response {
            if let Some(id) = msg.get("id").and_then(Value::as_i64) {
                let (lock, cv) = &*state;
                lock.lock().unwrap().responses.insert(id, msg);
                cv.notify_all();
            }
            continue;
        }

        match msg.get("method").and_then(Value::as_str) {
            Some("textDocument/publishDiagnostics") => {
                let (lock, cv) = &*state;
                lock.lock().unwrap().got_diagnostics = true;
                cv.notify_all();
            }
            Some("$/progress") => {
                let kind = msg
                    .pointer("/params/value/kind")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if kind == "end" {
                    let (lock, cv) = &*state;
                    lock.lock().unwrap().progress_ended = true;
                    cv.notify_all();
                }
            }
            Some(_) if has_id => {
                // A server→client request: reply `null` so it does not block.
                if let Some(id) = msg.get("id").cloned() {
                    let reply = json!({"jsonrpc": "2.0", "id": id, "result": Value::Null});
                    if let Ok(data) = serde_json::to_vec(&reply) {
                        if let Ok(mut w) = stdin.lock() {
                            let _ = write!(w, "Content-Length: {}\r\n\r\n", data.len());
                            let _ = w.write_all(&data);
                            let _ = w.flush();
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Read one `Content-Length`-framed message body, or `None` at clean EOF / a
/// broken stream.
fn read_frame(reader: &mut impl Read) -> Option<Vec<u8>> {
    // Accumulate header bytes until the CRLFCRLF separator.
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        match reader.read(&mut byte) {
            Ok(0) | Err(_) => return None,
            Ok(_) => header.push(byte[0]),
        }
    }
    let mut length = 0usize;
    for line in header.split(|&b| b == b'\n') {
        let line = String::from_utf8_lossy(line);
        if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = rest.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    let mut read = 0;
    while read < length {
        match reader.read(&mut body[read..]) {
            Ok(0) | Err(_) => return None,
            Ok(n) => read += n,
        }
    }
    Some(body)
}

/// Drain the server's stderr into the shared buffer so it can be surfaced.
fn err_loop(stderr: std::process::ChildStderr, state: Arc<(Mutex<Shared>, Condvar)>) {
    let mut reader = BufReader::new(stderr);
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if byte[0] == b'\n' {
                    let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                    state.0.lock().unwrap().stderr.push(line);
                    buf.clear();
                } else {
                    buf.push(byte[0]);
                }
            }
        }
    }
}

/// Convert a `file://` URI to a filesystem path, percent-decoding it. LSP
/// servers hand back `targetUri`/`uri` as `file://…`.
pub fn uri_to_path(u: &str) -> String {
    let without_scheme = u.strip_prefix("file://").unwrap_or(u);
    percent_decode(without_scheme)
}

/// Build a `file://` URI from an absolute path.
pub fn path_to_uri(path: &std::path::Path) -> String {
    let p = path.to_string_lossy();
    format!("file://{}", percent_encode(&p))
}

/// Minimal percent-decoding (`%XX`), enough for filesystem URIs.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Minimal percent-encoding for a path: only the characters an LSP server is
/// picky about (space, and the few reserved ones). Paths are otherwise ASCII.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            ' ' => out.push_str("%20"),
            _ => out.push(c),
        }
    }
    out
}
