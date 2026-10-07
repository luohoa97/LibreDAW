// SPDX-License-Identifier: GPL-3.0-or-later
//! Client for the DAW control socket (17.1, docs/phase2-interfaces.md).
//!
//! One `Request` per line out, one `Reply` per line in, matched by id. The
//! first line is a hello. Calls are blocking with a deadline. After a
//! timeout or an I/O error the client is broken and every later call
//! fails with `Closed`: a half-read line cannot be resynchronised, so the
//! caller drops the client and connects again.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use protocol::control::{Outcome, Reply, Request, RequestBody, Transport};
use serde_json::{Value, json};

/// Control protocol revision this client speaks (hello reply).
pub const CONTROL_PROTOCOL: u64 = 1;
/// Longest reply line accepted (a whole project snapshot fits easily).
pub const MAX_REPLY_LINE_BYTES: usize = 64 << 20;

#[derive(Debug)]
pub enum ClientError {
    /// No socket, or nobody listening.
    Connect(io::Error),
    Io(io::Error),
    Timeout,
    /// The peer sent something that is not the protocol.
    Protocol(String),
    /// The connection is closed or was marked broken by an earlier error.
    Closed,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Connect(e) => write!(f, "cannot connect to LibreDAW: {e}"),
            ClientError::Io(e) => write!(f, "control socket I/O error: {e}"),
            ClientError::Timeout => write!(f, "LibreDAW did not answer in time"),
            ClientError::Protocol(s) => write!(f, "control protocol error: {s}"),
            ClientError::Closed => write!(f, "control connection closed"),
        }
    }
}

impl std::error::Error for ClientError {}

/// `$XDG_RUNTIME_DIR/libredaw/control.sock`, or `None` if the variable is
/// unset (never fall back to `/tmp`, 17.1).
pub fn default_socket_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty())?;
    Some(PathBuf::from(dir).join("libredaw").join("control.sock"))
}

pub struct Client {
    stream: UnixStream,
    /// Bytes read from the socket but not yet consumed as a line.
    buf: Vec<u8>,
    next_id: u64,
    timeout: Duration,
    broken: bool,
}

impl Client {
    /// Connects and completes the hello within `timeout`. `timeout` is also
    /// the default per-call deadline.
    pub fn connect(
        path: &Path,
        transport: Transport,
        client_name: &str,
        timeout: Duration,
    ) -> Result<Client, ClientError> {
        let stream = UnixStream::connect(path).map_err(ClientError::Connect)?;
        let mut c = Client {
            stream,
            buf: Vec::new(),
            next_id: 1,
            timeout,
            broken: false,
        };
        let kind = match transport {
            Transport::Script => "script",
            Transport::Agent => "agent",
        };
        let hello = json!({"hello": {"transport": kind, "client": client_name}});
        c.send_line(&hello.to_string())?;
        let line = c.read_line(Instant::now() + timeout)?;
        let v: Value = serde_json::from_str(&line)
            .map_err(|_| ClientError::Protocol(truncate_for_error(&line)))?;
        match v.pointer("/hello_ok/protocol").and_then(Value::as_u64) {
            Some(CONTROL_PROTOCOL) => Ok(c),
            Some(other) => Err(ClientError::Protocol(format!(
                "LibreDAW speaks control protocol {other}, this client speaks {CONTROL_PROTOCOL}"
            ))),
            None => Err(ClientError::Protocol(truncate_for_error(&line))),
        }
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    pub fn is_broken(&self) -> bool {
        self.broken
    }

    /// Sends one request and waits for the reply with the same id.
    pub fn call(
        &mut self,
        body: RequestBody,
        base_revision: Option<u64>,
    ) -> Result<Outcome, ClientError> {
        let timeout = self.timeout;
        self.call_with_timeout(body, base_revision, timeout)
    }

    pub fn call_with_timeout(
        &mut self,
        body: RequestBody,
        base_revision: Option<u64>,
        timeout: Duration,
    ) -> Result<Outcome, ClientError> {
        if self.broken {
            return Err(ClientError::Closed);
        }
        let id = self.next_id;
        self.next_id += 1;
        let req = Request {
            id,
            base_revision,
            body,
        };
        let line = serde_json::to_string(&req).map_err(|e| ClientError::Protocol(e.to_string()))?;
        self.send_line(&line)?;
        let deadline = Instant::now() + timeout;
        loop {
            let line = self.read_line(deadline)?;
            let reply: Reply = match serde_json::from_str(&line) {
                Ok(r) => r,
                Err(e) => {
                    self.broken = true;
                    return Err(ClientError::Protocol(format!(
                        "bad reply ({e}): {}",
                        truncate_for_error(&line)
                    )));
                }
            };
            if reply.id == id {
                return Ok(reply.outcome);
            }
            // A reply to an earlier request we stopped waiting for: skip it.
        }
    }

    fn send_line(&mut self, line: &str) -> Result<(), ClientError> {
        let mut data = Vec::with_capacity(line.len() + 1);
        data.extend_from_slice(line.as_bytes());
        data.push(b'\n');
        self.stream
            .set_write_timeout(Some(self.timeout.max(Duration::from_millis(1))))
            .map_err(ClientError::Io)?;
        if let Err(e) = self.stream.write_all(&data) {
            self.broken = true;
            return Err(map_io(e));
        }
        Ok(())
    }

    fn read_line(&mut self, deadline: Instant) -> Result<String, ClientError> {
        loop {
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                let rest = self.buf.split_off(pos + 1);
                let mut line = std::mem::replace(&mut self.buf, rest);
                line.pop();
                return String::from_utf8(line).map_err(|_| {
                    self.broken = true;
                    ClientError::Protocol("reply is not UTF-8".into())
                });
            }
            if self.buf.len() > MAX_REPLY_LINE_BYTES {
                self.broken = true;
                return Err(ClientError::Protocol("reply line too long".into()));
            }
            let now = Instant::now();
            if now >= deadline {
                self.broken = true;
                return Err(ClientError::Timeout);
            }
            self.stream
                .set_read_timeout(Some(deadline - now))
                .map_err(ClientError::Io)?;
            let mut chunk = [0u8; 16 * 1024];
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    self.broken = true;
                    return Err(ClientError::Closed);
                }
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.broken = true;
                    return Err(map_io(e));
                }
            }
        }
    }
}

fn map_io(e: io::Error) -> ClientError {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => ClientError::Timeout,
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset => ClientError::Closed,
        _ => ClientError::Io(e),
    }
}

fn truncate_for_error(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(200).collect()
}
