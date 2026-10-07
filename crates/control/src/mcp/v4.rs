// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio clips, patterns, shapes and effect on/off (SPEC 20.7, 21.1,
//! 24.2-1): what the window does with drops, the Patterns lane, shape
//! lanes and the On switch, as tools. Same edits, so undo, authors and
//! history match.

use protocol::consts::PPQ;
use protocol::edit::{Edit, NewInstrument};
use protocol::ids::{ChannelId, ClipId, GroupId, InstanceId, ShapeId, TrackId};
use protocol::model::{Curve, Insert, Instrument, Project, SampleHash, ShapePoint, ShapeTarget};
use serde::Deserialize;
use serde_json::{Value, json};

use super::compose::{BuildResult, Built, ticks_of};
use super::ids::IdGen;
use super::notes;
use super::summary::quoted;
use crate::shapes::{self, Preset};

fn tpb(project: &Project) -> u32 {
    protocol::model::ticks_per_bar(project.time_sig_num)
}

/// A length in seconds as ticks at the project's tempo.
fn ticks_of_seconds(project: &Project, seconds: f64) -> u32 {
    (seconds * project.tempo_bpm / 60.0 * PPQ as f64)
        .round()
        .max(1.0) as u32
}

// ---- audio clips --------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioClipAddArgs {
    /// A sample hash already in the project.
    pub sample: String,
    /// An audio row; none: a new audio row with its own mixer track.
    pub instrument: Option<ChannelId>,
    pub start: Value,
    /// Bars.
    pub length: Option<Value>,
    /// Or the length in seconds.
    pub seconds: Option<f64>,
    /// Bars into the sample where the clip starts playing.
    pub offset: Option<Value>,
    pub name: Option<String>,
}

fn sample_name(project: &Project, hash: &str) -> String {
    project
        .samples
        .iter()
        .find(|s| s.hash == hash)
        .map(|s| {
            let n = &s.orig_name;
            n.rsplit_once('.')
                .map_or(n.as_str(), |(a, _)| a)
                .to_string()
        })
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "Audio".into())
}

pub fn audio_clip_add(project: &Project, ids: &mut IdGen, a: &AudioClipAddArgs) -> BuildResult {
    if !project.samples.iter().any(|s| s.hash == a.sample) {
        return Err(format!(
            "sample {} is not in the project; use a hash from inspect, or import it first",
            a.sample.chars().take(70).collect::<String>()
        ));
    }
    let hash = SampleHash::parse(&a.sample).ok_or("sample must be a 64-digit hash")?;
    let t = tpb(project);
    let start = ticks_of(&a.start, t, "start")?;
    let len = match (&a.length, a.seconds) {
        (Some(_), Some(_)) => return Err("give length or seconds, not both".into()),
        (Some(v), None) => ticks_of(v, t, "length")?,
        (None, Some(s)) if s > 0.0 => ticks_of_seconds(project, s),
        _ => {
            return Err(
                "give the clip length: length in bars, or seconds (the length of the whole sound)"
                    .into(),
            );
        }
    };
    let offset = match &a.offset {
        Some(v) => ticks_of(v, t, "offset")?,
        None => 0,
    };
    let mut b = Built::default();
    let name = a
        .name
        .clone()
        .unwrap_or_else(|| sample_name(project, &a.sample));
    let (row, own_row) = match a.instrument {
        Some(i) => {
            let ch = project
                .channel(i)
                .ok_or_else(|| format!("instrument {i} does not exist"))?;
            if !matches!(ch.instrument, Instrument::Audio) {
                return Err(format!(
                    "instrument {i} is not an audio row; leave instrument out to get a new audio row"
                ));
            }
            (i, false)
        }
        None => {
            b.push("its mixer track", Edit::AddTrack { name: name.clone() });
            let track = TrackId(ids.alloc());
            b.push(
                "the audio row",
                Edit::AddChannel {
                    name: name.clone(),
                    instrument: NewInstrument::Audio,
                    root_key: 60,
                    track,
                },
            );
            (ChannelId(ids.alloc()), true)
        }
    };
    b.push(
        "the audio clip",
        Edit::AddAudioClip {
            instrument: row,
            sample: hash,
            start,
            len,
            offset,
        },
    );
    let clip = ids.alloc();
    b.report_clips.push(ClipId(clip));
    b.diff.push(format!(
        "{}C{clip} audio {} on I{row} @{}+{}",
        if own_row { "new audio row, " } else { "" },
        quoted(&name),
        notes::fraction(start, t),
        notes::fraction(len, t)
    ));
    b.reply.insert("instrument".into(), json!(row.0));
    b.reply.insert("clip".into(), json!(clip));
    b.predicted = ids.predicted.clone();
    Ok(b)
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioClipSetArgs {
    pub clip: ClipId,
    /// Bars to cut off the start (negative: uncover earlier sound).
    pub trim_start: Option<Value>,
    /// Bars to cut off the end (negative: make it longer).
    pub trim_end: Option<Value>,
    pub gain_db: Option<f64>,
    /// Bars.
    pub fade_in: Option<Value>,
    pub fade_out: Option<Value>,
}

fn signed(v: &Value, t: u32, what: &str) -> Result<i64, String> {
    let s = match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.trim().to_string(),
        _ => return Err(format!("{what} must be a number of bars")),
    };
    match s.strip_prefix('-') {
        Some(rest) => Ok(-(ticks_of(&json!(rest), t, what)? as i64)),
        None => Ok(ticks_of(&json!(s), t, what)? as i64),
    }
}

pub fn audio_clip_set(project: &Project, a: &AudioClipSetArgs) -> BuildResult {
    let clip = project
        .clips
        .iter()
        .find(|c| c.id == a.clip)
        .ok_or_else(|| format!("clip {} does not exist", a.clip))?;
    let src = clip
        .audio
        .ok_or_else(|| format!("clip {} is not an audio clip", a.clip))?;
    let t = tpb(project);
    let mut b = Built::default();
    if let Some(v) = &a.trim_start {
        let d = signed(v, t, "trim_start")?;
        if d != 0 {
            b.push(
                "trim the start",
                Edit::ResizeClips {
                    clips: vec![clip.id],
                    dlen: -d,
                    from_start: true,
                },
            );
            b.diff.push(format!("C{} start trimmed", clip.id));
        }
    }
    if let Some(v) = &a.trim_end {
        let d = signed(v, t, "trim_end")?;
        if d != 0 {
            b.push(
                "trim the end",
                Edit::ResizeClips {
                    clips: vec![clip.id],
                    dlen: -d,
                    from_start: false,
                },
            );
            b.diff.push(format!("C{} end trimmed", clip.id));
        }
    }
    if a.gain_db.is_some() || a.fade_in.is_some() || a.fade_out.is_some() {
        let gain_mdb = match a.gain_db {
            Some(g) if (-100.0..=24.0).contains(&g) => (g * 1000.0).round() as i32,
            Some(_) => return Err("gain_db must be -100 to 24".into()),
            None => src.gain_mdb,
        };
        let fade = |v: &Option<Value>, old: u32, what: &str| match v {
            Some(v) => ticks_of(v, t, what),
            None => Ok(old),
        };
        let fade_in = fade(&a.fade_in, src.fade_in, "fade_in")?;
        let fade_out = fade(&a.fade_out, src.fade_out, "fade_out")?;
        b.push(
            "gain and fades",
            Edit::SetClipAudio {
                clip: clip.id,
                gain_mdb,
                fade_in,
                fade_out,
            },
        );
        b.diff.push(format!(
            "C{} gain {:+.1}dB, fade in {}, fade out {}",
            clip.id,
            gain_mdb as f64 / 1000.0,
            notes::fraction(fade_in, t),
            notes::fraction(fade_out, t)
        ));
    }
    if b.edits.is_empty() {
        return Err(
            "nothing to change: give trim_start, trim_end, gain_db, fade_in or fade_out".into(),
        );
    }
    b.report_clips = vec![clip.id];
    Ok(b)
}

// ---- patterns -----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatternMakeArgs {
    pub clips: Vec<ClipId>,
    pub name: String,
}

pub fn pattern_make(project: &Project, ids: &mut IdGen, a: &PatternMakeArgs) -> BuildResult {
    if a.clips.is_empty() {
        return Err("clips is empty: give the clip ids that make up the pattern".into());
    }
    for c in &a.clips {
        if !project.clips.iter().any(|x| x.id == *c) {
            return Err(format!("clip {c} does not exist"));
        }
    }
    let mut b = Built::default();
    b.push(
        "make the pattern",
        Edit::MakePattern {
            clips: a.clips.clone(),
            name: a.name.clone(),
        },
    );
    let g = ids.alloc();
    b.diff.push(format!(
        "pattern G{g} {} of {} clip(s)",
        quoted(&a.name),
        a.clips.len()
    ));
    b.reply.insert("pattern".into(), json!(g));
    b.report_clips = a.clips.clone();
    b.predicted = ids.predicted.clone();
    Ok(b)
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatternPlaceArgs {
    pub pattern: GroupId,
    pub start: Value,
}

pub fn pattern_place(project: &Project, ids: &mut IdGen, a: &PatternPlaceArgs) -> BuildResult {
    let g = project
        .groups
        .iter()
        .find(|g| g.id == a.pattern)
        .ok_or_else(|| {
            format!(
                "pattern {} does not exist; pattern_list shows them",
                a.pattern
            )
        })?;
    let members = project
        .clips
        .iter()
        .filter(|c| c.group.is_some_and(|x| x.group == g.id))
        .map(|c| c.group.map_or(0, |x| x.instance))
        .min();
    let Some(first) = members else {
        return Err("the pattern has no clips".into());
    };
    let n = project
        .clips
        .iter()
        .filter(|c| {
            c.group
                .is_some_and(|x| x.group == g.id && x.instance == first)
        })
        .count();
    let t = tpb(project);
    let start = ticks_of(&a.start, t, "start")?;
    let mut b = Built::default();
    b.push(
        "place the pattern",
        Edit::PlacePattern { group: g.id, start },
    );
    let made: Vec<u32> = (0..n).map(|_| ids.alloc()).collect();
    b.report_clips = made.iter().map(|c| ClipId(*c)).collect();
    b.diff.push(format!(
        "pattern G{} {} placed at bar {}",
        g.id,
        quoted(&g.name),
        notes::fraction(start, t)
    ));
    b.predicted = ids.predicted.clone();
    Ok(b)
}

/// `pattern_list`: every pattern, where it is placed and what is in it.
pub fn pattern_list(project: &Project) -> Value {
    let t = tpb(project);
    let list: Vec<Value> = project
        .groups
        .iter()
        .map(|g| {
            let mut places: Vec<u32> = project
                .clips
                .iter()
                .filter_map(|c| c.group.filter(|x| x.group == g.id).map(|x| x.instance))
                .collect();
            places.sort_unstable();
            places.dedup();
            let placed: Vec<Value> = places
                .iter()
                .map(|inst| {
                    let m: Vec<_> = project
                        .clips
                        .iter()
                        .filter(|c| c.group.is_some_and(|x| x.group == g.id && x.instance == *inst))
                        .collect();
                    let at = m.iter().map(|c| c.start).min().unwrap_or(0);
                    json!({"start": notes::fraction(at, t), "clips": m.iter().map(|c| c.id.0).collect::<Vec<_>>()})
                })
                .collect();
            json!({"pattern": g.id.0, "name": g.name, "placed": placed})
        })
        .collect();
    json!({"patterns": list, "units": "start is in bars; ids: G pattern, C clip"})
}

// ---- shapes -------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShapeTargetArg {
    /// volume, pan, pitch, filter or fx.
    pub kind: String,
    pub track: Option<TrackId>,
    pub instrument: Option<ChannelId>,
    pub insert: Option<InstanceId>,
    pub param: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointArg {
    pub at: Value,
    pub value: f64,
    pub curve: Option<Curve>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShapeAddArgs {
    pub target: ShapeTargetArg,
    pub preset: Option<String>,
    pub points: Option<Vec<PointArg>>,
    pub start: Option<Value>,
    pub end: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShapeSetArgs {
    pub shape: ShapeId,
    pub preset: Option<String>,
    pub points: Option<Vec<PointArg>>,
    pub start: Option<Value>,
    pub end: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShapeRemoveArgs {
    pub shapes: Vec<ShapeId>,
}

/// The target, checked against the project, and the range of its values.
fn resolve_target(
    project: &Project,
    a: &ShapeTargetArg,
) -> Result<(ShapeTarget, shapes::Range), String> {
    let need_track = || {
        let t = a.track.ok_or("this kind needs track")?;
        project
            .track(t)
            .map(|_| t)
            .ok_or(format!("mixer track {t} does not exist"))
    };
    let need_inst = || {
        let i = a.instrument.ok_or("this kind needs instrument")?;
        project
            .channel(i)
            .map(|_| i)
            .ok_or(format!("instrument {i} does not exist"))
    };
    let target = match a.kind.as_str() {
        "volume" => ShapeTarget::Volume {
            track: need_track()?,
        },
        "pan" => ShapeTarget::Pan {
            track: need_track()?,
        },
        "pitch" => ShapeTarget::Pitch {
            instrument: need_inst()?,
        },
        "filter" => ShapeTarget::Filter {
            instrument: need_inst()?,
        },
        "fx" => {
            let inst = a.insert.ok_or("kind fx needs insert (the effect's id)")?;
            let (track, ins) = project
                .tracks
                .iter()
                .find_map(|t| {
                    t.inserts
                        .iter()
                        .find(|i| i.instance() == inst)
                        .map(|i| (t.id, i))
                })
                .ok_or(format!("insert {inst} does not exist"))?;
            let Insert::Builtin { fx, .. } = ins else {
                return Err("a plugin effect has no shape; use a built-in effect".into());
            };
            let name = a
                .param
                .as_deref()
                .ok_or("kind fx needs param (the setting's name)")?;
            let param = (0..fx.param_count())
                .find(|i| fx.param_name(*i) == Some(name))
                .ok_or_else(|| {
                    format!(
                        "this effect has no setting \"{}\"; it has: {}",
                        name.chars().take(40).collect::<String>(),
                        (0..fx.param_count())
                            .filter_map(|i| fx.param_name(i))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?;
            let range = fx.param_range(param);
            let target = ShapeTarget::FxParam {
                track,
                instance: inst,
                param: param as u16,
            };
            return Ok((target, shapes::range_of(&target, range)));
        }
        other => {
            return Err(format!(
                "kind \"{}\" is not known: use volume, pan, pitch, filter or fx",
                other.chars().take(20).collect::<String>()
            ));
        }
    };
    Ok((target, shapes::range_of(&target, None)))
}

/// Points from a preset over a range, or from the given list.
fn points_for(
    project: &Project,
    range: shapes::Range,
    preset: &Option<String>,
    points: &Option<Vec<PointArg>>,
    start: &Option<Value>,
    end: &Option<Value>,
) -> Result<Vec<ShapePoint>, String> {
    let t = tpb(project);
    match (preset, points) {
        (Some(_), Some(_)) => Err("give preset or points, not both".into()),
        (None, None) => Err(format!("give preset ({}) or points", shapes::NAMES)),
        (Some(name), None) => {
            let p = Preset::parse(name).ok_or_else(|| {
                format!(
                    "no preset \"{}\"; the presets are: {}",
                    name.chars().take(30).collect::<String>(),
                    shapes::NAMES
                )
            })?;
            let l = project.loop_region;
            let (s, e) = match (start, end) {
                (Some(s), Some(e)) => (ticks_of(s, t, "start")?, ticks_of(e, t, "end")?),
                (None, None) if l.end > l.start => (l.start, l.end),
                (Some(s), None) => {
                    let s = ticks_of(s, t, "start")?;
                    (s, s + t)
                }
                _ => return Err("give start and end (bars), or set a loop region first".into()),
            };
            if e <= s {
                return Err("end must be after start".into());
            }
            Ok(shapes::preset_points(p, range, s, e))
        }
        (None, Some(list)) => {
            let (lo, hi) = (range.lo.min(range.hi) as f64, range.lo.max(range.hi) as f64);
            list.iter()
                .map(|p| {
                    if !(lo..=hi).contains(&p.value) {
                        return Err(format!(
                            "value {} is outside {lo}..{hi} for this shape",
                            p.value
                        ));
                    }
                    Ok(ShapePoint {
                        tick: ticks_of(&p.at, t, "at")?,
                        value: p.value as f32,
                        curve: p.curve.unwrap_or_default(),
                    })
                })
                .collect()
        }
    }
}

pub fn shape_add(project: &Project, ids: &mut IdGen, a: &ShapeAddArgs) -> BuildResult {
    let (target, range) = resolve_target(project, &a.target)?;
    let points = points_for(project, range, &a.preset, &a.points, &a.start, &a.end)?;
    let mut b = Built::default();
    let n = points.len();
    b.push("add the shape", Edit::AddShape { target, points });
    let id = ids.alloc();
    b.diff
        .push(format!("S{id} {} shape of {n} points", a.target.kind));
    b.reply.insert("shape".into(), json!(id));
    b.predicted = ids.predicted.clone();
    Ok(b)
}

pub fn shape_set(project: &Project, a: &ShapeSetArgs) -> BuildResult {
    let s = project
        .shapes
        .iter()
        .find(|s| s.id == a.shape)
        .ok_or_else(|| {
            format!(
                "shape {} does not exist; project_summary lists them as S<id>",
                a.shape
            )
        })?;
    let fx = match s.target {
        ShapeTarget::FxParam {
            track,
            instance,
            param,
        } => project
            .track(track)
            .and_then(|t| t.inserts.iter().find(|i| i.instance() == instance))
            .and_then(|i| match i {
                Insert::Builtin { fx, .. } => fx.param_range(param as usize),
                _ => None,
            }),
        _ => None,
    };
    let range = shapes::range_of(&s.target, fx);
    let points = points_for(project, range, &a.preset, &a.points, &a.start, &a.end)?;
    let mut b = Built::default();
    let n = points.len();
    b.push(
        "set the points",
        Edit::SetShapePoints {
            shape: s.id,
            points,
        },
    );
    b.diff.push(format!("S{} now {n} points", s.id));
    Ok(b)
}

pub fn shape_remove(project: &Project, a: &ShapeRemoveArgs) -> BuildResult {
    if a.shapes.is_empty() {
        return Err("shapes is empty: give shape ids (S<id> in project_summary)".into());
    }
    let mut b = Built::default();
    for id in &a.shapes {
        if !project.shapes.iter().any(|s| s.id == *id) {
            return Err(format!("shape {id} does not exist"));
        }
        b.push(
            format!("remove shape {id}"),
            Edit::RemoveShape { shape: *id },
        );
        b.diff.push(format!("- S{id}"));
    }
    Ok(b)
}

// ---- effect on/off ------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FxBypassArgs {
    pub insert: InstanceId,
    /// true: the effect is off (skipped); false: on.
    pub bypass: bool,
}

pub fn fx_bypass(project: &Project, a: &FxBypassArgs) -> BuildResult {
    let (track, ins) = project
        .tracks
        .iter()
        .find_map(|t| {
            t.inserts
                .iter()
                .find(|i| i.instance() == a.insert)
                .map(|i| (t.id, i))
        })
        .ok_or_else(|| format!("insert {} does not exist", a.insert))?;
    let Insert::Builtin { bypass, .. } = ins else {
        return Err("only built-in effects have an On switch here".into());
    };
    let mut b = Built::default();
    if *bypass != a.bypass {
        b.push(
            "switch the effect",
            Edit::SetInsertBypass {
                track,
                instance: a.insert,
                bypass: a.bypass,
            },
        );
        b.diff.push(format!(
            "insert {} {}",
            a.insert,
            if a.bypass { "off" } else { "on" }
        ));
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_clip_needs_a_known_sample() {
        let p = Project::empty();
        let mut ids = IdGen::new(&p, None);
        let a = AudioClipAddArgs {
            sample: "0".repeat(64),
            instrument: None,
            start: json!(0),
            length: Some(json!(2)),
            seconds: None,
            offset: None,
            name: None,
        };
        assert!(
            audio_clip_add(&p, &mut ids, &a)
                .unwrap_err()
                .contains("not in the project")
        );
    }

    #[test]
    fn seconds_follow_the_tempo() {
        let mut p = Project::empty();
        p.tempo_bpm = 120.0;
        assert_eq!(ticks_of_seconds(&p, 2.0), 3840);
    }
}
