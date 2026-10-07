// SPDX-License-Identifier: GPL-3.0-or-later
//! Writes a Milestone B demo project to the directory given as the first
//! argument, for screenshots: swing, per-step lanes, an 808, built-in
//! effects, a return track with a send, and a short song:
//! `cargo run -p libredaw-ui --example make_beats -- /tmp/Beats.ldaw`

use doc::bundle;
use doc::document::{Document, apply_batch};
use protocol::beats::BuiltinFxKind;
use protocol::edit::{Edit, MixValue, NewInstrument, NewNote};
use protocol::ids::{ChannelId, PatternId, PlaylistTrackId, TrackId};
use protocol::model::SynthParams;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: make_beats <bundle dir>");
        std::process::exit(2);
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
            Edit::AddTrack {
                name: "Space".into(),
            },
            Edit::SetTempo { bpm: 140.0 },
            Edit::AddPattern {
                name: "Pattern 1".into(),
                length_steps: 16,
            },
            Edit::AddPattern {
                name: "Pattern 2".into(),
                length_steps: 16,
            },
        ],
    )
    .expect("setup");
    let (drums, bass, space) = (TrackId(ids[0]), TrackId(ids[1]), TrackId(ids[2]));
    let (p1, p2) = (PatternId(ids[3]), PatternId(ids[4]));
    let mut edits = Vec::new();
    for (name, key, track, instrument) in [
        (
            "Kick",
            36u8,
            drums,
            NewInstrument::Synth {
                params: SynthParams::default(),
            },
        ),
        (
            "Snare",
            38,
            drums,
            NewInstrument::Synth {
                params: SynthParams::default(),
            },
        ),
        (
            "Hat",
            42,
            drums,
            NewInstrument::Synth {
                params: SynthParams::default(),
            },
        ),
        ("808", 36, bass, NewInstrument::Bass808 { mono: true }),
    ] {
        edits.push(Edit::AddChannel {
            name: name.into(),
            instrument,
            root_key: key,
            track,
        });
    }
    let (d, ids) = apply_batch(&d, &edits).expect("channels");
    let ch: Vec<ChannelId> = ids.iter().map(|i| ChannelId(*i)).collect();
    let mut edits = vec![Edit::SetSwing {
        pattern: p1,
        swing: 300,
    }];
    for s in [0u8, 3, 6, 10] {
        edits.push(Edit::SetStep {
            pattern: p1,
            channel: ch[0],
            step: s,
            on: true,
            vel: Some(118),
        });
    }
    for s in [4u8, 12] {
        edits.push(Edit::SetStep {
            pattern: p1,
            channel: ch[1],
            step: s,
            on: true,
            vel: Some(100),
        });
    }
    for s in 0..16u8 {
        edits.push(Edit::SetStep {
            pattern: p1,
            channel: ch[2],
            step: s,
            on: true,
            vel: Some(if s % 4 == 0 { 120 } else { 70 + (s * 3) % 40 }),
        });
    }
    // Ratchets on two hats, a pitch lane on the 808.
    for (s, r) in [(7u8, 3u8), (14, 4), (15, 2)] {
        edits.push(Edit::SetStepLanes {
            pattern: p1,
            channel: ch[2],
            step: s,
            vel: None,
            off: None,
            repeat: Some(r),
        });
    }
    for (s, off) in [(0u8, 0i8), (3, 3), (6, -2), (10, 5), (13, 7)] {
        edits.push(Edit::SetStep {
            pattern: p1,
            channel: ch[3],
            step: s,
            on: true,
            vel: Some(105),
        });
        if off != 0 {
            edits.push(Edit::SetStepLanes {
                pattern: p1,
                channel: ch[3],
                step: s,
                vel: None,
                off: Some(off),
                repeat: None,
            });
        }
    }
    // A piano roll pattern for the second slot of the song.
    edits.push(Edit::AddNotes {
        pattern: p2,
        channel: ch[3],
        notes: [(0u32, 480u32, 36u8), (480, 480, 39), (960, 960, 43)]
            .iter()
            .map(|&(start, len, key)| NewNote {
                start,
                len,
                key,
                vel: 100,
            })
            .collect(),
    });
    edits.push(Edit::AddBuiltinInsert {
        track: space,
        index: 0,
        fx: BuiltinFxKind::Reverb,
    });
    edits.push(Edit::AddBuiltinInsert {
        track: drums,
        index: 0,
        fx: BuiltinFxKind::Compressor,
    });
    edits.push(Edit::SetSend {
        track: drums,
        to: space,
        level_db: -9.0,
        pre_fader: false,
    });
    edits.push(Edit::SetTrackMix {
        track: drums,
        value: MixValue::VolumeDb(-4.0),
    });
    edits.push(Edit::AddPlaylistTrack {
        name: "Drums".into(),
    });
    edits.push(Edit::AddPlaylistTrack {
        name: "Bass".into(),
    });
    let (d, ids) = apply_batch(&d, &edits).expect("beats");
    let n = ids.len();
    // The two playlist tracks are the last two ids created.
    let (pl1, pl2) = (PlaylistTrackId(ids[n - 2]), PlaylistTrackId(ids[n - 1]));
    let bar = 3840u32;
    let (d, _) = apply_batch(
        &d,
        &[
            Edit::AddClip {
                track: pl1,
                pattern: p1,
                start: 0,
                len: bar * 4,
            },
            Edit::AddClip {
                track: pl2,
                pattern: p2,
                start: bar,
                len: bar * 2,
            },
            Edit::AddClip {
                track: pl1,
                pattern: p1,
                start: bar * 6,
                len: bar,
            },
        ],
    )
    .expect("song");
    bundle::save(std::path::Path::new(&path), &d).expect("save");
    println!("wrote {path}");
}
