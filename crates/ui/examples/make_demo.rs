// SPDX-License-Identifier: GPL-3.0-or-later
//! Writes a small demo project (a four-channel beat) to the directory given
//! as the first argument, for trying the app and for screenshots:
//! `cargo run -p libredaw-ui --example make_demo -- /tmp/Demo.ldaw`

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
            Edit::AddPattern {
                name: "Pattern 1".into(),
                length_steps: 16,
            },
        ],
    )
    .expect("setup");
    let (drums, bass, pattern) = (TrackId(ids[0]), TrackId(ids[1]), PatternId(ids[2]));
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
    let mut edits = Vec::new();
    for s in [0u8, 4, 8, 12] {
        edits.push(Edit::SetStep {
            pattern,
            channel: ch[0],
            step: s,
            on: true,
            vel: Some(110),
        });
    }
    for s in [4u8, 12] {
        edits.push(Edit::SetStep {
            pattern,
            channel: ch[1],
            step: s,
            on: true,
            vel: Some(100),
        });
    }
    for s in 0..16u8 {
        edits.push(Edit::SetStep {
            pattern,
            channel: ch[2],
            step: s,
            on: s % 2 == 0,
            vel: Some(if s % 4 == 0 { 120 } else { 80 }),
        });
    }
    // The bass plays piano roll notes, so its row shows the marker.
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
        pattern,
        channel: ch[3],
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
    let (d, _) = apply_batch(&d, &edits).expect("notes");
    bundle::save(std::path::Path::new(&path), &d).expect("save");
    println!("wrote {path}");
}
