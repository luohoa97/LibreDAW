// SPDX-License-Identifier: GPL-3.0-or-later
//! v4 engine features: audio clips (21.1), insert bypass (24.1) and shapes
//! (24.2-1).

mod common;

use common::*;
use engine::render::{RangeRequest, render_range_with_block};
use engine::runtime::{Runtime, Shared, rings};
use engine::samples::{SampleData, SampleStore, hash_hex};
use engine::{Slots, compile_with, write_controls};
use protocol::beats::{BuiltinFx, BuiltinFxKind, LimiterParam};
use protocol::engine::{EngineCommand, TrackSlot, fx_param_index};
use protocol::ids::{ChannelId, ClipId, InstanceId, PatternId, ShapeId, TrackId};
use protocol::model::{
    AudioSource, Channel, Clip, Curve, Insert, Instrument, LoopRegion, Mix, Project, SampleHash,
    Shape, ShapePoint, ShapeTarget,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32};

const SR: f64 = 48000.0;
/// 120 BPM at 48 kHz: 25 samples per tick, 96000 per 4/4 bar (3840 ticks).
const BAR: u32 = 3840;
const BAR_S: usize = 96000;

fn hash(n: u8) -> SampleHash {
    SampleHash([n; 32])
}

/// Stereo sample whose left frame `i` is `(i + 1) / len` and right is half.
fn ramp(frames: usize) -> SampleData {
    let mut v = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let x = (i + 1) as f32 / frames as f32;
        v.push(x);
        v.push(x * 0.5);
    }
    SampleData::from_vec(2, SR as u32, v)
}

fn constant(frames: usize, x: f32) -> SampleData {
    SampleData::from_vec(2, SR as u32, vec![x; frames * 2])
}

fn store_with(items: Vec<(u8, SampleData)>) -> SampleStore {
    let store = SampleStore::new(SR as u32, 1 << 30);
    for (n, d) in items {
        store.insert(&hash_hex(&hash(n).0), d);
    }
    store
}

fn audio_row(id: u32, track: u32) -> Channel {
    Channel {
        id: ChannelId(id),
        name: format!("audio{id}"),
        root_key: 60,
        track: TrackId(track),
        mix: Mix::default(),
        instrument: Instrument::Audio,
        choke_group: 0,
    }
}

#[derive(Clone, Copy)]
struct AClip {
    row: u32,
    sample: u8,
    start: u32,
    len: u32,
    offset: u32,
    gain_mdb: i32,
    fade_in: u32,
    fade_out: u32,
}

fn aclip(row: u32, sample: u8, start: u32, len: u32) -> AClip {
    AClip {
        row,
        sample,
        start,
        len,
        offset: 0,
        gain_mdb: 0,
        fade_in: 0,
        fade_out: 0,
    }
}

fn base_project(clips: &[AClip]) -> Project {
    let mut p = Project::empty();
    p.tracks.push(Arc::new(track(1)));
    p.channels.push(Arc::new(audio_row(1, 1)));
    for (i, c) in clips.iter().enumerate() {
        p.clips.push(Clip {
            id: ClipId(100 + i as u32),
            instrument: ChannelId(c.row),
            pattern: PatternId::NONE,
            start: c.start,
            len: c.len,
            offset: c.offset,
            muted: false,
            audio: Some(AudioSource {
                sample: hash(c.sample),
                gain_mdb: c.gain_mdb,
                fade_in: c.fade_in,
                fade_out: c.fade_out,
            }),
            group: None,
        });
    }
    p
}

struct Setup {
    rt: Runtime,
    shared: Shared,
    slots: Slots,
}

fn setup(p: &Project, store: &SampleStore, play: bool) -> Setup {
    let mut slots = Slots::new();
    slots.sync(p).unwrap();
    let shared = Shared::new();
    write_controls(p, &slots, &shared.controls, &shared.params);
    let (_ui, ends) = rings();
    let mut rt = Runtime::new(SR, shared.clone(), ends);
    rt.install(compile_with(p, &slots, SR, Some(store)));
    if play {
        rt.command(EngineCommand::Play);
    }
    Setup { rt, shared, slots }
}

fn run(rt: &mut Runtime, frames: usize, cb: usize) -> (Vec<f32>, Vec<f32>) {
    let mut l = vec![0.0f32; frames];
    let mut r = vec![0.0f32; frames];
    let mut at = 0;
    while at < frames {
        let n = cb.min(frames - at);
        rt.process_planar(&mut l[at..at + n], &mut r[at..at + n]);
        at += n;
    }
    (l, r)
}

fn near(a: f32, b: f32, tol: f32, what: &str) {
    assert!((a - b).abs() <= tol, "{what}: {a} vs {b}");
}

#[test]
fn clip_at_bar_two_plays_the_sample_with_trim_and_length() {
    let store = store_with(vec![(1, ramp(20000))]);
    // starts at bar 2, skips 480 ticks (12000 frames), 960 ticks long
    // (24000 frames) but only 8000 frames of sample are left
    let mut c = aclip(1, 1, BAR, 960);
    c.offset = 480;
    let p = base_project(&[c]);
    let mut s = setup(&p, &store, true);
    let (l, r) = run(&mut s.rt, BAR_S + 40000, 256);
    assert_eq!(peak(&l[..BAR_S]), 0.0, "silent before the clip");
    near(l[BAR_S], 12001.0 / 20000.0, 1e-5, "first frame");
    near(r[BAR_S], 12001.0 / 40000.0, 1e-5, "right channel");
    near(l[BAR_S + 7999], 1.0, 1e-5, "last frame of the sample");
    assert_eq!(
        peak(&l[BAR_S + 8000..]),
        0.0,
        "silent after the sample ends"
    );
}

#[test]
fn clip_length_cuts_the_sample() {
    let store = store_with(vec![(1, constant(100000, 0.5))]);
    let p = base_project(&[aclip(1, 1, 960, 480)]); // 24000..36000
    let mut s = setup(&p, &store, true);
    let (l, _) = run(&mut s.rt, 60000, 100);
    assert_eq!(l[23999], 0.0);
    near(l[24000], 0.5, 1e-5, "first");
    near(l[35999], 0.5, 1e-5, "last");
    assert_eq!(l[36000], 0.0);
}

#[test]
fn gain_and_fades_are_linear_in_ticks() {
    let store = store_with(vec![(1, constant(200000, 1.0))]);
    let mut c = aclip(1, 1, 0, 2880); // 72000 frames
    c.fade_in = 960; // 24000 frames
    c.fade_out = 960;
    let p = base_project(&[c]);
    let mut s = setup(&p, &store, true);
    let (l, _) = run(&mut s.rt, 80000, 256);
    near(l[0], 0.0, 1e-4, "starts silent");
    near(l[12000], 0.5, 1e-3, "half way in");
    near(l[24000], 1.0, 1e-3, "full");
    near(l[36000], 1.0, 1e-5, "middle");
    near(l[60000], 0.5, 1e-3, "half way out");
    near(l[71999], 1.0 / 24000.0, 1e-4, "ends silent");
    assert_eq!(l[72000], 0.0);
    // clip gain
    let mut c = aclip(1, 1, 0, 960);
    c.gain_mdb = -6000;
    let p = base_project(&[c]);
    let mut s = setup(&p, &store, true);
    let (l, _) = run(&mut s.rt, 1000, 256);
    near(l[500], 10f32.powf(-6.0 / 20.0), 1e-5, "-6 dB");
}

#[test]
fn a_split_clip_equals_one_clip() {
    for bpm in [120.0, 133.33] {
        let store = store_with(vec![(1, ramp(150000))]);
        let mut one = base_project(&[aclip(1, 1, 480, 3840)]);
        let mut two = {
            let a = aclip(1, 1, 480, 1920);
            let mut b = aclip(1, 1, 2400, 1920);
            b.offset = 1920;
            base_project(&[a, b])
        };
        one.tempo_bpm = bpm;
        two.tempo_bpm = bpm;
        let mut a = setup(&one, &store, true);
        let mut b = setup(&two, &store, true);
        let (la, ra) = run(&mut a.rt, 120000, 256);
        let (lb, rb) = run(&mut b.rt, 120000, 77);
        assert!(peak(&la) > 0.5);
        for i in 0..la.len() {
            assert!((la[i] - lb[i]).abs() <= 1e-6, "left {i} at {bpm}");
            assert!((ra[i] - rb[i]).abs() <= 1e-6, "right {i} at {bpm}");
        }
    }
}

#[test]
fn the_loop_wraps_inside_a_clip_and_a_seek_lands_mid_clip() {
    let store = store_with(vec![(1, ramp(400000))]);
    let mut p = base_project(&[aclip(1, 1, 0, 7680)]);
    p.loop_region = LoopRegion {
        start: 480,
        end: 1920,
        enabled: true,
    }; // 12000..48000
    let mut a = setup(&p, &store, true);
    a.rt.command(EngineCommand::Seek { tick: 480 });
    let (l, _) = run(&mut a.rt, 120000, 256);
    let (l2, _) = {
        let mut b = setup(&p, &store, true);
        b.rt.command(EngineCommand::Seek { tick: 480 });
        run(&mut b.rt, 120000, 191)
    };
    assert_eq!(l, l2, "wrap is independent of the callback size");
    // the seek put tick 480 (frame 12000 of the sample) at output frame 0
    near(l[0], 12001.0 / 400000.0, 1e-6, "seek position");
    // loop length 36000 frames: after it the output repeats
    for k in [0usize, 1, 100, 5000, 35999] {
        assert_eq!(l[36000 + k], l[k], "frame {k} after the wrap");
        assert_eq!(l[72000 + k], l[k], "frame {k} after two wraps");
    }
}

#[test]
fn seeking_while_playing_starts_at_the_right_position() {
    let store = store_with(vec![(1, ramp(400000))]);
    let p = base_project(&[aclip(1, 1, 0, 7680)]);
    let mut s = setup(&p, &store, true);
    let _ = run(&mut s.rt, 1000, 256);
    s.rt.command(EngineCommand::Seek { tick: 960 });
    let (l, _) = run(&mut s.rt, 600, 256);
    near(l[0], 24001.0 / 400000.0, 1e-6, "after seek");
    near(l[100], 24101.0 / 400000.0, 1e-6, "after seek +100");
}

#[test]
fn muted_clips_and_muted_rows_are_silent() {
    let store = store_with(vec![(1, constant(50000, 0.5))]);
    let mut p = base_project(&[aclip(1, 1, 0, 960)]);
    let mut c = p.clips[0];
    c.muted = true;
    p.clips[0] = c;
    let mut s = setup(&p, &store, true);
    let (l, _) = run(&mut s.rt, 2000, 256);
    assert_eq!(peak(&l), 0.0);
    let mut p = base_project(&[aclip(1, 1, 0, 960)]);
    let mut ch = (*p.channels[0]).clone();
    ch.mix.mute = true;
    p.channels[0] = Arc::new(ch);
    let mut s = setup(&p, &store, true);
    let (l, _) = run(&mut s.rt, 2000, 256);
    assert_eq!(peak(&l), 0.0);
}

#[test]
fn a_clip_sample_of_the_wrong_hash_is_silent_and_the_song_still_ends() {
    let store = store_with(vec![]);
    let p = base_project(&[aclip(1, 9, 0, 960)]);
    let c = compile_with(
        &p,
        &{
            let mut s = Slots::new();
            s.sync(&p).unwrap();
            s
        },
        SR,
        Some(&store),
    );
    assert_eq!(c.song_len_ticks, 960);
}

#[test]
fn offline_render_includes_audio_clips() {
    let store = Arc::new(store_with(vec![(1, constant(100000, 0.25))]));
    let p = base_project(&[aclip(1, 1, BAR, 960)]);
    let mut slots = Slots::new();
    slots.sync(&p).unwrap();
    let req = RangeRequest {
        project: Arc::new(p),
        range: None,
        tail_seconds: 0.1,
        sample_rate: SR as u32,
        store: Some(store),
    };
    let progress = AtomicU32::new(0);
    let cancel = AtomicBool::new(false);
    let out = render_range_with_block(&req, &slots, &[], &progress, &cancel, 512).unwrap();
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    // 0..end of clip, where the end is bar 2 + 960 ticks
    assert_eq!(out.audio.len(), BAR_S + 24000 + 4800);
    assert_eq!(out.audio[BAR_S - 1][0], 0.0);
    near(out.audio[BAR_S][0], 0.25, 1e-5, "first");
    near(out.audio[BAR_S + 23999][0], 0.25, 1e-5, "last");
    assert_eq!(out.audio[BAR_S + 24000][0], 0.0);
    let other = render_range_with_block(&req, &slots, &[], &progress, &cancel, 77).unwrap();
    assert!(out.audio == other.audio, "callback size independent");
}

fn limiter_insert(bypass: bool) -> Insert {
    let mut fx = BuiltinFx::new(BuiltinFxKind::Limiter);
    fx.set_param(LimiterParam::CeilingDb.index(), -20.0);
    Insert::Builtin {
        instance: InstanceId(10),
        fx,
        bypass,
    }
}

fn with_track1(p: &mut Project, f: impl FnOnce(&mut protocol::model::Track)) {
    let t = p.tracks.iter_mut().find(|t| t.id == TrackId(1)).unwrap();
    f(Arc::make_mut(t));
}

#[test]
fn bypass_skips_the_insert_and_toggles_within_a_block() {
    let store = store_with(vec![(1, constant(400000, 0.9))]);
    let mut p = base_project(&[aclip(1, 1, 0, 7680)]);
    with_track1(&mut p, |t| t.inserts.push(limiter_insert(false)));
    let mut s = setup(&p, &store, true);
    let (l, _) = run(&mut s.rt, 2000, 256);
    let limited = l[1999];
    assert!(limited < 0.12, "limited to -20 dB: {limited}");
    // bypass on: the next block is the dry signal
    let mut q = p.clone();
    with_track1(&mut q, |t| t.inserts[0] = limiter_insert(true));
    s.rt.install(compile_with(&q, &s.slots, SR, Some(&store)));
    let (l, _) = run(&mut s.rt, 512, 256);
    near(l[0], 0.9, 1e-5, "dry from the first frame");
    near(l[511], 0.9, 1e-5, "dry");
    // and off again: processed again
    s.rt.install(compile_with(&p, &s.slots, SR, Some(&store)));
    let (l, _) = run(&mut s.rt, 2000, 256);
    assert!(l[1999] < 0.12, "{}", l[1999]);
}

fn shape(id: u32, target: ShapeTarget, pts: &[(u32, f32, Curve)]) -> Shape {
    Shape {
        id: ShapeId(id),
        target,
        points: pts
            .iter()
            .map(|&(tick, value, curve)| ShapePoint { tick, value, curve })
            .collect(),
    }
}

#[test]
fn a_volume_shape_fades_the_track_from_minus_60_to_0_db() {
    let store = store_with(vec![(1, constant(400000, 1.0))]);
    let mut p = base_project(&[aclip(1, 1, 0, 2 * BAR)]);
    p.shapes.push(shape(
        1,
        ShapeTarget::Volume { track: TrackId(1) },
        &[(0, -60.0, Curve::Linear), (BAR, 0.0, Curve::Hold)],
    ));
    let mut s = setup(&p, &store, true);
    let (l, _) = run(&mut s.rt, BAR_S + 4000, 100);
    assert!(l[100] < 0.002, "starts silent: {}", l[100]);
    for (frame, db) in [
        (BAR_S / 4, -45.0f32),
        (BAR_S / 2, -30.0),
        (3 * BAR_S / 4, -15.0),
    ] {
        let want = 10f32.powf(db / 20.0);
        let got = l[frame];
        assert!(
            (got / want - 1.0).abs() < 0.04,
            "frame {frame}: {got} vs {want}"
        );
    }
    near(l[BAR_S + 2000], 1.0, 1e-4, "ends at 0 dB");
    // the control table (what the UI reads) followed the shape
    let ts = s.slots.track_slot(TrackId(1)).unwrap();
    let v = s.shared.controls.get(protocol::engine::track_control(
        TrackSlot(ts.0),
        protocol::engine::MixControl::VolumeDb,
    ));
    near(v, 0.0, 1e-4, "control table");
}

#[test]
fn a_pan_shape_moves_the_sound_across() {
    let store = store_with(vec![(1, constant(400000, 1.0))]);
    let mut p = base_project(&[aclip(1, 1, 0, 2 * BAR)]);
    p.shapes.push(shape(
        1,
        ShapeTarget::Pan { track: TrackId(1) },
        &[(0, -1.0, Curve::Linear), (BAR, 1.0, Curve::Hold)],
    ));
    let mut s = setup(&p, &store, true);
    let (l, r) = run(&mut s.rt, BAR_S + 2000, 256);
    assert!(l[200] > 0.98 && r[200] < 0.05, "{} {}", l[200], r[200]);
    near(l[BAR_S / 2], r[BAR_S / 2], 0.03, "centre");
    assert!(r[BAR_S + 1500] > 0.98 && l[BAR_S + 1500] < 0.02);
}

fn zero_crossings(x: &[f32]) -> usize {
    x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count()
}

#[test]
fn a_pitch_shape_transposes_the_instrument() {
    let mut p = project(
        120.0,
        vec![track(1)],
        vec![synth_channel(1, 1, tone_params())],
        vec![pattern(1, 16, &[(1, vec![(1, 0, 3840, 69, 127)])])],
    );
    p.loop_region = LoopRegion::default();
    let count = |p: &Project| {
        let mut r = rig(p, SR, true);
        let (l, _) = r.run(48000, 256);
        zero_crossings(&l[2000..])
    };
    let dry = count(&p);
    p.shapes.push(shape(
        1,
        ShapeTarget::Pitch {
            instrument: ChannelId(1),
        },
        &[(0, 12.0, Curve::Hold)],
    ));
    let up = count(&p);
    assert!((dry as i64 - 422).abs() <= 2, "{dry}");
    assert!((up as i64 - 843).abs() <= 3, "{up}");
}

#[test]
fn a_filter_shape_closes_the_channel() {
    // 8 kHz sine as audio
    let tone: Vec<f32> = (0..96000)
        .flat_map(|i| {
            let x = (i as f32 * 8000.0 * std::f32::consts::TAU / 48000.0).sin();
            [x, x]
        })
        .collect();
    let store = store_with(vec![(1, SampleData::from_vec(2, SR as u32, tone))]);
    let mut p = base_project(&[aclip(1, 1, 0, 1920)]);
    let mut s = setup(&p, &store, true);
    let (l, _) = run(&mut s.rt, 40000, 256);
    let open = rms(&l[10000..40000]);
    p.shapes.push(shape(
        1,
        ShapeTarget::Filter {
            instrument: ChannelId(1),
        },
        &[(0, 0.3, Curve::Hold)],
    ));
    let mut s = setup(&p, &store, true);
    let (l, _) = run(&mut s.rt, 40000, 256);
    let closed = rms(&l[10000..40000]);
    assert!(open > 0.5, "{open}");
    assert!(closed < open * 0.01, "{closed} vs {open}");
}

#[test]
fn an_fx_param_shape_drives_the_effect() {
    let store = store_with(vec![(1, constant(400000, 0.9))]);
    let mut p = base_project(&[aclip(1, 1, 0, 2 * BAR)]);
    with_track1(&mut p, |t| {
        let mut fx = BuiltinFx::new(BuiltinFxKind::Limiter);
        fx.set_param(LimiterParam::CeilingDb.index(), 0.0);
        t.inserts.push(Insert::Builtin {
            instance: InstanceId(10),
            fx,
            bypass: false,
        });
    });
    p.shapes.push(shape(
        1,
        ShapeTarget::FxParam {
            track: TrackId(1),
            instance: InstanceId(10),
            param: LimiterParam::CeilingDb.index() as u16,
        },
        &[(0, 0.0, Curve::Linear), (BAR, -20.0, Curve::Hold)],
    ));
    let mut s = setup(&p, &store, true);
    let (l, _) = run(&mut s.rt, BAR_S + 4000, 256);
    near(l[3000], 0.9, 1e-3, "ceiling open at the start");
    let want_mid = 10f32.powf(-10.0 / 20.0);
    assert!((l[BAR_S / 2] - want_mid).abs() < 0.02, "{}", l[BAR_S / 2]);
    assert!(l[BAR_S + 3000] < 0.101, "{}", l[BAR_S + 3000]);
    let ts = s.slots.track_slot(TrackId(1)).unwrap();
    let v = s.shared.params.get(fx_param_index(
        TrackSlot(ts.0),
        0,
        LimiterParam::CeilingDb.index(),
    ));
    near(v, -20.0, 1e-4, "param table");
}

#[test]
fn shapes_are_independent_of_the_callback_size_and_do_not_allocate() {
    let store = store_with(vec![(1, ramp(400000))]);
    let mut p = base_project(&[aclip(1, 1, 0, 2 * BAR)]);
    with_track1(&mut p, |t| t.inserts.push(limiter_insert(false)));
    p.shapes.push(shape(
        1,
        ShapeTarget::Volume { track: TrackId(1) },
        &[
            (0, -40.0, Curve::Smooth),
            (BAR, 0.0, Curve::Wave),
            (2 * BAR, -10.0, Curve::Hold),
        ],
    ));
    p.shapes.push(shape(
        2,
        ShapeTarget::Pan { track: TrackId(1) },
        &[
            (0, -1.0, Curve::Stairs),
            (BAR, 1.0, Curve::Pulse),
            (2 * BAR, 0.0, Curve::Hold),
        ],
    ));
    p.shapes.push(shape(
        3,
        ShapeTarget::Filter {
            instrument: ChannelId(1),
        },
        &[(0, 0.2, Curve::Linear), (BAR, 1.0, Curve::Hold)],
    ));
    let mut reference = None;
    for cb in [256usize, 1, 7, 64, 100, 513] {
        let mut s = setup(&p, &store, true);
        let (l, r) = run(&mut s.rt, 60000, cb);
        match &reference {
            None => reference = Some((l, r)),
            Some((rl, rr)) => assert!(*rl == l && *rr == r, "callback {cb} differs"),
        }
    }
}
