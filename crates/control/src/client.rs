// SPDX-License-Identifier: GPL-3.0-or-later
//! The accept loop and the per-client threads: a reader that checks the
//! hello, the capability mask, and the size limits (17.1), and a writer
//! that sends reply lines so the GTK thread never blocks on a socket.

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use protocol::consts::{MAX_EDITS_PER_REQUEST, MAX_REQUEST_LINE_BYTES};
use protocol::control::{
    ControlError, Outcome, Reply, Request, RequestBody, Transport, agent_string,
};
use serde_json::{Value, json};

use crate::state::{HELLO_TIMEOUT, HelloErr, Out, Shared};

/// The hello is a short line; anything longer is not a hello.
const MAX_HELLO_BYTES: usize = 4096;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// After a protocol violation we swallow this much input so the peer can
/// still read our error before the connection resets.
const DRAIN_BYTES: usize = 8 << 20;
const DRAIN_TIME: Duration = Duration::from_secs(1);

pub fn accept_loop(listener: UnixListener, shared: Arc<Shared>) {
    for stream in listener.incoming() {
        if shared.is_shutting_down() {
            break;
        }
        let Ok(stream) = stream else { continue };
        let Some(id) = shared.next_client_id() else {
            continue;
        };
        let shared = Arc::clone(&shared);
        let _ = thread::Builder::new()
            .name(format!("control-client-{id}"))
            .spawn(move || run_client(shared, stream, id));
    }
}

enum ReadLine {
    Line(String),
    TooLong,
    NotUtf8,
    Timeout,
    Closed,
}

struct LineReader {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl LineReader {
    fn read_line(&mut self, limit: usize) -> ReadLine {
        loop {
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                if pos > limit {
                    return ReadLine::TooLong;
                }
                let rest = self.buf.split_off(pos + 1);
                let mut line = std::mem::replace(&mut self.buf, rest);
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return match String::from_utf8(line) {
                    Ok(s) => ReadLine::Line(s),
                    Err(_) => ReadLine::NotUtf8,
                };
            }
            if self.buf.len() > limit {
                return ReadLine::TooLong;
            }
            let mut chunk = [0u8; 16 * 1024];
            match self.stream.read(&mut chunk) {
                Ok(0) => return ReadLine::Closed,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return ReadLine::Timeout;
                }
                Err(_) => return ReadLine::Closed,
            }
        }
    }

    fn drain(&mut self) {
        let end = Instant::now() + DRAIN_TIME;
        let mut total = 0;
        let mut chunk = [0u8; 16 * 1024];
        while total < DRAIN_BYTES {
            let now = Instant::now();
            if now >= end || self.stream.set_read_timeout(Some(end - now)).is_err() {
                break;
            }
            match self.stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => total += n,
            }
        }
    }
}

fn writer_loop(mut stream: UnixStream, rx: Receiver<Out>) {
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    for out in rx {
        match out {
            Out::Line(mut l) => {
                l.push('\n');
                if stream.write_all(l.as_bytes()).is_err() {
                    let _ = stream.shutdown(Shutdown::Both);
                    return;
                }
            }
            Out::Close => break,
        }
    }
    let _ = stream.shutdown(Shutdown::Write);
}

fn error_line(id: u64, error: ControlError) -> String {
    let r = Reply {
        id,
        outcome: Outcome::Err { error },
    };
    serde_json::to_string(&r).unwrap_or_default()
}

fn hello_err_line(reason: &str) -> String {
    json!({"hello_err": {"reason": reason}}).to_string()
}

/// `None` is `bad_hello`.
fn parse_hello(line: &str) -> Option<(Transport, String)> {
    let v: Value = serde_json::from_str(line).ok()?;
    let h = v.get("hello")?;
    let transport = match h.get("transport")?.as_str()? {
        "agent" => Transport::Agent,
        "script" => Transport::Script,
        _ => return None,
    };
    let name = agent_string(h.get("client")?.as_str()?);
    Some((transport, name))
}

fn run_client(shared: Arc<Shared>, stream: UnixStream, id: u64) {
    let (Ok(read_half), Ok(write_half), Ok(kill_half)) =
        (stream.try_clone(), stream.try_clone(), stream.try_clone())
    else {
        return;
    };
    drop(stream);
    let (tx, rx) = channel::<Out>();
    let writer = thread::Builder::new()
        .name(format!("control-writer-{id}"))
        .spawn(move || writer_loop(write_half, rx));
    if writer.is_err() {
        return;
    }
    let mut reader = LineReader {
        stream: read_half,
        buf: Vec::new(),
    };
    let _ = reader.stream.set_read_timeout(Some(HELLO_TIMEOUT));
    let refuse = |tx: &Sender<Out>, reason: &str| {
        let _ = tx.send(Out::Line(hello_err_line(reason)));
        let _ = tx.send(Out::Close);
    };
    let hello = match reader.read_line(MAX_HELLO_BYTES) {
        ReadLine::Line(l) => l,
        ReadLine::TooLong | ReadLine::NotUtf8 => {
            refuse(&tx, "bad_hello");
            reader.drain();
            return;
        }
        ReadLine::Timeout | ReadLine::Closed => {
            let _ = tx.send(Out::Close);
            return;
        }
    };
    let Some((transport, name)) = parse_hello(&hello) else {
        refuse(&tx, "bad_hello");
        reader.drain();
        return;
    };
    let info = match shared.register(id, transport, name, tx.clone(), kill_half) {
        Ok(i) => i,
        Err(e) => {
            refuse(
                &tx,
                match e {
                    HelloErr::AgentsDisabled => "agents_disabled",
                    HelloErr::NotAllowed => "not_allowed",
                    HelloErr::BusyOwner => "busy_owner",
                },
            );
            return;
        }
    };
    let _ = tx.send(Out::Line(json!({"hello_ok": {"protocol": 1}}).to_string()));
    let _ = reader.stream.set_read_timeout(None);
    serve(&shared, &mut reader, &tx, info.id, transport);
    shared.unregister(id);
    let _ = tx.send(Out::Close);
}

fn serve(
    shared: &Shared,
    reader: &mut LineReader,
    tx: &Sender<Out>,
    id: u64,
    transport: Transport,
) {
    loop {
        let line = match reader.read_line(MAX_REQUEST_LINE_BYTES) {
            ReadLine::Line(l) => l,
            ReadLine::NotUtf8 => {
                send_err(tx, 0, bad_request("request is not UTF-8"));
                continue;
            }
            ReadLine::TooLong => {
                send_err(
                    tx,
                    0,
                    ControlError::TooLarge {
                        what: "request line".into(),
                        max: MAX_REQUEST_LINE_BYTES,
                    },
                );
                // Cannot resynchronise: close after the error.
                let _ = tx.send(Out::Close);
                reader.drain();
                return;
            }
            ReadLine::Timeout => continue,
            ReadLine::Closed => return,
        };
        if line.trim().is_empty() {
            continue;
        }
        match check_request(&line, transport) {
            Ok(req) => {
                if let Err(l) = shared.submit(id, req) {
                    let _ = tx.send(Out::Line(l));
                }
            }
            Err((rid, e)) => send_err(tx, rid, e),
        }
    }
}

fn send_err(tx: &Sender<Out>, id: u64, e: ControlError) {
    let _ = tx.send(Out::Line(error_line(id, e)));
}

fn bad_request(reason: &str) -> ControlError {
    ControlError::BadRequest {
        reason: reason
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect(),
    }
}

/// Parses one request line and applies the capability mask and the size
/// limits. The error carries the request id when it could be read.
fn check_request(line: &str, transport: Transport) -> Result<Request, (u64, ControlError)> {
    let value: Value = serde_json::from_str(line).map_err(|e| (0, bad_request(&e.to_string())))?;
    let id = value.get("id").and_then(Value::as_u64).unwrap_or(0);
    let req: Request =
        serde_json::from_value(value).map_err(|e| (id, bad_request(&e.to_string())))?;
    if !req.body.allowed_for(transport) {
        return Err((id, ControlError::NotAllowed));
    }
    if let RequestBody::Edit { edits } = &req.body
        && edits.len() > MAX_EDITS_PER_REQUEST
    {
        return Err((
            id,
            ControlError::TooLarge {
                what: "edits".into(),
                max: MAX_EDITS_PER_REQUEST,
            },
        ));
    }
    Ok(req)
}
