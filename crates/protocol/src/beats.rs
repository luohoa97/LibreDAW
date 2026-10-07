// SPDX-License-Identifier: GPL-3.0-or-later
//! Milestone B instruments and effects (SPEC 15.1, 15.2, 15.5, 17.2).
//!
//! Every native instrument and effect splits its settings into structural
//! fields (compiled into `Compiled`: modes, sample choice, routing) and a
//! flat set of continuous `f64` parameters that the engine reads from a
//! table of atomics, so moving a knob never recompiles (17.1).
//!
//! The continuous parameters are declared once with `native_params!`, which
//! generates the struct, the parameter enum with stable table indices,
//! ranges, defaults, and `get`/`set`. Indices are part of the interface:
//! append new parameters at the end, never reorder.

use serde::{Deserialize, Serialize};

use crate::ids::TrackId;

macro_rules! native_params {
    (
        $(#[$sm:meta])*
        $params:ident, $param:ident {
            $( $(#[$fm:meta])* $field:ident / $variant:ident : $lo:expr, $hi:expr, $def:expr; )+
        }
    ) => {
        $(#[$sm])*
        #[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $params {
            $( $(#[$fm])* pub $field: f64, )+
        }

        impl Default for $params {
            fn default() -> $params {
                $params { $( $field: $def, )+ }
            }
        }

        /// Continuous parameters and their table index.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        #[repr(u8)]
        pub enum $param {
            $( $variant, )+
        }

        impl $param {
            pub const ALL: &'static [$param] = &[ $( $param::$variant, )+ ];

            /// Index into the owner's slot in the parameter table.
            pub fn index(self) -> usize {
                self as usize
            }

            /// Inclusive valid range.
            pub fn range(self) -> (f64, f64) {
                match self { $( $param::$variant => ($lo, $hi), )+ }
            }

            pub fn from_index(i: usize) -> Option<$param> {
                Self::ALL.get(i).copied()
            }

            /// Field name in the project file.
            pub fn name(self) -> &'static str {
                match self { $( $param::$variant => stringify!($field), )+ }
            }
        }

        impl $params {
            pub fn get(&self, p: $param) -> f64 {
                match p { $( $param::$variant => self.$field, )+ }
            }

            pub fn set(&mut self, p: $param, v: f64) {
                match p { $( $param::$variant => self.$field = v, )+ }
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Sampler (15.1)

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleMode {
    /// Plays to the end of the sample; ignores note-off and key.
    OneShot,
    /// Key-tracked from the instrument's root key; note-off starts release.
    Pitched,
}

native_params!(
    /// Continuous sampler settings.
    SamplerParams, SamplerParam {
        /// Start point as a fraction of the sample, 0 to 1.
        start / Start: 0.0, 1.0, 0.0;
        /// End point as a fraction of the sample, 0 to 1 (must exceed start).
        end / End: 0.0, 1.0, 1.0;
        semitones / Semitones: -48.0, 48.0, 0.0;
        cents / Cents: -100.0, 100.0, 0.0;
        attack_ms / AttackMs: 0.0, 10000.0, 0.0;
        decay_ms / DecayMs: 0.0, 10000.0, 0.0;
        sustain / Sustain: 0.0, 1.0, 1.0;
        release_ms / ReleaseMs: 0.0, 10000.0, 30.0;
        gain_db / GainDb: -96.0, 12.0, 0.0;
    }
);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sampler {
    /// Sample hash (`SampleRef::hash`); `None` plays silence.
    #[serde(default)]
    pub sample: Option<String>,
    pub mode: SampleMode,
    #[serde(default)]
    pub reverse: bool,
    pub params: SamplerParams,
}

// ---------------------------------------------------------------------------
// 808 (15.2)

native_params!(
    /// Continuous 808 settings.
    Bass808Params, Bass808Param {
        /// Tuning relative to the played key, semitones.
        tune / Tune: -24.0, 24.0, 0.0;
        /// Pitch envelope start above the note, semitones.
        drop_semitones / DropSemitones: 0.0, 48.0, 12.0;
        /// Pitch envelope time.
        drop_ms / DropMs: 1.0, 500.0, 40.0;
        decay_ms / DecayMs: 50.0, 10000.0, 1200.0;
        /// Transient click level, 0 to 1.
        click / Click: 0.0, 1.0, 0.3;
        /// Saturation amount, 0 to 1.
        drive / Drive: 0.0, 1.0, 0.2;
        /// Lowpass after the saturator.
        tone_hz / ToneHz: 200.0, 20000.0, 8000.0;
        /// Slide time between legato notes in mono mode.
        glide_ms / GlideMs: 0.0, 1000.0, 60.0;
        gain_db / GainDb: -96.0, 12.0, -3.0;
    }
);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bass808 {
    /// Mono with last-note priority and legato glide (17.2).
    pub mono: bool,
    pub params: Bass808Params,
}

impl Default for Bass808 {
    fn default() -> Bass808 {
        Bass808 {
            mono: true,
            params: Bass808Params::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Built-in effects (15.5, 17.2)

native_params!(
    /// Four-band EQ: low shelf, peak, high shelf, plus a low cut.
    EqParams, EqParam {
        low_cut_hz / LowCutHz: 10.0, 1000.0, 10.0;
        low_hz / LowHz: 20.0, 1000.0, 120.0;
        low_gain_db / LowGainDb: -24.0, 24.0, 0.0;
        mid_hz / MidHz: 100.0, 10000.0, 1000.0;
        mid_q / MidQ: 0.1, 10.0, 0.7;
        mid_gain_db / MidGainDb: -24.0, 24.0, 0.0;
        high_hz / HighHz: 1000.0, 20000.0, 8000.0;
        high_gain_db / HighGainDb: -24.0, 24.0, 0.0;
    }
);

native_params!(
    /// Compressor; the key comes from `sidechain` when set.
    CompressorParams, CompressorParam {
        threshold_db / ThresholdDb: -60.0, 0.0, -18.0;
        ratio / Ratio: 1.0, 20.0, 4.0;
        attack_ms / AttackMs: 0.1, 200.0, 5.0;
        release_ms / ReleaseMs: 5.0, 2000.0, 120.0;
        knee_db / KneeDb: 0.0, 24.0, 6.0;
        makeup_db / MakeupDb: 0.0, 24.0, 0.0;
        mix / Mix: 0.0, 1.0, 1.0;
    }
);

native_params!(
    /// Saturator / distortion.
    SaturatorParams, SaturatorParam {
        drive_db / DriveDb: 0.0, 48.0, 12.0;
        tone_hz / ToneHz: 200.0, 20000.0, 12000.0;
        mix / Mix: 0.0, 1.0, 1.0;
        output_db / OutputDb: -24.0, 12.0, -6.0;
    }
);

native_params!(
    /// Algorithmic reverb (our own code, Freeverb class).
    ReverbParams, ReverbParam {
        size / Size: 0.0, 1.0, 0.5;
        damping / Damping: 0.0, 1.0, 0.5;
        width / Width: 0.0, 1.0, 1.0;
        predelay_ms / PredelayMs: 0.0, 200.0, 10.0;
        mix / Mix: 0.0, 1.0, 0.25;
    }
);

native_params!(
    /// Tempo-synced delay. Maximum time 4 s (17.2).
    DelayParams, DelayParam {
        /// Delay time in beats (quarter notes), 1/16 to 4.
        time_beats / TimeBeats: 0.0625, 4.0, 0.75;
        feedback / Feedback: 0.0, 0.95, 0.35;
        tone_hz / ToneHz: 200.0, 20000.0, 6000.0;
        mix / Mix: 0.0, 1.0, 0.25;
    }
);

native_params!(
    /// Zero-latency peak limiter (17.2).
    LimiterParams, LimiterParam {
        ceiling_db / CeilingDb: -24.0, 0.0, -1.0;
        release_ms / ReleaseMs: 1.0, 1000.0, 80.0;
    }
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaturatorCurve {
    Soft,
    Hard,
    Fold,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuiltinFx {
    Eq {
        params: EqParams,
    },
    Compressor {
        params: CompressorParams,
        /// Key input: another track's signal, tapped before its fader and
        /// regardless of its mute and solo (17.2).
        #[serde(default)]
        sidechain: Option<TrackId>,
    },
    Saturator {
        curve: SaturatorCurve,
        params: SaturatorParams,
    },
    Reverb {
        params: ReverbParams,
    },
    Delay {
        #[serde(default)]
        ping_pong: bool,
        params: DelayParams,
    },
    Limiter {
        params: LimiterParams,
    },
}

/// Which effect to create; `apply()` fills in default parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinFxKind {
    Eq,
    Compressor,
    Saturator,
    Reverb,
    Delay,
    Limiter,
}

impl BuiltinFx {
    pub fn new(kind: BuiltinFxKind) -> BuiltinFx {
        match kind {
            BuiltinFxKind::Eq => BuiltinFx::Eq {
                params: EqParams::default(),
            },
            BuiltinFxKind::Compressor => BuiltinFx::Compressor {
                params: CompressorParams::default(),
                sidechain: None,
            },
            BuiltinFxKind::Saturator => BuiltinFx::Saturator {
                curve: SaturatorCurve::Soft,
                params: SaturatorParams::default(),
            },
            BuiltinFxKind::Reverb => BuiltinFx::Reverb {
                params: ReverbParams::default(),
            },
            BuiltinFxKind::Delay => BuiltinFx::Delay {
                ping_pong: false,
                params: DelayParams::default(),
            },
            BuiltinFxKind::Limiter => BuiltinFx::Limiter {
                params: LimiterParams::default(),
            },
        }
    }

    pub fn kind(&self) -> BuiltinFxKind {
        match self {
            BuiltinFx::Eq { .. } => BuiltinFxKind::Eq,
            BuiltinFx::Compressor { .. } => BuiltinFxKind::Compressor,
            BuiltinFx::Saturator { .. } => BuiltinFxKind::Saturator,
            BuiltinFx::Reverb { .. } => BuiltinFxKind::Reverb,
            BuiltinFx::Delay { .. } => BuiltinFxKind::Delay,
            BuiltinFx::Limiter { .. } => BuiltinFxKind::Limiter,
        }
    }

    /// Number of continuous parameters (each below `FX_PARAMS_PER_INSERT`).
    pub fn param_count(&self) -> usize {
        match self {
            BuiltinFx::Eq { .. } => EqParam::ALL.len(),
            BuiltinFx::Compressor { .. } => CompressorParam::ALL.len(),
            BuiltinFx::Saturator { .. } => SaturatorParam::ALL.len(),
            BuiltinFx::Reverb { .. } => ReverbParam::ALL.len(),
            BuiltinFx::Delay { .. } => DelayParam::ALL.len(),
            BuiltinFx::Limiter { .. } => LimiterParam::ALL.len(),
        }
    }

    /// Continuous parameter `i` by table index.
    pub fn param(&self, i: usize) -> Option<f64> {
        Some(match self {
            BuiltinFx::Eq { params } => params.get(EqParam::from_index(i)?),
            BuiltinFx::Compressor { params, .. } => params.get(CompressorParam::from_index(i)?),
            BuiltinFx::Saturator { params, .. } => params.get(SaturatorParam::from_index(i)?),
            BuiltinFx::Reverb { params } => params.get(ReverbParam::from_index(i)?),
            BuiltinFx::Delay { params, .. } => params.get(DelayParam::from_index(i)?),
            BuiltinFx::Limiter { params } => params.get(LimiterParam::from_index(i)?),
        })
    }

    /// Field name of continuous parameter `i` in the project file.
    pub fn param_name(&self, i: usize) -> Option<&'static str> {
        Some(match self {
            BuiltinFx::Eq { .. } => EqParam::from_index(i)?.name(),
            BuiltinFx::Compressor { .. } => CompressorParam::from_index(i)?.name(),
            BuiltinFx::Saturator { .. } => SaturatorParam::from_index(i)?.name(),
            BuiltinFx::Reverb { .. } => ReverbParam::from_index(i)?.name(),
            BuiltinFx::Delay { .. } => DelayParam::from_index(i)?.name(),
            BuiltinFx::Limiter { .. } => LimiterParam::from_index(i)?.name(),
        })
    }

    /// Range of continuous parameter `i`.
    pub fn param_range(&self, i: usize) -> Option<(f64, f64)> {
        Some(match self {
            BuiltinFx::Eq { .. } => EqParam::from_index(i)?.range(),
            BuiltinFx::Compressor { .. } => CompressorParam::from_index(i)?.range(),
            BuiltinFx::Saturator { .. } => SaturatorParam::from_index(i)?.range(),
            BuiltinFx::Reverb { .. } => ReverbParam::from_index(i)?.range(),
            BuiltinFx::Delay { .. } => DelayParam::from_index(i)?.range(),
            BuiltinFx::Limiter { .. } => LimiterParam::from_index(i)?.range(),
        })
    }

    /// Sets continuous parameter `i`; returns false for a bad index.
    pub fn set_param(&mut self, i: usize, v: f64) -> bool {
        match self {
            BuiltinFx::Eq { params } => EqParam::from_index(i).map(|p| params.set(p, v)),
            BuiltinFx::Compressor { params, .. } => {
                CompressorParam::from_index(i).map(|p| params.set(p, v))
            }
            BuiltinFx::Saturator { params, .. } => {
                SaturatorParam::from_index(i).map(|p| params.set(p, v))
            }
            BuiltinFx::Reverb { params } => ReverbParam::from_index(i).map(|p| params.set(p, v)),
            BuiltinFx::Delay { params, .. } => DelayParam::from_index(i).map(|p| params.set(p, v)),
            BuiltinFx::Limiter { params } => LimiterParam::from_index(i).map(|p| params.set(p, v)),
        }
        .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::{FX_PARAMS_PER_INSERT, MAX_PARAMS_PER_SLOT};

    #[test]
    fn defaults_are_in_range_and_tables_fit() {
        assert!(SamplerParam::ALL.len() <= MAX_PARAMS_PER_SLOT);
        assert!(Bass808Param::ALL.len() <= MAX_PARAMS_PER_SLOT);
        for p in SamplerParam::ALL {
            let (lo, hi) = p.range();
            let v = SamplerParams::default().get(*p);
            assert!(lo <= v && v <= hi, "{p:?}");
        }
        for p in Bass808Param::ALL {
            let (lo, hi) = p.range();
            let v = Bass808Params::default().get(*p);
            assert!(lo <= v && v <= hi, "{p:?}");
        }
        for kind in [
            BuiltinFxKind::Eq,
            BuiltinFxKind::Compressor,
            BuiltinFxKind::Saturator,
            BuiltinFxKind::Reverb,
            BuiltinFxKind::Delay,
            BuiltinFxKind::Limiter,
        ] {
            let fx = BuiltinFx::new(kind);
            assert_eq!(fx.kind(), kind);
            assert!(fx.param_count() <= FX_PARAMS_PER_INSERT);
            for i in 0..fx.param_count() {
                let (lo, hi) = fx.param_range(i).unwrap();
                let v = fx.param(i).unwrap();
                assert!(lo <= v && v <= hi, "{kind:?} {i}");
            }
            assert!(fx.param(fx.param_count()).is_none());
        }
    }

    #[test]
    fn set_and_get_agree() {
        let mut fx = BuiltinFx::new(BuiltinFxKind::Compressor);
        assert!(fx.set_param(1, 8.0));
        assert_eq!(fx.param(1), Some(8.0));
        assert!(!fx.set_param(99, 1.0));
        let mut s = SamplerParams::default();
        s.set(SamplerParam::Semitones, -12.0);
        assert_eq!(s.semitones, -12.0);
        assert_eq!(SamplerParam::from_index(2), Some(SamplerParam::Semitones));
    }
}
