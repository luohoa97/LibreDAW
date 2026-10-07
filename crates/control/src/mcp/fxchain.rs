// SPDX-License-Identifier: GPL-3.0-or-later
//! Effect chains built from the built-in effects (SPEC 24.2): Drive on a
//! row, Duck to Kick on a row or mixer track, and Loudness on Main Output.
//! Pure builders on a `Project` snapshot; the window and the MCP tools both
//! call them, so they change the project in the same way (PARITY.md).

use protocol::beats::{
    BuiltinFx, BuiltinFxKind, CompressorParam, LimiterParam, SaturatorCurve, SaturatorParam,
};
use protocol::consts::MAX_INSERTS;
use protocol::edit::Edit;
use protocol::ids::{ChannelId, InstanceId, TrackId};
use protocol::model::{Insert, Project, Track};
use serde::Deserialize;

use super::compose::{BuildResult, Built};
use super::ids::IdGen;
use crate::fxpresets::{self, FxPreset};

fn set(track: TrackId, instance: InstanceId, param: usize, value: f64) -> Edit {
    Edit::SetFxParam {
        track,
        instance,
        param: param as u8,
        value,
    }
}

/// An effect of `kind` that `fx` is, with its instance.
fn builtins(t: &Track) -> impl Iterator<Item = (usize, InstanceId, &BuiltinFx)> {
    t.inserts.iter().enumerate().filter_map(|(i, x)| match x {
        Insert::Builtin { instance, fx } => Some((i, *instance, fx)),
        _ => None,
    })
}

/// Adds an effect of `kind` at the end of `track` with the values of
/// `preset` (or the defaults), unless `reuse` finds one already there.
fn add_with(
    b: &mut Built,
    ids: &mut IdGen,
    t: &Track,
    extra: usize,
    kind: BuiltinFxKind,
    preset: Option<&FxPreset>,
    label: &str,
) -> Result<InstanceId, String> {
    let n = t.inserts.len() + extra;
    if n >= MAX_INSERTS {
        return Err(format!(
            "{} already has {MAX_INSERTS} effects; remove one first",
            t.name
        ));
    }
    let instance = InstanceId(ids.alloc());
    b.push(
        format!("{label}: add the effect"),
        Edit::AddBuiltinInsert {
            track: t.id,
            index: n as u8,
            fx: kind,
        },
    );
    if let Some(p) = preset {
        let base = BuiltinFx::new(kind);
        for (i, e) in fxpresets::edits(t.id, instance, &base, p)
            .into_iter()
            .enumerate()
        {
            b.push(format!("{label}: setting {i}"), e);
        }
    }
    Ok(instance)
}

// ---- Drive ------------------------------------------------------------------

/// Drive on the mixer track of instrument `channel`: the track's last
/// Saturator takes the style; without one a Saturator is added.
pub fn drive(project: &Project, ids: &mut IdGen, channel: ChannelId, style: &str) -> BuildResult {
    let ch = project
        .channel(channel)
        .ok_or_else(|| format!("instrument {channel} does not exist"))?;
    let preset = fxpresets::find(BuiltinFxKind::Saturator, style).ok_or_else(|| {
        format!(
            "no drive style \"{}\"; the styles are: {}",
            style.chars().take(40).collect::<String>(),
            fxpresets::names(BuiltinFxKind::Saturator)
        )
    })?;
    let t = project
        .track(ch.track)
        .ok_or_else(|| format!("mixer track {} does not exist", ch.track))?;
    let mut b = Built::default();
    let existing = builtins(t)
        .filter(|(_, _, fx)| fx.kind() == BuiltinFxKind::Saturator)
        .last();
    match existing {
        Some((_, instance, fx)) => {
            for (i, e) in fxpresets::edits(t.id, instance, fx, preset)
                .into_iter()
                .enumerate()
            {
                b.push(format!("drive: setting {i}"), e);
            }
        }
        None => {
            add_with(
                &mut b,
                ids,
                t,
                0,
                BuiltinFxKind::Saturator,
                Some(preset),
                "drive",
            )?;
        }
    }
    b.diff.push(format!("T{} drive {}", t.id, preset.name));
    b.predicted = ids.predicted.clone();
    Ok(b)
}

// ---- Duck to Kick -------------------------------------------------------------

/// Duck to Kick arguments. `row` or `track` is what gets ducked.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DuckArgs {
    /// The instrument to duck (its mixer track takes the effect).
    pub row: Option<ChannelId>,
    /// Or the mixer track to duck.
    pub track: Option<TrackId>,
    /// 0 (off) to 100 percent. Default 100.
    pub amount: Option<f64>,
    /// The instrument that triggers it; default: the row named Kick, else
    /// the first drum row.
    pub kick: Option<ChannelId>,
}

/// The compressor values for a duck of `amount` (0 to 1): a threshold low
/// enough that a kick triggers it, and a ratio that dips the sound by up to
/// 24 dB for a kick at -6 dB; fast attack, about 120 ms release.
pub fn duck_values(amount: f64) -> [f64; 7] {
    let depth = 24.0 * amount.clamp(0.0, 1.0);
    let threshold = -36.0;
    let ratio = (1.0 / (1.0 - depth / (-6.0 - threshold))).clamp(1.0, 20.0);
    [threshold, ratio, 0.1, 120.0, 6.0, 0.0, 1.0]
}

/// The row that triggers ducking: named Kick, else the first drum row (a
/// sampler or 808 instrument), skipping `not` and rows on `not_track`.
pub fn find_kick(project: &Project, not_track: TrackId) -> Option<ChannelId> {
    let ok = |c: &&std::sync::Arc<protocol::model::Channel>| c.track != not_track;
    let by_name =
        |c: &&std::sync::Arc<protocol::model::Channel>, w: &str| c.name.to_lowercase().contains(w);
    project
        .channels
        .iter()
        .filter(ok)
        .find(|c| by_name(c, "kick"))
        .or_else(|| {
            project
                .channels
                .iter()
                .filter(ok)
                .find(|c| by_name(c, "drum"))
        })
        .or_else(|| {
            project
                .channels
                .iter()
                .filter(ok)
                .find(|c| matches!(c.instrument, protocol::model::Instrument::Sampler(_)))
        })
        .map(|c| c.id)
}

/// The compressor on `t` that is keyed by `key`, if any.
pub fn duck_on(t: &Track, key: TrackId) -> Option<(InstanceId, &BuiltinFx)> {
    builtins(t).find_map(|(_, i, fx)| match fx {
        BuiltinFx::Compressor { sidechain, .. } if *sidechain == Some(key) => Some((i, fx)),
        _ => None,
    })
}

/// The duck on `t`, whatever keys it: its key track and amount (0 to 1).
pub fn duck_state(project: &Project, t: &Track) -> Option<(TrackId, f64)> {
    builtins(t).find_map(|(_, _, fx)| match fx {
        BuiltinFx::Compressor {
            sidechain: Some(k),
            params,
        } if project.track(*k).is_some() => {
            let ratio = params.get(CompressorParam::Ratio);
            let range = -6.0 - params.get(CompressorParam::ThresholdDb);
            let depth = (1.0 - 1.0 / ratio.max(1.0)) * range;
            Some((*k, (depth / 24.0).clamp(0.0, 1.0)))
        }
        _ => None,
    })
}

pub fn duck_to_kick(project: &Project, ids: &mut IdGen, a: &DuckArgs) -> BuildResult {
    let target = match (a.row, a.track) {
        (Some(r), None) => project
            .channel(r)
            .map(|c| c.track)
            .ok_or_else(|| format!("instrument {r} does not exist"))?,
        (None, Some(t)) => t,
        _ => return Err("name exactly one of row (an instrument) or track".into()),
    };
    if target == TrackId::MASTER {
        return Err("Main Output cannot duck: pick an instrument or a mixer track".into());
    }
    let t = project
        .track(target)
        .ok_or_else(|| format!("mixer track {target} does not exist"))?;
    let amount = a.amount.unwrap_or(100.0);
    if !(0.0..=100.0).contains(&amount) {
        return Err("amount is 0 to 100 (percent)".into());
    }
    let amount = amount / 100.0;
    let kick_ch = match a.kick {
        Some(k) => k,
        None => find_kick(project, target).ok_or_else(|| {
            "there is no Kick instrument to duck to: add one (or name it Kick), or give kick"
                .to_string()
        })?,
    };
    let key = project
        .channel(kick_ch)
        .map(|c| c.track)
        .ok_or_else(|| format!("instrument {kick_ch} does not exist"))?;
    if key == target {
        return Err("the Kick shares this mixer track: give the Kick its own track first".into());
    }
    let mut b = Built::default();
    let existing = duck_on(t, key).map(|(i, _)| i);
    if amount <= 0.0 {
        if let Some(instance) = existing {
            b.push(
                "duck: remove",
                Edit::RemoveInsert {
                    track: target,
                    instance,
                },
            );
        }
        b.diff.push(format!("T{target} duck off"));
        b.predicted = ids.predicted.clone();
        return Ok(b);
    }
    let values = duck_values(amount);
    let instance = match existing {
        Some(i) => i,
        None => {
            let i = add_with(&mut b, ids, t, 0, BuiltinFxKind::Compressor, None, "duck")?;
            b.push(
                "duck: key from the kick",
                Edit::SetSidechain {
                    track: target,
                    instance: i,
                    source: Some(key),
                },
            );
            i
        }
    };
    let have = existing.and_then(|i| {
        t.inserts.iter().find_map(|x| match x {
            Insert::Builtin { instance, fx } if *instance == i => Some(fx),
            _ => None,
        })
    });
    for (p, v) in values.iter().enumerate() {
        if have
            .and_then(|f| f.param(p))
            .is_none_or(|c| (c - v).abs() > 1e-12)
        {
            b.push(format!("duck: setting {p}"), set(target, instance, p, *v));
        }
    }
    b.diff.push(format!(
        "T{target} ducks to the kick on T{key} by {:.0}%",
        amount * 100.0
    ));
    b.predicted = ids.predicted.clone();
    Ok(b)
}

// ---- Loudness -----------------------------------------------------------------

/// Loudness arguments: an amount from 0 to 10, or one of Clean, Punchy,
/// Hard.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoudnessArgs {
    pub amount: Option<f64>,
    pub preset: Option<String>,
}

/// The Loudness presets and the amount each stands for.
pub const LOUDNESS_PRESETS: &[(&str, f64)] = &[("Clean", 3.0), ("Punchy", 6.0), ("Hard", 9.0)];

pub fn loudness_amount(a: &LoudnessArgs) -> Result<f64, String> {
    match (&a.amount, &a.preset) {
        (Some(v), None) if (0.0..=10.0).contains(v) => Ok(*v),
        (Some(_), None) => Err("amount is 0 to 10".into()),
        (None, Some(p)) => {
            let want: String = p
                .chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect();
            LOUDNESS_PRESETS
                .iter()
                .find(|(n, _)| want == n.to_lowercase() || (want == "hardphonk" && *n == "Hard"))
                .map(|(_, v)| *v)
                .ok_or_else(|| "the loudness presets are: Clean, Punchy, Hard (Phonk)".into())
        }
        _ => Err("give exactly one of amount (0 to 10) or preset".into()),
    }
}

/// The values of the three stages for `amount` 0 to 10: a gentle glue
/// compressor, a soft clipper and a limiter at -0.3 dB that sinks to -0.6 dB at 10, so the true peak (with the gaps between samples) stays under -0.3.
pub fn loudness_values(amount: f64) -> ([f64; 7], [f64; 4], [f64; 2]) {
    let u = (amount / 10.0).clamp(0.0, 1.0);
    let on = if amount > 0.0 { 1.0 } else { 0.0 };
    (
        // threshold, ratio, attack, release, knee, makeup, mix
        [
            -10.0 - 10.0 * u,
            1.0 + 2.5 * u,
            30.0,
            150.0,
            12.0,
            6.0 * u,
            on,
        ],
        // drive, tone, mix, output
        [2.0 + 10.0 * u, 16000.0, on, 0.0],
        // ceiling, release
        [-0.3 - 0.3 * u, 80.0 - 50.0 * u],
    )
}

/// The Loudness stages on `t`: a Compressor, a Saturator and a Limiter next
/// to each other, the last such run. Returns their positions.
pub fn loudness_chain(t: &Track) -> Option<(InstanceId, InstanceId, InstanceId)> {
    let b: Vec<_> = builtins(t).collect();
    b.windows(3).rev().find_map(|w| {
        let ok = w[1].0 == w[0].0 + 1
            && w[2].0 == w[1].0 + 1
            && w[0].2.kind() == BuiltinFxKind::Compressor
            && w[1].2.kind() == BuiltinFxKind::Saturator
            && w[2].2.kind() == BuiltinFxKind::Limiter;
        ok.then_some((w[0].1, w[1].1, w[2].1))
    })
}

/// The amount (0 to 10) the chain on `t` is set to, if there is one.
pub fn loudness_state(t: &Track) -> Option<f64> {
    let (_, s, _) = loudness_chain(t)?;
    let fx = builtins(t).find(|(_, i, _)| *i == s)?.2;
    let drive = fx.param(SaturatorParam::DriveDb.index())?;
    let on = fx.param(SaturatorParam::Mix.index())? > 0.0;
    Some(if on {
        (((drive - 2.0) / 10.0) * 10.0).clamp(0.0, 10.0)
    } else {
        0.0
    })
}

pub fn loudness(project: &Project, ids: &mut IdGen, a: &LoudnessArgs) -> BuildResult {
    let amount = loudness_amount(a)?;
    let t = project
        .track(TrackId::MASTER)
        .ok_or("the project has no Main Output")?;
    let mut b = Built::default();
    let (c, s, l) = match loudness_chain(t) {
        Some(x) => x,
        None => {
            if t.inserts.len() + 3 > MAX_INSERTS {
                return Err("Main Output needs room for three effects: remove some first".into());
            }
            let c = add_with(
                &mut b,
                ids,
                t,
                0,
                BuiltinFxKind::Compressor,
                None,
                "loudness",
            )?;
            let s = add_with(
                &mut b,
                ids,
                t,
                1,
                BuiltinFxKind::Saturator,
                None,
                "loudness",
            )?;
            let l = add_with(&mut b, ids, t, 2, BuiltinFxKind::Limiter, None, "loudness")?;
            b.push(
                "loudness: soft clip",
                Edit::SetSaturatorCurve {
                    track: TrackId::MASTER,
                    instance: s,
                    curve: SaturatorCurve::Soft,
                },
            );
            (c, s, l)
        }
    };
    let (cv, sv, lv) = loudness_values(amount);
    let m = TrackId::MASTER;
    let have = |i: InstanceId| builtins(t).find(|(_, x, _)| *x == i).map(|(_, _, fx)| fx);
    for (inst, vals) in [(c, &cv[..]), (s, &sv[..]), (l, &lv[..])] {
        for (p, v) in vals.iter().enumerate() {
            if have(inst)
                .and_then(|f| f.param(p))
                .is_none_or(|x| (x - v).abs() > 1e-12)
            {
                b.push(format!("loudness: setting {p}"), set(m, inst, p, *v));
            }
        }
    }
    let _ = LimiterParam::CeilingDb;
    b.diff.push(format!("Main Output loudness {amount:.1}"));
    b.predicted = ids.predicted.clone();
    Ok(b)
}
