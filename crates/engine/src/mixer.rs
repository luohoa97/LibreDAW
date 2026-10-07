// SPDX-License-Identifier: GPL-3.0-or-later
//! Mixer math (SPEC 8): dB to linear, the -3 dB constant-power pan law,
//! per-sample gain smoothing over 10 ms, and solo-in-place resolution.

use protocol::consts::MIN_GAIN_DB;

/// Smoothing time of faders and pans (SPEC 8).
pub const SMOOTH_SECONDS: f64 = 0.010;

/// Linear gain for a fader value. `MIN_GAIN_DB` and below is silence.
pub fn db_to_lin(db: f32) -> f32 {
    if (db as f64) <= MIN_GAIN_DB {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// Constant-power pan for a mono source: `(left, right)` gains, -3 dB each
/// at the center, 0 dB on the loud side at the extremes. `pan` is -1 to 1.
pub fn pan_mono(pan: f32) -> (f32, f32) {
    let theta = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    (theta.cos(), theta.sin())
}

/// Balance for a stereo source: same angle as `pan_mono`, scaled so the
/// center is unity and the near side never exceeds unity.
pub fn pan_stereo(pan: f32) -> (f32, f32) {
    let (l, r) = pan_mono(pan);
    let s = std::f32::consts::SQRT_2;
    ((l * s).min(1.0), (r * s).min(1.0))
}

/// A gain that ramps linearly to a new target in a fixed number of samples.
/// The first target is taken immediately.
#[derive(Clone, Copy, Debug)]
pub struct Smoother {
    cur: f32,
    target: f32,
    step: f32,
    left: u32,
    init: bool,
}

impl Smoother {
    pub const fn new() -> Smoother {
        Smoother {
            cur: 0.0,
            target: 0.0,
            step: 0.0,
            left: 0,
            init: false,
        }
    }

    /// Forget the state: the next `set` snaps to its value.
    pub fn reset(&mut self) {
        *self = Smoother::new();
    }

    pub fn set(&mut self, target: f32, ramp_samples: u32) {
        if !self.init {
            *self = Smoother {
                cur: target,
                target,
                step: 0.0,
                left: 0,
                init: true,
            };
        } else if target != self.target {
            self.target = target;
            self.left = ramp_samples.max(1);
            self.step = (target - self.cur) / self.left as f32;
        }
    }

    #[inline]
    pub fn tick(&mut self) -> f32 {
        if self.left > 0 {
            self.cur += self.step;
            self.left -= 1;
            if self.left == 0 {
                self.cur = self.target;
            }
        }
        self.cur
    }

    pub fn value(&self) -> f32 {
        self.cur
    }

    pub fn settled(&self) -> bool {
        self.left == 0
    }
}

impl Default for Smoother {
    fn default() -> Smoother {
        Smoother::new()
    }
}

/// Left and right gain of one fader, both smoothed.
#[derive(Clone, Copy, Debug, Default)]
pub struct Fader {
    pub l: Smoother,
    pub r: Smoother,
}

impl Fader {
    pub fn reset(&mut self) {
        self.l.reset();
        self.r.reset();
    }

    /// `audible` false drives both gains to 0 (mute, or silenced by solo).
    pub fn set(&mut self, volume_db: f32, pan: f32, audible: bool, mono_source: bool, ramp: u32) {
        let vol = if audible { db_to_lin(volume_db) } else { 0.0 };
        let (l, r) = if mono_source {
            pan_mono(pan)
        } else {
            pan_stereo(pan)
        };
        self.l.set(vol * l, ramp);
        self.r.set(vol * r, ramp);
    }
}

/// Mute and solo flags of one channel or track for solo resolution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MuteSolo {
    pub present: bool,
    pub mute: bool,
    pub solo: bool,
}

/// Solo-in-place. If nothing is soloed, everything unmuted is audible. If
/// anything is soloed, a channel is audible when it or its track is soloed,
/// and a track is audible when it is soloed or carries a soloed channel.
/// Mute always wins over solo. The master (`tracks[0]`) is never silenced by
/// solo. `channel_track[i]` is the track slot channel `i` feeds.
pub fn resolve_solo(
    channels: &[MuteSolo],
    channel_track: &[u16],
    tracks: &[MuteSolo],
    out_channels: &mut [bool],
    out_tracks: &mut [bool],
) {
    let any_solo = channels.iter().any(|c| c.present && c.solo)
        || tracks.iter().skip(1).any(|t| t.present && t.solo);
    for t in out_tracks.iter_mut() {
        *t = false;
    }
    for (i, c) in channels.iter().enumerate() {
        let track_solo = tracks[channel_track[i] as usize].solo && channel_track[i] != 0;
        out_channels[i] = c.present && !c.mute && (!any_solo || c.solo || track_solo);
        if c.present && c.solo {
            out_tracks[channel_track[i] as usize] = true; // carries a soloed channel
        }
    }
    for (i, t) in tracks.iter().enumerate() {
        if !t.present {
            out_tracks[i] = false;
        } else if i == 0 {
            out_tracks[0] = !t.mute;
        } else {
            let carries = out_tracks[i];
            out_tracks[i] = !t.mute && (!any_solo || t.solo || carries);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pan_law_is_minus_3_db_at_center_and_constant_power() {
        let (l, r) = pan_mono(0.0);
        let db = 20.0 * l.log10();
        assert!((db + 3.0103).abs() < 1e-3, "{db}");
        assert!((l - r).abs() < 1e-6);
        for i in -10..=10 {
            let (l, r) = pan_mono(i as f32 / 10.0);
            assert!((l * l + r * r - 1.0).abs() < 1e-5);
        }
        assert_eq!(pan_mono(-1.0).0, 1.0);
        assert!(pan_mono(-1.0).1.abs() < 1e-6);
        assert!(pan_mono(1.0).0.abs() < 1e-6);
        let (l, r) = pan_stereo(0.0);
        assert!((l - 1.0).abs() < 1e-6 && (r - 1.0).abs() < 1e-6);
        assert_eq!(pan_stereo(-1.0).0, 1.0);
    }

    #[test]
    fn db_conversion() {
        assert_eq!(db_to_lin(-96.0), 0.0);
        assert_eq!(db_to_lin(0.0), 1.0);
        assert!((db_to_lin(-6.0206) - 0.5).abs() < 1e-4);
        assert!((db_to_lin(6.0206) - 2.0).abs() < 1e-3);
    }

    #[test]
    fn smoother_reaches_target_in_exactly_the_ramp() {
        let mut s = Smoother::new();
        s.set(1.0, 480);
        assert_eq!(s.tick(), 1.0, "first target is immediate");
        s.set(0.0, 480);
        let mut last = 1.0;
        for i in 0..480 {
            let v = s.tick();
            assert!(v < last || i == 479, "monotone");
            last = v;
        }
        assert_eq!(s.value(), 0.0);
        assert!(s.settled());
    }

    fn solo_case(ch: &[(bool, bool, u16)], tr: &[(bool, bool)]) -> (Vec<bool>, Vec<bool>) {
        let channels: Vec<MuteSolo> = ch
            .iter()
            .map(|c| MuteSolo {
                present: true,
                mute: c.0,
                solo: c.1,
            })
            .collect();
        let tracks: Vec<MuteSolo> = tr
            .iter()
            .map(|t| MuteSolo {
                present: true,
                mute: t.0,
                solo: t.1,
            })
            .collect();
        let map: Vec<u16> = ch.iter().map(|c| c.2).collect();
        let mut oc = vec![false; ch.len()];
        let mut ot = vec![false; tr.len()];
        resolve_solo(&channels, &map, &tracks, &mut oc, &mut ot);
        (oc, ot)
    }

    #[test]
    fn solo_in_place_math() {
        // no solo: mute only
        let (c, t) = solo_case(
            &[(false, false, 1), (true, false, 1)],
            &[(false, false), (false, false)],
        );
        assert_eq!(c, [true, false]);
        assert_eq!(t, [true, true]);
        // channel solo silences other channels and tracks without a soloed channel
        let (c, t) = solo_case(
            &[(false, true, 1), (false, false, 1), (false, false, 2)],
            &[(false, false), (false, false), (false, false)],
        );
        assert_eq!(c, [true, false, false]);
        assert_eq!(t, [true, true, false]);
        // track solo passes all its channels, silences other tracks
        let (c, t) = solo_case(
            &[(false, false, 1), (false, false, 2)],
            &[(false, false), (false, true), (false, false)],
        );
        assert_eq!(c, [true, false]);
        assert_eq!(t, [true, true, false]);
        // mute wins over solo
        let (c, _) = solo_case(&[(true, true, 1)], &[(false, false), (false, false)]);
        assert_eq!(c, [false]);
        // master mute silences master only
        let (_, t) = solo_case(&[], &[(true, false), (false, false)]);
        assert_eq!(t, [false, true]);
    }
}
