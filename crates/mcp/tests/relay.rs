// SPDX-License-Identifier: GPL-3.0-or-later
//! The relay end to end: MCP bytes in through stdio, across the real Unix
//! socket to the real control server, answered by a fake UI loop
//! (`control::fake_ui`). No GTK.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use control::fake_ui::{Action, FakeUi};
use control::{ControlConfig, ControlServer, Incoming, PROTOCOL_VERSION};
use mcp::relay::{self, Config};
use protocol::control::{Outcome, ReplyBody, RequestBody};
use protocol::model::Project;
use serde_json::{Value, json};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("ldaw-relay-{}-{n}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn daw(dir: &Path, agents: bool) -> FakeUi {
    let mut cfg = ControlConfig::new(dir.join("libredaw"));
    cfg.agents_enabled = agents;
    cfg.agents_wait = Duration::from_millis(200);
    let server = ControlServer::start(cfg).expect("server");
    FakeUi::start(server, |inc: &Incoming| match &inc.request.body {
        RequestBody::ProjectGet => Action::Reply(Outcome::Ok {
            body: ReplyBody::Project {
                revision: 1,
                project: Arc::new(Project::empty()),
                next_id: 1,
            },
        }),
        _ => Action::Reply(Outcome::Ok {
            body: ReplyBody::Done,
        }),
    })
}

fn socket_of(dir: &Path) -> PathBuf {
    dir.join("libredaw").join("control.sock")
}

fn config(dir: &Path) -> Config {
    let mut c = Config::new(socket_of(dir));
    c.startup_wait = Duration::from_secs(5);
    c.retry_interval = Duration::from_millis(20);
    c
}

/// The relay running in a thread, fed and read through socket pairs.
struct Relay {
    to_relay: UnixStream,
    from_relay: BufReader<UnixStream>,
    thread: Option<JoinHandle<u32>>,
    next: u64,
}

impl Relay {
    fn start(cfg: Config) -> Relay {
        let (client_in, relay_in) = UnixStream::pair().unwrap();
        let (relay_out, client_out) = UnixStream::pair().unwrap();
        client_out
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let thread =
            thread::spawn(move || relay::run(cfg, BufReader::new(relay_in), relay_out).unwrap());
        Relay {
            to_relay: client_in,
            from_relay: BufReader::new(client_out),
            thread: Some(thread),
            next: 1,
        }
    }

    fn send(&mut self, v: &Value) {
        writeln!(self.to_relay, "{v}").unwrap();
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.from_relay.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|_| panic!("not JSON: {line:?}"))
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let m = self.read();
            if m["id"] == json!(id) && m.get("method").is_none() {
                return m;
            }
        }
    }

    fn init(&mut self) -> Value {
        let r = self.rpc(
            "initialize",
            json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {},
                   "clientInfo": {"name": "relay-test", "version": "1"}}),
        );
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        r
    }

    /// Ends the input and returns how many connections the relay made.
    fn finish(mut self) -> u32 {
        drop(std::mem::replace(
            &mut self.to_relay,
            UnixStream::pair().unwrap().0,
        ));
        self.thread.take().unwrap().join().unwrap()
    }
}

#[test]
fn mcp_flows_through_the_relay_to_the_real_control_server() {
    let dir = temp_dir("flow");
    let ui = daw(&dir, true);
    let mut r = Relay::start(config(&dir));
    let init = r.init();
    assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(init["result"]["serverInfo"]["name"], "libredaw");
    let tools = r.rpc("tools/list", json!({}));
    assert!(
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "beat_grid_set")
    );
    let play = r.rpc("tools/call", json!({"name": "play", "arguments": {}}));
    assert_eq!(play["result"]["isError"], false, "{play}");
    assert!(
        ui.requests()
            .iter()
            .any(|q| matches!(q.body, RequestBody::Play))
    );
    let sum = r.rpc(
        "tools/call",
        json!({"name": "project_summary", "arguments": {}}),
    );
    assert!(
        sum["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("LibreDAW r1")
    );
    assert_eq!(r.finish(), 1);
}

#[test]
fn agents_disabled_reaches_the_client_as_a_clear_error() {
    let dir = temp_dir("disabled");
    let ui = daw(&dir, false);
    let mut r = Relay::start(config(&dir));
    let init = r.init();
    assert_eq!(init["error"]["data"]["reason"], "agents_disabled", "{init}");
    assert!(
        init["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not enabled")
    );
    let end = Instant::now() + Duration::from_secs(5);
    while !ui
        .events()
        .iter()
        .any(|e| matches!(e, control::UiEvent::AgentRequestedControl { .. }))
    {
        assert!(Instant::now() < end, "no AgentRequestedControl event");
        thread::sleep(Duration::from_millis(10));
    }
    r.finish();
}

fn fake_daw_script(dir: &Path, marker: &Path) -> PathBuf {
    let p = dir.join("fake-libredaw");
    let tmp = dir.join(".fake-libredaw.tmp");
    std::fs::write(
        &tmp,
        format!(
            "#!/bin/sh\n# SPDX-License-Identifier: GPL-3.0-or-later\necho \"$@\" >> '{}'\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&tmp, &p).unwrap();
    p
}

#[test]
fn starts_the_daw_with_agent_request_and_connects_when_it_appears() {
    let dir = temp_dir("start");
    let marker = dir.join("started");
    let mut cfg = config(&dir);
    cfg.daw_command = fake_daw_script(&dir, &marker).into();
    cfg.startup_wait = Duration::from_secs(10);
    // The "DAW" binds its socket a moment after it was started.
    let (m2, d2) = (marker.clone(), dir.clone());
    let t = thread::spawn(move || {
        while !m2.exists() {
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(150));
        daw(&d2, true)
    });
    let mut r = Relay::start(cfg);
    let init = r.init();
    assert_eq!(
        init["result"]["protocolVersion"], PROTOCOL_VERSION,
        "{init}"
    );
    let _ui = t.join().unwrap();
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().trim(),
        "--agent-request"
    );
    r.finish();
}

#[test]
fn gives_up_after_the_wait_with_a_waiting_message_and_starts_the_daw_once() {
    let dir = temp_dir("giveup");
    let marker = dir.join("started");
    let mut cfg = config(&dir);
    cfg.daw_command = fake_daw_script(&dir, &marker).into();
    cfg.startup_wait = Duration::from_millis(700);
    let mut r = Relay::start(cfg);
    let t = Instant::now();
    let init = r.init();
    assert!(
        t.elapsed() >= Duration::from_millis(650),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(init["error"]["data"]["reason"], "waiting_for_user");
    assert_eq!(init["error"]["message"], "waiting for the user in LibreDAW");
    // One attempt to start it within this wait.
    r.finish();
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);
}

#[test]
fn missing_daw_program_is_reported() {
    let dir = temp_dir("nodaw");
    let mut cfg = config(&dir);
    cfg.daw_command = "/nonexistent/libredaw".into();
    let mut r = Relay::start(cfg);
    let init = r.init();
    assert_eq!(init["error"]["data"]["reason"], "daw_not_running", "{init}");
    r.finish();
}

#[test]
fn a_restarted_daw_continues_the_session() {
    let dir = temp_dir("restart");
    let ui = daw(&dir, true);
    let mut r = Relay::start(config(&dir));
    r.init();
    assert_eq!(
        r.rpc("tools/call", json!({"name": "play", "arguments": {}}))["result"]["isError"],
        false
    );
    // The DAW quits and a new one starts at the same place.
    drop(ui);
    let ui2 = daw(&dir, true);
    // The client does not initialize again; the relay replays its handshake.
    let play = r.rpc("tools/call", json!({"name": "stop", "arguments": {}}));
    assert_eq!(play["result"]["isError"], false, "{play}");
    assert!(
        ui2.requests()
            .iter()
            .any(|q| matches!(q.body, RequestBody::Stop))
    );
    assert_eq!(r.finish(), 2);
}

#[test]
fn the_binary_relays_stdio() {
    let dir = temp_dir("bin");
    let ui = daw(&dir, true);
    let mut child = Command::new(env!("CARGO_BIN_EXE_libredaw-mcp"))
        .env("XDG_RUNTIME_DIR", &dir)
        .env_remove("FLATPAK_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    fn ask(
        stdin: &mut std::process::ChildStdin,
        out: &mut BufReader<std::process::ChildStdout>,
        v: Value,
    ) -> Value {
        writeln!(stdin, "{v}").unwrap();
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
    let r = ask(
        &mut stdin,
        &mut out,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}),
    );
    assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSION);
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .unwrap();
    let r = ask(
        &mut stdin,
        &mut out,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "play", "arguments": {}}}),
    );
    assert_eq!(r["result"]["isError"], false);
    drop(stdin);
    assert!(child.wait().unwrap().success());
    assert!(
        ui.requests()
            .iter()
            .any(|q| matches!(q.body, RequestBody::Play))
    );
}

#[test]
fn the_binary_needs_xdg_runtime_dir() {
    let out = Command::new(env!("CARGO_BIN_EXE_libredaw-mcp"))
        .env_remove("XDG_RUNTIME_DIR")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("XDG_RUNTIME_DIR"));
}
