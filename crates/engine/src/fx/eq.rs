// SPDX-License-Identifier: GPL-3.0-or-later
//! Four-band EQ (SPEC 15.5): a second-order low cut, a low shelf, a
//! peaking band and a high shelf. Coefficients are the RBJ cookbook forms,
//! state is f64 transposed direct form II.

use super::{FADE_IN_FRAMES, fade_in};
use protocol::beats::EqParam;

const TAU: f64 = std::f64::consts::TAU;

/// One normalized biquad.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Biquad {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Biquad {
    pub const THROUGH: Biquad = Biquad {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn norm(b: [f64; 3], a: [f64; 3]) -> Biquad {
        Biquad {
            b0: b[0] / a[0],
            b1: b[1] / a[0],
            b2: b[2] / a[0],
            a1: a[1] / a[0],
            a2: a[2] / a[0],
        }
    }

    fn setup(sr: f64, f: f64) -> (f64, f64) {
        let w0 = TAU * f.clamp(1.0, sr * 0.45) / sr;
        (w0.cos(), w0.sin())
    }

    pub fn highpass(sr: f64, f: f64, q: f64) -> Biquad {
        let (c, s) = Biquad::setup(sr, f);
        let al = s / (2.0 * q);
        Biquad::norm(
            [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0],
            [1.0 + al, -2.0 * c, 1.0 - al],
        )
    }

    pub fn peaking(sr: f64, f: f64, q: f64, gain_db: f64) -> Biquad {
        let (c, s) = Biquad::setup(sr, f);
        let a = 10f64.powf(gain_db / 40.0);
        let al = s / (2.0 * q);
        Biquad::norm(
            [1.0 + al * a, -2.0 * c, 1.0 - al * a],
            [1.0 + al / a, -2.0 * c, 1.0 - al / a],
        )
    }

    /// Shelf slope S = 1.
    pub fn low_shelf(sr: f64, f: f64, gain_db: f64) -> Biquad {
        let (c, s) = Biquad::setup(sr, f);
        let a = 10f64.powf(gain_db / 40.0);
        let al = s / 2.0 * 2f64.sqrt();
        let k = 2.0 * a.sqrt() * al;
        Biquad::norm(
            [
                a * ((a + 1.0) - (a - 1.0) * c + k),
                2.0 * a * ((a - 1.0) - (a + 1.0) * c),
                a * ((a + 1.0) - (a - 1.0) * c - k),
            ],
            [
                (a + 1.0) + (a - 1.0) * c + k,
                -2.0 * ((a - 1.0) + (a + 1.0) * c),
                (a + 1.0) + (a - 1.0) * c - k,
            ],
        )
    }

    pub fn high_shelf(sr: f64, f: f64, gain_db: f64) -> Biquad {
        let (c, s) = Biquad::setup(sr, f);
        let a = 10f64.powf(gain_db / 40.0);
        let al = s / 2.0 * 2f64.sqrt();
        let k = 2.0 * a.sqrt() * al;
        Biquad::norm(
            [
                a * ((a + 1.0) + (a - 1.0) * c + k),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
                a * ((a + 1.0) + (a - 1.0) * c - k),
            ],
            [
                (a + 1.0) - (a - 1.0) * c + k,
                2.0 * ((a - 1.0) - (a + 1.0) * c),
                (a + 1.0) - (a - 1.0) * c - k,
            ],
        )
    }

    /// Magnitude of the response at `f` Hz.
    pub fn magnitude(&self, sr: f64, f: f64) -> f64 {
        let w = TAU * f / sr;
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }

    #[inline]
    fn run(&self, z: &mut [f64; 2], x: f64) -> f64 {
        let y = self.b0 * x + z[0];
        z[0] = self.b1 * x - self.a1 * y + z[1];
        z[1] = self.b2 * x - self.a2 * y;
        y
    }
}

const BANDS: usize = 4;
const N_PARAMS: usize = 8;

#[derive(Clone)]
pub struct Eq {
    sr: f64,
    last: [f32; N_PARAMS],
    have: bool,
    bands: [Biquad; BANDS],
    /// Band is audible (a flat band is skipped, so a flat EQ is bit exact).
    on: [bool; BANDS],
    z: [[[f64; 2]; 2]; BANDS],
    fade: u32,
}

impl Eq {
    pub fn new(sr: f64) -> Eq {
        Eq {
            sr,
            last: [0.0; N_PARAMS],
            have: false,
            bands: [Biquad::THROUGH; BANDS],
            on: [false; BANDS],
            z: [[[0.0; 2]; 2]; BANDS],
            fade: 0,
        }
    }

    /// Clears the state and fades the input in (a newly assigned pool entry).
    pub fn reset(&mut self) {
        self.z = [[[0.0; 2]; 2]; BANDS];
        self.have = false;
        self.fade = FADE_IN_FRAMES;
    }

    fn update(&mut self, p: &[f32]) {
        let g = |q: EqParam| p[q.index()] as f64;
        use EqParam::*;
        let same = self.have && self.last[..] == p[..N_PARAMS];
        if same {
            return;
        }
        self.last.copy_from_slice(&p[..N_PARAMS]);
        self.have = true;
        let sr = self.sr;
        self.on[0] = g(LowCutHz) > 10.0;
        self.bands[0] = Biquad::highpass(sr, g(LowCutHz), std::f64::consts::FRAC_1_SQRT_2);
        self.on[1] = g(LowGainDb) != 0.0;
        self.bands[1] = Biquad::low_shelf(sr, g(LowHz), g(LowGainDb));
        self.on[2] = g(MidGainDb) != 0.0;
        self.bands[2] = Biquad::peaking(sr, g(MidHz), g(MidQ), g(MidGainDb));
        self.on[3] = g(HighGainDb) != 0.0;
        self.bands[3] = Biquad::high_shelf(sr, g(HighHz), g(HighGainDb));
    }

    pub fn process(&mut self, p: &[f32], l: &mut [f32], r: &mut [f32]) {
        self.update(p);
        fade_in(&mut self.fade, l, r);
        for b in 0..BANDS {
            if !self.on[b] {
                continue;
            }
            let bq = self.bands[b];
            for (ch, buf) in [&mut *l, &mut *r].into_iter().enumerate() {
                let z = &mut self.z[b][ch];
                for x in buf.iter_mut() {
                    *x = bq.run(z, *x as f64) as f32;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tone_level;

    const SR: f64 = 48000.0;

    fn db(x: f64) -> f64 {
        20.0 * x.log10()
    }

    #[test]
    fn shelf_peak_and_cut_hit_their_closed_form_gains() {
        // Peaking: gain at the centre frequency is exactly the set gain.
        for g in [-12.0, -3.0, 6.0, 15.0] {
            let b = Biquad::peaking(SR, 1000.0, 1.3, g);
            assert!((db(b.magnitude(SR, 1000.0)) - g).abs() < 1e-9, "{g}");
            assert!(db(b.magnitude(SR, 20.0)).abs() < 0.05);
            assert!(db(b.magnitude(SR, 20000.0)).abs() < 0.3);
        }
        // Shelves: full gain at the far end, half the dB gain at the corner
        // (S = 1), unity at the other end.
        for g in [-9.0, 6.0, 12.0] {
            let lo = Biquad::low_shelf(SR, 200.0, g);
            assert!((db(lo.magnitude(SR, 1.0)) - g).abs() < 1e-3);
            assert!((db(lo.magnitude(SR, 200.0)) - g / 2.0).abs() < 1e-6);
            assert!(db(lo.magnitude(SR, 20000.0)).abs() < 0.05);
            let hi = Biquad::high_shelf(SR, 5000.0, g);
            assert!((db(hi.magnitude(SR, 23999.0)) - g).abs() < 0.3);
            assert!((db(hi.magnitude(SR, 5000.0)) - g / 2.0).abs() < 1e-6);
            assert!(db(hi.magnitude(SR, 50.0)).abs() < 0.01);
        }
        // Highpass at Q = 1/sqrt 2: -3.0103 dB at the cutoff, 12 dB/oct.
        let hp = Biquad::highpass(SR, 100.0, std::f64::consts::FRAC_1_SQRT_2);
        assert!((db(hp.magnitude(SR, 100.0)) + 3.0103).abs() < 1e-3);
        assert!((db(hp.magnitude(SR, 50.0)) + 12.3).abs() < 0.2);
        assert!(db(hp.magnitude(SR, 5000.0)).abs() < 0.01);
    }

    fn params(f: impl Fn(&mut [f32; 8])) -> [f32; 8] {
        // low_cut, low_hz, low_gain, mid_hz, mid_q, mid_gain, high_hz, high_gain
        let mut p = [10.0, 120.0, 0.0, 1000.0, 0.7, 0.0, 8000.0, 0.0];
        f(&mut p);
        p
    }

    fn sine(f: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (TAU * f * i as f64 / SR).sin() as f32)
            .collect()
    }

    fn gain_at(p: &[f32; 8], f: f64) -> f64 {
        let mut eq = Eq::new(SR);
        let mut l = sine(f, 24000);
        let mut r = l.clone();
        // Past the fade-in, measure the settled second half.
        eq.process(p, &mut l, &mut r);
        let out = &l[12000..];
        let reference = sine(f, 24000);
        tone_level(out, SR, f) / tone_level(&reference[12000..], SR, f)
    }

    #[test]
    fn processing_matches_the_analytic_response() {
        let p = params(|p| {
            p[5] = 9.0; // mid +9 dB at 1 kHz
            p[4] = 1.0;
        });
        let m = db(gain_at(&p, 1000.0));
        assert!((m - 9.0).abs() < 0.05, "{m}");
        assert!(db(gain_at(&p, 100.0)).abs() < 1.0);
        let p = params(|p| p[7] = -6.0);
        assert!((db(gain_at(&p, 20000.0)) + 6.0).abs() < 0.5);
        let p = params(|p| p[0] = 200.0);
        assert!((db(gain_at(&p, 200.0)) + 3.01).abs() < 0.05);
        assert!(db(gain_at(&p, 4000.0)).abs() < 0.05);
    }

    #[test]
    fn a_flat_eq_is_bit_exact_and_the_fade_in_ends() {
        let mut eq = Eq::new(SR);
        eq.reset();
        let x = sine(440.0, 1000);
        let (mut l, mut r) = (x.clone(), x.clone());
        eq.process(&params(|_| {}), &mut l, &mut r);
        assert!(l[0].abs() < 1e-6);
        assert_eq!(&l[FADE_IN_FRAMES as usize..], &x[FADE_IN_FRAMES as usize..]);
        assert_eq!(l, r);
    }
}
