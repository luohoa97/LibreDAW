// SPDX-License-Identifier: GPL-3.0-or-later
//! Integration tests for the control socket server over real Unix sockets,
//! driven by the script crate's client and by raw sockets.

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use control::{ControlConfig, ControlServer, ControlStartError, Polled, UiEvent};
use protocol::control::{ControlError, Outcome, Reply, ReplyBody, Request, RequestBody, Transport};
use protocol::edit::Edit;
use script::control::{Client, ClientError};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("ldaw-ctl-{}-{n}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

struct Env {
    dir: PathBuf,
    server: ControlServer,
}

impl Env {
    fn sock(&self) -> PathBuf {
        self.dir.join("libredaw").join("control.sock")
    }
}

fn config(dir: &Path, agents: bool) -> ControlConfig {
    let mut c = ControlConfig::new(dir.join("libredaw"));
    c.agents_enabled = agents;
    c
}

fn start(tag: &str, edit: impl FnOnce(&mut ControlConfig)) -> Env {
    let dir = temp_dir(tag);
    let mut cfg = config(&dir, true);
    edit(&mut cfg);
    let server = ControlServer::start(cfg).expect("start");
    Env { dir, server }
}

fn connect(env: &Env, t: Transport) -> Result<Client, ClientError> {
    Client::connect(&env.sock(), t, "test", Duration::from_secs(5))
}

/// Polls every few ms until `f` returns something; panics after 5 s.
fn poll_until<T>(server: &ControlServer, mut f: impl FnMut(Polled) -> Option<T>) -> T {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if let Some(v) = f(server.poll()) {
            return v;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for the server");
}

fn next_request(server: &ControlServer) -> control::Incoming {
    poll_until(server, |p| p.requests.into_iter().next())
}

fn done() -> Outcome {
    Outcome::Ok {
        body: ReplyBody::Done,
    }
}

fn raw(env: &Env) -> UnixStream {
    let s = UnixStream::connect(env.sock()).expect("raw connect");
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s
}

fn read_line(s: &mut UnixStream) -> Option<String> {
    let mut out = Vec::new();
    let mut b = [0u8; 1];
    loop {
        match s.read(&mut b) {
            Ok(0) | Err(_) => {
                return if out.is_empty() {
                    None
                } else {
                    Some(text(out))
                };
            }
            Ok(_) if b[0] == b'\n' => return Some(text(out)),
            Ok(_) => out.push(b[0]),
        }
    }
}

fn text(v: Vec<u8>) -> String {
    String::from_utf8(v).expect("utf8")
}

fn hello(s: &mut UnixStream, transport: &str) -> String {
    let line = format!("{{\"hello\":{{\"transport\":\"{transport}\",\"client\":\"raw\"}}}}\n");
    s.write_all(line.as_bytes()).unwrap();
    read_line(s).expect("hello reply")
}

fn parse_reply(line: &str) -> Reply {
    serde_json::from_str(line).expect("reply json")
}

#[test]
fn agent_hello_and_round_trip() {
    let env = start("roundtrip", |_| {});
    let mut c = connect(&env, Transport::Agent).expect("hello");
    let t = thread::spawn(move || c.call(RequestBody::Play, None));
    let inc = next_request(&env.server);
    assert_eq!(inc.request.body, RequestBody::Play);
    assert_eq!(inc.client.transport, Transport::Agent);
    assert_eq!(inc.client.name, "test");
    assert!(env.server.reply(inc.ticket, done()));
    assert!(
        !env.server.reply(inc.ticket, done()),
        "ticket is single use"
    );
    assert_eq!(t.join().unwrap().unwrap(), done());
}

#[test]
fn client_events_connected_and_gone() {
    let env = start("events", |_| {});
    let c = connect(&env, Transport::Agent).unwrap();
    let id = poll_until(&env.server, |p| {
        p.events.into_iter().find_map(|e| match e {
            UiEvent::ClientConnected(i) => Some(i.id),
            _ => None,
        })
    });
    assert_eq!(env.server.clients().len(), 1);
    drop(c);
    let gone = poll_until(&env.server, |p| {
        p.events.into_iter().find_map(|e| match e {
            UiEvent::ClientGone { id } => Some(id),
            _ => None,
        })
    });
    assert_eq!(gone, id);
    assert!(env.server.clients().is_empty());
}

#[test]
fn agents_disabled_refuses_once_and_raises_event() {
    let env = start("disabled", |c| c.agents_enabled = false);
    for _ in 0..2 {
        match connect(&env, Transport::Agent) {
            Err(ClientError::Refused(r)) => assert_eq!(r, "agents_disabled"),
            other => panic!("expected refusal, got {:?}", other.err()),
        }
    }
    let p = env.server.poll();
    let asks = p
        .events
        .iter()
        .filter(|e| matches!(e, UiEvent::AgentRequestedControl { .. }))
        .count();
    assert_eq!(asks, 1, "one banner request per disabled period");
    env.server.set_agents_enabled(true);
    assert!(connect(&env, Transport::Agent).is_ok());
}

#[test]
fn scripts_connect_even_when_agents_are_disabled() {
    let env = start("script", |c| c.agents_enabled = false);
    let mut c = connect(&env, Transport::Script).expect("script hello");
    let t = thread::spawn(move || c.call(RequestBody::Stop, None));
    let inc = next_request(&env.server);
    assert_eq!(inc.client.transport, Transport::Script);
    env.server.reply(inc.ticket, done());
    assert_eq!(t.join().unwrap().unwrap(), done());
}

#[test]
fn scripts_can_be_turned_off_entirely() {
    let env = start("noscript", |c| c.allow_scripts = false);
    match connect(&env, Transport::Script) {
        Err(ClientError::Refused(r)) => assert_eq!(r, "not_allowed"),
        other => panic!("expected not_allowed, got {:?}", other.err()),
    }
}

#[test]
fn bad_hellos_are_refused_and_closed() {
    let env = start("badhello", |_| {});
    for bad in [
        "not json\n",
        "{\"hello\":{\"transport\":\"toaster\",\"client\":\"x\"}}\n",
        "{\"hello\":{\"transport\":\"agent\"}}\n",
        "{\"id\":1,\"body\":{\"op\":\"play\"}}\n",
    ] {
        let mut s = raw(&env);
        s.write_all(bad.as_bytes()).unwrap();
        let line = read_line(&mut s).expect("hello_err");
        assert_eq!(
            line, "{\"hello_err\":{\"reason\":\"bad_hello\"}}",
            "for {bad:?}"
        );
        assert!(read_line(&mut s).is_none(), "connection closed");
    }
}

#[test]
fn client_name_is_cleaned() {
    let env = start("name", |_| {});
    let mut s = raw(&env);
    let name = format!("a\\u0007b{}", "x".repeat(200));
    let line = format!("{{\"hello\":{{\"transport\":\"agent\",\"client\":\"{name}\"}}}}\n");
    s.write_all(line.as_bytes()).unwrap();
    assert_eq!(
        read_line(&mut s).unwrap(),
        "{\"hello_ok\":{\"protocol\":1}}"
    );
    let info = &env.server.clients()[0];
    assert!(info.name.starts_with("ab"));
    assert_eq!(info.name.chars().count(), 64);
}

#[test]
fn second_agent_gets_busy_owner() {
    let env = start("owner", |_| {});
    let _first = connect(&env, Transport::Agent).unwrap();
    match connect(&env, Transport::Agent) {
        Err(ClientError::Refused(r)) => assert_eq!(r, "busy_owner"),
        other => panic!("expected busy_owner, got {:?}", other.err()),
    }
    assert!(connect(&env, Transport::Script).is_ok());
}

#[test]
fn script_cannot_send_agent_only_requests() {
    let env = start("mask", |_| {});
    let mut c = connect(&env, Transport::Script).unwrap();
    let out = c.call(RequestBody::PluginScan, None).unwrap();
    assert_eq!(
        out,
        Outcome::Err {
            error: ControlError::NotAllowed
        }
    );
    assert!(
        env.server.poll().requests.is_empty(),
        "never reaches the UI"
    );
}

#[test]
fn bad_request_keeps_the_connection() {
    let env = start("badreq", |_| {});
    let mut s = raw(&env);
    hello(&mut s, "agent");
    s.write_all(b"{\"id\":7,\"body\":{\"op\":\"nope\"}}\n")
        .unwrap();
    let r = parse_reply(&read_line(&mut s).unwrap());
    assert_eq!(r.id, 7);
    assert!(matches!(
        r.outcome,
        Outcome::Err {
            error: ControlError::BadRequest { .. }
        }
    ));
    s.write_all(b"{\"id\":8,\"body\":{\"op\":\"play\"}}\n")
        .unwrap();
    let inc = next_request(&env.server);
    env.server.reply(inc.ticket, done());
    assert_eq!(parse_reply(&read_line(&mut s).unwrap()).id, 8);
}

#[test]
fn line_too_long_is_rejected_and_closed() {
    let env = start("long", |_| {});
    let mut s = raw(&env);
    hello(&mut s, "agent");
    let junk = vec![b'a'; (1 << 20) + 10];
    s.write_all(&junk).unwrap();
    s.write_all(b"\n").unwrap();
    let r = parse_reply(&read_line(&mut s).expect("error reply"));
    match r.outcome {
        Outcome::Err {
            error: ControlError::TooLarge { max, .. },
        } => assert_eq!(max, 1 << 20),
        other => panic!("unexpected {other:?}"),
    }
    assert!(read_line(&mut s).is_none(), "closed after the violation");
    assert!(env.server.poll().requests.is_empty());
}

#[test]
fn too_many_edits_is_rejected() {
    let env = start("edits", |_| {});
    let mut c = connect(&env, Transport::Agent).unwrap();
    let edits = vec![Edit::SetTempo { bpm: 120.0 }; 10_001];
    let out = c.call(RequestBody::Edit { edits }, Some(0)).unwrap();
    assert_eq!(
        out,
        Outcome::Err {
            error: ControlError::TooLarge {
                what: "edits".into(),
                max: 10_000
            }
        }
    );
    // The limit itself passes.
    let edits = vec![Edit::SetTempo { bpm: 120.0 }; 10_000];
    let t = thread::spawn(move || c.call(RequestBody::Edit { edits }, Some(0)));
    let inc = next_request(&env.server);
    env.server.reply(inc.ticket, done());
    assert_eq!(t.join().unwrap().unwrap(), done());
}

fn client_call_in_thread(
    mut c: Client,
    body: RequestBody,
) -> thread::JoinHandle<Result<Outcome, ClientError>> {
    thread::spawn(move || c.call(body, None))
}

#[test]
fn approval_allowed_then_executed() {
    let env = start("allow", |_| {});
    let c = connect(&env, Transport::Agent).unwrap();
    let t = client_call_in_thread(c, RequestBody::ProjectNew { template: None });
    let inc = next_request(&env.server);
    assert!(
        env.server
            .require_approval(inc.ticket, "new project".into())
    );
    let ev = poll_until(&env.server, |p| {
        p.events.into_iter().find_map(|e| match e {
            UiEvent::ApprovalNeeded { ticket, summary } => Some((ticket, summary)),
            _ => None,
        })
    });
    assert_eq!(ev, (inc.ticket, "new project".to_string()));
    assert!(env.server.approval(inc.ticket, true));
    assert!(env.server.reply(inc.ticket, done()));
    assert_eq!(t.join().unwrap().unwrap(), done());
}

#[test]
fn approval_denied_replies_denied() {
    let env = start("deny", |_| {});
    let c = connect(&env, Transport::Agent).unwrap();
    let t = client_call_in_thread(c, RequestBody::ProjectNew { template: None });
    let inc = next_request(&env.server);
    env.server.require_approval(inc.ticket, "x".into());
    assert!(!env.server.approval(inc.ticket, false));
    assert_eq!(
        t.join().unwrap().unwrap(),
        Outcome::Err {
            error: ControlError::Denied
        }
    );
    assert!(!env.server.reply(inc.ticket, done()), "ticket is gone");
}

#[test]
fn approval_timeout_replies_needs_user_approval() {
    let env = start("approval-timeout", |c| {
        c.approval_timeout = Duration::from_millis(150)
    });
    let c = connect(&env, Transport::Agent).unwrap();
    let t = client_call_in_thread(c, RequestBody::ProjectNew { template: None });
    let inc = next_request(&env.server);
    env.server.require_approval(inc.ticket, "x".into());
    let timed = poll_until(&env.server, |p| {
        p.events.into_iter().find_map(|e| match e {
            UiEvent::ApprovalTimedOut { ticket } => Some(ticket),
            _ => None,
        })
    });
    assert_eq!(timed, inc.ticket);
    assert_eq!(
        t.join().unwrap().unwrap(),
        Outcome::Err {
            error: ControlError::NeedsUserApproval
        }
    );
    assert!(!env.server.approval(inc.ticket, true), "too late");
}

#[test]
fn deferred_request_gets_busy_after_deadline() {
    let env = start("busy", |c| c.busy_timeout = Duration::from_millis(150));
    let c = connect(&env, Transport::Agent).unwrap();
    let t = client_call_in_thread(c, RequestBody::Play);
    let inc = next_request(&env.server);
    assert!(env.server.defer(inc.ticket));
    let timed = poll_until(&env.server, |p| {
        p.events.into_iter().find_map(|e| match e {
            UiEvent::DeferTimedOut { ticket } => Some(ticket),
            _ => None,
        })
    });
    assert_eq!(timed, inc.ticket);
    assert_eq!(
        t.join().unwrap().unwrap(),
        Outcome::Err {
            error: ControlError::Busy
        }
    );
}

#[test]
fn deferred_request_answered_in_time_is_fine() {
    let env = start("defer-ok", |c| c.busy_timeout = Duration::from_secs(5));
    let c = connect(&env, Transport::Agent).unwrap();
    let t = client_call_in_thread(c, RequestBody::Play);
    let inc = next_request(&env.server);
    env.server.defer(inc.ticket);
    thread::sleep(Duration::from_millis(50));
    assert!(env.server.reply(inc.ticket, done()));
    assert_eq!(t.join().unwrap().unwrap(), done());
}

#[test]
fn disabling_agents_disconnects_them_but_not_scripts() {
    let env = start("disable", |_| {});
    let mut agent = connect(&env, Transport::Agent).unwrap();
    let _script = connect(&env, Transport::Script).unwrap();
    env.server.set_agents_enabled(false);
    let r = agent.call(RequestBody::Play, None);
    assert!(r.is_err(), "agent connection was closed");
    poll_until(&env.server, |_| {
        (env.server.clients().len() == 1).then_some(())
    });
    assert_eq!(env.server.clients()[0].transport, Transport::Script);
}

#[test]
fn client_leaving_cancels_held_tickets() {
    let env = start("cancel", |_| {});
    let c = connect(&env, Transport::Agent).unwrap();
    let t = client_call_in_thread(c, RequestBody::ProjectNew { template: None });
    let inc = next_request(&env.server);
    env.server.require_approval(inc.ticket, "x".into());
    drop(t); // detach; the client thread owns the connection
    env.server.set_agents_enabled(false);
    let cancelled = poll_until(&env.server, |p| {
        p.events.into_iter().find_map(|e| match e {
            UiEvent::TicketCancelled { ticket } => Some(ticket),
            _ => None,
        })
    });
    assert_eq!(cancelled, inc.ticket);
    assert!(!env.server.reply(inc.ticket, done()));
}

#[test]
fn directory_and_socket_permissions() {
    let env = start("perms", |_| {});
    let dir = env.dir.join("libredaw");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&env.sock()), 0o600);
    assert!(dir.join("control.lock").exists());
}

#[test]
fn loose_directory_mode_is_tightened() {
    let dir = temp_dir("loose");
    let sub = dir.join("libredaw");
    std::fs::create_dir(&sub).unwrap();
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _server = ControlServer::start(config(&dir, true)).unwrap();
    assert_eq!(
        std::fs::metadata(&sub).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn second_daw_is_refused_while_the_first_runs() {
    let dir = temp_dir("second");
    let first = ControlServer::start(config(&dir, true)).unwrap();
    match ControlServer::start(config(&dir, true)) {
        Err(ControlStartError::AlreadyRunning) => {}
        other => panic!("expected AlreadyRunning, got {:?}", other.err()),
    }
    // The first one still works and its socket was not touched.
    assert!(
        Client::connect(
            first.socket_path(),
            Transport::Agent,
            "t",
            Duration::from_secs(5)
        )
        .is_ok()
    );
    first.shutdown();
    // After a clean shutdown the next DAW can start.
    assert!(ControlServer::start(config(&dir, true)).is_ok());
}

#[test]
fn many_daws_racing_for_the_lock_have_one_winner() {
    let dir = temp_dir("race");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let dir = dir.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                ControlServer::start(config(&dir, true))
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let winners = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(winners, 1);
    for r in &results {
        if let Err(e) = r {
            assert!(matches!(e, ControlStartError::AlreadyRunning), "{e}");
        }
    }
}

#[test]
fn stale_socket_from_a_dead_daw_is_replaced() {
    let dir = temp_dir("stale");
    let sub = dir.join("libredaw");
    std::fs::create_dir(&sub).unwrap();
    // A crashed DAW leaves its socket file behind (and no lock holder).
    drop(UnixListener::bind(sub.join("control.sock")).unwrap());
    assert!(sub.join("control.sock").exists());
    let server = ControlServer::start(config(&dir, true)).expect("stale socket detected");
    assert!(
        Client::connect(
            server.socket_path(),
            Transport::Agent,
            "t",
            Duration::from_secs(5)
        )
        .is_ok()
    );
}

#[test]
fn a_regular_file_at_the_socket_path_is_not_deleted() {
    let dir = temp_dir("file-in-way");
    let sub = dir.join("libredaw");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("control.sock"), b"precious").unwrap();
    assert!(matches!(
        ControlServer::start(config(&dir, true)),
        Err(ControlStartError::Io(_))
    ));
    assert_eq!(
        std::fs::read(sub.join("control.sock")).unwrap(),
        b"precious"
    );
}

#[test]
fn shutdown_removes_the_socket_and_closes_clients() {
    let env = start("shutdown", |_| {});
    let mut c = connect(&env, Transport::Agent).unwrap();
    let sock = env.sock();
    env.server.shutdown();
    assert!(!sock.exists());
    assert!(c.call(RequestBody::Play, None).is_err());
}

#[test]
fn requests_serialise_in_order_per_client() {
    let env = start("order", |_| {});
    let mut s = raw(&env);
    hello(&mut s, "agent");
    for id in 1..=3u64 {
        let r = Request {
            id,
            base_revision: None,
            body: RequestBody::Play,
        };
        writeln!(s, "{}", serde_json::to_string(&r).unwrap()).unwrap();
    }
    let mut seen = Vec::new();
    while seen.len() < 3 {
        let p = env.server.poll();
        seen.extend(p.requests.into_iter().map(|i| (i.request.id, i.ticket)));
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(seen.iter().map(|s| s.0).collect::<Vec<_>>(), vec![1, 2, 3]);
}
