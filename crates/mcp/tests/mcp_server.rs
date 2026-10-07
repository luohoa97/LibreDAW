// SPDX-License-Identifier: GPL-3.0-or-later
//! The MCP server against a fake control socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use mcp::PROTOCOL_VERSION;
use mcp::conn::Config;
use mcp::server::Server;
use protocol::control::{ControlError, Outcome, ReplyBody, Request, RequestBody};
use protocol::edit::{Applied, Edit};
use protocol::ids::{ChannelId, PatternId};
use protocol::model::{Pattern, Project};
use serde_json::{Value, json};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("libredaw-mcp-test-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

type Log = Arc<Mutex<Vec<(Request,)>>>;

struct Fake {
    socket: PathBuf,
    log: Log,
    hellos: Arc<Mutex<Vec<Value>>>,
}

impl Fake {
    fn requests(&self) -> Vec<Request> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|(r,)| r.clone())
            .collect()
    }
}

/// A fake DAW: answers hello, then every request through `handler`.
fn spawn_fake(
    socket: &Path,
    hello_reply: Value,
    mut handler: impl FnMut(&Request) -> Outcome + Send + 'static,
) -> Fake {
    let listener = UnixListener::bind(socket).unwrap();
    let log: Log = Arc::default();
    let hellos: Arc<Mutex<Vec<Value>>> = Arc::default();
    let (l2, h2) = (log.clone(), hellos.clone());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let mut w = stream.try_clone().unwrap();
            let mut r = BufReader::new(stream);
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            h2.lock()
                .unwrap()
                .push(serde_json::from_str(&line).unwrap());
            writeln!(w, "{hello_reply}").unwrap();
            if hello_reply.get("hello_err").is_some() {
                continue;
            }
            loop {
                line.clear();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let req: Request = serde_json::from_str(&line).unwrap();
                let outcome = handler(&req);
                l2.lock().unwrap().push((req.clone(),));
                let reply = protocol::control::Reply {
                    id: req.id,
                    outcome,
                };
                writeln!(w, "{}", serde_json::to_string(&reply).unwrap()).unwrap();
            }
        }
    });
    Fake {
        socket: socket.to_path_buf(),
        log,
        hellos,
    }
}

fn ok_hello() -> Value {
    json!({"hello_ok": {"protocol": 1}})
}

fn config(socket: &Path) -> Config {
    let mut c = Config::new(socket.to_path_buf());
    c.startup_wait = Duration::from_millis(600);
    c.retry_interval = Duration::from_millis(20);
    c.call_timeout = Duration::from_secs(5);
    c
}

fn rpc(server: &mut Server, id: u64, method: &str, params: Value) -> Value {
    let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let out = server.handle_line(&line.to_string()).expect("a reply");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["id"], json!(id));
    v
}

fn init(server: &mut Server) {
    let r = rpc(
        server,
        1,
        "initialize",
        json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {},
               "clientInfo": {"name": "test", "version": "0"}}),
    );
    assert!(r["error"].is_null(), "{r}");
    assert!(
        server
            .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none()
    );
}

fn call(server: &mut Server, name: &str, args: Value) -> Value {
    let r = rpc(
        server,
        7,
        "tools/call",
        json!({"name": name, "arguments": args}),
    );
    assert!(r["error"].is_null(), "{r}");
    r["result"].clone()
}

fn project_with_pattern(len: u8) -> Outcome {
    let mut p = Project::empty();
    let mut pat = Pattern::new(PatternId(2), "A \u{7}name".into());
    pat.length_steps = len;
    p.patterns.push(Arc::new(pat));
    Outcome::Ok {
        body: ReplyBody::Project {
            revision: 5,
            project: Arc::new(p),
        },
    }
}

fn applied(revision: u64, created: Vec<u32>) -> Outcome {
    Outcome::Ok {
        body: ReplyBody::Applied(Applied { revision, created }),
    }
}

#[test]
fn initialize_and_list_tools() {
    let dir = temp_dir("init");
    let mut s = Server::new(config(&dir.join("none.sock")));
    init(&mut s);
    let r = rpc(&mut s, 2, "tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for want in [
        "project_list",
        "project_new",
        "project_open",
        "project_save",
        "project_info",
        "project_get",
        "play",
        "stop",
        "set_playing_pattern",
        "transport_state",
        "edit",
        "set_tempo",
        "channel_add",
        "steps_set",
        "notes_add",
        "notes_remove",
        "notes_list",
        "track_set",
        "undo",
        "redo",
        "history",
        "export_wav",
        "analyze",
        "job_status",
        "job_result",
        "job_cancel",
        "settings_get",
        "settings_set",
        "plugin_scan",
        "plugin_list",
    ] {
        assert!(names.contains(&want), "missing {want}");
    }
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object");
        assert!(t["description"].as_str().unwrap().len() > 10);
        assert!(t["inputSchema"]["required"].is_array());
    }
    let desc = tools.iter().find(|t| t["name"] == "project_get").unwrap()["description"]
        .as_str()
        .unwrap();
    assert!(desc.contains("960") && desc.contains("MIDI"));
}

#[test]
fn initialize_result_names_the_pinned_revision() {
    let dir = temp_dir("initres");
    let mut s = Server::new(config(&dir.join("none.sock")));
    let r = rpc(
        &mut s,
        1,
        "initialize",
        json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
    );
    assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert!(r["result"]["capabilities"]["tools"].is_object());
    assert_eq!(r["result"]["serverInfo"]["name"], "libredaw-mcp");
    assert!(
        r["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("960")
    );
}

#[test]
fn unknown_protocol_version_is_an_error_naming_the_supported_one() {
    let dir = temp_dir("badver");
    let mut s = Server::new(config(&dir.join("none.sock")));
    let r = rpc(
        &mut s,
        1,
        "initialize",
        json!({"protocolVersion": "1999-01-01", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
    );
    assert_eq!(r["error"]["code"], -32602);
    assert_eq!(r["error"]["data"]["supported"], json!([PROTOCOL_VERSION]));
    assert_eq!(r["error"]["data"]["requested"], "1999-01-01");
    // Still not initialized.
    let r = rpc(&mut s, 2, "tools/list", json!({}));
    assert_eq!(r["error"]["code"], -32002);
}

#[test]
fn calls_before_initialize_are_refused_but_ping_works() {
    let dir = temp_dir("preinit");
    let mut s = Server::new(config(&dir.join("none.sock")));
    for (m, p) in [
        ("tools/list", json!({})),
        ("tools/call", json!({"name": "play"})),
    ] {
        let r = rpc(&mut s, 3, m, p);
        assert_eq!(r["error"]["code"], -32002, "{m}");
    }
    let r = rpc(&mut s, 4, "ping", json!({}));
    assert_eq!(r["result"], json!({}));
}

#[test]
fn malformed_json_and_unknown_methods() {
    let dir = temp_dir("malformed");
    let mut s = Server::new(config(&dir.join("none.sock")));
    let out: Value = serde_json::from_str(&s.handle_line("{nope").unwrap()).unwrap();
    assert_eq!(out["error"]["code"], -32700);
    assert!(out["id"].is_null());
    init(&mut s);
    let r = rpc(&mut s, 9, "resources/list", json!({}));
    assert_eq!(r["error"]["code"], -32601);
    let r = rpc(
        &mut s,
        10,
        "tools/call",
        json!({"name": "format_disk", "arguments": {}}),
    );
    assert_eq!(r["error"]["code"], -32602);
}

#[test]
fn play_reaches_the_daw_as_an_agent_with_a_hello() {
    let dir = temp_dir("play");
    let fake = spawn_fake(&dir.join("control.sock"), ok_hello(), |_| Outcome::Ok {
        body: ReplyBody::Done,
    });
    let mut s = Server::new(config(&fake.socket));
    init(&mut s);
    let r = call(&mut s, "play", json!({}));
    assert_eq!(r["isError"], false);
    assert_eq!(r["structuredContent"], json!({"kind": "done"}));
    assert_eq!(r["content"][0]["type"], "text");
    assert_eq!(fake.requests()[0].body, RequestBody::Play);
    assert_eq!(
        fake.hellos.lock().unwrap()[0],
        json!({"hello": {"transport": "agent", "client": "libredaw-mcp"}})
    );
}

#[test]
fn edits_carry_the_revision_the_agent_last_saw() {
    let dir = temp_dir("rev");
    let fake = spawn_fake(&dir.join("control.sock"), ok_hello(), |req| {
        match &req.body {
            RequestBody::ProjectGet => project_with_pattern(16),
            RequestBody::Edit { .. } => applied(req.base_revision.unwrap() + 1, vec![9]),
            other => panic!("{other:?}"),
        }
    });
    let mut s = Server::new(config(&fake.socket));
    init(&mut s);
    // Never read the project: the server learns the revision first.
    let r = call(&mut s, "set_tempo", json!({"bpm": 140}));
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["created"], json!([9]));
    // The next edit is checked against the revision the last one returned.
    call(&mut s, "set_tempo", json!({"bpm": 150}));
    let reqs = fake.requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[0].body, RequestBody::ProjectGet);
    assert_eq!(reqs[1].base_revision, Some(5));
    assert_eq!(
        reqs[1].body,
        RequestBody::Edit {
            edits: vec![Edit::SetTempo { bpm: 140.0 }]
        }
    );
    assert_eq!(reqs[2].base_revision, Some(6));
    // An explicit base_revision wins.
    call(
        &mut s,
        "edit",
        json!({"edits": [{"edit": "set_tempo", "bpm": 90}], "base_revision": 2}),
    );
    assert_eq!(fake.requests()[3].base_revision, Some(2));
}

#[test]
fn steps_set_turns_every_step_on_or_off() {
    let dir = temp_dir("steps");
    let fake = spawn_fake(&dir.join("control.sock"), ok_hello(), |req| {
        match &req.body {
            RequestBody::ProjectGet => project_with_pattern(8),
            RequestBody::Edit { .. } => applied(6, vec![]),
            other => panic!("{other:?}"),
        }
    });
    let mut s = Server::new(config(&fake.socket));
    init(&mut s);
    let r = call(
        &mut s,
        "steps_set",
        json!({"pattern": 2, "channel": 1, "steps": [0, 4]}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let reqs = fake.requests();
    let RequestBody::Edit { edits } = &reqs.last().unwrap().body else {
        panic!()
    };
    assert_eq!(edits.len(), 8);
    assert_eq!(
        edits[4],
        Edit::SetStep {
            pattern: PatternId(2),
            channel: ChannelId(1),
            step: 4,
            on: true,
            vel: Some(100)
        }
    );
    assert!(matches!(edits[5], Edit::SetStep { on: false, .. }));
    // A step past the end is refused locally.
    let r = call(
        &mut s,
        "steps_set",
        json!({"pattern": 2, "channel": 1, "steps": [9]}),
    );
    assert_eq!(r["isError"], true);
    // An unknown pattern is a clear error.
    let r = call(
        &mut s,
        "steps_set",
        json!({"pattern": 77, "channel": 1, "steps": [0]}),
    );
    assert_eq!(r["isError"], true);
    assert_eq!(r["structuredContent"]["error"]["code"], "not_found");
}

#[test]
fn untrusted_names_are_cleaned_in_structured_fields() {
    let dir = temp_dir("names");
    let fake = spawn_fake(&dir.join("control.sock"), ok_hello(), |_| {
        project_with_pattern(16)
    });
    let mut s = Server::new(config(&fake.socket));
    init(&mut s);
    let r = call(&mut s, "project_get", json!({}));
    assert_eq!(
        r["structuredContent"]["project"]["patterns"][0]["name"],
        "A name"
    );
    assert_eq!(r["structuredContent"]["revision"], 5);
}

#[test]
fn daw_errors_become_tool_errors_with_instructions() {
    let dir = temp_dir("errors");
    let mut n = 0;
    let fake = spawn_fake(&dir.join("control.sock"), ok_hello(), move |req| {
        n += 1;
        if matches!(req.body, RequestBody::ProjectGet) {
            return project_with_pattern(16);
        }
        Outcome::Err {
            error: match n {
                2 => ControlError::Stale { current: 12 },
                3 => ControlError::NeedsUserApproval,
                4 => ControlError::Denied,
                5 => ControlError::Edit {
                    index: Some(3),
                    error: protocol::edit::EditError::NotFound {
                        what: "channel".into(),
                        id: 9,
                    },
                },
                _ => ControlError::Busy,
            },
        }
    });
    let mut s = Server::new(config(&fake.socket));
    init(&mut s);
    let r = call(&mut s, "set_tempo", json!({"bpm": 100}));
    assert_eq!(r["isError"], true);
    assert_eq!(r["structuredContent"]["code"], "stale");
    assert!(
        r["structuredContent"]["message"]
            .as_str()
            .unwrap()
            .contains("project_get")
    );
    assert_eq!(r["structuredContent"]["error"]["current"], 12);
    let r = call(&mut s, "project_open", json!({"path": "/x"}));
    assert_eq!(r["structuredContent"]["code"], "needs_user_approval");
    assert!(
        r["structuredContent"]["message"]
            .as_str()
            .unwrap()
            .contains("Approve")
    );
    let r = call(&mut s, "project_new", json!({}));
    assert_eq!(r["structuredContent"]["code"], "denied");
    let r = call(&mut s, "play", json!({}));
    assert_eq!(r["structuredContent"]["code"], "edit");
    let m = r["structuredContent"]["message"].as_str().unwrap();
    assert!(
        m.contains("Edit 3") && m.contains("nothing in the batch"),
        "{m}"
    );
    let r = call(&mut s, "play", json!({}));
    assert_eq!(r["structuredContent"]["code"], "busy");
}

#[test]
fn bad_arguments_are_tool_errors_not_protocol_errors() {
    let dir = temp_dir("badargs");
    let mut s = Server::new(config(&dir.join("none.sock")));
    init(&mut s);
    for (name, args) in [
        ("set_tempo", json!({})),
        ("edit", json!({"edits": [{"edit": "explode"}]})),
        ("edit", json!({"edits": [], "confirm": true})),
        (
            "settings_set",
            json!({"key": "socket_path", "value": "/tmp/x"}),
        ),
        ("play", json!({"loud": true})),
    ] {
        let r = call(&mut s, name, args);
        assert_eq!(r["isError"], true, "{name}");
        assert_eq!(r["structuredContent"]["error"]["code"], "bad_arguments");
    }
}

#[test]
fn jobs_and_settings_map_to_requests() {
    let dir = temp_dir("jobs");
    let fake = spawn_fake(&dir.join("control.sock"), ok_hello(), |req| {
        match &req.body {
            RequestBody::ExportWav { .. } => Outcome::Ok {
                body: ReplyBody::Job {
                    job: 3,
                    revision: 8,
                },
            },
            _ => Outcome::Ok {
                body: ReplyBody::Done,
            },
        }
    });
    let mut s = Server::new(config(&fake.socket));
    init(&mut s);
    let r = call(
        &mut s,
        "export_wav",
        json!({"pattern": 2, "loops": 2, "format": "float32"}),
    );
    assert_eq!(
        r["structuredContent"],
        json!({"kind": "job", "job": 3, "revision": 8})
    );
    call(&mut s, "job_status", json!({"job": 3}));
    call(
        &mut s,
        "settings_set",
        json!({"key": "buffer_size", "value": "128"}),
    );
    call(&mut s, "analyze", json!({"pattern": 2}));
    let bodies: Vec<String> = fake
        .requests()
        .iter()
        .map(|r| serde_json::to_string(&r.body).unwrap())
        .collect();
    assert_eq!(
        bodies[0],
        r#"{"op":"export_wav","pattern":2,"loops":2,"format":"float32"}"#
    );
    assert_eq!(bodies[1], r#"{"op":"job_status","job":3}"#);
    assert_eq!(
        bodies[2],
        r#"{"op":"settings_set","setting":{"key":"buffer_size","value":"128"}}"#
    );
    assert_eq!(bodies[3], r#"{"op":"analyze","pattern":2,"loops":1}"#);
}

#[test]
fn agents_disabled_is_explained() {
    let dir = temp_dir("disabled");
    let fake = spawn_fake(
        &dir.join("control.sock"),
        json!({"hello_err": {"reason": "agents_disabled"}}),
        |_| unreachable!(),
    );
    let mut s = Server::new(config(&fake.socket));
    init(&mut s);
    let r = call(&mut s, "play", json!({}));
    assert_eq!(r["isError"], true);
    assert_eq!(r["structuredContent"]["error"]["code"], "agents_disabled");
    assert!(
        r["structuredContent"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("enable agent control")
    );
}

fn fake_daw_script(dir: &Path, marker: &Path) -> PathBuf {
    let p = dir.join("fake-libredaw");
    std::fs::write(
        &p,
        format!(
            "#!/usr/bin/env fish\n# SPDX-License-Identifier: GPL-3.0-or-later\necho $argv >> {}\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[test]
fn starts_the_daw_with_agent_request_and_connects_when_it_appears() {
    let dir = temp_dir("start");
    let marker = dir.join("started");
    let socket = dir.join("control.sock");
    let mut cfg = config(&socket);
    cfg.daw_command = fake_daw_script(&dir, &marker).into();
    cfg.startup_wait = Duration::from_secs(10);
    // The "DAW" binds its socket a moment after it was started.
    let (m2, s2) = (marker.clone(), socket.clone());
    let t = thread::spawn(move || {
        while !m2.exists() {
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(150));
        spawn_fake(&s2, ok_hello(), |_| Outcome::Ok {
            body: ReplyBody::Done,
        })
    });
    let mut s = Server::new(cfg);
    init(&mut s);
    let r = call(&mut s, "stop", json!({}));
    assert_eq!(r["isError"], false, "{r}");
    let _fake = t.join().unwrap();
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().trim(),
        "--agent-request"
    );
}

#[test]
fn gives_up_after_the_wait_with_a_waiting_message() {
    let dir = temp_dir("giveup");
    let marker = dir.join("started");
    let mut cfg = config(&dir.join("control.sock"));
    cfg.daw_command = fake_daw_script(&dir, &marker).into();
    cfg.startup_wait = Duration::from_millis(700);
    let mut s = Server::new(cfg);
    init(&mut s);
    let t = Instant::now();
    let r = call(&mut s, "play", json!({}));
    assert!(
        t.elapsed() >= Duration::from_millis(650),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(r["isError"], true);
    assert_eq!(r["structuredContent"]["error"]["code"], "waiting_for_user");
    assert_eq!(
        r["structuredContent"]["error"]["message"],
        "waiting for the user in LibreDAW"
    );
    // Started exactly once.
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);
}

#[test]
fn missing_daw_program_is_reported() {
    let dir = temp_dir("nodaw");
    let mut cfg = config(&dir.join("control.sock"));
    cfg.daw_command = "/nonexistent/libredaw".into();
    let mut s = Server::new(cfg);
    init(&mut s);
    let r = call(&mut s, "play", json!({}));
    assert_eq!(r["structuredContent"]["error"]["code"], "daw_not_running");
}

#[test]
fn a_dropped_connection_is_reported_and_the_next_call_reconnects() {
    let dir = temp_dir("drop");
    let socket = dir.join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    thread::spawn(move || {
        // First connection: hello, then hang up on the first request.
        let (st, _) = listener.accept().unwrap();
        let mut w = st.try_clone().unwrap();
        let mut r = BufReader::new(st);
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        writeln!(w, "{}", ok_hello()).unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        drop(w);
        drop(r);
        // Second connection works.
        let (st, _) = listener.accept().unwrap();
        let mut w = st.try_clone().unwrap();
        let mut r = BufReader::new(st);
        line.clear();
        r.read_line(&mut line).unwrap();
        writeln!(w, "{}", ok_hello()).unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        let req: Request = serde_json::from_str(&line).unwrap();
        let reply = protocol::control::Reply {
            id: req.id,
            outcome: Outcome::Ok {
                body: ReplyBody::Done,
            },
        };
        writeln!(w, "{}", serde_json::to_string(&reply).unwrap()).unwrap();
    });
    let mut s = Server::new(config(&socket));
    init(&mut s);
    let r = call(&mut s, "play", json!({}));
    assert_eq!(r["structuredContent"]["error"]["code"], "connection_lost");
    let r = call(&mut s, "play", json!({}));
    assert_eq!(r["isError"], false, "{r}");
}

#[test]
fn the_binary_speaks_mcp_over_stdio() {
    let dir = temp_dir("stdio");
    let rt = dir.join("rt");
    std::fs::create_dir_all(rt.join("libredaw")).unwrap();
    let _fake = spawn_fake(&rt.join("libredaw/control.sock"), ok_hello(), |_| {
        Outcome::Ok {
            body: ReplyBody::Done,
        }
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_libredaw-mcp"))
        .env("XDG_RUNTIME_DIR", &rt)
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
}
