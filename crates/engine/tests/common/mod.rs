// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared helpers for the engine integration tests.
#![allow(dead_code)]

use engine::runtime::{Runtime, Shared, UiEnds, rings};
use engine::{Slots, compile, write_controls};
use protocol::engine::EngineCommand;
use protocol::ids::{ChannelId, ClipId, NoteId, PatternId, TrackId};
use protocol::model::{
    Adsr, Channel, Clip, Instrument, LoopRegion, Mix, Note, Osc, Pattern, Project, SynthParams,
    Track, Wave,
};
use std::sync::Arc;

/// A steady tone: sine, no filter movement, instant attack, sustain 1.
pub fn tone_params() -> SynthParams {
    let osc = Osc {
        wave: Wave::Sine,
        semitones: 0.0,
        cents: 0.0,
    };
    SynthParams {
        osc1: osc,
        osc2: osc,
        osc_mix: 0.0,
        cutoff_hz: 20000.0,
        resonance: 0.0,
        filter_env_octaves: 0.0,
        amp_env: Adsr {
            attack_ms: 0.0,
            decay_ms: 0.0,
            sustain: 1.0,
            release_ms: 5.0,
        },
        filter_env: Adsr {
            attack_ms: 0.0,
            decay_ms: 0.0,
            sustain: 1.0,
            release_ms: 5.0,
        },
        gain_db: 0.0,
    }
}

pub fn synth_channel(id: u32, track: u32, params: SynthParams) -> Channel {
    Channel {
        id: ChannelId(id),
        name: format!("ch{id}"),
        root_key: 60,
        track: TrackId(track),
        mix: Mix::default(),
        instrument: Instrument::Synth(params),
        choke_group: 0,
    }
}

pub fn track(id: u32) -> Track {
    Track {
        id: TrackId(id),
        name: format!("t{id}"),
        mix: Mix::default(),
        inserts: Vec::new(),
        sends: Vec::new(),
    }
}

/// A note: `(id, start, len, key, vel)`.
pub type N = (u32, u32, u32, u8, u8);

/// One "beat" of the old pattern model: clip contents (one per instrument
/// with notes, all the same length) that `project` places as one clip each
/// at tick 0, plus a loop region over the first beat's length, which
/// reproduces the former looping pattern playback on the timeline.
#[derive(Clone)]
pub struct Beat {
    pub contents: Vec<Pattern>,
    pub len_ticks: u32,
}

impl From<Pattern> for Beat {
    fn from(p: Pattern) -> Beat {
        Beat {
            len_ticks: p.length_ticks(),
            contents: vec![p],
        }
    }
}

/// Contents of instrument notes with ids `id` (one instrument) or
/// `id * 100 + instrument` (several), `steps` steps long.
pub fn pattern(id: u32, steps: u8, notes: &[(u32, Vec<N>)]) -> Beat {
    let mut contents = Vec::new();
    for (ch, ns) in notes {
        let pid = if notes.len() == 1 { id } else { id * 100 + ch };
        let mut p = Pattern::new(PatternId(pid), format!("p{pid}"), ChannelId(*ch));
        p.length_steps = steps;
        let mut v: Vec<Note> = ns
            .iter()
            .map(|&(id, start, len, key, vel)| Note {
                id: NoteId(id),
                start,
                len,
                key,
                vel,
                off: 0,
                repeat: 1,
            })
            .collect();
        v.sort_by_key(|n| (n.start, n.key, n.id));
        p.notes = v;
        contents.push(p);
    }
    let mut probe = Pattern::new(PatternId(id), String::new(), ChannelId(0));
    probe.length_steps = steps;
    Beat {
        contents,
        len_ticks: probe.length_ticks(),
    }
}

/// A project whose timeline plays each beat's contents as one clip at tick
/// 0 (clip ids `1000 + content id`), with the loop region on over the
/// first beat's length.
pub fn project<B: Into<Beat>>(
    bpm: f64,
    tracks: Vec<Track>,
    channels: Vec<Channel>,
    beats: Vec<B>,
) -> Project {
    let mut p = Project::empty();
    p.tempo_bpm = bpm;
    for t in tracks {
        p.tracks.push(Arc::new(t));
    }
    for c in channels {
        p.channels.push(Arc::new(c));
    }
    let mut first = true;
    for b in beats {
        let b: Beat = b.into();
        if first {
            p.loop_region = LoopRegion {
                start: 0,
                end: b.len_ticks,
                enabled: true,
            };
            first = false;
        }
        for c in b.contents {
            p.clips.push(Clip {
                id: ClipId(1000 + c.id.0),
                instrument: c.instrument,
                pattern: c.id,
                start: 0,
                len: b.len_ticks,
                offset: 0,
                muted: false,
            });
            p.patterns.push(Arc::new(c));
        }
    }
    p
}

pub struct Rig {
    pub rt: Runtime,
    pub ui: UiEnds,
    pub shared: Shared,
    pub slots: Slots,
    pub sr: f64,
}

/// A runtime with the project compiled and installed, controls written,
/// tracing on and the transport started when `play`.
pub fn rig(project: &Project, sr: f64, play: bool) -> Rig {
    let mut slots = Slots::new();
    slots.sync(project).unwrap();
    let shared = Shared::new();
    write_controls(project, &slots, &shared.controls, &shared.params);
    let (ui, ends) = rings();
    let mut rt = Runtime::new(sr, shared.clone(), ends);
    rt.enable_trace(1 << 16);
    rt.install(compile(project, &slots, sr));
    if play {
        rt.command(EngineCommand::Play);
    }
    Rig {
        rt,
        ui,
        shared,
        slots,
        sr,
    }
}

impl Rig {
    /// Renders `frames` in callbacks of `cb` frames.
    pub fn run(&mut self, frames: usize, cb: usize) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![0.0f32; frames];
        let mut r = vec![0.0f32; frames];
        let mut at = 0;
        while at < frames {
            let n = cb.min(frames - at);
            self.rt
                .process_planar(&mut l[at..at + n], &mut r[at..at + n]);
            at += n;
        }
        (l, r)
    }
}

pub fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, v| m.max(v.abs()))
}

pub fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// Exact `round(ticks * sample_rate * 60 / (bpm * 960))` in integers for a
/// tempo given as a fraction `num / den` BPM.
pub fn ideal_sample(ticks: i128, rate: i128, num: i128, den: i128) -> u64 {
    let n = ticks * rate * 60 * den * 2 + num * 960;
    let d = num * 960 * 2;
    (n / d) as u64
}
