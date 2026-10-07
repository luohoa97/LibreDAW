// SPDX-License-Identifier: GPL-3.0-or-later
//! Mixer through the whole runtime: pan law, smoothing, mute, solo, meters,
//! routing, metronome.

mod common;

use common::*;
use protocol::engine::{
    CTL_METRONOME_ENABLED, CTL_METRONOME_GAIN_DB, ChannelSlot, MixControl, TrackSlot,
    channel_control, track_control,
};
use std::sync::atomic::Ordering::Relaxed;

/// One 440 Hz sine of peak 0.5 on channel `id` (slot `id - 1`).
fn two_channel_project() -> protocol::model::Project {
    let mut hi = tone_params();
    hi.osc1.semitones = 12.0;
    project(
        120.0,
        vec![track(1), track(2)],
        vec![synth_channel(1, 1, tone_params()), synth_channel(2, 2, hi)],
        vec![pattern(
            1,
            16,
            &[
                (1, vec![(1, 0, 3840, 69, 127)]),
                (2, vec![(2, 0, 3840, 69, 127)]),
            ],
        )],
    )
}

fn settled(r: &mut Rig) -> (Vec<f32>, Vec<f32>) {
    r.run(4800, 256);
    r.run(4800, 256)
}

fn set_ch(r: &Rig, slot: u16, c: MixControl, v: f32) {
    r.shared
        .controls
        .set(channel_control(ChannelSlot(slot), c), v);
}

fn set_tr(r: &Rig, slot: u16, c: MixControl, v: f32) {
    r.shared.controls.set(track_control(TrackSlot(slot), c), v);
}

#[test]
fn channel_pan_is_constant_power_minus_3_db_at_center() {
    let mut r = rig(&two_channel_project(), 48000.0, true);
    set_ch(&r, 1, MixControl::Mute, 1.0);
    let (l, rr) = settled(&mut r);
    let c = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
    assert!((peak(&l) - c).abs() < 0.002, "{}", peak(&l));
    assert!((peak(&rr) - c).abs() < 0.002);
    set_ch(&r, 0, MixControl::Pan, -1.0);
    let (l, rr) = settled(&mut r);
    assert!((peak(&l) - 0.5).abs() < 0.002);
    assert!(peak(&rr) < 1e-4);
    set_ch(&r, 0, MixControl::Pan, 0.5);
    let (l, rr) = settled(&mut r);
    let th = 1.5 * std::f32::consts::FRAC_PI_4;
    assert!((peak(&l) - 0.5 * th.cos()).abs() < 0.002);
    assert!((peak(&rr) - 0.5 * th.sin()).abs() < 0.002);
}

#[test]
fn track_and_master_faders_scale_stereo_audio() {
    let mut r = rig(&two_channel_project(), 48000.0, true);
    set_ch(&r, 1, MixControl::Mute, 1.0);
    let (l0, _) = settled(&mut r);
    // track fader -6.0206 dB then master fader the same: quarter level
    set_tr(&r, 1, MixControl::VolumeDb, -6.0206);
    set_tr(&r, 0, MixControl::VolumeDb, -6.0206);
    let (l1, _) = settled(&mut r);
    assert!((peak(&l1) / peak(&l0) - 0.25).abs() < 0.005);
    // a stereo stage keeps the balance center at unity: track pan 0 changed
    // nothing above; hard right on the track silences the left side
    set_tr(&r, 0, MixControl::VolumeDb, 0.0);
    set_tr(&r, 1, MixControl::VolumeDb, 0.0);
    set_tr(&r, 1, MixControl::Pan, 1.0);
    let (l, rr) = settled(&mut r);
    assert!(peak(&l) < 1e-4);
    assert!(
        (peak(&rr) - 0.5 * std::f32::consts::FRAC_1_SQRT_2).abs() < 0.002,
        "right side keeps the channel level (balance is unity on the near side)"
    );
}

#[test]
fn fader_changes_ramp_over_ten_milliseconds() {
    let mut r = rig(&two_channel_project(), 48000.0, true);
    set_ch(&r, 1, MixControl::Mute, 1.0);
    settled(&mut r);
    set_ch(&r, 0, MixControl::VolumeDb, -96.0);
    // gain goes from 1 to 0 in 480 samples: after 240 it is about half
    let (l, _) = r.run(1024, 64);
    let env = |from: usize| peak(&l[from..from + 48]);
    assert!(env(0) > 0.2, "not an instant cut: {}", env(0));
    assert!(env(200) > 0.05 && env(200) < env(0));
    assert!(peak(&l[520..]) < 1e-4, "silent after 10 ms");
}

#[test]
fn mute_and_solo_in_place() {
    let both = {
        let mut r = rig(&two_channel_project(), 48000.0, true);
        settled(&mut r).0
    };
    let only = |solo: Option<u16>, mute: Option<u16>| {
        let mut r = rig(&two_channel_project(), 48000.0, true);
        if let Some(s) = solo {
            set_ch(&r, s, MixControl::Solo, 1.0);
        }
        if let Some(m) = mute {
            set_ch(&r, m, MixControl::Mute, 1.0);
        }
        settled(&mut r).0
    };
    let one = only(None, Some(1)); // channel 0 alone
    let two = only(None, Some(0)); // channel 1 alone
    // solo channel 0 == mute channel 1; solo channel 1 == mute channel 0
    assert_eq!(only(Some(0), None), one);
    assert_eq!(only(Some(1), None), two);
    assert!(rms(&both) > rms(&one) && rms(&both) > rms(&two));
    // mute wins over solo
    let mut r = rig(&two_channel_project(), 48000.0, true);
    set_ch(&r, 0, MixControl::Solo, 1.0);
    set_ch(&r, 0, MixControl::Mute, 1.0);
    set_ch(&r, 1, MixControl::Mute, 1.0);
    assert_eq!(peak(&settled(&mut r).0), 0.0);
    // soloing a track passes its channels and silences the other track
    let mut r = rig(&two_channel_project(), 48000.0, true);
    set_tr(&r, 2, MixControl::Solo, 1.0);
    assert_eq!(settled(&mut r).0, two);
    // master mute silences everything
    let mut r = rig(&two_channel_project(), 48000.0, true);
    set_tr(&r, 0, MixControl::Mute, 1.0);
    assert_eq!(peak(&settled(&mut r).0), 0.0);
}

#[test]
fn peak_meters_follow_the_signal_and_reset_on_read() {
    let mut r = rig(&two_channel_project(), 48000.0, true);
    set_ch(&r, 1, MixControl::Mute, 1.0);
    let (l, _) = settled(&mut r);
    let st = r.shared.status.clone();
    let master_l = f32::from_bits(st.track_peaks[0].swap(0, Relaxed));
    assert!(
        (master_l - peak(&l)).abs() < 0.002,
        "{master_l} {}",
        peak(&l)
    );
    // track 1 carries channel 0 (slot 1 in the table), track 2 is idle
    assert!(f32::from_bits(st.track_peaks[2].load(Relaxed)) > 0.3);
    assert_eq!(st.track_peaks[4].load(Relaxed), 0);
    // reading resets: a silent block leaves 0
    for p in st.track_peaks.iter() {
        p.swap(0, Relaxed);
    }
    set_ch(&r, 0, MixControl::Mute, 1.0);
    r.run(2000, 256);
    for p in st.track_peaks.iter() {
        p.swap(0, Relaxed);
    }
    r.run(256, 256);
    assert_eq!(st.track_peaks[0].load(Relaxed), 0);
}

#[test]
fn metronome_clicks_on_beats_with_its_gain_and_enable() {
    let p = project(120.0, vec![], vec![], vec![pattern(1, 16, &[])]);
    let mut r = rig(&p, 48000.0, true);
    // off: silence
    let (l, _) = r.run(48000, 256);
    assert_eq!(peak(&l), 0.0);
    r.shared.controls.set(CTL_METRONOME_ENABLED, 1.0);
    r.shared.controls.set(CTL_METRONOME_GAIN_DB, 0.0);
    // next beat is at tick 1920 (bar 2, beat 1 of the 4/4 pattern loop)
    let (l, _) = r.run(48000, 256);
    assert!(peak(&l) > 0.5, "accented click");
    r.shared.controls.set(CTL_METRONOME_GAIN_DB, -6.0206);
    let before = r.rt.position();
    let (l2, _) = r.run(48000, 300);
    assert!(
        peak(&l2) > 0.2 && peak(&l2) < peak(&l) * 0.9,
        "{} {}",
        peak(&l2),
        peak(&l)
    );
    let _ = before;
}

#[test]
fn offline_style_runtime_can_disable_the_metronome() {
    let p = project(120.0, vec![], vec![], vec![pattern(1, 16, &[])]);
    let mut r = rig(&p, 48000.0, true);
    r.rt.set_metronome_allowed(false);
    r.shared.controls.set(CTL_METRONOME_ENABLED, 1.0);
    let (l, _) = r.run(48000, 256);
    assert_eq!(peak(&l), 0.0);
}
