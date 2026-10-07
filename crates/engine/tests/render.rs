// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline render and the WAV writer.

mod common;

use common::*;
use engine::render::{RangeRequest, render_range_with_block};
use engine::wav::write_wav_to;
use engine::{EngineError, Slots, render_range};
use protocol::control::WavFormat;
use protocol::model::SynthParams;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

fn song() -> protocol::model::Project {
    let mut p = project(
        133.33,
        vec![track(1)],
        vec![
            synth_channel(1, 1, SynthParams::default()),
            synth_channel(2, 0, tone_params()),
        ],
        vec![pattern(
            1,
            16,
            &[
                (
                    1,
                    vec![
                        (1, 0, 960, 48, 100),
                        (2, 480, 960, 48, 90), // overlapping same key
                        (3, 960, 480, 55, 80),
                        (4, 2000, 3000, 60, 127), // crosses the clip end
                        (5, 3600, 120, 72, 60),
                    ],
                ),
                (2, vec![(6, 0, 240, 69, 100), (7, 1000, 200, 64, 100)]),
            ],
        )],
    );
    p.metronome.enabled = true;
    let mut c = (*p.channels[0]).clone();
    c.mix.pan = -0.4;
    c.mix.volume_db = -3.0;
    p.channels[0] = Arc::new(c);
    p
}

fn request(p: protocol::model::Project) -> (RangeRequest, Slots) {
    let mut slots = Slots::new();
    slots.sync(&p).unwrap();
    (
        RangeRequest {
            project: Arc::new(p),
            range: Some((0, 3840)),
            tail_seconds: 0.5,
            sample_rate: 44100,
            store: None,
        },
        slots,
    )
}

fn bits(v: &[[f32; 2]]) -> Vec<[u32; 2]> {
    v.iter().map(|f| [f[0].to_bits(), f[1].to_bits()]).collect()
}

fn render(req: &RangeRequest, slots: &Slots) -> Vec<[f32; 2]> {
    render_range(req, slots, &[], &AtomicU32::new(0), &AtomicBool::new(false))
        .unwrap()
        .audio
}

#[test]
fn offline_render_is_bit_identical_for_any_callback_size() {
    let (req, slots) = request(song());
    let progress = AtomicU32::new(0);
    let cancel = AtomicBool::new(false);
    let reference = render_range_with_block(&req, &slots, &[], &progress, &cancel, 512)
        .unwrap()
        .audio;
    // 3840 ticks at 133.33 BPM and 44.1 kHz, plus 0.5 s of tail
    let main = ideal_sample(3840, 44100, 13333, 100) as usize;
    assert_eq!(reference.len(), main + 22050);
    assert_eq!(progress.load(Relaxed), 100);
    let peak = reference
        .iter()
        .flatten()
        .fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(peak > 0.05 && peak < 1.5, "{peak}");
    assert!(reference.iter().flatten().all(|v| v.is_finite()));
    // the tail is the release of the last notes and decays to silence
    let end = reference[reference.len() - 2000..]
        .iter()
        .flatten()
        .fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(end < 1e-3, "{end}");
    for cb in [7usize, 64, 255, 256, 257, 1000, 4096] {
        let out = render_range_with_block(&req, &slots, &[], &progress, &cancel, cb)
            .unwrap()
            .audio;
        assert!(bits(&out) == bits(&reference), "callback size {cb} differs");
    }
    // the public entry point renders the same thing
    assert!(bits(&render(&req, &slots)) == bits(&reference));
}

#[test]
fn offline_render_leaves_the_metronome_out_and_validates_requests() {
    let (req, slots) = request(song());
    let a = render(&req, &slots);
    let mut quiet = song();
    quiet.metronome.enabled = false;
    let (req2, slots2) = request(quiet);
    let b = render(&req2, &slots2);
    assert!(bits(&a) == bits(&b), "export has no click");

    let err = |r: Result<_, EngineError>| r.err().unwrap();
    let p = AtomicU32::new(0);
    let c = AtomicBool::new(false);
    let (mut bad, slots) = request(song());
    bad.range = Some((960, 960));
    assert!(matches!(
        err(render_range(&bad, &slots, &[], &p, &c)),
        EngineError::Invalid(_)
    ));
    bad.range = Some((1000, 10));
    assert!(matches!(
        err(render_range(&bad, &slots, &[], &p, &c)),
        EngineError::Invalid(_)
    ));
    bad.range = Some((0, 960));
    bad.sample_rate = 0;
    assert!(matches!(
        err(render_range(&bad, &slots, &[], &p, &c)),
        EngineError::Invalid(_)
    ));
    // nothing to render: no clips and no range
    let empty = project(120.0, vec![], vec![], Vec::<Beat>::new());
    let (mut e, slots) = request(empty);
    e.range = None;
    assert!(matches!(
        err(render_range(&e, &slots, &[], &p, &c)),
        EngineError::Invalid(_)
    ));
}

#[test]
fn the_default_range_is_the_loop_region_or_the_whole_arrangement() {
    // `song()` has clips 0..3840 and the loop region over the same.
    let (mut req, slots) = request(song());
    let whole = render(&req, &slots);
    req.range = None;
    // Loop region enabled: the loop region, 0..3840.
    assert!(bits(&render(&req, &slots)) == bits(&whole));
    // Loop region disabled: 0..arrangement end, the same here.
    let mut p = song();
    p.loop_region.enabled = false;
    let (mut req2, slots2) = request(p);
    req2.range = None;
    assert!(bits(&render(&req2, &slots2)) == bits(&whole));
    // A loop region over the second half renders only that, and never
    // loops (length is one pass plus the tail).
    let mut p = song();
    p.loop_region.start = 1920;
    p.loop_region.end = 3840;
    let (mut req3, slots3) = request(p);
    req3.range = None;
    let half = render(&req3, &slots3);
    let main = ideal_sample(1920, 44100, 13333, 100) as usize;
    assert_eq!(half.len(), main + 22050);
}

#[test]
fn a_range_start_inside_the_clip_skips_earlier_notes() {
    let p = project(
        120.0,
        vec![],
        vec![synth_channel(1, 0, tone_params())],
        vec![pattern(
            1,
            16,
            &[(1, vec![(1, 960, 480, 69, 127), (2, 2880, 480, 72, 127)])],
        )],
    );
    let (mut req, slots) = request(p);
    req.sample_rate = 48000;
    req.tail_seconds = 0.0;
    req.range = Some((1920, 3840));
    let out = render(&req, &slots);
    assert_eq!(out.len(), ideal_sample(1920, 48000, 120, 1) as usize);
    let on = ideal_sample(2880 - 1920, 48000, 120, 1) as usize;
    assert!(out[..on].iter().all(|f| f[0] == 0.0 && f[1] == 0.0));
    assert!(out[on + 1][0].abs() > 0.0 || out[on + 2][0].abs() > 0.0);
}

#[test]
fn offline_render_can_be_cancelled() {
    let (req, slots) = request(song());
    let cancel = AtomicBool::new(true);
    let r = render_range(&req, &slots, &[], &AtomicU32::new(0), &cancel);
    assert!(matches!(r, Err(EngineError::Cancelled)));
}

#[test]
fn offline_render_notes_land_on_the_closed_form_grid() {
    // one tone note at tick 960 of a 1-pass render: silence before the
    // exact sample, sound from it
    let p = project(
        120.0,
        vec![],
        vec![synth_channel(1, 0, tone_params())],
        vec![pattern(1, 16, &[(1, vec![(1, 960, 480, 69, 127)])])],
    );
    let (mut req, slots) = request(p);
    req.tail_seconds = 0.0;
    req.sample_rate = 48000;
    let out = render(&req, &slots);
    let on = ideal_sample(960, 48000, 120, 1) as usize;
    assert!(out[..on].iter().all(|f| f[0] == 0.0 && f[1] == 0.0));
    assert!(out[on + 1][0].abs() > 0.0 || out[on + 2][0].abs() > 0.0);
}

// ---- WAV

struct Wav {
    tag: u16,
    channels: u16,
    rate: u32,
    bits: u16,
    data: Vec<u8>,
    has_fact: bool,
}

fn parse(b: &[u8]) -> Wav {
    assert_eq!(&b[0..4], b"RIFF");
    assert_eq!(&b[8..12], b"WAVE");
    assert_eq!(
        u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize,
        b.len() - 8
    );
    let mut at = 12;
    let (mut tag, mut channels, mut rate, mut bits) = (0, 0, 0, 0);
    let mut data = Vec::new();
    let mut has_fact = false;
    while at + 8 <= b.len() {
        let id = &b[at..at + 4];
        let len = u32::from_le_bytes(b[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = &b[at + 8..at + 8 + len];
        match id {
            b"fmt " => {
                tag = u16::from_le_bytes(body[0..2].try_into().unwrap());
                channels = u16::from_le_bytes(body[2..4].try_into().unwrap());
                rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                let byte_rate = u32::from_le_bytes(body[8..12].try_into().unwrap());
                let align = u16::from_le_bytes(body[12..14].try_into().unwrap());
                bits = u16::from_le_bytes(body[14..16].try_into().unwrap());
                assert_eq!(align, channels * bits / 8);
                assert_eq!(byte_rate, rate * align as u32);
            }
            b"fact" => has_fact = true,
            b"data" => data = body.to_vec(),
            _ => panic!("unexpected chunk"),
        }
        at += 8 + len + (len & 1);
    }
    Wav {
        tag,
        channels,
        rate,
        bits,
        data,
        has_fact,
    }
}

fn encode(frames: &[[f32; 2]], rate: u32, fmt: WavFormat) -> Wav {
    let mut v = Vec::new();
    write_wav_to(&mut v, frames, rate, fmt).unwrap();
    parse(&v)
}

fn ramp() -> Vec<[f32; 2]> {
    (0..1000)
        .map(|i| {
            let x = i as f32 / 1000.0 * 2.0 - 1.0;
            [x * 0.9, -x * 0.5]
        })
        .collect()
}

#[test]
fn wav_float32_round_trips_exactly() {
    let f = ramp();
    let w = encode(&f, 48000, WavFormat::Float32);
    assert_eq!((w.tag, w.channels, w.rate, w.bits), (3, 2, 48000, 32));
    assert!(w.has_fact);
    let back: Vec<f32> = w
        .data
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    let want: Vec<f32> = f.iter().flatten().copied().collect();
    assert_eq!(back, want);
}

#[test]
fn wav_pcm16_and_pcm24_round_trip_within_dither() {
    let f = ramp();
    let w = encode(&f, 44100, WavFormat::Pcm16);
    assert_eq!((w.tag, w.channels, w.rate, w.bits), (1, 2, 44100, 16));
    assert!(!w.has_fact);
    let got: Vec<i16> = w
        .data
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes(c.try_into().unwrap()))
        .collect();
    assert_eq!(got.len(), 2000);
    for (g, x) in got.iter().zip(f.iter().flatten()) {
        let want = x * 32768.0;
        assert!((*g as f32 - want).abs() <= 2.0, "{g} vs {want}");
    }
    let w = encode(&f, 96000, WavFormat::Pcm24);
    assert_eq!((w.tag, w.rate, w.bits), (1, 96000, 24));
    assert_eq!(w.data.len(), 2000 * 3);
    for (c, x) in w.data.chunks_exact(3).zip(f.iter().flatten()) {
        let v = i32::from_le_bytes([c[0], c[1], c[2], if c[2] & 0x80 != 0 { 0xff } else { 0 }]);
        let want = x * 8388608.0;
        assert!((v as f32 - want).abs() <= 2.0, "{v} vs {want}");
    }
}

#[test]
fn wav_dither_is_triangular_zero_mean_and_deterministic() {
    let silence = vec![[0.0f32; 2]; 50000];
    let a = encode(&silence, 48000, WavFormat::Pcm16);
    let b = encode(&silence, 48000, WavFormat::Pcm16);
    assert_eq!(a.data, b.data, "same input, same output");
    let v: Vec<i16> = a
        .data
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes(c.try_into().unwrap()))
        .collect();
    assert!(
        v.iter().all(|x| (-1..=1).contains(x)),
        "TPDF is within +-1 LSB"
    );
    let mean = v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64;
    assert!(mean.abs() < 0.02, "{mean}");
    let ones = v.iter().filter(|&&x| x != 0).count() as f64 / v.len() as f64;
    assert!(
        (ones - 0.25).abs() < 0.02,
        "P(nonzero) = 1/4 for TPDF, got {ones}"
    );
}

#[test]
fn wav_clips_and_writes_files() {
    let f = vec![[2.0f32, -2.0]];
    let w = encode(&f, 48000, WavFormat::Pcm16);
    assert_eq!(i16::from_le_bytes([w.data[0], w.data[1]]), i16::MAX);
    assert_eq!(i16::from_le_bytes([w.data[2], w.data[3]]), i16::MIN);
    let dir = std::env::temp_dir().join(format!("libredaw-wav-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.wav");
    engine::write_wav(&path, &ramp(), 48000, WavFormat::Float32).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(parse(&bytes).data.len(), 1000 * 8);
    std::fs::remove_dir_all(&dir).unwrap();
}
