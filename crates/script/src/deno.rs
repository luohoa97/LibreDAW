// SPDX-License-Identifier: GPL-3.0-or-later
//! The Deno bridge (SPEC 10, 13.5).
//!
//! `run_script` starts `deno run` on a user script with no permission but
//! reading that one file, speaks newline-delimited JSON with it over
//! stdin/stdout, and turns its requests into control requests. LibreDAW never
//! ships or downloads deno; it must be on `PATH` (or `ScriptOptions::deno`).
//!
//! Wire format (every object carries `"lr": 1`):
//!
//! ```text
//! host   -> script  {"lr":1,"type":"init","api":1}
//! script -> host    {"lr":1,"type":"ready","api":1}
//! script -> host    {"lr":1,"id":N,"op":"project.get"}
//! script -> host    {"lr":1,"id":N,"op":"edit","edits":[..],"base_revision":R}
//! host   -> script  {"lr":1,"id":N,"ok":<ReplyBody>}  or  {"lr":1,"id":N,"err":<ControlError>}
//! ```
//!
//! Deadlines: the script must answer `init` (send `ready`) within
//! `ready_timeout`, and must finish within `max_runtime`. A script that
//! misses either is killed. There are no host-to-script events in
//! Milestone A, so there is nothing else for a script to answer yet.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use protocol::consts::{MAX_EDITS_PER_REQUEST, MAX_REQUEST_LINE_BYTES};
use protocol::control::{ControlError, Outcome, RequestBody};
use protocol::edit::Edit;
use serde_json::{Map, Value, json};

use crate::control::{Client, ClientError};

/// Scripting API version the bundled `libredaw.ts` implements.
pub const API_VERSION: u64 = 1;
/// Oldest deno major version we accept. LibreDAW pins this, not the user.
pub const MIN_DENO_MAJOR: u64 = 2;
/// The TypeScript module scripts import as `"libredaw"`.
pub const LIBREDAW_TS: &str = include_str!("../ts/libredaw.ts");

const STDERR_KEEP_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct ScriptOptions {
    /// The deno executable (`deno` on `PATH` by default).
    pub deno: PathBuf,
    /// Time allowed from spawn to the script's `ready` message (10: a script
    /// that does not answer within 2 s is killed).
    pub ready_timeout: Duration,
    /// Longest a script may run in total.
    pub max_runtime: Duration,
}

impl Default for ScriptOptions {
    fn default() -> ScriptOptions {
        ScriptOptions {
            deno: PathBuf::from("deno"),
            ready_timeout: Duration::from_secs(2),
            max_runtime: Duration::from_secs(60),
        }
    }
}

#[derive(Debug)]
pub enum ScriptError {
    /// `deno` was not found or could not be started.
    DenoMissing(io::Error),
    /// `deno --version` said something we cannot use.
    DenoVersion(String),
    /// The script path is not usable (missing, or contains a comma, which
    /// `--allow-read` cannot express).
    BadScriptPath(String),
    Io(io::Error),
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScriptError::DenoMissing(e) => write!(
                f,
                "cannot run deno ({e}); install deno {MIN_DENO_MAJOR} or newer and put it on PATH"
            ),
            ScriptError::DenoVersion(s) => write!(f, "unsupported deno: {s}"),
            ScriptError::BadScriptPath(s) => write!(f, "bad script path: {s}"),
            ScriptError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for ScriptError {}

#[derive(Debug, PartialEq, Eq)]
pub enum KillReason {
    /// No `ready` within `ready_timeout`.
    NoReady,
    /// Ran longer than `max_runtime`.
    MaxRuntime,
    /// Sent something that is not the protocol before `ready`, or a line
    /// longer than the request limit.
    BadMessage(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ScriptEnd {
    /// The process ended on its own. `None` if a signal ended it.
    Exited(Option<i32>),
    Killed(KillReason),
}

#[derive(Debug)]
pub struct ScriptReport {
    pub end: ScriptEnd,
    /// What the script wrote to stderr (including `console.log`), last 64 KiB.
    pub stderr: String,
    /// Requests the script made.
    pub requests: u32,
}

impl ScriptReport {
    pub fn succeeded(&self) -> bool {
        self.end == ScriptEnd::Exited(Some(0))
    }
}

/// Answers one control request on behalf of the script. The second argument
/// is the revision the script last saw.
pub type Handler<'a> = dyn FnMut(RequestBody, Option<u64>) -> Outcome + 'a;

/// A handler that forwards to a control socket client. Transport failures
/// become `ControlError::Internal` so the script sees a typed error.
pub fn client_handler(client: &mut Client) -> impl FnMut(RequestBody, Option<u64>) -> Outcome + '_ {
    move |body, base| match client.call(body, base) {
        Ok(o) => o,
        Err(e) => Outcome::Err {
            error: ControlError::Internal {
                reason: match e {
                    ClientError::Timeout => "LibreDAW did not answer in time".to_string(),
                    other => other.to_string(),
                },
            },
        },
    }
}

/// Checks that `deno` runs and is at least `MIN_DENO_MAJOR`. Returns the
/// version string.
pub fn check_deno(deno: &Path) -> Result<String, ScriptError> {
    let out = Command::new(deno)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(ScriptError::DenoMissing)?;
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next().unwrap_or("");
    let version = first
        .strip_prefix("deno ")
        .and_then(|s| s.split_whitespace().next())
        .ok_or_else(|| ScriptError::DenoVersion(first.chars().take(80).collect()))?;
    let major: u64 = version
        .split('.')
        .next()
        .and_then(|m| m.parse().ok())
        .ok_or_else(|| ScriptError::DenoVersion(version.to_string()))?;
    if major < MIN_DENO_MAJOR {
        return Err(ScriptError::DenoVersion(format!(
            "deno {version} is too old, need {MIN_DENO_MAJOR}.0 or newer"
        )));
    }
    Ok(version.to_string())
}

/// Runs `script` to completion and answers its requests with `handler`.
pub fn run_script(
    script: &Path,
    opts: &ScriptOptions,
    handler: &mut Handler<'_>,
) -> Result<ScriptReport, ScriptError> {
    let script = fs::canonicalize(script)
        .map_err(|e| ScriptError::BadScriptPath(format!("{}: {e}", script.display())))?;
    let script_str = script
        .to_str()
        .filter(|s| !s.contains(','))
        .ok_or_else(|| ScriptError::BadScriptPath("must be UTF-8 without commas".into()))?
        .to_string();
    check_deno(&opts.deno)?;

    let support = SupportDir::create().map_err(ScriptError::Io)?;

    let mut child = Command::new(&opts.deno)
        .arg("run")
        .arg("--no-prompt")
        // Deno denies everything not allowed; the explicit denies make the
        // intent visible and survive a changed default.
        .args([
            "--deny-net",
            "--deny-env",
            "--deny-run",
            "--deny-write",
            "--deny-sys",
            "--deny-ffi",
            "--deny-import",
        ])
        .arg(format!("--allow-read={script_str}"))
        .arg(format!("--import-map={}", support.import_map.display()))
        .args(["--no-remote", "--no-npm", "--no-config", "--no-lock"])
        .arg(&script_str)
        .env("DENO_NO_UPDATE_CHECK", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(ScriptError::DenoMissing)?;
    let start = Instant::now();

    let mut stdin = child.stdin.take().expect("piped");
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");

    let (tx, rx) = mpsc::channel::<Line>();
    let reader = thread::spawn(move || read_lines(stdout, tx));
    let stderr_thread = thread::spawn(move || capture_tail(stderr));

    let mut ready = false;
    let mut requests = 0u32;
    let mut killed: Option<KillReason> = None;

    // Send init at once; the pipe buffers it until the script reads.
    let init = json!({"lr": 1, "type": "init", "api": API_VERSION});
    let _ = write_line(&mut stdin, &init);

    loop {
        let now = Instant::now();
        let deadline = if ready {
            start + opts.max_runtime
        } else {
            (start + opts.ready_timeout).min(start + opts.max_runtime)
        };
        if now >= deadline {
            killed = Some(if ready {
                KillReason::MaxRuntime
            } else {
                KillReason::NoReady
            });
            break;
        }
        let line = match rx.recv_timeout(deadline - now) {
            Ok(Line::Text(l)) => l,
            Ok(Line::TooLong) => {
                killed = Some(KillReason::BadMessage("line too long".into()));
                break;
            }
            Ok(Line::Eof) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => continue,
        };
        let Ok(Value::Object(msg)) = serde_json::from_str::<Value>(&line) else {
            continue; // stray stdout text, not protocol
        };
        if msg.get("lr") != Some(&json!(1)) {
            continue;
        }
        match msg.get("type").and_then(Value::as_str) {
            Some("ready") => {
                ready = true;
                continue;
            }
            Some(_) => continue,
            None => {}
        }
        if !ready {
            killed = Some(KillReason::BadMessage("request before ready".into()));
            break;
        }
        requests += 1;
        let reply = answer(&msg, handler);
        if write_line(&mut stdin, &reply).is_err() {
            // The script is gone; the loop sees EOF next.
        }
    }

    drop(stdin);
    let mut status = None;
    if killed.is_none() {
        // The script closed stdout; give it until `max_runtime` to exit.
        match wait_until(&mut child, start + opts.max_runtime) {
            Some(s) => status = Some(s),
            None => killed = Some(KillReason::MaxRuntime),
        }
    }
    if killed.is_some() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = reader.join();
    let stderr = stderr_thread.join().unwrap_or_default();
    let end = match killed {
        Some(r) => ScriptEnd::Killed(r),
        None => ScriptEnd::Exited(status.and_then(|s| s.code())),
    };
    Ok(ScriptReport {
        end,
        stderr,
        requests,
    })
}

/// Builds the reply object for one script request.
fn answer(msg: &Map<String, Value>, handler: &mut Handler<'_>) -> Value {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let outcome = match script_request(msg) {
        Ok((body, base)) => handler(body, base),
        Err(reason) => Outcome::Err {
            error: ControlError::BadRequest { reason },
        },
    };
    match outcome {
        Outcome::Ok { body } => {
            json!({"lr": 1, "id": id, "ok": serde_json::to_value(body).unwrap_or(Value::Null)})
        }
        Outcome::Err { error } => {
            json!({"lr": 1, "id": id, "err": serde_json::to_value(error).unwrap_or(Value::Null)})
        }
    }
}

/// Maps a script request to a control request. Milestone A: `project.get`
/// and `edit` only.
fn script_request(msg: &Map<String, Value>) -> Result<(RequestBody, Option<u64>), String> {
    if !msg.get("id").is_some_and(Value::is_u64) {
        return Err("request has no numeric id".into());
    }
    let op = msg.get("op").and_then(Value::as_str).unwrap_or("");
    match op {
        "project.get" => Ok((RequestBody::ProjectGet, None)),
        "edit" => {
            let edits = msg.get("edits").cloned().unwrap_or(Value::Null);
            let edits: Vec<Edit> =
                serde_json::from_value(edits).map_err(|e| format!("invalid edits: {e}"))?;
            if edits.len() > MAX_EDITS_PER_REQUEST {
                return Err(format!(
                    "{} edits in one batch, at most {MAX_EDITS_PER_REQUEST}",
                    edits.len()
                ));
            }
            let base = match msg.get("base_revision") {
                None | Some(Value::Null) => None,
                Some(v) => Some(v.as_u64().ok_or("base_revision must be a number")?),
            };
            Ok((RequestBody::Edit { edits }, base))
        }
        other => Err(format!(
            "unknown op {:?}",
            other.chars().take(40).collect::<String>()
        )),
    }
}

fn write_line(w: &mut impl Write, v: &Value) -> io::Result<()> {
    let mut s = v.to_string();
    s.push('\n');
    w.write_all(s.as_bytes())?;
    w.flush()
}

/// Waits for the child to exit; `None` if `deadline` passes first.
fn wait_until(child: &mut Child, deadline: Instant) -> Option<std::process::ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return Some(s),
            Ok(None) => {}
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

enum Line {
    Text(String),
    TooLong,
    Eof,
}

/// Reads lines of at most `MAX_REQUEST_LINE_BYTES` from the script.
fn read_lines(stdout: impl Read, tx: mpsc::Sender<Line>) {
    let mut r = BufReader::new(stdout);
    loop {
        let mut buf = Vec::new();
        let n = r
            .by_ref()
            .take(MAX_REQUEST_LINE_BYTES as u64 + 1)
            .read_until(b'\n', &mut buf)
            .unwrap_or_default();
        if n == 0 {
            let _ = tx.send(Line::Eof);
            return;
        }
        if buf.len() > MAX_REQUEST_LINE_BYTES && buf.last() != Some(&b'\n') {
            let _ = tx.send(Line::TooLong);
            return;
        }
        while matches!(buf.last(), Some(b'\n' | b'\r')) {
            buf.pop();
        }
        if tx
            .send(Line::Text(String::from_utf8_lossy(&buf).into_owned()))
            .is_err()
        {
            return;
        }
    }
}

/// Reads stderr to the end, keeping the last `STDERR_KEEP_BYTES`.
fn capture_tail(mut stderr: impl Read) -> String {
    let mut kept: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    while let Ok(n) = stderr.read(&mut chunk) {
        if n == 0 {
            break;
        }
        kept.extend_from_slice(&chunk[..n]);
        if kept.len() > 2 * STDERR_KEEP_BYTES {
            let cut = kept.len() - STDERR_KEEP_BYTES;
            kept.drain(..cut);
        }
    }
    if kept.len() > STDERR_KEEP_BYTES {
        let cut = kept.len() - STDERR_KEEP_BYTES;
        kept.drain(..cut);
    }
    String::from_utf8_lossy(&kept).into_owned()
}

/// A private directory holding `libredaw.ts` and the import map that makes
/// `import ... from "libredaw"` work. Removed on drop.
struct SupportDir {
    dir: PathBuf,
    import_map: PathBuf,
}

impl SupportDir {
    fn create() -> io::Result<SupportDir> {
        use std::os::unix::fs::DirBuilderExt;
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let base = match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
            Some(d) => PathBuf::from(d).join("libredaw"),
            None => std::env::temp_dir(),
        };
        let name = format!(
            "script-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let dir = base.join(name);
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        let ts = dir.join("libredaw.ts");
        fs::write(&ts, LIBREDAW_TS)?;
        let url = file_url(&ts);
        let import_map = dir.join("import-map.json");
        fs::write(
            &import_map,
            json!({"imports": {"libredaw": url}}).to_string(),
        )?;
        Ok(SupportDir { dir, import_map })
    }
}

impl Drop for SupportDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// `file://` URL for an absolute path, percent-encoding unusual bytes.
fn file_url(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::from("file://");
    for &b in p.as_os_str().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                s.push(b as char)
            }
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_url_escapes() {
        assert_eq!(file_url(Path::new("/a b/c.ts")), "file:///a%20b/c.ts");
    }

    #[test]
    fn edit_requests_are_typed() {
        let m = json!({"lr":1,"id":3,"op":"edit","base_revision":4,
            "edits":[{"edit":"set_tempo","bpm":140.0}]});
        let Value::Object(m) = m else { unreachable!() };
        let (body, base) = script_request(&m).unwrap();
        assert_eq!(base, Some(4));
        assert_eq!(
            body,
            RequestBody::Edit {
                edits: vec![Edit::SetTempo { bpm: 140.0 }]
            }
        );
        let bad = json!({"lr":1,"id":3,"op":"edit","edits":[{"edit":"explode"}]});
        let Value::Object(bad) = bad else {
            unreachable!()
        };
        assert!(script_request(&bad).unwrap_err().contains("invalid edits"));
        let unknown = json!({"lr":1,"id":3,"op":"transport.play"});
        let Value::Object(unknown) = unknown else {
            unreachable!()
        };
        assert!(script_request(&unknown).is_err());
    }
}
