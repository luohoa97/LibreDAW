// SPDX-License-Identifier: GPL-3.0-or-later
//! Knob math and the knob set of the built-in synth (docs/ui-design.md
//! 3.7), without GTK. A knob works in a unit range 0 to 1; a `KnobSpec`
//! maps that to a synth parameter with a curve that suits it (frequencies
//! and times are not linear to the ear).

use protocol::model::{SynthParam, SynthParams};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Curve {
    Linear,
    /// Equal steps per octave (frequencies).
    Log,
    /// `value = max * unit^exponent` (times: fine control near zero).
    Power(f64),
}

#[derive(Clone, Copy, Debug)]
pub struct KnobSpec {
    pub param: SynthParam,
    pub name: &'static str,
    pub curve: Curve,
    pub tip: &'static str,
}

/// The eight knobs of the Sound page for the built-in synth. The names are
/// plain words; the parameter behind each is in `param`.
pub const MACROS: [KnobSpec; 8] = [
    KnobSpec {
        param: SynthParam::CutoffHz,
        name: "Tone",
        curve: Curve::Log,
        tip: "How bright the sound is",
    },
    KnobSpec {
        param: SynthParam::Resonance,
        name: "Edge",
        curve: Curve::Linear,
        tip: "How sharply the tone rings",
    },
    KnobSpec {
        param: SynthParam::OscMix,
        name: "Blend",
        curve: Curve::Linear,
        tip: "Mix between the two sound sources",
    },
    KnobSpec {
        param: SynthParam::FilterEnvOctaves,
        name: "Sweep",
        curve: Curve::Linear,
        tip: "How far the tone opens at the start of each note",
    },
    KnobSpec {
        param: SynthParam::AmpAttackMs,
        name: "Attack",
        curve: Curve::Power(3.0),
        tip: "How slowly each note fades in",
    },
    KnobSpec {
        param: SynthParam::AmpDecayMs,
        name: "Length",
        curve: Curve::Power(3.0),
        tip: "How long each note takes to die away",
    },
    KnobSpec {
        param: SynthParam::AmpSustain,
        name: "Hold",
        curve: Curve::Linear,
        tip: "How loud a held note stays after the start",
    },
    KnobSpec {
        param: SynthParam::AmpReleaseMs,
        name: "Tail",
        curve: Curve::Power(3.0),
        tip: "How long the sound rings after you let go",
    },
];

/// Parameters shown under "More Controls" (the rest of the synth).
pub const MORE: [(SynthParam, &str); 9] = [
    (SynthParam::GainDb, "Level"),
    (SynthParam::Osc1Semitones, "Source 1 pitch (semitones)"),
    (SynthParam::Osc1Cents, "Source 1 fine tune (cents)"),
    (SynthParam::Osc2Semitones, "Source 2 pitch (semitones)"),
    (SynthParam::Osc2Cents, "Source 2 fine tune (cents)"),
    (SynthParam::FilterAttackMs, "Tone attack (ms)"),
    (SynthParam::FilterDecayMs, "Tone decay (ms)"),
    (SynthParam::FilterSustain, "Tone hold"),
    (SynthParam::FilterReleaseMs, "Tone tail (ms)"),
];

fn range_of(p: SynthParam) -> (f64, f64) {
    p.range()
}

/// Value to unit position, 0 to 1.
pub fn to_unit(spec: &KnobSpec, value: f64) -> f64 {
    let (lo, hi) = range_of(spec.param);
    let v = value.clamp(lo, hi);
    let u = match spec.curve {
        Curve::Linear => (v - lo) / (hi - lo),
        Curve::Log => (v / lo).ln() / (hi / lo).ln(),
        Curve::Power(e) => ((v - lo) / (hi - lo)).powf(1.0 / e),
    };
    u.clamp(0.0, 1.0)
}

/// Unit position to value.
pub fn from_unit(spec: &KnobSpec, unit: f64) -> f64 {
    let (lo, hi) = range_of(spec.param);
    let u = unit.clamp(0.0, 1.0);
    let v = match spec.curve {
        Curve::Linear => lo + u * (hi - lo),
        Curve::Log => lo * (hi / lo).powf(u),
        Curve::Power(e) => lo + (hi - lo) * u.powf(e),
    };
    v.clamp(lo, hi)
}

/// The text shown under a knob: "2.0 kHz", "120 ms", "35 percent".
pub fn format_value(p: SynthParam, v: f64) -> String {
    use SynthParam::*;
    match p {
        CutoffHz => {
            if v >= 1000.0 {
                format!("{:.1} kHz", v / 1000.0)
            } else {
                format!("{:.0} Hz", v)
            }
        }
        AmpAttackMs | AmpDecayMs | AmpReleaseMs | FilterAttackMs | FilterDecayMs
        | FilterReleaseMs => {
            if v >= 1000.0 {
                format!("{:.1} s", v / 1000.0)
            } else {
                format!("{:.0} ms", v)
            }
        }
        Resonance | OscMix | AmpSustain | FilterSustain => format!("{:.0} percent", v * 100.0),
        FilterEnvOctaves => format!("{v:+.1} octaves"),
        GainDb => format!("{v:.1} dB"),
        Osc1Semitones | Osc2Semitones => format!("{v:+.0} semitones"),
        Osc1Cents | Osc2Cents => format!("{v:+.0} cents"),
    }
}

/// Unit after a vertical drag of `dy` pixels (down is negative). A full
/// range takes 200 px, 1000 px with the fine modifier.
pub fn drag_unit(start: f64, dy: f64, fine: bool) -> f64 {
    let per_px = if fine { 1.0 / 1000.0 } else { 1.0 / 200.0 };
    (start - dy * per_px).clamp(0.0, 1.0)
}

/// Unit after one wheel notch or arrow key: 2 percent, 0.5 percent fine.
pub fn step_unit(unit: f64, dir: f64, fine: bool) -> f64 {
    let s = if fine { 0.005 } else { 0.02 };
    (unit + dir * s).clamp(0.0, 1.0)
}

/// Small random changes to the macro knobs ("Vary"): each knob moves by up
/// to `amount` of its range. `seed` makes it repeatable in tests.
pub fn vary(params: &SynthParams, seed: u64, amount: f64) -> Vec<(SynthParam, f64)> {
    let mut x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x >> 11) as f64 / (1u64 << 53) as f64
    };
    MACROS
        .iter()
        .map(|spec| {
            let u = to_unit(spec, params.get(spec.param));
            let d = (next() * 2.0 - 1.0) * amount;
            (spec.param, from_unit(spec, (u + d).clamp(0.0, 1.0)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_round_trip_on_every_knob() {
        for spec in &MACROS {
            for i in 0..=20 {
                let u = i as f64 / 20.0;
                let v = from_unit(spec, u);
                let (lo, hi) = spec.param.range();
                assert!((lo..=hi).contains(&v), "{} {v}", spec.name);
                assert!((to_unit(spec, v) - u).abs() < 1e-9, "{} {u}", spec.name);
            }
        }
    }

    #[test]
    fn curves_are_monotonic_and_hit_the_ends() {
        for spec in &MACROS {
            let (lo, hi) = spec.param.range();
            assert!((from_unit(spec, 0.0) - lo).abs() < 1e-9);
            assert!((from_unit(spec, 1.0) - hi).abs() < 1e-6);
            let mut last = f64::MIN;
            for i in 0..=50 {
                let v = from_unit(spec, i as f64 / 50.0);
                assert!(v >= last, "{}", spec.name);
                last = v;
            }
        }
    }

    #[test]
    fn tone_is_logarithmic_and_times_are_fine_near_zero() {
        let tone = &MACROS[0];
        // Halfway is the geometric middle, about 632 Hz, not 10 kHz.
        let mid = from_unit(tone, 0.5);
        assert!((mid - (20.0f64 * 20000.0).sqrt()).abs() < 1.0);
        let attack = &MACROS[4];
        assert!(from_unit(attack, 0.5) < 2000.0, "half a turn is under 2 s");
    }

    #[test]
    fn every_macro_names_a_distinct_parameter() {
        let mut seen = std::collections::HashSet::new();
        for spec in &MACROS {
            assert!(seen.insert(spec.param), "{}", spec.name);
        }
        for (p, _) in MORE {
            assert!(seen.insert(p), "{p:?}");
        }
        assert_eq!(seen.len(), SynthParam::ALL.len(), "all params reachable");
    }

    #[test]
    fn value_text() {
        assert_eq!(format_value(SynthParam::CutoffHz, 2000.0), "2.0 kHz");
        assert_eq!(format_value(SynthParam::CutoffHz, 440.0), "440 Hz");
        assert_eq!(format_value(SynthParam::AmpDecayMs, 200.0), "200 ms");
        assert_eq!(format_value(SynthParam::AmpDecayMs, 2500.0), "2.5 s");
        assert_eq!(format_value(SynthParam::AmpSustain, 0.7), "70 percent");
        assert_eq!(
            format_value(SynthParam::FilterEnvOctaves, 2.0),
            "+2.0 octaves"
        );
    }

    #[test]
    fn dragging_and_stepping() {
        assert!((drag_unit(0.5, -100.0, false) - 1.0).abs() < 1e-9);
        assert!((drag_unit(0.5, 100.0, false) - 0.0).abs() < 1e-9);
        assert!((drag_unit(0.5, -100.0, true) - 0.6).abs() < 1e-9);
        assert_eq!(drag_unit(0.9, -1000.0, false), 1.0);
        assert!((step_unit(0.5, 1.0, false) - 0.52).abs() < 1e-9);
        assert!((step_unit(0.5, -1.0, true) - 0.495).abs() < 1e-9);
        assert_eq!(step_unit(1.0, 1.0, false), 1.0);
    }

    #[test]
    fn vary_stays_in_range_and_moves_things() {
        let p = SynthParams::default();
        let a = vary(&p, 1, 0.1);
        let b = vary(&p, 2, 0.1);
        assert_eq!(a.len(), MACROS.len());
        assert_ne!(a, b);
        assert_eq!(a, vary(&p, 1, 0.1), "same seed, same result");
        for (param, v) in &a {
            let (lo, hi) = param.range();
            assert!((lo..=hi).contains(v));
        }
        // Small: no knob moves more than 10 percent of its unit range.
        for (spec, (_, v)) in MACROS.iter().zip(&a) {
            let d = (to_unit(spec, *v) - to_unit(spec, p.get(spec.param))).abs();
            assert!(d <= 0.1 + 1e-9, "{} moved {d}", spec.name);
        }
    }
}
