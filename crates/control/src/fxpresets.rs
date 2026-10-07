// SPDX-License-Identifier: GPL-3.0-or-later
//! Sounds for the built-in effects (SPEC 24.2): named settings that the
//! effect panel, the Drive quick action and the `fx_add` / `fx_set` tools
//! share. Each one lists every continuous parameter in table order, so
//! applying it is one `SetFxParam` per value that differs, plus the curve
//! or ping pong switch where the effect has one.
//!
//! Pure data and edit builders on the protocol types: the engine tests
//! (`crates/engine/tests/fxwave.rs`) load these tables, so the output gain
//! of the Drive sounds is checked against what the engine really does.

use protocol::beats::{BuiltinFx, BuiltinFxKind, SaturatorCurve};
use protocol::edit::Edit;
use protocol::ids::{InstanceId, TrackId};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FxPreset {
    /// Shown in the picker and accepted by the tools (any letter case).
    pub name: &'static str,
    pub about: &'static str,
    /// Saturator only.
    pub curve: Option<SaturatorCurve>,
    /// Delay only.
    pub ping_pong: Option<bool>,
    /// Every continuous parameter, in table order.
    pub values: &'static [f64],
}

const fn p(name: &'static str, about: &'static str, values: &'static [f64]) -> FxPreset {
    FxPreset {
        name,
        about,
        curve: None,
        ping_pong: None,
        values,
    }
}

const fn sat(
    name: &'static str,
    about: &'static str,
    curve: SaturatorCurve,
    values: &'static [f64],
) -> FxPreset {
    FxPreset {
        name,
        about,
        curve: Some(curve),
        ping_pong: None,
        values,
    }
}

// low cut, low freq, low gain, mid freq, mid width, mid gain, high freq, high gain
const EQ: &[FxPreset] = &[
    p(
        "Flat",
        "No change",
        &[10.0, 120.0, 0.0, 1000.0, 0.7, 0.0, 8000.0, 0.0],
    ),
    p(
        "Bass Boost",
        "Fuller lows",
        &[10.0, 100.0, 6.0, 1000.0, 0.7, 0.0, 8000.0, 0.0],
    ),
    p(
        "Tight Low End",
        "Removes rumble below the bass",
        &[35.0, 120.0, 0.0, 1000.0, 0.7, 0.0, 8000.0, 0.0],
    ),
    p(
        "Cut Mud",
        "Clears the muddy middle",
        &[10.0, 120.0, 0.0, 300.0, 1.0, -5.0, 8000.0, 0.0],
    ),
    p(
        "Bright",
        "More air and bite",
        &[10.0, 120.0, 0.0, 1000.0, 0.7, 0.0, 6000.0, 5.0],
    ),
    p(
        "Phone",
        "Thin, like a small speaker",
        &[300.0, 120.0, -12.0, 1500.0, 0.7, 4.0, 4000.0, -12.0],
    ),
];

// threshold, ratio, attack, release, knee, makeup, mix
const COMPRESSOR: &[FxPreset] = &[
    p(
        "Gentle",
        "Evens things out softly",
        &[-18.0, 2.0, 20.0, 150.0, 10.0, 2.0, 1.0],
    ),
    p(
        "Punch",
        "Keeps the hit, tames the tail",
        &[-20.0, 4.0, 30.0, 100.0, 6.0, 3.0, 1.0],
    ),
    p(
        "Tight",
        "Short and controlled",
        &[-24.0, 6.0, 5.0, 60.0, 3.0, 4.0, 1.0],
    ),
    p(
        "Squash",
        "Heavy and loud",
        &[-30.0, 10.0, 2.0, 80.0, 3.0, 8.0, 1.0],
    ),
    p(
        "Parallel",
        "Squashed sound blended with the original",
        &[-30.0, 8.0, 2.0, 80.0, 3.0, 6.0, 0.5],
    ),
];

// drive, tone, mix, output. Output gains are measured: the engine test
// `drive_sounds_keep_the_loudness` keeps each within 1 dB of the input.
const SATURATOR: &[FxPreset] = &[
    sat(
        "Warm",
        "A little warmth",
        SaturatorCurve::Soft,
        &[6.0, 9000.0, 0.7, -3.1],
    ),
    sat(
        "Crunch",
        "Gritty and forward",
        SaturatorCurve::Soft,
        &[14.0, 7000.0, 1.0, -7.8],
    ),
    sat(
        "Phonk 808",
        "Thick, growling bass",
        SaturatorCurve::Soft,
        &[22.0, 3500.0, 1.0, -8.8],
    ),
    sat(
        "Phonk Cowbell",
        "Hard and metallic",
        SaturatorCurve::Hard,
        &[18.0, 9000.0, 1.0, -8.9],
    ),
    sat(
        "Hard Clip",
        "Flat-topped, loud and aggressive",
        SaturatorCurve::Hard,
        &[12.0, 16000.0, 1.0, -8.2],
    ),
];

// size, damping, width, pre-delay, mix
const REVERB: &[FxPreset] = &[
    p(
        "Small Room",
        "Close and short",
        &[0.25, 0.6, 1.0, 5.0, 0.15],
    ),
    p("Plate", "Smooth and bright", &[0.55, 0.2, 1.0, 0.0, 0.25]),
    p("Big Hall", "Large and airy", &[0.85, 0.4, 1.0, 25.0, 0.3]),
    p(
        "Wash",
        "A huge wash behind the sound",
        &[0.95, 0.3, 1.0, 40.0, 0.5],
    ),
];

// time in beats, feedback, tone, mix
const DELAY: &[FxPreset] = &[
    p("Slap", "One quick echo", &[0.25, 0.1, 6000.0, 0.2]),
    p(
        "Eighth",
        "Echoes on every half beat",
        &[0.5, 0.35, 6000.0, 0.25],
    ),
    p(
        "Dotted Eighth",
        "Bouncing echoes",
        &[0.75, 0.4, 5000.0, 0.25],
    ),
    p("Echo", "Long trailing echoes", &[1.0, 0.5, 4000.0, 0.3]),
    FxPreset {
        name: "Ping Pong",
        about: "Echoes that bounce left and right",
        curve: None,
        ping_pong: Some(true),
        values: &[0.75, 0.4, 5000.0, 0.3],
    },
];

// ceiling, release
const LIMITER: &[FxPreset] = &[
    p("Gentle", "Catches the loudest peaks", &[-1.0, 200.0]),
    p("Loud", "Louder, with a safe ceiling", &[-0.3, 80.0]),
    p("Hard", "Very loud and tight", &[-0.3, 30.0]),
];

pub fn presets(kind: BuiltinFxKind) -> &'static [FxPreset] {
    match kind {
        BuiltinFxKind::Eq => EQ,
        BuiltinFxKind::Compressor => COMPRESSOR,
        BuiltinFxKind::Saturator => SATURATOR,
        BuiltinFxKind::Reverb => REVERB,
        BuiltinFxKind::Delay => DELAY,
        BuiltinFxKind::Limiter => LIMITER,
    }
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A sound by name; letter case, spaces and dashes do not matter.
pub fn find(kind: BuiltinFxKind, name: &str) -> Option<&'static FxPreset> {
    let want = norm(name);
    presets(kind).iter().find(|x| norm(x.name) == want)
}

/// Every sound name of `kind`, for error messages.
pub fn names(kind: BuiltinFxKind) -> String {
    presets(kind)
        .iter()
        .map(|x| x.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The sound whose values `fx` holds exactly, if any.
pub fn current(fx: &BuiltinFx) -> Option<&'static FxPreset> {
    presets(fx.kind()).iter().find(|x| {
        x.values
            .iter()
            .enumerate()
            .all(|(i, v)| fx.param(i).is_some_and(|c| (c - v).abs() < 1e-9))
            && match fx {
                BuiltinFx::Saturator { curve, .. } => x.curve.is_none_or(|c| c == *curve),
                BuiltinFx::Delay { ping_pong, .. } => x.ping_pong.unwrap_or(false) == *ping_pong,
                _ => true,
            }
    })
}

/// `fx` with the sound applied.
pub fn applied(fx: &BuiltinFx, preset: &FxPreset) -> BuiltinFx {
    let mut out = fx.clone();
    for (i, v) in preset.values.iter().enumerate() {
        out.set_param(i, *v);
    }
    match &mut out {
        BuiltinFx::Saturator { curve, .. } => {
            if let Some(c) = preset.curve {
                *curve = c;
            }
        }
        BuiltinFx::Delay { ping_pong, .. } => *ping_pong = preset.ping_pong.unwrap_or(false),
        _ => {}
    }
    out
}

/// Edits that turn `fx` (the insert `instance` on `track`) into the sound.
/// Only values that differ are written.
pub fn edits(track: TrackId, instance: InstanceId, fx: &BuiltinFx, preset: &FxPreset) -> Vec<Edit> {
    let mut out = Vec::new();
    for (i, v) in preset.values.iter().enumerate() {
        if fx.param(i).is_some_and(|c| (c - v).abs() > 1e-12) {
            out.push(Edit::SetFxParam {
                track,
                instance,
                param: i as u8,
                value: *v,
            });
        }
    }
    match fx {
        BuiltinFx::Saturator { curve, .. } => {
            if let Some(c) = preset.curve.filter(|c| c != curve) {
                out.push(Edit::SetSaturatorCurve {
                    track,
                    instance,
                    curve: c,
                });
            }
        }
        BuiltinFx::Delay { ping_pong, .. } => {
            let want = preset.ping_pong.unwrap_or(false);
            if want != *ping_pong {
                out.push(Edit::SetDelayPingPong {
                    track,
                    instance,
                    ping_pong: want,
                });
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sound_fits_its_effect() {
        for kind in [
            BuiltinFxKind::Eq,
            BuiltinFxKind::Compressor,
            BuiltinFxKind::Saturator,
            BuiltinFxKind::Reverb,
            BuiltinFxKind::Delay,
            BuiltinFxKind::Limiter,
        ] {
            let fx = BuiltinFx::new(kind);
            assert!(!presets(kind).is_empty());
            for s in presets(kind) {
                assert_eq!(s.values.len(), fx.param_count(), "{}", s.name);
                for (i, v) in s.values.iter().enumerate() {
                    let (lo, hi) = fx.param_range(i).unwrap();
                    assert!((lo..=hi).contains(v), "{} #{i} = {v}", s.name);
                }
                let back = applied(&fx, s);
                assert_eq!(current(&back).map(|c| c.name), Some(s.name));
                let e = edits(TrackId(1), InstanceId(2), &fx, s);
                assert!(e.len() <= fx.param_count() + 1);
            }
        }
    }

    #[test]
    fn names_ignore_case_and_spaces() {
        assert!(find(BuiltinFxKind::Saturator, "phonk-808").is_some());
        assert!(find(BuiltinFxKind::Saturator, "HARD CLIP").is_some());
        assert!(find(BuiltinFxKind::Saturator, "nope").is_none());
    }
}
