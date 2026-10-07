// SPDX-License-Identifier: GPL-3.0-or-later
//! An agent finds and adds sounds over the real control socket: the built-in
//! catalogue, the user's FL Studio library (a synthetic fixture here, the
//! real install in the ignored test) and Surge XT. The bridge is the one the
//! window runs; the engine is a stub.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use doc::document::Document;
use doc::persist::Dirs;
use library::{FlInstall, InstallSource};
use protocol::model::Instrument;
use ui::app::App;
use ui::control_bridge::{start_in, tick};
use ui::engine_adapter::EngineLink;
use ui::fl_library::{self as fl, Status};
use ui::registry::Registry;
use ui::session::Session;

static ENV: Once = Once::new();

/// The per-machine sample registry goes to a temp folder, never the user's.
/// Set once, before any thread of this test binary reads it.
fn isolate() {
    ENV.call_once(|| {
        let dir = std::env::temp_dir().join(format!("ldaw-sounds-data-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: first thing every test does, before any other thread exists
        // that could read the environment.
        unsafe { std::env::set_var("XDG_DATA_HOME", &dir) };
    });
}

struct Rig {
    app: Rc<App>,
    socket: PathBuf,
    dir: PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(b) = self.app.bridge.borrow_mut().take() {
            b.shutdown();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn rig(name: &str) -> Rig {
    isolate();
    let dir = std::env::temp_dir().join(format!("ldaw-sounds-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dirs = Dirs {
        music: dir.join("music"),
        data: dir.join("data"),
        config: dir.join("config"),
    };
    let mut session = Session::new(
        Document::new(),
        true,
        EngineLink::stub(48000.0),
        Registry::new(Vec::new(), 48000.0),
    );
    session.set_sample_home(Some(dir.join("project")));
    let app = App::with_dirs(session, dirs);
    let (bridge, e) = start_in(dir.join("run"), false);
    let mut bridge = bridge.unwrap_or_else(|| panic!("start: {e:?}"));
    bridge.set_enabled(true);
    let socket = bridge.socket_path().to_path_buf();
    *app.bridge.borrow_mut() = Some(bridge);
    fl::set_status(Status::Off);
    Rig { app, socket, dir }
}

/// Sends the requests as an agent and returns the replies, running the
/// window's tick and its background tasks meanwhile.
fn talk(rig: &Rig, requests: Vec<String>) -> Vec<String> {
    let path = rig.socket.clone();
    let h = std::thread::spawn(move || {
        let mut s = UnixStream::connect(path).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        writeln!(s, r#"{{"hello":{{"transport":"agent","client":"test"}}}}"#).unwrap();
        let mut hello = String::new();
        r.read_line(&mut hello).unwrap();
        let mut out = Vec::new();
        for q in requests {
            writeln!(s, "{q}").unwrap();
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            out.push(line);
        }
        out
    });
    let t0 = Instant::now();
    while !h.is_finished() && t0.elapsed().as_secs() < 25 {
        tick(&rig.app);
        rig.app.tasks.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    h.join().expect("client thread")
}

fn search(q: &str) -> String {
    format!(r#"{{"id":1,"body":{{"op":"sound_search",{q}}}}}"#)
}

/// The first `"id":"<prefix>..."` in a reply.
fn id_with(reply: &str, prefix: &str) -> String {
    let key = format!("\"id\":\"{prefix}");
    let at = reply
        .find(&key)
        .unwrap_or_else(|| panic!("no {prefix} in {reply}"));
    let rest = &reply[at + 6..];
    rest[..rest.find('"').unwrap()].to_string()
}

fn wav(hz: f64) -> Vec<u8> {
    let rate = 44100u32;
    let n = (f64::from(rate) * 0.6) as usize;
    let mut d = Vec::new();
    for i in 0..n {
        let s = (2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(rate)).sin() * 0.5;
        d.extend_from_slice(&((s * 32767.0) as i16).to_le_bytes());
    }
    let mut w = b"RIFF".to_vec();
    w.extend_from_slice(&(36 + d.len() as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    for v in [16u32, 0x0001_0001, rate, rate * 2, 0x0010_0002] {
        w.extend_from_slice(&v.to_le_bytes());
    }
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(d.len() as u32).to_le_bytes());
    w.extend_from_slice(&d);
    w
}

/// A tree shaped like FL's `Packs` folder, with synthetic sounds.
fn fixture(root: &Path) -> FlInstall {
    let put = |rel: &str, hz: f64| {
        let p = root.join("Packs").join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, wav(hz)).unwrap();
    };
    for rel in [
        "Drums/Kicks/909 Kick.wav",
        "Drums/Kicks/808 Kick.wav",
        "Drums/Snares/909 Snare.wav",
        "Drums/Hats/909 CH.wav",
        "Drums/Hats/909 OH.wav",
        "Drums/Percussion/909 Clap.wav",
    ] {
        put(rel, 220.0);
    }
    for (i, hz) in [220.0, 246.94, 277.18].iter().enumerate() {
        put(
            &format!("Instruments/Guitar/Jazz/Jazz Guitar ({}).wav", i + 1),
            *hz,
        );
    }
    FlInstall {
        name: "FL Studio Fixture".into(),
        version: String::new(),
        packs: root.join("Packs"),
        prefix: root.to_path_buf(),
        source: InstallSource::Folder,
    }
}

fn samplers(app: &App) -> Vec<(String, bool)> {
    let s = app.session.borrow();
    s.document()
        .project
        .channels
        .iter()
        .filter_map(|c| match &c.instrument {
            Instrument::Sampler(sm) => {
                let hash = sm.sample.as_ref()?;
                let local = s
                    .document()
                    .project
                    .samples
                    .iter()
                    .any(|r| &r.hash == hash && r.local_only);
                Some((c.name.clone(), local))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn fl_sounds_are_off_until_the_user_turns_them_on_then_found_and_added() {
    let rig = rig("fl");
    // Off: the agent is told what to ask the user, in plain words.
    let r = talk(
        &rig,
        vec![search(r#""tags":["kick","source:FL Studio"],"limit":20"#)],
    );
    assert!(
        r[0].contains("not turned on") && r[0].contains("Use Your FL Studio Sounds"),
        "{}",
        r[0]
    );

    // The user turned it on (the pane's scan, here on a fixture).
    let install = fixture(&rig.dir.join("fl"));
    let loaded = fl::scan(&install, &rig.dir.join("cache"), true).expect("fixture scans");
    fl::set_status(Status::Ready(Arc::new(loaded)));

    let kicks = talk(
        &rig,
        vec![search(r#""tags":["kick","source:FL Studio"],"limit":20"#)],
    )
    .remove(0);
    assert!(
        kicks.contains("909 Kick") && kicks.contains("808 Kick"),
        "{kicks}"
    );
    assert!(!kicks.contains("909 Snare"), "{kicks}");
    let sound = id_with(&kicks, "fl:sound:");

    let added = talk(
        &rig,
        vec![format!(
            r#"{{"id":2,"body":{{"op":"kit_add","pack":"@sound","kit":"{sound}"}}}}"#
        )],
    )
    .remove(0);
    assert!(added.contains("\"status\":\"ok\""), "{added}");
    let s = samplers(&rig.app);
    assert_eq!(s.len(), 1, "{s:?}");
    assert!(s[0].1, "the FL file stays on this computer (local_only)");

    // A kit: six slots at most, one undo step.
    let kits = talk(
        &rig,
        vec![search(
            r#""tags":["source:FL Studio"],"role":"drums","limit":50"#,
        )],
    )
    .remove(0);
    let kit = id_with(&kits, "fl:kit:");
    let before = rig.app.session.borrow().document().project.channels.len();
    let r = talk(
        &rig,
        vec![
            format!(r#"{{"id":3,"body":{{"op":"kit_add","pack":"@sound","kit":"{kit}"}}}}"#),
            r#"{"id":4,"body":{"op":"undo"}}"#.into(),
        ],
    );
    assert!(r[0].contains("\"status\":\"ok\""), "{}", r[0]);
    let created = r[0]
        .split("\"created\":[")
        .nth(1)
        .unwrap()
        .split(']')
        .next()
        .unwrap();
    let n = created.split(',').count();
    assert!(
        (4..=6).contains(&n),
        "a kit has up to six pieces: {created}"
    );
    assert!(r[1].contains("\"status\":\"ok\""), "{}", r[1]);
    assert_eq!(
        rig.app.session.borrow().document().project.channels.len(),
        before,
        "the whole kit is one undo step"
    );

    // An instrument plays from its root sample.
    let inst = talk(
        &rig,
        vec![search(r#""tags":["jazz","source:FL Studio"],"limit":50"#)],
    )
    .remove(0);
    let inst = id_with(&inst, "fl:instrument:");
    let added = talk(
        &rig,
        vec![format!(
            r#"{{"id":5,"body":{{"op":"kit_add","pack":"@sound","kit":"{inst}"}}}}"#
        )],
    )
    .remove(0);
    assert!(added.contains("\"status\":\"ok\""), "{added}");
    assert!(
        samplers(&rig.app)
            .iter()
            .any(|(n, l)| n.contains("Jazz") && *l)
    );

    // Unknown ids and file paths are refused.
    let bad = talk(
        &rig,
        vec![r#"{"id":6,"body":{"op":"kit_add","pack":"@sound","kit":"/etc/passwd"}}"#.into()],
    )
    .remove(0);
    assert!(bad.contains("\"status\":\"err\""), "{bad}");
}

#[test]
fn surge_sounds_are_listed_with_their_source() {
    let rig = rig("surge");
    let r = talk(
        &rig,
        vec![search(
            r#""role":"bass","tags":["source:Surge XT"],"limit":5"#,
        )],
    );
    assert!(
        r[0].contains("\"id\":\"surge:") && r[0].contains("Surge XT"),
        "{}",
        r[0]
    );
    // Only Surge XT answers; the FL message is not for this search.
    assert!(!r[0].contains("FL Studio sounds"), "{}", r[0]);
}

/// The owner's real FL install and Surge XT. Run with
/// `cargo test -p libredaw-ui --test sounds_over_socket -- --ignored --nocapture`.
#[test]
#[ignore = "needs the owner's FL Studio install"]
fn the_real_fl_install_is_searchable_and_addable() {
    let rig = rig("real");
    let Some(install) = library::detect_installs().into_iter().next() else {
        println!("no FL Studio install here");
        return;
    };
    let cache = library::default_cache_dir().unwrap_or_else(|| rig.dir.join("cache"));
    let loaded = fl::scan(&install, &cache, true).expect("the install scans");
    println!(
        "{}: {} sounds, {} kits, {} instruments",
        install.name,
        loaded.index.entries.len(),
        loaded.kits.len(),
        loaded.instruments.len()
    );
    fl::set_status(Status::Ready(Arc::new(loaded)));
    let kicks = talk(
        &rig,
        vec![search(r#""tags":["kick","source:FL Studio"],"limit":5"#)],
    )
    .remove(0);
    println!("{kicks}");
    let id = id_with(&kicks, "fl:sound:");
    let added = talk(
        &rig,
        vec![format!(
            r#"{{"id":2,"body":{{"op":"kit_add","pack":"@sound","kit":"{id}"}}}}"#
        )],
    )
    .remove(0);
    assert!(added.contains("\"status\":\"ok\""), "{added}");
    assert!(samplers(&rig.app).iter().all(|(_, local)| *local));
    let kits = talk(
        &rig,
        vec![search(
            r#""tags":["source:FL Studio"],"role":"drums","limit":3"#,
        )],
    )
    .remove(0);
    let kit = id_with(&kits, "fl:kit:");
    let added = talk(
        &rig,
        vec![format!(
            r#"{{"id":3,"body":{{"op":"kit_add","pack":"@sound","kit":"{kit}"}}}}"#
        )],
    )
    .remove(0);
    assert!(added.contains("\"status\":\"ok\""), "{added}");
    println!("channels now: {:?}", samplers(&rig.app));
    let surge = talk(
        &rig,
        vec![search(r#""tags":["source:Surge XT"],"limit":3"#)],
    )
    .remove(0);
    println!("{surge}");
    assert!(surge.contains("surge:"));
}

#[test]
fn sounds_know_their_kit_and_kits_list_their_pieces() {
    let rig = rig("kits");
    let install = fixture(&rig.dir.join("fl"));
    let loaded = fl::scan(&install, &rig.dir.join("cache"), true).expect("fixture scans");
    fl::set_status(Status::Ready(Arc::new(loaded)));
    let kits = talk(
        &rig,
        vec![search(
            r#""tags":["source:FL Studio"],"role":"drums","limit":50"#,
        )],
    )
    .remove(0);
    let kit_id = id_with(&kits, "fl:kit:");
    // The kit entry lists its pieces: role, sound id and name.
    let got = talk(
        &rig,
        vec![search(&format!(r#""tags":["kit_id:{kit_id}"],"limit":1"#))],
    )
    .remove(0);
    assert!(got.contains(&kit_id), "{got}");
    for slot in [
        "slot:kick\u{1f}fl:sound:",
        "slot:snare\u{1f}fl:sound:",
        "slot:hat\u{1f}fl:sound:",
    ] {
        // The wire escapes the separator as \u001f.
        let wire = slot.replace('\u{1f}', "\\u001f");
        assert!(got.contains(&wire), "{slot} in {got}");
    }
    // Every piece names its kit, and `kit:` filters by it.
    let kick = talk(
        &rig,
        vec![search(
            r#""tags":["kit:909","kick","source:FL Studio"],"limit":50"#,
        )],
    )
    .remove(0);
    assert!(kick.contains("909 Kick"), "{kick}");
    assert!(
        kick.contains(&format!("{kit_id}\\u001f")),
        "kit on the sound: {kick}"
    );
    let none = talk(
        &rig,
        vec![search(
            r#""tags":["kit:nonesuch","source:FL Studio"],"limit":50"#,
        )],
    )
    .remove(0);
    assert!(!none.contains("909 Kick"), "{none}");
    // Built-in Oto Kit sounds are not offered.
    let oto = talk(&rig, vec![search(r#""tags":["kick"],"limit":50"#)]).remove(0);
    assert!(!oto.contains("\"oto:"), "{oto}");
}
