// SPDX-License-Identifier: GPL-3.0-or-later
//! An agent makes a beat end to end through MCP only (SPEC 16.5, 20,
//! 15.12): instruments with drum steps and an 808 line, a 4-bar loop,
//! play, analyze, export, three versions on branches, switching between
//! them, and undo that only touches the agent's own commits. Everything
//! runs through the real socket, the real control server and the real
//! document (`support::RefDaw`).

mod support;

use protocol::control::RequestBody;
use protocol::edit::{Edit, MixValue};
use protocol::ids::{ChannelId, ClipId, PatternId, TrackId};
use protocol::model::{Instrument, Project};
use serde_json::{Value, json};
use support::{Mcp, RefDaw, rig_with, text_of};

fn step_hits(p: &Project, content: u64) -> usize {
    let pat = p.pattern(PatternId(content as u32)).unwrap();
    let root = p.channel(pat.instrument).unwrap().root_key;
    pat.notes
        .iter()
        .filter(|n| n.is_step_note(root, pat))
        .count()
}

fn id(v: &Value) -> u64 {
    v.as_u64().unwrap_or_else(|| panic!("not an id: {v}"))
}

#[test]
fn an_agent_makes_a_beat_and_three_versions_of_it() {
    let rig = rig_with(RefDaw::empty(), true, |_| {});
    let mut c = Mcp::connect(&rig);
    c.init();
    let tools = c.rpc("tools/list", json!({}))["tools"].clone();
    assert!(tools.as_array().unwrap().len() >= 40);

    c.ok("activity_set", json!({"text": "Making a trap beat"}));
    let s = text_of(&c.tool("project_summary", json!({})));
    assert!(s.contains("(none: add some"), "{s}");
    c.ok("song_set", json!({"tempo": 140}));

    // ---- one call: four instruments, their tracks and first clips --------
    let r = c.ok(
        "instruments_add",
        json!({"instruments": [
            {"name": "Kick", "root_key": 36, "clip": {"grid": "x...|..x.|x...|...."}},
            {"name": "Snare", "clip": {"grid": "....|x...|....|x..."}},
            {"name": "Hat", "clip": {"grid": "x.x.|x.x.|x.x.|x.x.", "vel": 80}},
            {"name": "808", "kind": "808", "clip": {"notes": "C2:0:1/4 C2:3/8:1/8 G1:1/2:1/4"}}
        ]}),
    );
    let made = r["instruments"].as_array().unwrap().clone();
    assert_eq!(made.len(), 4, "{r}");
    let p = rig.project();
    assert_eq!(p.channels.len(), 4);
    assert_eq!(p.tracks.len(), 5, "each instrument got its own mixer track");
    for m in &made {
        let ch = p.channel(ChannelId(id(&m["instrument"]) as u32)).unwrap();
        assert_eq!(ch.track.0 as u64, id(&m["track"]));
        assert_eq!(p.track(ch.track).unwrap().name, ch.name);
        let clip = p
            .clips
            .iter()
            .find(|c| c.id.0 as u64 == id(&m["clip"]))
            .unwrap();
        assert_eq!(clip.pattern.0 as u64, id(&m["content"]));
        assert_eq!(clip.instrument, ch.id);
    }
    assert!(matches!(
        p.channel(ChannelId(id(&made[3]["instrument"]) as u32))
            .unwrap()
            .instrument,
        Instrument::Bass808(_)
    ));
    assert_eq!(step_hits(&p, id(&made[0]["content"])), 3);
    assert_eq!(step_hits(&p, id(&made[2]["content"])), 8);
    let bass = p
        .pattern(PatternId(id(&made[3]["content"]) as u32))
        .unwrap();
    assert_eq!(bass.notes.len(), 3);
    // Everything above was ONE undo group.
    assert_eq!(rig.edit_batches().len(), 2, "song_set and instruments_add");

    // ---- fill four bars with linked copies -------------------------------
    let clips: Vec<u64> = made.iter().map(|m| id(&m["clip"])).collect();
    let r = c.ok("clips_copy", json!({"clips": clips, "times": 3}));
    let copies = r["clips"].as_array().unwrap();
    let new: Vec<&Value> = copies
        .iter()
        .filter(|c| !clips.contains(&id(&c["clip"])))
        .collect();
    assert_eq!(new.len(), 12, "{r}");
    assert!(new.iter().any(|c| c["start"] == "3"));
    let p = rig.project();
    assert_eq!(p.clips.len(), 16);
    assert_eq!(p.patterns.len(), 4, "linked copies share content");

    // A hat fill in bar 4 only: make that clip unique, then roll it.
    let hat_bar4 = new
        .iter()
        .find(|c| c["instrument"] == made[2]["instrument"] && c["start"] == "3")
        .map(|c| id(&c["clip"]))
        .unwrap();
    let r = c.ok(
        "clips_change",
        json!({"clips": [hat_bar4], "make_unique": true}),
    );
    let unique = id(&r["clips"][0]["content"]);
    assert_ne!(unique, id(&made[2]["content"]));
    c.ok(
        "beat_grid_set",
        json!({"rows": [{"clip": hat_bar4, "grid": "x.x.|x.x.|x.3.|x.68", "vel": 80}]}),
    );
    let p = rig.project();
    assert_eq!(
        step_hits(&p, id(&made[2]["content"])),
        8,
        "the other bars keep their hats"
    );
    let rolled = p.pattern(PatternId(unique as u32)).unwrap();
    assert!(rolled.notes.iter().any(|n| n.repeat == 8));

    // ---- loop, play, listen with numbers, export -------------------------
    let r = c.ok("loop_set", json!({"start": 0, "end": 4}));
    assert_eq!(r["diff"][0], "loop 0..4 on");
    c.ok("play", json!({}));
    let t = c.ok("transport_state", json!({}));
    assert_eq!(t["playing"], true);
    assert_eq!(t["loop"]["end"], "4");
    let a = c.ok("analyze", json!({}));
    assert!(a["integrated_lufs"].is_number(), "{a}");
    assert_eq!(a["clipped_samples"], 0);
    c.ok(
        "mix_set",
        json!({"changes": [{"instrument": made[3]["instrument"], "volume_db": -6}, {"track": 0, "volume_db": -1}]}),
    );
    let e = c.ok("export_wav", json!({"format": "pcm24"}));
    assert_eq!(e["path"], "/exports/Test.wav");
    c.ok("stop", json!({}));
    let saved = c.ok("version_save", json!({"name": "First beat"}));
    let base = saved["commit"].as_str().unwrap().to_string();

    // ---- three versions from the same commit ------------------------------
    let bass_id = made[3]["instrument"].clone();
    let hat_id = made[2]["instrument"].clone();
    let a = c.ok(
        "branch_create",
        json!({"name": "Version A: darker", "from": base}),
    );
    assert_eq!(a["base"], json!(base));
    assert_eq!(a["previous_branch"], "main");
    c.ok(
        "instrument_set",
        json!({"instrument": bass_id, "params": {"tone_hz": 600, "drive": 0.6}}),
    );
    let b = c.ok(
        "branch_create",
        json!({"name": "Version B: faster", "from": a["base"]}),
    );
    // Version B starts from the original, not from A.
    let p = rig.project();
    let Instrument::Bass808(b808) = &p
        .channel(ChannelId(id(&bass_id) as u32))
        .unwrap()
        .instrument
    else {
        panic!()
    };
    assert_eq!(b808.params.tone_hz, 8000.0);
    c.ok("song_set", json!({"tempo": 160}));
    let cbr = c.ok(
        "branch_create",
        json!({"name": "Version C: sparse", "from": base}),
    );
    let hats: Vec<u64> = rig
        .project()
        .clips
        .iter()
        .filter(|cl| cl.instrument.0 as u64 == id(&hat_id) && cl.start >= 7680)
        .map(|cl| cl.id.0 as u64)
        .collect();
    assert_eq!(hats.len(), 2);
    c.ok("clips_remove", json!({"clips": hats}));

    let list = c.ok("branch_list", json!({}));
    let branches = list["branches"].as_array().unwrap();
    assert_eq!(branches.len(), 4, "{list}");
    assert_eq!(list["current"], cbr["branch"]);
    for (v, tempo, tone, clips) in [
        (&a, 140.0, 600.0, 16),
        (&b, 160.0, 8000.0, 16),
        (&cbr, 140.0, 8000.0, 14),
    ] {
        c.ok("branch_switch", json!({"branch": v["branch"]}));
        let p = rig.project();
        assert_eq!(p.tempo_bpm, tempo, "{v}");
        let Instrument::Bass808(b) = &p
            .channel(ChannelId(id(&bass_id) as u32))
            .unwrap()
            .instrument
        else {
            panic!()
        };
        assert_eq!(b.params.tone_hz, tone, "{v}");
        assert_eq!(p.clips.len(), clips, "{v}");
    }
    let d = c.ok("history_diff", json!({"from": base}));
    assert!(!d["lines"].as_array().unwrap().is_empty(), "{d}");
    let h = c.ok("history", json!({"limit": 50}));
    let nodes = h["nodes"].as_array().unwrap();
    assert!(nodes.iter().any(|n| n["name"] == "First beat"), "{h}");
    assert!(nodes.iter().all(|n| {
        n["author"] == "user"
            || n["author"]
                .as_str()
                .unwrap()
                .starts_with("agent:test-agent")
    }));

    // ---- undo only touches the agent's own commits ------------------------
    c.ok("branch_switch", json!({"branch": "main"}));
    c.ok("song_set", json!({"tempo": 141}));
    rig.user_edit(vec![Edit::SetTrackMix {
        track: TrackId::MASTER,
        value: MixValue::VolumeDb(-4.0),
    }]);
    let m = c.err("undo", json!({}));
    assert!(
        m.contains("Nothing to undo") && m.contains("own changes"),
        "{m}"
    );
    assert_eq!(
        rig.project().track(TrackId::MASTER).unwrap().mix.volume_db,
        -4.0
    );
    assert_eq!(rig.project().tempo_bpm, 141.0);
    // After the user's step is undone by the user, the agent can undo its own.
    {
        let mut d = rig.daw.lock().unwrap();
        d.editor.undo(&doc::history::Scope::Any).unwrap();
    }
    let u = c.ok("undo", json!({}));
    assert_eq!(u["undone"], 1);
    assert_eq!(rig.project().tempo_bpm, 140.0);
    c.ok("redo", json!({}));
    assert_eq!(rig.project().tempo_bpm, 141.0);
    c.ok("activity_set", json!({"text": null}));

    // The activity the user saw, in order.
    let acts = rig.daw.lock().unwrap().activity.clone();
    assert_eq!(
        acts.first().unwrap().0.as_deref(),
        Some("Making a trap beat")
    );
    assert_eq!(acts.last().unwrap().0, None);
}

#[test]
fn overlaps_name_both_clips_and_change_nothing() {
    let rig = support::rig(true, |_| {});
    let mut c = Mcp::connect(&rig);
    c.init();
    let before = rig.revision();
    let m = c.err(
        "clips_add",
        json!({"clips": [{"instrument": 1, "start": "1/2", "grid": "x..............."}]}),
    );
    assert!(
        m.contains("would overlap clip 4")
            && m.contains("Nothing was changed")
            && m.contains("clip 0 (instrument 1)"),
        "{m}"
    );
    assert_eq!(rig.revision(), before);
    // Right after the first clip is fine and links when asked.
    let r = c.ok(
        "clips_add",
        json!({"clips": [{"instrument": 1, "start": 1, "content": 3}]}),
    );
    assert_eq!(r["clips"][0]["content"], 3);
    let m = c.err(
        "clips_change",
        json!({"clips": [r["clips"][0]["clip"]], "move_by": "-1/2"}),
    );
    assert!(m.contains("overlap clip 4"), "{m}");
}

#[test]
fn predicted_ids_survive_the_user_undoing_a_creation() {
    let rig = support::rig(true, |_| {});
    // The user adds a clip and undoes it: its ids are spent, not reused.
    rig.user_edit(vec![Edit::AddClip {
        instrument: ChannelId(1),
        pattern: None,
        start: 3840,
        len: 3840,
    }]);
    {
        let mut d = rig.daw.lock().unwrap();
        d.editor.undo(&doc::history::Scope::Any).unwrap();
    }
    let mut c = Mcp::connect(&rig);
    c.init();
    let r = c.ok(
        "instruments_add",
        json!({"instruments": [{"name": "Clap", "clip": {"start": 1, "grid": "....x.......x..."}}]}),
    );
    let m = &r["instruments"][0];
    let p = rig.project();
    let ch = p
        .channel(ChannelId(id(&m["instrument"]) as u32))
        .expect("instrument id is real");
    assert_eq!(ch.track.0 as u64, id(&m["track"]));
    let clip = p
        .clips
        .iter()
        .find(|c| c.id == ClipId(id(&m["clip"]) as u32))
        .expect("clip id is real");
    assert_eq!(clip.pattern.0 as u64, id(&m["content"]));
    assert_eq!(step_hits(&p, id(&m["content"])), 2);
    // The wrong first guess was taken back: one agent commit remains.
    let agent_commits = rig
        .daw
        .lock()
        .unwrap()
        .editor
        .history_nodes(None, 0)
        .into_iter()
        .filter(|n| n.author.starts_with("agent:") && n.branch == "main")
        .count();
    assert!(agent_commits >= 1);
    let mut undo = 0;
    while c.tool("undo", json!({}))["isError"] == json!(false) {
        undo += 1;
    }
    assert!(
        rig.project().channel(ch.id).is_none(),
        "undone after {undo}"
    );
}

#[test]
fn branch_from_a_branch_name_and_errors_that_explain() {
    let rig = support::rig(true, |_| {});
    let mut c = Mcp::connect(&rig);
    c.init();
    let a = c.ok("branch_create", json!({"name": "Idea", "from": "main"}));
    assert_eq!(a["name"], "Idea");
    let m = c.err("branch_switch", json!({"branch": "nope"}));
    assert!(m.contains("branch nope does not exist"), "{m}");
    let m = c.err("history_diff", json!({"from": "zzzzzzzz"}));
    assert!(m.contains("does not exist"), "{m}");
    c.ok("branch_set", json!({"branch": "main", "name": "Original"}));
    let l = c.ok("branch_list", json!({}));
    assert!(
        l["branches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["name"] == "Original")
    );
    // Restoring a version waits for the user's approval (granted here).
    let h = c.ok("history", json!({"limit": 1}));
    let commit = h["head"].as_str().unwrap().to_string();
    c.ok("song_set", json!({"tempo": 99}));
    let r = c.ok("version_restore", json!({"commit": commit}));
    assert!(r["revision"].is_number(), "{r}");
    assert_eq!(rig.project().tempo_bpm, 120.0);
    assert!(
        rig.ui
            .events()
            .iter()
            .any(|e| matches!(e, control::UiEvent::ApprovalNeeded { .. }))
    );
    assert!(
        rig.ui
            .requests()
            .iter()
            .any(|q| matches!(q.body, RequestBody::VersionRestore { .. })
                && q.base_revision.is_some())
    );
}
