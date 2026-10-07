// SPDX-License-Identifier: GPL-3.0-or-later
//! Knobs for the native Milestone B parameters (SPEC 15.1, 15.2, 15.5),
//! without GTK: the sampler, the 808 and every built-in effect declare
//! their continuous parameters in `protocol::beats`; this module gives each
//! one a plain-word name, a value format, and a knob curve. The order of a
//! `Vec<NativeSpec>` is the parameter table order, so index `i` is the
//! parameter index the edits use.

use protocol::beats::{
    Bass808Param, Bass808Params, BuiltinFx, BuiltinFxKind, CompressorParam, DelayParam, EqParam,
    LimiterParam, ReverbParam, SamplerParam, SamplerParams, SaturatorParam,
};

use crate::knob_logic::Curve;

/// How a value reads next to its knob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fmt {
    Hz,
    Ms,
    /// A 0 to 1 value shown as a percentage.
    Percent,
    Db,
    Semitones,
    Cents,
    Ratio,
    /// A time in beats.
    Beats,
    /// A plain number with two decimals.
    Number,
}

#[derive(Clone, Copy, Debug)]
pub struct NativeSpec {
    pub name: &'static str,
    pub tip: &'static str,
    pub lo: f64,
    pub hi: f64,
    pub default: f64,
    pub fmt: Fmt,
}

impl NativeSpec {
    /// Frequencies and long times are not linear to the ear.
    pub fn curve(&self) -> Curve {
        match self.fmt {
            Fmt::Hz => Curve::Log,
            Fmt::Ms if self.lo <= 0.0 => Curve::Power(3.0),
            Fmt::Ms if self.hi / self.lo > 50.0 => Curve::Log,
            Fmt::Beats => Curve::Log,
            _ => Curve::Linear,
        }
    }

    pub fn to_unit(&self, value: f64) -> f64 {
        let (lo, hi) = (self.lo, self.hi);
        let v = value.clamp(lo, hi);
        let u = match self.curve() {
            Curve::Linear => (v - lo) / (hi - lo),
            Curve::Log => (v / lo).ln() / (hi / lo).ln(),
            Curve::Power(e) => ((v - lo) / (hi - lo)).powf(1.0 / e),
        };
        u.clamp(0.0, 1.0)
    }

    pub fn from_unit(&self, unit: f64) -> f64 {
        let (lo, hi) = (self.lo, self.hi);
        let u = unit.clamp(0.0, 1.0);
        let v = match self.curve() {
            Curve::Linear => lo + u * (hi - lo),
            Curve::Log => lo * (hi / lo).powf(u),
            Curve::Power(e) => lo + (hi - lo) * u.powf(e),
        };
        v.clamp(lo, hi)
    }

    /// The text under a knob: "2.0 kHz", "120 ms", "35 percent".
    pub fn format(&self, v: f64) -> String {
        match self.fmt {
            Fmt::Hz => {
                if v >= 1000.0 {
                    format!("{:.1} kHz", v / 1000.0)
                } else {
                    format!("{v:.0} Hz")
                }
            }
            Fmt::Ms => {
                if v >= 1000.0 {
                    format!("{:.1} s", v / 1000.0)
                } else if v < 10.0 {
                    format!("{v:.1} ms")
                } else {
                    format!("{v:.0} ms")
                }
            }
            Fmt::Percent => format!("{:.0} percent", v * 100.0),
            Fmt::Db => format!("{v:.1} dB"),
            Fmt::Semitones => format!("{v:+.1} st"),
            Fmt::Cents => format!("{v:+.0} ct"),
            Fmt::Ratio => format!("{v:.1} to 1"),
            Fmt::Beats => format!("{v:.2} beats"),
            Fmt::Number => format!("{v:.2}"),
        }
    }
}

fn spec(
    name: &'static str,
    tip: &'static str,
    fmt: Fmt,
    range: (f64, f64),
    default: f64,
) -> NativeSpec {
    NativeSpec {
        name,
        tip,
        lo: range.0,
        hi: range.1,
        default,
        fmt,
    }
}

/// The sampler's knobs, in `SamplerParam` order.
pub fn sampler_specs() -> Vec<NativeSpec> {
    let d = SamplerParams::default();
    let r = |p: SamplerParam| p.range();
    use SamplerParam::*;
    vec![
        spec(
            "Start",
            "Where the sound begins",
            Fmt::Percent,
            r(Start),
            d.start,
        ),
        spec("End", "Where the sound ends", Fmt::Percent, r(End), d.end),
        spec(
            "Pitch",
            "Raise or lower the sound",
            Fmt::Semitones,
            r(Semitones),
            d.semitones,
        ),
        spec("Fine", "Fine tuning", Fmt::Cents, r(Cents), d.cents),
        spec(
            "Attack",
            "How slowly each note fades in",
            Fmt::Ms,
            r(AttackMs),
            d.attack_ms,
        ),
        spec(
            "Decay",
            "How quickly it settles to the held level",
            Fmt::Ms,
            r(DecayMs),
            d.decay_ms,
        ),
        spec(
            "Hold",
            "How loud a held note stays",
            Fmt::Percent,
            r(Sustain),
            d.sustain,
        ),
        spec(
            "Tail",
            "How long the sound rings after you let go",
            Fmt::Ms,
            r(ReleaseMs),
            d.release_ms,
        ),
        spec(
            "Level",
            "Loudness of the channel",
            Fmt::Db,
            r(GainDb),
            d.gain_db,
        ),
    ]
}

/// The 808's knobs, in `Bass808Param` order.
pub fn bass808_specs() -> Vec<NativeSpec> {
    let d = Bass808Params::default();
    let r = |p: Bass808Param| p.range();
    use Bass808Param::*;
    vec![
        spec(
            "Tune",
            "Tuning against the played key",
            Fmt::Semitones,
            r(Tune),
            d.tune,
        ),
        spec(
            "Drop",
            "How far the pitch falls at the start",
            Fmt::Semitones,
            r(DropSemitones),
            d.drop_semitones,
        ),
        spec(
            "Drop time",
            "How quickly the pitch falls",
            Fmt::Ms,
            r(DropMs),
            d.drop_ms,
        ),
        spec(
            "Length",
            "How long each note rings",
            Fmt::Ms,
            r(DecayMs),
            d.decay_ms,
        ),
        spec(
            "Click",
            "The knock at the start of each note",
            Fmt::Percent,
            r(Click),
            d.click,
        ),
        spec("Drive", "Warmth and grit", Fmt::Percent, r(Drive), d.drive),
        spec(
            "Tone",
            "How bright the sound is",
            Fmt::Hz,
            r(ToneHz),
            d.tone_hz,
        ),
        spec(
            "Glide",
            "How slowly notes slide into each other",
            Fmt::Ms,
            r(GlideMs),
            d.glide_ms,
        ),
        spec(
            "Level",
            "Loudness of the channel",
            Fmt::Db,
            r(GainDb),
            d.gain_db,
        ),
    ]
}

/// The sampler parameter at table index `i`.
pub fn sampler_value(p: &SamplerParams, i: usize) -> f64 {
    SamplerParam::from_index(i).map_or(0.0, |k| p.get(k))
}

/// The 808 parameter at table index `i`.
pub fn bass808_value(p: &Bass808Params, i: usize) -> f64 {
    Bass808Param::from_index(i).map_or(0.0, |k| p.get(k))
}

/// The knobs of a built-in effect, in table order.
pub fn fx_specs(kind: BuiltinFxKind) -> Vec<NativeSpec> {
    let fx = BuiltinFx::new(kind);
    let names: &[(&'static str, &'static str, Fmt)] = match kind {
        BuiltinFxKind::Eq => &[
            ("Low cut", "Remove rumble below this", Fmt::Hz),
            ("Low freq", "Where the low shelf starts", Fmt::Hz),
            ("Low gain", "Boost or cut the lows", Fmt::Db),
            ("Mid freq", "Center of the middle band", Fmt::Hz),
            ("Mid width", "How narrow the middle band is", Fmt::Number),
            ("Mid gain", "Boost or cut the middle", Fmt::Db),
            ("High freq", "Where the high shelf starts", Fmt::Hz),
            ("High gain", "Boost or cut the highs", Fmt::Db),
        ],
        BuiltinFxKind::Compressor => &[
            ("Threshold", "Level where squashing starts", Fmt::Db),
            ("Ratio", "How hard loud parts are squashed", Fmt::Ratio),
            ("Attack", "How quickly it reacts", Fmt::Ms),
            ("Release", "How quickly it lets go", Fmt::Ms),
            ("Knee", "How gently squashing starts", Fmt::Db),
            ("Makeup", "Level added after squashing", Fmt::Db),
            ("Mix", "Blend of squashed and original", Fmt::Percent),
        ],
        BuiltinFxKind::Saturator => &[
            ("Drive", "How much the sound is pushed", Fmt::Db),
            ("Tone", "Cut the harsh highs", Fmt::Hz),
            ("Mix", "Blend of processed and original", Fmt::Percent),
            ("Output", "Level after the effect", Fmt::Db),
        ],
        BuiltinFxKind::Reverb => &[
            ("Size", "How big the room is", Fmt::Percent),
            ("Damping", "How quickly highs fade", Fmt::Percent),
            ("Width", "How wide the room sounds", Fmt::Percent),
            ("Pre-delay", "Gap before the room answers", Fmt::Ms),
            ("Mix", "Blend of room and original", Fmt::Percent),
        ],
        BuiltinFxKind::Delay => &[
            ("Time", "Time between echoes, in beats", Fmt::Beats),
            ("Feedback", "How many echoes you hear", Fmt::Percent),
            ("Tone", "Cut the harsh highs of the echoes", Fmt::Hz),
            ("Mix", "Blend of echoes and original", Fmt::Percent),
        ],
        BuiltinFxKind::Limiter => &[
            ("Ceiling", "The loudest the output can get", Fmt::Db),
            ("Release", "How quickly it lets go", Fmt::Ms),
        ],
    };
    (0..fx.param_count())
        .map(|i| {
            let (name, tip, fmt) = names[i];
            let (lo, hi) = fx.param_range(i).unwrap_or((0.0, 1.0));
            spec(name, tip, fmt, (lo, hi), fx.param(i).unwrap_or(lo))
        })
        .collect()
}

/// The effect's name in the mixer.
pub fn fx_name(kind: BuiltinFxKind) -> &'static str {
    match kind {
        BuiltinFxKind::Eq => "EQ",
        BuiltinFxKind::Compressor => "Compressor",
        BuiltinFxKind::Saturator => "Saturator",
        BuiltinFxKind::Reverb => "Reverb",
        BuiltinFxKind::Delay => "Delay",
        BuiltinFxKind::Limiter => "Limiter",
    }
}

/// Every effect kind in menu order.
pub const FX_KINDS: [BuiltinFxKind; 6] = [
    BuiltinFxKind::Eq,
    BuiltinFxKind::Compressor,
    BuiltinFxKind::Saturator,
    BuiltinFxKind::Reverb,
    BuiltinFxKind::Delay,
    BuiltinFxKind::Limiter,
];

/// A parameter table index as the typed enums name it, so specs and
/// protocol cannot drift apart unnoticed.
pub fn param_count(kind: BuiltinFxKind) -> usize {
    match kind {
        BuiltinFxKind::Eq => EqParam::ALL.len(),
        BuiltinFxKind::Compressor => CompressorParam::ALL.len(),
        BuiltinFxKind::Saturator => SaturatorParam::ALL.len(),
        BuiltinFxKind::Reverb => ReverbParam::ALL.len(),
        BuiltinFxKind::Delay => DelayParam::ALL.len(),
        BuiltinFxKind::Limiter => LimiterParam::ALL.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(specs: &[NativeSpec], what: &str) {
        for (i, s) in specs.iter().enumerate() {
            assert!(s.lo < s.hi, "{what} {}", s.name);
            assert!((s.lo..=s.hi).contains(&s.default), "{what} {}", s.name);
            assert!(!s.name.is_empty() && !s.tip.is_empty());
            for k in 0..=20 {
                let u = k as f64 / 20.0;
                let v = s.from_unit(u);
                assert!((s.lo..=s.hi).contains(&v), "{what} {} {v}", s.name);
                assert!((s.to_unit(v) - u).abs() < 1e-9, "{what} {} {u}", s.name);
            }
            assert!((s.from_unit(0.0) - s.lo).abs() < 1e-9, "{what} {i}");
            assert!((s.from_unit(1.0) - s.hi).abs() < 1e-6, "{what} {i}");
        }
    }

    #[test]
    fn specs_follow_the_parameter_tables() {
        assert_eq!(sampler_specs().len(), SamplerParam::ALL.len());
        assert_eq!(bass808_specs().len(), Bass808Param::ALL.len());
        for (s, p) in sampler_specs().iter().zip(SamplerParam::ALL) {
            assert_eq!((s.lo, s.hi), p.range());
        }
        for (s, p) in bass808_specs().iter().zip(Bass808Param::ALL) {
            assert_eq!((s.lo, s.hi), p.range());
        }
        for k in FX_KINDS {
            let n = BuiltinFx::new(k).param_count();
            assert_eq!(fx_specs(k).len(), n, "{k:?}");
            assert_eq!(param_count(k), n);
        }
    }

    #[test]
    fn every_knob_round_trips_and_hits_its_ends() {
        check(&sampler_specs(), "sampler");
        check(&bass808_specs(), "808");
        for k in FX_KINDS {
            check(&fx_specs(k), fx_name(k));
        }
    }

    #[test]
    fn frequencies_are_logarithmic_and_zero_based_times_fine() {
        let tone = &bass808_specs()[6];
        assert_eq!(tone.curve(), Curve::Log);
        let mid = tone.from_unit(0.5);
        assert!((mid - (200.0f64 * 20000.0).sqrt()).abs() < 1.0);
        let attack = &sampler_specs()[4];
        assert!(attack.from_unit(0.5) < 2000.0);
    }

    #[test]
    fn value_text() {
        let s = sampler_specs();
        assert_eq!(s[8].format(-3.0), "-3.0 dB");
        assert_eq!(s[2].format(7.0), "+7.0 st");
        assert_eq!(s[3].format(-12.0), "-12 ct");
        assert_eq!(s[6].format(1.0), "100 percent");
        assert_eq!(s[7].format(30.0), "30 ms");
        assert_eq!(s[7].format(2500.0), "2.5 s");
        let c = fx_specs(BuiltinFxKind::Compressor);
        assert_eq!(c[1].format(4.0), "4.0 to 1");
        assert_eq!(c[2].format(0.5), "0.5 ms");
        let e = fx_specs(BuiltinFxKind::Eq);
        assert_eq!(e[3].format(1000.0), "1.0 kHz");
    }
}
