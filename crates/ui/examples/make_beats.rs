// SPDX-License-Identifier: GPL-3.0-or-later
//! Writes a Milestone B demo project to the directory given as the first
//! argument, for screenshots: swing, per-step lanes, an 808, built-in
//! effects, a return track with a send, and a short song:
//! `cargo run -p libredaw-ui --example make_beats -- /tmp/Beats.ldaw`

use doc::bundle;
use doc::document::{Document, apply_batch};
use protocol::beats::{BuiltinFxKind, SampleMode};
use protocol::edit::{Edit, MixValue, NewInstrument, NewNote};
use protocol::ids::{ChannelId, PatternId, TrackId};
use protocol::model::{SampleRef, SynthParams};

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
        ],
    )
    .expect("setup");
    let (drums, bass, space) = (TrackId(ids[0]), TrackId(ids[1]), TrackId(ids[2]));
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
    // With a WAV file as the second argument: a sampler channel that plays
    // it, and one whose sample file is missing (the placeholder).
    if let Some(wav) = std::env::args().nth(2) {
        let r = doc::samples::import_sample(
            std::path::Path::new(&path),
            std::path::Path::new(&wav),
            false,
        )
        .expect("import");
        edits.push(Edit::AddSample { sample: r.clone() });
        edits.push(Edit::AddChannel {
            name: "Clap".into(),
            instrument: NewInstrument::Sampler {
                sample: Some(r.hash),
                mode: SampleMode::OneShot,
            },
            root_key: 60,
            track: drums,
        });
        let ghost = SampleRef {
            hash: format!("{:0>64}", "1"),
            orig_name: "lost.wav".into(),
            size: 10,
            local_only: false,
        };
        edits.push(Edit::AddSample {
            sample: ghost.clone(),
        });
        edits.push(Edit::AddChannel {
            name: "Lost sound".into(),
            instrument: NewInstrument::Sampler {
                sample: Some(ghost.hash),
                mode: SampleMode::Pitched,
            },
            root_key: 48,
            track: drums,
        });
    }
    let (d, ids) = apply_batch(&d, &edits).expect("channels");
    let ch: Vec<ChannelId> = ids.iter().map(|i| ChannelId(*i)).collect();
    // One clip per instrument over four bars (its one-bar content loops),
    // and a second 808 clip with notes from bar 5 (SPEC 20).
    let bar = 3840u32;
    let mut clips = Vec::new();
    for c in &ch {
        clips.push(Edit::AddClip {
            instrument: *c,
            pattern: None,
            start: 0,
            len: bar * 4,
        });
    }
    clips.push(Edit::AddClip {
        instrument: ch[3],
        pattern: None,
        start: bar * 4,
        len: bar * 2,
    });
    let (d, ids) = apply_batch(&d, &clips).expect("clips");
    // Each AddClip made a content, then a clip.
    let content: Vec<PatternId> = ids.chunks(2).map(|p| PatternId(p[0])).collect();
    let mut edits = vec![Edit::SetSwing {
        pattern: content[0],
        swing: 300,
    }];
    let step = |pattern: PatternId, s: u8, vel: u8| Edit::SetStep {
        pattern,
        step: s,
        on: true,
        vel: Some(vel),
    };
    for s in [0u8, 3, 6, 10] {
        edits.push(step(content[0], s, 118));
    }
    for s in [4u8, 12] {
        edits.push(step(content[1], s, 100));
    }
    for s in 0..16u8 {
        edits.push(step(
            content[2],
            s,
            if s % 4 == 0 { 120 } else { 70 + (s * 3) % 40 },
        ));
    }
    // Ratchets on two hats, a pitch lane on the 808.
    for (s, r) in [(7u8, 3u8), (14, 4), (15, 2)] {
        edits.push(Edit::SetStepLanes {
            pattern: content[2],
            step: s,
            vel: None,
            off: None,
            repeat: Some(r),
        });
    }
    for (s, off) in [(0u8, 0i8), (3, 3), (6, -2), (10, 5), (13, 7)] {
        edits.push(step(content[3], s, 105));
        if off != 0 {
            edits.push(Edit::SetStepLanes {
                pattern: content[3],
                step: s,
                vel: None,
                off: Some(off),
                repeat: None,
            });
        }
    }
    edits.push(Edit::AddNotes {
        pattern: content[4],
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
    edits.push(Edit::SetLoopRegion {
        start: 0,
        end: bar * 4,
        enabled: true,
    });
    let (d, _) = apply_batch(&d, &edits).expect("beats");
    bundle::save(std::path::Path::new(&path), &d).expect("save");
    println!("wrote {path}");
}
