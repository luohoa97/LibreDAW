// SPDX-License-Identifier: GPL-3.0-or-later
//! The relay (SPEC 18.3): stdio on one side, the control socket on the
//! other. The DAW speaks MCP itself, so every line goes through unchanged.
//!
//! What the relay adds is only what a stdio server must do before there is
//! a socket to talk to:
//!
//! - It connects lazily, when the client's first message arrives. If no DAW
//!   is running it starts `libredaw --agent-request` once and retries for up
//!   to 30 s (17.1, mcp-8). If that fails it answers the request itself with
//!   a JSON-RPC error that says what to do.
//! - If the DAW closes the connection (it was restarted) the next message
//!   reconnects, and the relay replays the client's `initialize` and
//!   `notifications/initialized` so the session continues. Subscriptions
//!   and anything else the old session held are gone; clients re-subscribe.

use std::ffi::OsString;
use std::io::{self, BufRead, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// What the agent is told when the DAW did not come up in time.
pub const WAITING_MESSAGE: &str = "waiting for the user in LibreDAW";

#[derive(Clone, Debug)]
pub struct Config {
    pub socket: PathBuf,
    /// Program started when no DAW is running.
    pub daw_command: OsString,
    /// How long to keep retrying after starting the DAW (30 s).
    pub startup_wait: Duration,
    pub retry_interval: Duration,
}

impl Config {
    pub fn new(socket: PathBuf) -> Config {
        Config {
            socket,
            daw_command: OsString::from("libredaw"),
            startup_wait: Duration::from_secs(30),
            retry_interval: Duration::from_millis(200),
        }
    }

    /// `None` when `XDG_RUNTIME_DIR` is unset.
    pub fn from_env() -> Option<Config> {
        control::default_socket_path().map(Config::new)
    }
}

#[derive(Debug)]
pub enum ConnectError {
    /// The DAW did not accept a connection in time.
    Waiting,
    /// The DAW program could not be started.
    CannotStart(String),
    Other(String),
}

impl ConnectError {
    fn code(&self) -> &'static str {
        match self {
            ConnectError::Waiting => "waiting_for_user",
            ConnectError::CannotStart(_) => "daw_not_running",
            ConnectError::Other(_) => "connection_failed",
        }
    }
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Waiting => write!(f, "{WAITING_MESSAGE}"),
            ConnectError::CannotStart(s) => write!(
                f,
                "LibreDAW is not running and could not be started ({s}); ask the user to start LibreDAW"
            ),
            ConnectError::Other(s) => write!(f, "cannot connect to LibreDAW: {s}"),
        }
    }
}

struct Connector {
    cfg: Config,
    spawned: Option<Child>,
}

impl Connector {
    fn daw_running(&mut self) -> bool {
        match &mut self.spawned {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    fn start_daw(&mut self) -> Result<(), ConnectError> {
        let child = Command::new(&self.cfg.daw_command)
            .arg("--agent-request")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| ConnectError::CannotStart(e.to_string()))?;
        self.spawned = Some(child);
        Ok(())
    }

    fn connect(&mut self) -> Result<UnixStream, ConnectError> {
        let deadline = Instant::now() + self.cfg.startup_wait;
        let mut tried_start = false;
        loop {
            match UnixStream::connect(&self.cfg.socket) {
                Ok(s) => return Ok(s),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    if !tried_start && !self.daw_running() {
                        tried_start = true;
                        self.start_daw()?;
                    }
                }
                Err(e) => return Err(ConnectError::Other(e.to_string())),
            }
            if Instant::now() >= deadline {
                return Err(ConnectError::Waiting);
            }
            thread::sleep(self.cfg.retry_interval);
        }
    }
}

/// One connection to the DAW and the thread copying its output.
struct Link {
    stream: UnixStream,
    broken: Arc<AtomicBool>,
}

struct Shared<W> {
    out: Mutex<W>,
    /// The id of a replayed `initialize` whose response is dropped.
    swallow: Mutex<Option<Value>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Socket -> stdout, line by line, dropping the swallowed response.
fn pump<W: Write>(stream: UnixStream, shared: Arc<Shared<W>>, broken: Arc<AtomicBool>) {
    let mut reader = io::BufReader::new(stream);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let drop_it = {
            let mut sw = lock(&shared.swallow);
            match sw.as_ref() {
                Some(id) => {
                    let hit = serde_json::from_slice::<Value>(&line)
                        .ok()
                        .is_some_and(|v| v.get("id") == Some(id) && v.get("method").is_none());
                    if hit {
                        *sw = None;
                    }
                    hit
                }
                None => false,
            }
        };
        if drop_it {
            continue;
        }
        let mut out = lock(&shared.out);
        if out.write_all(&line).is_err() || out.flush().is_err() {
            break;
        }
    }
    broken.store(true, Ordering::SeqCst);
}

/// If `line` is a request, the JSON-RPC error that answers it.
fn error_reply(line: &[u8], e: &ConnectError) -> Option<String> {
    let v: Value = serde_json::from_slice(line).ok()?;
    let id = v.get("id")?.clone();
    v.get("method")?;
    let message: String = e.to_string().chars().filter(|c| !c.is_control()).collect();
    Some(
        json!({"jsonrpc": "2.0", "id": id,
               "error": {"code": -32000, "message": message, "data": {"reason": e.code()}}})
        .to_string(),
    )
}

/// What the relay must remember of the client to continue after a reconnect.
#[derive(Default)]
struct Handshake {
    initialize: Option<(Vec<u8>, Value)>,
    initialized: Option<Vec<u8>>,
}

fn note_handshake(h: &mut Handshake, line: &[u8]) {
    let Ok(v) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    match v.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            if let Some(id) = v.get("id") {
                h.initialize = Some((line.to_vec(), id.clone()));
                h.initialized = None;
            }
        }
        Some("notifications/initialized") => h.initialized = Some(line.to_vec()),
        _ => {}
    }
}

/// Copies MCP between `input` and the DAW until `input` ends. Returns the
/// number of connections made (for tests and diagnostics).
pub fn run<R, W>(cfg: Config, mut input: R, output: W) -> io::Result<u32>
where
    R: BufRead,
    W: Write + Send + 'static,
{
    let shared = Arc::new(Shared {
        out: Mutex::new(output),
        swallow: Mutex::new(None),
    });
    let mut connector = Connector { cfg, spawned: None };
    let mut link: Option<Link> = None;
    let mut handshake = Handshake::default();
    let mut connections = 0u32;
    let mut line = Vec::new();
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if !line.ends_with(b"\n") {
            line.push(b'\n');
        }
        let parsed = serde_json::from_slice::<Value>(&line).ok();
        let has_id = parsed.as_ref().is_some_and(|v| v.get("id").is_some());
        let is_handshake = parsed.as_ref().is_some_and(|v| {
            matches!(
                v.get("method").and_then(Value::as_str),
                Some("initialize" | "notifications/initialized")
            )
        });
        if link
            .as_ref()
            .is_some_and(|l| l.broken.load(Ordering::SeqCst))
        {
            link = None;
        }
        let reconnecting = link.is_none() && connections > 0;
        if link.is_none() {
            // A notification has nobody waiting for an answer: do not hold
            // the input for up to 30 s or start the DAW for it.
            let attempt = if has_id {
                connector.connect()
            } else {
                UnixStream::connect(&connector.cfg.socket)
                    .map_err(|e| ConnectError::Other(e.to_string()))
            };
            match attempt {
                Ok(stream) => {
                    connections += 1;
                    let broken = Arc::new(AtomicBool::new(false));
                    let reader = stream.try_clone()?;
                    let (sh, br) = (Arc::clone(&shared), Arc::clone(&broken));
                    thread::Builder::new()
                        .name("relay-pump".into())
                        .spawn(move || pump(reader, sh, br))?;
                    link = Some(Link { stream, broken });
                }
                Err(e) => {
                    if let Some(reply) = error_reply(&line, &e) {
                        let mut out = lock(&shared.out);
                        writeln!(out, "{reply}")?;
                        out.flush()?;
                    }
                    continue;
                }
            }
        }
        let l = link.as_mut().expect("connected");
        if reconnecting && !is_handshake {
            // Continue the client's session on the new connection.
            if let Some((init, id)) = &handshake.initialize {
                *lock(&shared.swallow) = Some(id.clone());
                let mut w = &l.stream;
                let mut ok = w.write_all(init).is_ok();
                if ok && let Some(n) = &handshake.initialized {
                    ok = w.write_all(n).is_ok();
                }
                if !ok {
                    link = None;
                    continue;
                }
            }
        }
        note_handshake(&mut handshake, &line);
        let l = link.as_mut().expect("connected");
        let mut w = &l.stream;
        if w.write_all(&line).is_err() {
            l.broken.store(true, Ordering::SeqCst);
            let e = ConnectError::Other("the connection to LibreDAW closed".into());
            if let Some(reply) = error_reply(&line, &e) {
                let mut out = lock(&shared.out);
                writeln!(out, "{reply}")?;
                out.flush()?;
            }
        }
    }
    if let Some(l) = link {
        let _ = l.stream.shutdown(Shutdown::Write);
        // Let the DAW's last answers through before leaving.
        let end = Instant::now() + Duration::from_millis(200);
        while !l.broken.load(Ordering::SeqCst) && Instant::now() < end {
            thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(connections)
}
