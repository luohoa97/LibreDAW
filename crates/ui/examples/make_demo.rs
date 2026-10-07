// SPDX-License-Identifier: GPL-3.0-or-later
//! Writes a small demo project (four instruments, a four-bar loop) to the
//! directory given as the first argument, for trying the app and for
//! screenshots: `cargo run -p libredaw-ui --example make_demo -- /tmp/Demo.ldaw`

use doc::bundle;
use doc::document::{Document, apply_batch};
use protocol::edit::{Edit, MixValue, NewInstrument, NewNote};
use protocol::ids::{ChannelId, PatternId, TrackId};
use protocol::model::SynthParams;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: make_demo <bundle dir>");
        std::process::exit(2);
    };
    let synth = || NewInstrument::Synth {
        params: SynthParams::default(),
    };
    let d = Document::new();
    let (d, ids) = apply_batch(
        &d,
        &[
            Edit::AddTrack {
                name: "Drums".into(),
            },
            Edit::AddTrack {
                name: "Bass".into(),
            },
            Edit::SetTempo { bpm: 128.0 },
        ],
    )
    .expect("setup");
    let (drums, bass) = (TrackId(ids[0]), TrackId(ids[1]));
    let mut edits = Vec::new();
    for (name, key, track) in [
        ("Kick", 36u8, drums),
        ("Snare", 38, drums),
        ("Hat", 42, drums),
        ("Bass", 36, bass),
    ] {
        edits.push(Edit::AddChannel {
            name: name.into(),
            instrument: synth(),
            root_key: key,
            track,
        });
    }
    let (d, ids) = apply_batch(&d, &edits).expect("channels");
    let ch: Vec<ChannelId> = ids.iter().map(|i| ChannelId(*i)).collect();
    // A one-bar clip per instrument, then linked copies over bars 2 to 4.
    let bar = 3840u32;
    let firsts: Vec<Edit> = ch
        .iter()
        .map(|c| Edit::AddClip {
            instrument: *c,
            pattern: None,
            start: 0,
            len: bar,
        })
        .collect();
    let (d, ids) = apply_batch(&d, &firsts).expect("clips");
    let content: Vec<PatternId> = ids.chunks(2).map(|p| PatternId(p[0])).collect();
    let mut edits = Vec::new();
    for (i, c) in ch.iter().enumerate() {
        for b in 1..4 {
            edits.push(Edit::AddClip {
                instrument: *c,
                pattern: Some(content[i]),
                start: b * bar,
                len: bar,
            });
        }
    }
    let step = |pattern: PatternId, s: u8, on: bool, vel: u8| Edit::SetStep {
        pattern,
        step: s,
        on,
        vel: Some(vel),
    };
    for s in [0u8, 4, 8, 12] {
        edits.push(step(content[0], s, true, 110));
    }
    for s in [4u8, 12] {
        edits.push(step(content[1], s, true, 100));
    }
    for s in 0..16u8 {
        edits.push(step(
            content[2],
            s,
            s % 2 == 0,
            if s % 4 == 0 { 120 } else { 80 },
        ));
    }
    // The bass plays notes (its steps show the "notes" hatch).
    let notes = [
        (0u32, 480u32, 36u8, 110u8),
        (480, 240, 39, 90),
        (720, 240, 43, 100),
        (960, 480, 36, 110),
        (1440, 240, 41, 85),
        (1920, 480, 34, 105),
        (2400, 480, 36, 95),
        (2880, 240, 46, 90),
        (3120, 360, 48, 100),
    ];
    edits.push(Edit::AddNotes {
        pattern: content[3],
        notes: notes
            .iter()
            .map(|&(start, len, key, vel)| NewNote {
                start,
                len,
                key,
                vel,
            })
            .collect(),
    });
    edits.push(Edit::SetTrackMix {
        track: drums,
        value: MixValue::VolumeDb(-4.0),
    });
    edits.push(Edit::SetChannelMix {
        channel: ch[2],
        value: MixValue::Pan(0.3),
    });
    edits.push(Edit::SetLoopRegion {
        start: 0,
        end: 4 * bar,
        enabled: true,
    });
    let (d, _) = apply_batch(&d, &edits).expect("notes");
    bundle::save(std::path::Path::new(&path), &d).expect("save");
    println!("wrote {path}");
}
