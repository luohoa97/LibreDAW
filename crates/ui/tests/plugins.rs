// SPDX-License-Identifier: GPL-3.0-or-later
//! Registry and session against the real fixture plugins from
//! `crates/plugin-host/examples/test_plugins.rs`. The tests build the
//! fixture themselves, so they do not depend on cargo's build order.

use std::path::PathBuf;

use protocol::edit::Edit;
use protocol::engine::{EngineCommand, EngineEvent, PluginSlot, TrackSlot};
use protocol::ids::{InstanceId, TrackId};
use protocol::model::{Insert, Instrument};
use ui::document::Document;
use ui::engine_adapter::EngineLink;
use ui::history::{Author, Scope, Submitted};
use ui::plugin_adapter::{PluginDesc, scan_paths};
use ui::registry::{Phase, Registry};
use ui::session::Session;

/// Builds the fixture cdylib if needed (a no-op when it is current), so the
/// tests do not depend on cargo's build order. A fixture that cannot be
/// built is a test failure, not a skip.
fn build_fixture(exe: &std::path::Path) -> PathBuf {
    let release = exe.components().any(|c| c.as_os_str() == "release");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = std::process::Command::new(cargo);
    cmd.args([
        "build",
        "-p",
        "libredaw-plugin-host",
        "--example",
        "test_plugins",
    ]);
    if release {
        cmd.arg("--release");
    }
    let st = cmd.status().expect("run cargo to build the fixture");
    assert!(st.success(), "building the test_plugins fixture failed");
    let so = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("target dir")
        .join("examples/libtest_plugins.so");
    assert!(so.exists(), "fixture missing after build: {so:?}");
    so
}

fn load_fixture() -> Vec<PluginDesc> {
    let exe = std::env::current_exe().expect("exe path");
    let so = build_fixture(&exe);
    let dir =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("ui-clap-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("nested")).expect("fixture dir");
    std::fs::copy(&so, dir.join("nested/test_plugins.clap")).expect("copy fixture");
    scan_paths(std::slice::from_ref(&dir))
}

fn fixture() -> Option<Vec<PluginDesc>> {
    use std::sync::OnceLock;
    static CATALOG: OnceLock<Vec<PluginDesc>> = OnceLock::new();
    Some(CATALOG.get_or_init(load_fixture).clone())
}

fn insert_slot(i: u8) -> PluginSlot {
    PluginSlot::Insert {
        track: TrackSlot::MASTER,
        index: i,
    }
}

fn add_insert(s: &mut Session, index: u8, plugin: &str) -> InstanceId {
    match s
        .submit(
            Author::User,
            None,
            vec![Edit::AddInsert {
                track: TrackId::MASTER,
                index,
                plugin_id: plugin.into(),
            }],
            0,
        )
        .expect("edit")
    {
        Submitted::Applied(a) => InstanceId(a.created[0]),
        Submitted::Queued => panic!("queued"),
    }
}

fn attaches(s: &Session) -> Vec<PluginSlot> {
    s.link
        .commands
        .iter()
        .filter_map(|c| match c {
            EngineCommand::AttachPlugin { slot, .. } => Some(*slot),
            _ => None,
        })
        .collect()
}

fn detaches(s: &Session) -> Vec<PluginSlot> {
    s.link
        .commands
        .iter()
        .filter_map(|c| match c {
            EngineCommand::DetachPlugin { slot } => Some(*slot),
            _ => None,
        })
        .collect()
}

fn session(catalog: Vec<PluginDesc>) -> Session {
    Session::new(
        Document::new(),
        true,
        EngineLink::stub(48000.0),
        Registry::new(catalog, 48000.0),
    )
}

#[test]
fn insert_is_created_attached_and_a_shifted_insert_goes_through_the_handshake() {
    let Some(catalog) = fixture() else { return };
    let mut s = session(catalog);
    let a = add_insert(&mut s, 0, "test.gain");
    assert!(s.take_messages().is_empty());
    assert_eq!(s.registry.phase(a), Some(Phase::Attached(insert_slot(0))));
    assert_eq!(attaches(&s), vec![insert_slot(0)]);

    // A second insert in front moves `a` to index 1: detach first.
    let b = add_insert(&mut s, 0, "test.gain");
    assert_eq!(detaches(&s), vec![insert_slot(0)]);
    assert_eq!(s.registry.phase(a), Some(Phase::Detaching(insert_slot(0))));
    assert_eq!(
        s.registry.phase(b),
        Some(Phase::Ready),
        "waits for the slot"
    );
    assert_eq!(attaches(&s).len(), 1);

    // The audio thread acks; the next tick attaches both in their slots.
    s.link.events.push_back(EngineEvent::DetachAck {
        slot: insert_slot(0),
    });
    s.tick();
    assert_eq!(s.registry.phase(a), Some(Phase::Attached(insert_slot(1))));
    assert_eq!(s.registry.phase(b), Some(Phase::Attached(insert_slot(0))));
    assert_eq!(s.registry.instance_at(insert_slot(1)), Some(a));
}

#[test]
fn removal_captures_state_first_and_undo_brings_the_patch_back() {
    let Some(catalog) = fixture() else { return };
    let mut s = session(catalog);
    let a = add_insert(&mut s, 0, "test.gain");
    // Move a parameter so the state differs from the default.
    s.submit(
        Author::User,
        None,
        vec![Edit::SetPluginParam {
            instance: a,
            param_id: 0,
            value: 0.25,
        }],
        0,
    )
    .unwrap();
    s.tick();
    assert_eq!(
        s.link.plugin_events.len(),
        1,
        "parameter event reached the ring"
    );

    s.submit(
        Author::User,
        None,
        vec![Edit::RemoveInsert {
            track: TrackId::MASTER,
            instance: a,
        }],
        0,
    )
    .unwrap();
    assert_eq!(detaches(&s), vec![insert_slot(0)]);
    s.link.events.push_back(EngineEvent::DetachAck {
        slot: insert_slot(0),
    });
    s.tick();
    assert!(s.registry.is_empty(), "dropped only after the ack");

    // Undo: the document gets the insert back, with the captured blob.
    s.undo(&Scope::Any).unwrap();
    let t = &s.document().project.tracks[0];
    let Insert::Clap(r) = &t.inserts[0];
    assert_eq!(r.instance, a);
    assert!(r.state_bytes.is_some(), "state was captured before removal");
    assert!(r.state_file.is_some());
    assert_eq!(r.params.len(), 1);
    assert_eq!(s.registry.phase(a), Some(Phase::Attached(insert_slot(0))));
}

#[test]
fn channel_instrument_gets_an_instrument_slot_and_save_captures_state() {
    let Some(catalog) = fixture() else { return };
    let mut s = session(catalog);
    let r = s
        .submit(
            Author::User,
            None,
            vec![Edit::AddChannel {
                name: "Sine".into(),
                instrument: protocol::edit::NewInstrument::Clap {
                    plugin_id: "test.sine".into(),
                },
                root_key: 60,
                track: TrackId::MASTER,
            }],
            0,
        )
        .unwrap();
    let Submitted::Applied(a) = r else { panic!() };
    let inst = InstanceId(a.created[1]);
    assert!(matches!(
        s.registry.phase(inst),
        Some(Phase::Attached(PluginSlot::Instrument(_)))
    ));
    let doc = s.snapshot_for_save();
    let Instrument::Clap(c) = &doc.project.channels[0].instrument else {
        panic!()
    };
    assert!(c.state_file.is_some() && c.state_bytes.is_some());
    // A second capture with unchanged state creates no new generation.
    let doc2 = s.snapshot_for_save();
    let Instrument::Clap(c2) = &doc2.project.channels[0].instrument else {
        panic!()
    };
    assert_eq!(c.state_file, c2.state_file);
}

#[test]
fn unknown_plugin_id_gives_a_message_and_keeps_the_reference() {
    let Some(catalog) = fixture() else { return };
    let mut s = session(catalog);
    add_insert(&mut s, 0, "no.such.plugin");
    let m = s.take_messages();
    assert_eq!(m.len(), 1, "{m:?}");
    assert_eq!(s.document().project.tracks[0].inserts.len(), 1);
    assert!(s.registry.is_empty());
}
