// SPDX-License-Identifier: GPL-3.0-or-later
//! A heavy but realistic beat for load measurements (`loadbench` and the
//! load test): 16 channels, 8 mixer tracks with EQ, compressor and
//! saturator, two return tracks, a sidechain, a limited master, and a song
//! of 8 clips. Everything is generated in memory.

use crate::samples::{SampleData, SampleStore};
use protocol::beats::{
    Bass808, BuiltinFx, BuiltinFxKind, SampleMode, Sampler, SamplerParams, SaturatorCurve,
};
use protocol::ids::{ChannelId, ClipId, InstanceId, NoteId, PatternId, PlaylistTrackId, TrackId};
use protocol::model::{
    Adsr, Channel, ChannelNotes, Clip, Insert, Instrument, Mix, Note, Osc, Pattern, PlaylistTrack,
    Project, SampleRef, Send, SynthParams, Track, Wave,
};
use std::sync::Arc;

/// Tempo of the load project.
pub const LOAD_BPM: f64 = 140.0;

/// One id counter for every kind of id (the validator wants them unique).
struct Ids(u32);

impl Ids {
    fn next(&mut self) -> u32 {
        self.0 += 1;
        self.0
    }
}

struct Rng(u64);

impl Rng {
    fn noise(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

/// Hash of the n-th synthetic sample (64 lowercase hex digits).
fn hash(n: u32) -> String {
    format!("{n:064x}")
}

/// The six synthetic one-shots at `rate`: kick, snare, clap, closed hat,
/// open hat, perc. Returns `(hash, data)`.
pub fn synthetic_samples(rate: u32) -> Vec<(String, SampleData)> {
    let sr = rate as f32;
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let secs = |s: f32| (s * sr) as usize;
    let tau = std::f32::consts::TAU;
    let mut out = Vec::new();

    // kick: sine drop 160 -> 45 Hz
    let mut ph = 0.0f32;
    let kick: Vec<f32> = (0..secs(0.45))
        .map(|i| {
            let t = i as f32 / sr;
            ph += tau * (45.0 + 115.0 * (-t * 30.0).exp()) / sr;
            ph.sin() * (-t * 7.0).exp()
        })
        .collect();
    out.push(SampleData::from_vec(1, rate, kick));

    // snare: tone plus noise burst
    let snare: Vec<f32> = (0..secs(0.28))
        .map(|i| {
            let t = i as f32 / sr;
            ((tau * 190.0 * t).sin() * (-t * 25.0).exp() * 0.5 + rng.noise() * (-t * 18.0).exp())
                * 0.8
        })
        .collect();
    out.push(SampleData::from_vec(1, rate, snare));

    // clap: three short noise bursts, then a tail
    let clap: Vec<f32> = (0..secs(0.25))
        .map(|i| {
            let t = i as f32 / sr;
            let burst = [0.0f32, 0.011, 0.022]
                .iter()
                .map(|&o| {
                    if t >= o {
                        (-(t - o) * 300.0).exp()
                    } else {
                        0.0
                    }
                })
                .fold(0.0, f32::max);
            rng.noise() * (burst * 0.8 + (-t * 14.0).exp() * 0.3)
        })
        .collect();
    out.push(SampleData::from_vec(1, rate, clap));

    // hats: differentiated noise, the open one in stereo
    for (len, decay, ch) in [(0.06f32, 70.0f32, 1usize), (0.35, 10.0, 2)] {
        let n = secs(len);
        let mut prev = 0.0f32;
        let mut data = Vec::with_capacity(n * ch);
        for i in 0..n {
            let t = i as f32 / sr;
            for _ in 0..ch {
                let x = rng.noise();
                let hp = x - prev;
                prev = x;
                data.push(hp * (-t * decay).exp() * 0.6);
            }
        }
        out.push(SampleData::from_vec(ch as u8, rate, data));
    }

    // perc: short pitched drop
    let mut ph = 0.0f32;
    let perc: Vec<f32> = (0..secs(0.18))
        .map(|i| {
            let t = i as f32 / sr;
            ph += tau * (500.0 + 700.0 * (-t * 40.0).exp()) / sr;
            ph.sin() * (-t * 22.0).exp() * 0.7
        })
        .collect();
    out.push(SampleData::from_vec(1, rate, perc));

    out.into_iter()
        .enumerate()
        .map(|(i, d)| (hash(i as u32 + 1), d))
        .collect()
}

/// A store at `rate` holding the load project's samples.
pub fn load_store(rate: u32) -> Arc<SampleStore> {
    let store = SampleStore::new(rate, 256 << 20);
    for (h, d) in synthetic_samples(rate) {
        store.insert(&h, d);
    }
    Arc::new(store)
}

fn synth(wave1: Wave, wave2: Wave, cutoff: f64, release_ms: f64) -> SynthParams {
    SynthParams {
        osc1: Osc {
            wave: wave1,
            semitones: 0.0,
            cents: -6.0,
        },
        osc2: Osc {
            wave: wave2,
            semitones: 0.0,
            cents: 7.0,
        },
        osc_mix: 0.5,
        cutoff_hz: cutoff,
        resonance: 0.3,
        filter_env_octaves: 2.5,
        amp_env: Adsr {
            attack_ms: 4.0,
            decay_ms: 250.0,
            sustain: 0.6,
            release_ms,
        },
        filter_env: Adsr {
            attack_ms: 2.0,
            decay_ms: 300.0,
            sustain: 0.3,
            release_ms: 200.0,
        },
        gain_db: -9.0,
    }
}

fn insert(ids: &mut Ids, f: BuiltinFx) -> Insert {
    Insert::Builtin {
        instance: InstanceId(ids.next()),
        fx: f,
    }
}

/// EQ, compressor (keyed by `key` when given) and saturator.
fn strip(ids: &mut Ids, key: Option<TrackId>) -> Vec<Insert> {
    let mut comp = BuiltinFx::new(BuiltinFxKind::Compressor);
    if let BuiltinFx::Compressor {
        sidechain, params, ..
    } = &mut comp
    {
        *sidechain = key;
        params.threshold_db = -20.0;
        params.ratio = 4.0;
    }
    let mut sat = BuiltinFx::new(BuiltinFxKind::Saturator);
    if let BuiltinFx::Saturator { curve, .. } = &mut sat {
        *curve = SaturatorCurve::Soft;
    }
    let mut eq = BuiltinFx::new(BuiltinFxKind::Eq);
    if let BuiltinFx::Eq { params } = &mut eq {
        params.low_cut_hz = 30.0;
        params.mid_gain_db = 2.0;
        params.high_gain_db = 1.5;
    }
    vec![insert(ids, eq), insert(ids, comp), insert(ids, sat)]
}

/// Notes of one pattern, collected per channel.
struct Notes {
    per_channel: Vec<(ChannelId, Vec<Note>)>,
}

impl Notes {
    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        ids: &mut Ids,
        ch: u32,
        start: u32,
        len: u32,
        key: u8,
        vel: u8,
        off: i8,
        repeat: u8,
    ) {
        let n = Note {
            id: NoteId(ids.next()),
            start,
            len,
            key,
            vel,
            off,
            repeat,
        };
        let ch = ChannelId(ch + CHANNEL_BASE);
        match self.per_channel.iter_mut().find(|c| c.0 == ch) {
            Some(c) => c.1.push(n),
            None => self.per_channel.push((ch, vec![n])),
        }
    }

    /// A step note (root key 60 + `off`): one 16th long on the grid, so
    /// swing applies to it.
    fn step(&mut self, ids: &mut Ids, ch: u32, step: u32, off: i8, vel: u8, repeat: u8) {
        self.push(
            ids,
            ch,
            step * 240,
            240,
            (60 + off as i16) as u8,
            vel,
            off,
            repeat,
        );
    }
}

/// Channel, track and instance ids share one namespace; channels start here.
const CHANNEL_BASE: u32 = 20;
const KICK: u32 = 1;
const SNARE: u32 = 2;
const CLAP: u32 = 3;
const HAT: u32 = 4;
const OHAT: u32 = 5;
const PERC: u32 = 6;
const B808: [u32; 4] = [7, 8, 9, 10];
const CHORD: [u32; 4] = [11, 12, 13, 14];
const LEAD: [u32; 2] = [15, 16];

fn pattern(ids: &mut Ids, variant: bool) -> Pattern {
    let mut n = Notes {
        per_channel: Vec::new(),
    };
    for s in 0..16u32 {
        // hats on every 16th step, ratchets 2 to 4
        let rep = [2u8, 3, 4, 2][(s % 4) as usize];
        n.step(ids, HAT, s, 0, 70 + (s % 3) as u8 * 15, rep);
        if s % 2 == 1 {
            n.step(ids, OHAT, s, 0, 80, 2 + (s % 3) as u8 % 3);
        }
        if s % 4 == 0 || (variant && s == 10) {
            n.step(ids, KICK, s, 0, 120, 1);
        }
        if s == 4 || s == 12 {
            n.step(ids, SNARE, s, 0, 110, 1);
            n.step(ids, CLAP, s, 0, 100, 1);
        }
        if variant && (s == 7 || s == 15) {
            n.step(ids, SNARE, s, 0, 80, 2);
        }
        if s % 3 == 2 {
            n.step(ids, PERC, s, 0, 90, if variant { 3 } else { 1 });
        }
    }
    // four 808 voices; the notes overlap a little so the glide runs
    let roots = [36u8, 43, 39, 46];
    for (i, &c) in B808.iter().enumerate() {
        for k in 0..4u32 {
            let key = roots[(k as usize + i) % 4] + if variant && k == 3 { 2 } else { 0 };
            n.push(
                ids,
                c,
                k * 960,
                if k == 3 { 960 } else { 1080 },
                key,
                110,
                0,
                1,
            );
        }
    }
    // four synths, each playing 4-note chords, one chord per beat
    let chords: [[u8; 4]; 4] = [
        [57, 60, 64, 67],
        [55, 59, 62, 65],
        [53, 57, 60, 64],
        [55, 58, 62, 67],
    ];
    for (i, &c) in CHORD.iter().enumerate() {
        for (beat, chord) in chords.iter().enumerate() {
            for &k in chord {
                n.push(
                    ids,
                    c,
                    beat as u32 * 960,
                    900,
                    k + 12 * (i as u8 % 2),
                    85,
                    0,
                    1,
                );
            }
        }
    }
    // two leads, 16th arpeggios on the step grid
    let arp: [i8; 8] = [9, 12, 16, 12, 19, 16, 12, 9];
    for (i, &c) in LEAD.iter().enumerate() {
        for s in 0..16u32 {
            if variant || s % 2 == i as u32 {
                n.step(ids, c, s, arp[(s as usize + 3 * i) % 8], 90, 1);
            }
        }
    }
    let mut p = Pattern::new(
        PatternId(ids.next()),
        if variant { "B" } else { "A" }.into(),
    );
    p.swing = 300; // 30 percent of a step
    n.per_channel.sort_by_key(|c| c.0);
    p.notes = n
        .per_channel
        .into_iter()
        .map(|(channel, mut notes)| {
            notes.sort_by_key(|n| (n.start, n.key, n.id));
            ChannelNotes { channel, notes }
        })
        .collect();
    p
}

/// The load project: see the module comment. Needs the samples of
/// `synthetic_samples` in the store used to compile it.
pub fn load_project() -> Project {
    load_project_clips(8)
}

/// The same with `clips` one-bar clips on the playlist, patterns A and B
/// alternating (every fourth is B).
pub fn load_project_clips(clips: u32) -> Project {
    let mut ids = Ids(100);
    let mut p = Project::empty();
    p.tempo_bpm = LOAD_BPM;

    // mixer: 1 kick, 2 snare+clap, 3 hats, 4 perc, 5 and 6 the 808s,
    // 7 chords, 8 leads; 9 reverb return, 10 delay return
    let (kick_t, ret_verb, ret_delay) = (TrackId(1), TrackId(9), TrackId(10));
    let send = |to: TrackId, db: f64| Send {
        to,
        level_db: db,
        pre_fader: false,
    };
    let sends: [Vec<Send>; 8] = [
        vec![],
        vec![send(ret_verb, -12.0)],
        vec![send(ret_delay, -14.0)],
        vec![send(ret_verb, -16.0), send(ret_delay, -18.0)],
        vec![],
        vec![],
        vec![send(ret_verb, -10.0), send(ret_delay, -12.0)],
        vec![send(ret_verb, -12.0), send(ret_delay, -9.0)],
    ];
    let names = [
        "Kick", "Snare", "Hats", "Perc", "808 A", "808 B", "Chords", "Leads",
    ];
    for i in 0..8usize {
        // the two 808 buses duck on the kick
        let key = (i == 4 || i == 5).then_some(kick_t);
        p.tracks.push(Arc::new(Track {
            id: TrackId(i as u32 + 1),
            name: names[i].into(),
            mix: Mix {
                volume_db: -3.0,
                pan: [0.0, 0.1, -0.2, 0.3, 0.0, 0.0, -0.15, 0.2][i],
                ..Mix::default()
            },
            inserts: strip(&mut ids, key),
            sends: sends[i].clone(),
        }));
    }
    let mut verb = BuiltinFx::new(BuiltinFxKind::Reverb);
    if let BuiltinFx::Reverb { params } = &mut verb {
        params.mix = 1.0;
    }
    let mut delay = BuiltinFx::new(BuiltinFxKind::Delay);
    if let BuiltinFx::Delay {
        ping_pong, params, ..
    } = &mut delay
    {
        *ping_pong = true;
        params.mix = 1.0;
    }
    for (id, name, f) in [(9, "Reverb", verb), (10, "Delay", delay)] {
        p.tracks.push(Arc::new(Track {
            id: TrackId(id),
            name: name.into(),
            mix: Mix::default(),
            inserts: vec![insert(&mut ids, f)],
            sends: vec![],
        }));
    }
    // master: EQ and limiter
    let master = Arc::make_mut(&mut p.tracks[0]);
    master.inserts = vec![
        insert(&mut ids, BuiltinFx::new(BuiltinFxKind::Eq)),
        insert(&mut ids, BuiltinFx::new(BuiltinFxKind::Limiter)),
    ];

    // channels
    let mut channel = |id: u32, name: &str, track: u32, instrument: Instrument| {
        p.channels.push(Arc::new(Channel {
            id: ChannelId(id + CHANNEL_BASE),
            name: name.into(),
            root_key: 60,
            track: TrackId(track),
            mix: Mix::default(),
            instrument,
            choke_group: 0,
        }));
    };
    let sampler = |n: u32, semitones: f64| {
        Instrument::Sampler(Sampler {
            sample: Some(hash(n)),
            mode: SampleMode::OneShot,
            reverse: false,
            params: SamplerParams {
                semitones,
                ..SamplerParams::default()
            },
        })
    };
    channel(KICK, "Kick", 1, sampler(1, 0.0));
    channel(SNARE, "Snare", 2, sampler(2, 0.0));
    channel(CLAP, "Clap", 2, sampler(3, 0.0));
    channel(HAT, "Hat", 3, sampler(4, 0.0));
    channel(OHAT, "Open hat", 3, sampler(5, 0.0));
    channel(PERC, "Perc", 4, sampler(6, 2.0));
    for (i, &c) in B808.iter().enumerate() {
        let mut b = Bass808::default();
        b.params.glide_ms = 80.0;
        b.params.drive = 0.5 + 0.1 * i as f64;
        channel(
            c,
            &format!("808 {}", i + 1),
            if i < 2 { 5 } else { 6 },
            Instrument::Bass808(b),
        );
    }
    for (i, &c) in CHORD.iter().enumerate() {
        let (w1, w2) = [
            (Wave::Saw, Wave::Square),
            (Wave::Saw, Wave::Saw),
            (Wave::Triangle, Wave::Saw),
            (Wave::Square, Wave::Triangle),
        ][i];
        channel(
            c,
            &format!("Chord {}", i + 1),
            7,
            Instrument::Synth(synth(w1, w2, 2500.0, 250.0)),
        );
    }
    for (i, &c) in LEAD.iter().enumerate() {
        channel(
            c,
            &format!("Lead {}", i + 1),
            8,
            Instrument::Synth(synth(Wave::Saw, Wave::Square, 6000.0, 120.0)),
        );
        let _ = i;
    }
    p.channels.sort_by_key(|c| c.id);

    // samples, patterns, song
    for (h, d) in synthetic_samples(48000) {
        p.samples.push(SampleRef {
            hash: h,
            orig_name: "synthetic.wav".into(),
            size: d.bytes() as u64,
            local_only: true,
        });
    }
    p.samples.sort_by(|a, b| a.hash.cmp(&b.hash));
    let a = Arc::new(pattern(&mut ids, false));
    let b = Arc::new(pattern(&mut ids, true));
    let (pa, pb) = (a.id, b.id);
    let bar = a.length_ticks();
    p.patterns = vec![a, b];
    let clips = (0..clips)
        .map(|i| Clip {
            id: ClipId(ids.next()),
            pattern: if i % 4 == 3 { pb } else { pa },
            start: i * bar,
            len: bar,
        })
        .collect();
    p.playlist.push(Arc::new(PlaylistTrack {
        id: PlaylistTrackId(ids.next()),
        name: "Song".into(),
        clips,
    }));
    p
}
