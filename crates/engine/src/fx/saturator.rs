// SPDX-License-Identifier: GPL-3.0-or-later
//! Saturator (SPEC 15.5): drive, a soft, hard or folding curve, a tone
//! lowpass, dry/wet mix and an output trim.

use super::{FADE_IN_FRAMES, fade_in};
use protocol::beats::{SaturatorCurve, SaturatorParam};

const TAU: f64 = std::f64::consts::TAU;
const DC: f32 = 1e-18;

/// The waveshaper for one sample.
#[inline]
pub fn shape(curve: SaturatorCurve, x: f32) -> f32 {
    match curve {
        SaturatorCurve::Soft => x.tanh(),
        SaturatorCurve::Hard => x.clamp(-1.0, 1.0),
        SaturatorCurve::Fold => {
            // Triangle fold: identity inside -1..1, mirrored outside.
            let t = 0.25 * x + 0.25;
            4.0 * (t - t.round()).abs() - 1.0
        }
    }
}

#[derive(Clone)]
pub struct Saturator {
    sr: f64,
    lp: [f32; 2],
    fade: u32,
}

impl Saturator {
    pub fn new(sr: f64) -> Saturator {
        Saturator {
            sr,
            lp: [0.0; 2],
            fade: 0,
        }
    }

    pub fn reset(&mut self) {
        self.lp = [0.0; 2];
        self.fade = FADE_IN_FRAMES;
    }

    pub fn process(&mut self, curve: SaturatorCurve, p: &[f32], l: &mut [f32], r: &mut [f32]) {
        use SaturatorParam::*;
        let drive = 10f32.powf(p[DriveDb.index()] / 20.0);
        let a = (1.0
            - (-TAU * (p[ToneHz.index()] as f64).clamp(20.0, self.sr * 0.45) / self.sr).exp())
            as f32;
        let mix = p[Mix.index()].clamp(0.0, 1.0);
        let out = 10f32.powf(p[OutputDb.index()] / 20.0);
        fade_in(&mut self.fade, l, r);
        for (ch, buf) in [l, r].into_iter().enumerate() {
            let lp = &mut self.lp[ch];
            for x in buf.iter_mut() {
                let dry = *x;
                let wet = shape(curve, dry * drive);
                *lp += a * (wet - *lp) + DC;
                *x = (dry * (1.0 - mix) + *lp * mix) * out;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::band_energy;

    const SR: f64 = 48000.0;

    #[test]
    fn curves_have_their_reference_values() {
        assert_eq!(shape(SaturatorCurve::Hard, 3.0), 1.0);
        assert_eq!(shape(SaturatorCurve::Hard, -0.4), -0.4);
        assert!((shape(SaturatorCurve::Soft, 0.5) - 0.5f32.tanh()).abs() < 1e-7);
        assert!(shape(SaturatorCurve::Soft, 20.0) <= 1.0);
        // Fold: identity inside, mirrored at 1, back to the start at 3.
        for x in [-0.9f32, -0.2, 0.0, 0.7] {
            assert!((shape(SaturatorCurve::Fold, x) - x).abs() < 1e-6, "{x}");
        }
        assert!((shape(SaturatorCurve::Fold, 1.5) - 0.5).abs() < 1e-6);
        assert!((shape(SaturatorCurve::Fold, 2.0) - 0.0).abs() < 1e-6);
        assert!((shape(SaturatorCurve::Fold, 3.0) - -1.0).abs() < 1e-6);
        assert!((shape(SaturatorCurve::Fold, -1.5) + 0.5).abs() < 1e-6);
    }

    fn sine(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (TAU * 1000.0 * i as f64 / SR).sin() as f32)
            .collect()
    }

    #[test]
    fn drive_adds_odd_harmonics_and_tone_removes_them() {
        // drive, tone, mix, output
        let hot = [24.0, 20000.0, 1.0, 0.0];
        let mut s = Saturator::new(SR);
        let (mut l, mut r) = (sine(16384), sine(16384));
        s.process(SaturatorCurve::Soft, &hot, &mut l, &mut r);
        let low = band_energy(&l[2048..], SR, 900.0, 1100.0);
        let h3 = band_energy(&l[2048..], SR, 2900.0, 3100.0);
        assert!(h3 / low > 0.01, "third harmonic present: {}", h3 / low);
        let even = band_energy(&l[2048..], SR, 1900.0, 2100.0);
        assert!(
            even / low < 1e-6,
            "no even harmonics from a symmetric curve"
        );
        let dark = [24.0, 1500.0, 1.0, 0.0];
        let mut s = Saturator::new(SR);
        let (mut l, mut r) = (sine(16384), sine(16384));
        s.process(SaturatorCurve::Soft, &dark, &mut l, &mut r);
        let h3d = band_energy(&l[2048..], SR, 2900.0, 3100.0);
        assert!(h3d < h3 * 0.4);
    }

    #[test]
    fn mix_zero_is_the_dry_signal_times_output() {
        let mut s = Saturator::new(SR);
        let x = sine(1000);
        let (mut l, mut r) = (x.clone(), x.clone());
        s.process(
            SaturatorCurve::Hard,
            &[30.0, 20000.0, 0.0, -6.0],
            &mut l,
            &mut r,
        );
        let g = 10f32.powf(-6.0 / 20.0);
        for i in 0..1000 {
            let want = if i < 256 { None } else { Some(x[i] * g) };
            if let Some(w) = want {
                assert!((l[i] - w).abs() < 1e-6);
            }
        }
    }
}
