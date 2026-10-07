// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline renders with a sample store: sampler channels sound, match a live
//! run, keep pitch across sample rates, and degrade to silence plus warnings.

mod common;

use common::*;
use engine::render::{RenderRequest, SongRequest, render_song_with_block, render_with_block};
use engine::runtime::{Runtime, Shared, rings};
use engine::{SampleData, SampleStore, Slots, compile_with, write_controls};
use protocol::beats::{SampleMode, Sampler, SamplerParams};
use protocol::engine::{EngineCommand, TransportMode};
use protocol::ids::{ChannelId, ClipId, PatternId, PlaylistTrackId, TrackId};
use protocol::model::{Channel, Clip, Instrument, Mix, PlaylistTrack, Project};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32};

const SR: u32 = 48000;

fn sampler_channel(id: u32, hash: &str) -> Channel {
    Channel {
        id: ChannelId(id),
        name: format!("s{id}"),
        root_key: 60,
        track: TrackId(0),
        mix: Mix::default(),
        instrument: Instrument::Sampler(Sampler {
            sample: Some(hash.to_string()),
            mode: SampleMode::OneShot,
            reverse: false,
            params: SamplerParams::default(),
        }),
        choke_group: 0,
    }
}

/// One second of a 480 Hz mono sine at `rate`.
fn sine(rate: u32) -> SampleData {
    let v = (0..rate)
        .map(|i| (std::f64::consts::TAU * 480.0 * i as f64 / rate as f64).sin() as f32 * 0.8)
        .collect();
    SampleData::from_vec(1, rate, v)
}

fn store_with(rate: u32, hash: &str, d: SampleData) -> Arc<SampleStore> {
    let s = SampleStore::new(rate, 1 << 28);
    s.insert(hash, d);
    Arc::new(s)
}

/// One sampler note covering the whole pattern (16 steps, 2 s at 120 BPM).
fn sampler_project(hash: &str) -> Project {
    project(
        120.0,
        vec![],
        vec![sampler_channel(1, hash)],
        vec![pattern(1, 16, &[(1, vec![(1, 0, 3840, 60, 127)])])],
    )
}

fn request(p: Project, rate: u32, store: Option<Arc<SampleStore>>) -> (RenderRequest, Slots) {
    let mut slots = Slots::new();
    slots.sync(&p).unwrap();
    (
        RenderRequest {
            project: Arc::new(p),
            pattern: PatternId(1),
            loops: 1,
            tail_seconds: 0.0,
            sample_rate: rate,
            store,
        },
        slots,
    )
}

fn bits(v: &[[f32; 2]]) -> Vec<[u32; 2]> {
    v.iter().map(|f| [f[0].to_bits(), f[1].to_bits()]).collect()
}

fn zero_crossings(v: &[[f32; 2]]) -> usize {
    v.windows(2)
        .filter(|w| (w[0][0] < 0.0) != (w[1][0] < 0.0) && w[0][0] != 0.0 && w[1][0] != 0.0)
        .count()
}

fn render(req: &RenderRequest, slots: &Slots, cb: usize) -> engine::render::Rendered {
    render_with_block(
        req,
        slots,
        &[],
        &AtomicU32::new(0),
        &AtomicBool::new(false),
        cb,
    )
    .unwrap()
}

#[test]
fn sampler_render_equals_a_live_run_for_any_callback_size() {
    let p = sampler_project("sine");
    let store = store_with(SR, "sine", sine(SR));
    let (req, slots) = request(p.clone(), SR, Some(store.clone()));
    let reference = render(&req, &slots, 512);
    assert!(reference.warnings.is_empty(), "{:?}", reference.warnings);
    let peak = reference
        .audio
        .iter()
        .flatten()
        .fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(peak > 0.3, "the sampler must sound, peak {peak}");

    // The live path: the same project and store through a plain Runtime.
    let shared = Shared::new();
    write_controls(&p, &slots, &shared.controls, &shared.params);
    let (_ui, ends) = rings();
    let mut rt = Runtime::new(SR as f64, shared, ends);
    rt.set_metronome_allowed(false);
    rt.install(compile_with(&p, &slots, SR as f64, Some(&store)));
    rt.command(EngineCommand::SetPlayingPattern {
        pattern: PatternId(1),
    });
    rt.command(EngineCommand::Seek { tick: 0 });
    rt.command(EngineCommand::Play);
    let n = reference.audio.len();
    let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
    let mut done = 0;
    while done < n {
        let k = 333.min(n - done);
        rt.process_planar(&mut l[done..done + k], &mut r[done..done + k]);
        done += k;
    }
    let live: Vec<[f32; 2]> = l.iter().zip(&r).map(|(&a, &b)| [a, b]).collect();
    assert!(
        bits(&live) == bits(&reference.audio),
        "live differs from render"
    );

    for cb in [7usize, 64, 1000, 4096] {
        let out = render(&req, &slots, cb);
        assert!(bits(&out.audio) == bits(&reference.audio), "block {cb}");
    }
}

#[test]
fn render_rate_differing_from_the_store_rate_keeps_pitch() {
    let p = sampler_project("sine");
    // Reference: the store at the render rate.
    let (req, slots) = request(
        p.clone(),
        44100,
        Some(store_with(44100, "sine", sine(44100))),
    );
    let want = zero_crossings(&render(&req, &slots, 512).audio);
    // A 48 kHz store rendered at 44.1 kHz.
    let (req, slots) = request(p, 44100, Some(store_with(SR, "sine", sine(SR))));
    let out = render(&req, &slots, 512);
    assert!(out.warnings.is_empty());
    assert_eq!(out.audio.len(), 88200);
    let got = zero_crossings(&out.audio);
    // 480 Hz for 1 s is 960 crossings.
    assert!((got as f64 - 960.0).abs() < 9.6, "{got}");
    assert!((got as f64 - want as f64).abs() < 9.6, "{got} vs {want}");
}

fn song_with_sampler(hash: &str) -> Project {
    let mut p = sampler_project(hash);
    p.playlist.push(Arc::new(PlaylistTrack {
        id: PlaylistTrackId(50),
        name: "A".into(),
        clips: vec![Clip {
            id: ClipId(51),
            pattern: PatternId(1),
            start: 960,
            len: 1920,
        }],
    }));
    p
}

#[test]
fn render_song_plays_samplers() {
    let p = song_with_sampler("sine");
    let store = store_with(SR, "sine", sine(SR));
    let mut slots = Slots::new();
    slots.sync(&p).unwrap();
    let req = SongRequest {
        project: Arc::new(p.clone()),
        tail_seconds: 0.0,
        sample_rate: SR,
        store: Some(store.clone()),
    };
    let run = |cb| {
        render_song_with_block(
            &req,
            &slots,
            &[],
            &AtomicU32::new(0),
            &AtomicBool::new(false),
            cb,
        )
        .unwrap()
    };
    let a = run(512);
    assert!(a.warnings.is_empty());
    let first = a.audio.iter().position(|f| f[0].abs() > 0.01).unwrap();
    // the clip starts at tick 960: 24000 frames at 120 BPM and 48 kHz
    assert!((24000..24100).contains(&first), "{first}");
    assert!(bits(&run(97).audio) == bits(&a.audio));

    // And the same through a live song run.
    let shared = Shared::new();
    write_controls(&p, &slots, &shared.controls, &shared.params);
    let (_ui, ends) = rings();
    let mut rt = Runtime::new(SR as f64, shared, ends);
    rt.set_metronome_allowed(false);
    rt.install(compile_with(&p, &slots, SR as f64, Some(&store)));
    rt.command(EngineCommand::SetTransportMode {
        mode: TransportMode::Song,
        loop_song: false,
    });
    rt.command(EngineCommand::Seek { tick: 0 });
    rt.command(EngineCommand::Play);
    let n = a.audio.len();
    let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
    rt.process_planar(&mut l, &mut r);
    for (i, f) in a.audio.iter().enumerate() {
        assert_eq!((f[0], f[1]), (l[i], r[i]), "frame {i}");
    }
}

#[test]
fn missing_samples_render_silence_with_warnings() {
    let p = sampler_project("ghost");
    // Not in the store.
    let (req, slots) = request(p.clone(), SR, Some(store_with(SR, "other", sine(SR))));
    let out = render(&req, &slots, 512);
    assert!(out.audio.iter().flatten().all(|v| *v == 0.0));
    assert_eq!(out.warnings.len(), 1);
    assert!(out.warnings[0].contains("ghost"), "{:?}", out.warnings);

    // No store at all.
    let (req, slots) = request(p.clone(), SR, None);
    let out = render(&req, &slots, 512);
    assert!(out.audio.iter().flatten().all(|v| *v == 0.0));
    assert_eq!(out.warnings.len(), 1);

    // A load that fails: the render waits for the verdict, then warns.
    let store = Arc::new(SampleStore::new(SR, 1 << 28));
    store.request("ghost", PathBuf::from("/nonexistent/ghost.wav"));
    let (req, slots) = request(p, SR, Some(store));
    let out = render(&req, &slots, 512);
    assert!(out.audio.iter().flatten().all(|v| *v == 0.0));
    assert_eq!(out.warnings.len(), 1);
    assert!(out.warnings[0].contains("failed"), "{:?}", out.warnings);
}
