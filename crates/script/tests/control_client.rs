// SPDX-License-Identifier: GPL-3.0-or-later
//! Control client against a fake server on a real Unix socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use protocol::control::{ControlError, Outcome, ReplyBody, RequestBody, Transport};
use script::{Client, ClientError};
use serde_json::{Value, json};

fn temp_socket(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("libredaw-script-test-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("control.sock");
    let _ = std::fs::remove_file(&p);
    p
}

fn read_json(r: &mut BufReader<UnixStream>) -> Value {
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn send(w: &mut UnixStream, v: Value) {
    writeln!(w, "{v}").unwrap();
}

#[test]
fn hello_then_call_matches_reply_by_id() {
    let path = temp_socket("ok");
    let listener = UnixListener::bind(&path).unwrap();
    let server = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        let hello = read_json(&mut r);
        assert_eq!(
            hello,
            json!({"hello": {"transport": "agent", "client": "test"}})
        );
        send(&mut w, json!({"hello_ok": {"protocol": 1}}));
        let req = read_json(&mut r);
        assert_eq!(req["body"], json!({"op": "play"}));
        assert_eq!(req["base_revision"], json!(7));
        let id = req["id"].as_u64().unwrap();
        // A reply for an id nobody waits for, then the real one.
        send(
            &mut w,
            json!({"id": id + 100, "outcome": {"status": "ok", "body": {"kind": "done"}}}),
        );
        send(
            &mut w,
            json!({"id": id, "outcome": {"status": "ok", "body": {"kind": "transport",
                "playing": true, "tick": 5, "tempo_bpm": 120.0,
                "loop_region": {"start": 0, "end": 7680, "enabled": true}}}}),
        );
    });
    let mut c = Client::connect(&path, Transport::Agent, "test", Duration::from_secs(2)).unwrap();
    let out = c.call(RequestBody::Play, Some(7)).unwrap();
    match out {
        Outcome::Ok {
            body: ReplyBody::Transport { playing, tick, .. },
        } => {
            assert!(playing);
            assert_eq!(tick, 5);
        }
        other => panic!("unexpected {other:?}"),
    }
    server.join().unwrap();
}

#[test]
fn ids_increase_and_errors_pass_through() {
    let path = temp_socket("err");
    let listener = UnixListener::bind(&path).unwrap();
    let server = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        read_json(&mut r);
        send(&mut w, json!({"hello_ok": {"protocol": 1}}));
        let mut ids = Vec::new();
        for _ in 0..2 {
            let req = read_json(&mut r);
            let id = req["id"].as_u64().unwrap();
            ids.push(id);
            send(
                &mut w,
                json!({"id": id, "outcome": {"status": "err", "error": {"code": "stale", "current": 9}}}),
            );
        }
        ids
    });
    let mut c = Client::connect(&path, Transport::Script, "t", Duration::from_secs(2)).unwrap();
    for _ in 0..2 {
        match c.call(RequestBody::ProjectGet, None).unwrap() {
            Outcome::Err {
                error: ControlError::Stale { current },
            } => assert_eq!(current, 9),
            other => panic!("unexpected {other:?}"),
        }
    }
    let ids = server.join().unwrap();
    assert_eq!(ids.len(), 2);
    assert!(ids[1] > ids[0]);
}

#[test]
fn unanswered_call_times_out_then_client_is_closed() {
    let path = temp_socket("timeout");
    let listener = UnixListener::bind(&path).unwrap();
    let server = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        read_json(&mut r);
        send(&mut w, json!({"hello_ok": {"protocol": 1}}));
        read_json(&mut r);
        // Never answer; keep the socket open until the client is done.
        thread::sleep(Duration::from_millis(600));
    });
    let mut c = Client::connect(&path, Transport::Agent, "t", Duration::from_secs(2)).unwrap();
    let err = c
        .call_with_timeout(RequestBody::Stop, None, Duration::from_millis(150))
        .unwrap_err();
    assert!(matches!(err, ClientError::Timeout), "{err:?}");
    assert!(c.is_broken());
    assert!(matches!(
        c.call(RequestBody::Stop, None),
        Err(ClientError::Closed)
    ));
    server.join().unwrap();
}

#[test]
fn hello_refused_or_wrong_protocol_is_an_error() {
    let path = temp_socket("hello");
    let listener = UnixListener::bind(&path).unwrap();
    let server = thread::spawn(move || {
        for reply in [
            json!({"hello_ok": {"protocol": 2}}),
            json!({"hello_err": {"reason": "agents_disabled"}}),
        ] {
            let (s, _) = listener.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut w = s;
            read_json(&mut r);
            send(&mut w, reply);
        }
    });
    let r = Client::connect(&path, Transport::Agent, "t", Duration::from_secs(2));
    assert!(matches!(r, Err(ClientError::Protocol(_))));
    let r = Client::connect(&path, Transport::Agent, "t", Duration::from_secs(2));
    assert!(matches!(r, Err(ClientError::Refused(ref x)) if x == "agents_disabled"));
    server.join().unwrap();
}

#[test]
fn peer_closing_reports_closed_and_missing_socket_reports_connect() {
    let path = temp_socket("closed");
    let listener = UnixListener::bind(&path).unwrap();
    let server = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        read_json(&mut r);
        send(&mut w, json!({"hello_ok": {"protocol": 1}}));
        read_json(&mut r);
        // Drop without replying.
    });
    let mut c = Client::connect(&path, Transport::Agent, "t", Duration::from_secs(2)).unwrap();
    assert!(matches!(
        c.call(RequestBody::Play, None),
        Err(ClientError::Closed)
    ));
    server.join().unwrap();

    let missing = path.with_file_name("nope.sock");
    assert!(matches!(
        Client::connect(&missing, Transport::Agent, "t", Duration::from_secs(1)),
        Err(ClientError::Connect(_))
    ));
}
