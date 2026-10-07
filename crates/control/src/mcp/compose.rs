// SPDX-License-Identifier: GPL-3.0-or-later
//! Tool calls that need the project to become edits (SPEC 18.4, 20).
//!
//! Each builder reads one project snapshot and returns one batch: one
//! request, one undo group, sent with that snapshot's revision so a
//! concurrent change by the user makes it `Stale` instead of landing on
//! something the agent did not see. Builders that create an entity and use
//! it in the same batch (an instrument with its own track and a clip of
//! drums) predict the ids (`ids.rs`). Pure: no I/O.

use std::collections::BTreeMap;

use protocol::beats::{
    Bass808Param, BuiltinFx, BuiltinFxKind, SampleMode, SamplerParam, SaturatorCurve,
};
use protocol::consts::{MAX_STEPS, MAX_SWING, RATCHETS};
use protocol::edit::{Edit, MixValue, NewInstrument};
use protocol::ids::{ChannelId, ClipId, InstanceId, NoteId, PatternId, TrackId};
use protocol::model::{Insert, Instrument, Project, SynthParam, SynthParams, Wave, ticks_per_bar};
use serde::Deserialize;
use serde_json::{Value, json};

use super::build::{self, GridRow, Target};
use super::grid;
use super::ids::IdGen;
use super::notes;
use super::summary::quoted;

/// A batch ready to send.
#[derive(Debug, Default)]
pub struct Built {
    pub edits: Vec<Edit>,
    /// What each edit is for, in words, so an error can say which part of
    /// the call failed.
    pub labels: Vec<String>,
    pub diff: Vec<String>,
    /// Extra reply fields, with predicted ids filled in.
    pub reply: serde_json::Map<String, Value>,
    /// Ids the batch predicted (`IdGen::predicted`).
    pub predicted: Vec<u32>,
    /// Re-read the project afterwards and report the clips these ids (or
    /// the created ones) now are.
    pub report_clips: Vec<ClipId>,
}

impl Built {
    pub(crate) fn push(&mut self, label: impl Into<String>, e: Edit) {
        self.labels.push(label.into());
        self.edits.push(e);
    }
}

pub type BuildResult = Result<Built, String>;

// ---- units ------------------------------------------------------------------

/// A time in bars: a JSON number (`2`, `0.5`) or text like the note text
/// (`"1/4"`, `"240t"`). Negative values only where a move allows it.
fn ticks_of(v: &Value, tpb: u32, what: &str) -> Result<u32, String> {
    let s = match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => {
            return Err(format!(
                "{what} must be a number of bars (2, 0.5) or text like \"1/4\" or \"240t\""
            ));
        }
    };
    if s.trim_start().starts_with('-') {
        return Err(format!("{what} cannot be negative"));
    }
    notes::parse_ticks(&s, tpb, what)
}

/// A signed amount of bars, for moves.
fn signed_ticks_of(v: &Value, tpb: u32, what: &str) -> Result<i64, String> {
    let s = match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.trim().to_string(),
        _ => {
            return Err(format!(
                "{what} must be a number of bars or text like \"-1/4\""
            ));
        }
    };
    match s.strip_prefix('-') {
        Some(rest) => Ok(-(ticks_of(&json!(rest), tpb, what)? as i64)),
        None => Ok(ticks_of(&json!(s), tpb, what)? as i64),
    }
}

fn tpb(project: &Project) -> u32 {
    ticks_per_bar(project.time_sig_num)
}

// ---- instruments ------------------------------------------------------------

/// The `track` argument of `instruments_add`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum TrackChoice {
    Id(TrackId),
    Word(String),
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirstClip {
    pub start: Option<Value>,
    pub length: Option<Value>,
    pub grid: Option<String>,
    pub vel: Option<u8>,
    pub ratchet: Option<u8>,
    pub notes: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentArg {
    pub name: String,
    pub kind: Option<String>,
    /// Built-in synth settings merged over the defaults.
    pub synth: Option<Value>,
    pub plugin_id: Option<String>,
    /// A factory sound of the plugin, from `sounds` in the plugins tool.
    pub preset: Option<String>,
    /// A sample already in the project (its hash, see `inspect`).
    pub sample: Option<String>,
    pub mode: Option<SampleMode>,
    pub mono: Option<bool>,
    pub root_key: Option<u8>,
    pub track: Option<TrackChoice>,
    /// Copy the sound of this instrument (built-in kinds only).
    pub copy_of: Option<ChannelId>,
    pub clip: Option<FirstClip>,
}

/// Recursive object merge: `over` wins.
pub fn merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (slot, v) => *slot = v,
    }
}

/// Edits that give channel `id` the sound of instrument `src` (after it was
/// created with that instrument kind's defaults).
fn copy_sound(src: &Instrument, id: ChannelId) -> Vec<Edit> {
    match src {
        Instrument::Sampler(s) => {
            let mut v = vec![Edit::SetSamplerMode {
                channel: id,
                mode: s.mode,
                reverse: s.reverse,
            }];
            v.extend(SamplerParam::ALL.iter().map(|p| Edit::SetSamplerParam {
                channel: id,
                param: *p,
                value: s.params.get(*p),
            }));
            v
        }
        Instrument::Bass808(b) => Bass808Param::ALL
            .iter()
            .map(|p| Edit::SetBass808Param {
                channel: id,
                param: *p,
                value: b.params.get(*p),
            })
            .collect(),
        Instrument::Synth(_) | Instrument::Clap(_) => Vec::new(),
    }
}

/// The picker entry an instrument argument names, if any: its key and level
/// suit that sound.
fn sound_of(a: &InstrumentArg) -> Option<&'static plugin_host::sounds::Sound> {
    plugin_host::sounds::find(a.plugin_id.as_deref()?, a.preset.as_deref()?)
}

fn new_instrument(
    project: &Project,
    a: &InstrumentArg,
) -> Result<(NewInstrument, u8, Option<&'static str>), String> {
    if let Some(src) = a.copy_of {
        let ch = project
            .channel(src)
            .ok_or_else(|| format!("copy_of: instrument {src} does not exist"))?;
        let ni = match &ch.instrument {
            Instrument::Synth(p) => NewInstrument::Synth { params: *p },
            Instrument::Sampler(s) => NewInstrument::Sampler {
                sample: s.sample.clone(),
                mode: s.mode,
            },
            Instrument::Bass808(b) => NewInstrument::Bass808 { mono: b.mono },
            Instrument::Clap(_) => {
                return Err(format!(
                    "copy_of: instrument {src} is a plugin; its sound cannot be copied, add the plugin again with kind \"plugin\""
                ));
            }
        };
        return Ok((ni, a.root_key.unwrap_or(ch.root_key), None));
    }
    let kind = a.kind.as_deref().unwrap_or(if a.plugin_id.is_some() {
        "plugin"
    } else if a.sample.is_some() {
        "sampler"
    } else {
        "synth"
    });
    let only = |field: &str, ok: bool| {
        if ok {
            Ok(())
        } else {
            Err(format!("`{field}` does not apply to kind \"{kind}\""))
        }
    };
    only("synth", a.synth.is_none() || kind == "synth")?;
    only("plugin_id", a.plugin_id.is_none() || kind == "plugin")?;
    only("preset", a.preset.is_none() || kind == "plugin")?;
    only("sample", a.sample.is_none() || kind == "sampler")?;
    only("mode", a.mode.is_none() || kind == "sampler")?;
    only("mono", a.mono.is_none() || kind == "808")?;
    Ok(match kind {
        "synth" => {
            let mut v = serde_json::to_value(SynthParams::default()).expect("serializes");
            if let Some(o) = &a.synth {
                merge(&mut v, o.clone());
            }
            let params: SynthParams =
                serde_json::from_value(v).map_err(|e| format!("invalid synth settings: {e}"))?;
            (
                NewInstrument::Synth { params },
                a.root_key.unwrap_or(60),
                None,
            )
        }
        "808" => (
            NewInstrument::Bass808 {
                mono: a.mono.unwrap_or(true),
            },
            a.root_key.unwrap_or(36),
            None,
        ),
        "sampler" => {
            if let Some(h) = &a.sample
                && !project.samples.iter().any(|s| &s.hash == h)
            {
                return Err(format!(
                    "sample {} is not in the project; use kit_add for pack sounds, or a hash from inspect",
                    quoted(h)
                ));
            }
            (
                NewInstrument::Sampler {
                    sample: a.sample.clone(),
                    mode: a.mode.unwrap_or(SampleMode::OneShot),
                },
                a.root_key.unwrap_or(60),
                None,
            )
        }
        "plugin" => {
            let plugin_id = a
                .plugin_id
                .clone()
                .ok_or("kind \"plugin\" needs `plugin_id` (from the plugins tool)")?;
            (
                NewInstrument::Clap {
                    plugin_id,
                    preset: a.preset.clone(),
                },
                a.root_key.unwrap_or(sound_of(a).map_or(60, |s| s.note)),
                Some("instance"),
            )
        }
        other => {
            return Err(format!(
                "kind \"{}\" is not known: use synth, 808, sampler or plugin",
                other
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(20)
                    .collect::<String>()
            ));
        }
    })
}

/// A new content and its first clip, written in the same batch.
struct NewContent<'a> {
    content: u32,
    steps: u8,
    grid: Option<GridRow<'a>>,
    notes: Option<Vec<protocol::edit::NewNote>>,
    label: String,
}

/// Steps a new content needs for `grid` or `notes`, and the clip length.
fn plan_content(
    project: &Project,
    grid_text: Option<&str>,
    notes_text: Option<&str>,
) -> Result<(u8, Option<Vec<protocol::edit::NewNote>>), String> {
    let t = tpb(project);
    let step = protocol::consts::DEFAULT_STEP_TICKS;
    let per_bar = (t / step).max(1) as usize;
    let mut steps = per_bar.min(MAX_STEPS as usize);
    if let Some(g) = grid_text {
        let n = g.chars().filter(|c| *c != '|' && *c != ' ').count();
        if n > 0 {
            steps = n;
        }
    }
    let mut parsed = None;
    if let Some(text) = notes_text {
        let max = MAX_STEPS as u32 * step;
        let v = notes::parse_notes(text, t, max)?;
        let end = v.iter().map(|n| n.start + n.len).max().unwrap_or(0);
        let need = end.div_ceil(step) as usize;
        let bars = need.div_ceil(per_bar).max(1);
        steps = steps.max(bars * per_bar);
        parsed = Some(v);
    }
    if steps == 0 || steps > MAX_STEPS as usize {
        return Err(format!(
            "a content holds 1 to {MAX_STEPS} steps (at most 4 bars of sixteenths); this one needs {steps}: use several clips"
        ));
    }
    Ok((steps as u8, parsed))
}

/// Writes for contents created in this batch: length, steps and notes.
fn write_new_contents(
    project: &Project,
    b: &mut Built,
    made: Vec<NewContent>,
) -> Result<(), String> {
    let t = tpb(project);
    for c in made {
        let pid = PatternId(c.content);
        if c.steps != 16 {
            b.push(
                format!("{}: set its length", c.label),
                Edit::SetPatternLength {
                    pattern: pid,
                    length_steps: c.steps,
                },
            );
        }
        if let Some(row) = c.grid {
            let row = GridRow {
                pattern: pid,
                ..row
            };
            let empty = vec![None; c.steps as usize];
            let (e, d) = build::row_edits_and_diff(&row, c.steps as usize, &empty, "")
                .map_err(|m| format!("{}: {m}", c.label))?;
            for e in e {
                b.push(format!("{}: steps", c.label), e);
            }
            b.diff.extend(d);
        }
        if let Some(n) = c.notes {
            b.diff.push(build::added_line(pid, &n, t));
            b.push(
                format!("{}: notes", c.label),
                Edit::AddNotes {
                    pattern: pid,
                    notes: n,
                },
            );
        }
    }
    Ok(())
}

pub fn instruments_add(project: &Project, ids: &mut IdGen, list: &[InstrumentArg]) -> BuildResult {
    if list.is_empty() {
        return Err("instruments is empty: give at least one {name}".into());
    }
    let mut b = Built::default();
    let mut out = Vec::new();
    let mut later = Vec::new();
    for (i, a) in list.iter().enumerate() {
        let label = format!("instrument {i} {}", quoted(&a.name));
        let ctx = |m: String| format!("{label}: {m}");
        let (instrument, root_key, extra) = new_instrument(project, a).map_err(ctx)?;
        let track = match &a.track {
            None => None,
            Some(TrackChoice::Word(w)) if w == "new" => None,
            Some(TrackChoice::Word(w)) if w == "master" => Some(TrackId::MASTER),
            Some(TrackChoice::Word(_)) => {
                return Err(ctx(
                    "track must be a mixer track id, \"new\" (default: its own new track) or \"master\"".into(),
                ));
            }
            Some(TrackChoice::Id(t)) => {
                if project.track(*t).is_none() {
                    return Err(ctx(format!("mixer track {t} does not exist")));
                }
                Some(*t)
            }
        };
        let own_track = track.is_none();
        let track = match track {
            Some(t) => t,
            None => {
                b.push(
                    format!("{label}: its mixer track"),
                    Edit::AddTrack {
                        name: a.name.clone(),
                    },
                );
                TrackId(ids.alloc())
            }
        };
        b.push(
            format!("{label}: add"),
            Edit::AddChannel {
                name: a.name.clone(),
                instrument,
                root_key,
                track,
            },
        );
        let ch = ChannelId(ids.alloc());
        if extra.is_some() {
            ids.alloc();
        }
        if let Some(s) = sound_of(a)
            && own_track
            && s.gain_db != 0.0
        {
            b.push(
                format!("{label}: turn the loud sound down"),
                Edit::SetTrackMix {
                    track,
                    value: protocol::edit::MixValue::VolumeDb(s.gain_db),
                },
            );
        }
        if let Some(src) = a.copy_of.and_then(|s| project.channel(s)) {
            for e in copy_sound(&src.instrument, ch) {
                b.push(format!("{label}: copy the sound"), e);
            }
        }
        b.diff
            .push(format!("added I{ch} {} ->T{track}", quoted(&a.name)));
        out.push(
            json!({"name": super::summary::plain(&a.name), "instrument": ch.0, "track": track.0}),
        );
        if let Some(c) = &a.clip {
            later.push((out.len() - 1, ch, c, label.clone()));
        }
    }
    let t = tpb(project);
    let mut made = Vec::new();
    for (slot, ch, c, label) in later {
        let (steps, parsed) = plan_content(project, c.grid.as_deref(), c.notes.as_deref())
            .map_err(|m| format!("{label}: clip: {m}"))?;
        let start = match &c.start {
            Some(v) => ticks_of(v, t, "clip.start").map_err(|m| format!("{label}: {m}"))?,
            None => 0,
        };
        let len = match &c.length {
            Some(v) => ticks_of(v, t, "clip.length").map_err(|m| format!("{label}: {m}"))?,
            None => steps as u32 * protocol::consts::DEFAULT_STEP_TICKS,
        };
        b.push(
            format!("{label}: its first clip"),
            Edit::AddClip {
                instrument: ch,
                pattern: None,
                start,
                len,
            },
        );
        let content = ids.alloc();
        let clip = ids.alloc();
        b.report_clips.push(ClipId(clip));
        out[slot]["clip"] = json!(clip);
        out[slot]["content"] = json!(content);
        grid::check_row_args(c.vel, c.ratchet).map_err(|m| format!("{label}: {m}"))?;
        made.push(NewContent {
            content,
            steps,
            grid: c.grid.as_deref().map(|g| GridRow {
                pattern: PatternId(content),
                grid: g,
                vel: c.vel,
                ratchet: c.ratchet,
            }),
            notes: parsed,
            label: format!("{label}: clip"),
        });
    }
    write_new_contents(project, &mut b, made)?;
    b.reply.insert("instruments".into(), json!(out));
    b.predicted = ids.predicted.clone();
    Ok(b)
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaveArg {
    pub osc: u8,
    pub wave: Wave,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentSetArgs {
    pub instrument: ChannelId,
    pub name: Option<String>,
    pub root_key: Option<u8>,
    pub track: Option<TrackId>,
    pub choke_group: Option<u8>,
    /// Sound parameters by name; for plugins by parameter id.
    pub params: Option<BTreeMap<String, f64>>,
    pub wave: Option<Vec<WaveArg>>,
    pub mode: Option<SampleMode>,
    pub reverse: Option<bool>,
    pub mono: Option<bool>,
    pub sample: Option<Option<String>>,
}

/// Parameter names and ranges of an instrument, for error messages.
pub fn param_names(i: &Instrument) -> String {
    let list: Vec<String> = match i {
        Instrument::Synth(_) => SynthParam::ALL
            .iter()
            .map(|p| {
                let (lo, hi) = p.range();
                format!("{} {lo}..{hi}", serde_name(p))
            })
            .collect(),
        Instrument::Sampler(_) => SamplerParam::ALL
            .iter()
            .map(|p| {
                let (lo, hi) = p.range();
                format!("{} {lo}..{hi}", p.name())
            })
            .collect(),
        Instrument::Bass808(_) => Bass808Param::ALL
            .iter()
            .map(|p| {
                let (lo, hi) = p.range();
                format!("{} {lo}..{hi}", p.name())
            })
            .collect(),
        Instrument::Clap(_) => vec!["plugin parameter ids as text, for example \"12\"".into()],
    };
    list.join(", ")
}

fn serde_name<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn in_range(name: &str, v: f64, (lo, hi): (f64, f64)) -> Result<(), String> {
    if v.is_finite() && v >= lo && v <= hi {
        Ok(())
    } else {
        Err(format!("{name} = {v} is out of range {lo} to {hi}"))
    }
}

pub fn instrument_set(project: &Project, a: &InstrumentSetArgs) -> BuildResult {
    let id = a.instrument;
    let ch = project.channel(id).ok_or_else(|| {
        format!("instrument {id} does not exist; call project_summary for the ids")
    })?;
    let mut b = Built::default();
    let label = format!("instrument {id} {}", quoted(&ch.name));
    if let Some(n) = &a.name {
        b.push(
            format!("{label}: rename"),
            Edit::RenameChannel {
                channel: id,
                name: n.clone(),
            },
        );
        b.diff.push(format!("I{id} renamed to {}", quoted(n)));
    }
    if let Some(k) = a.root_key {
        b.push(
            format!("{label}: root key"),
            Edit::SetRootKey {
                channel: id,
                key: k,
            },
        );
        b.diff.push(format!("I{id} root {}", notes::key_name(k)));
    }
    if let Some(t) = a.track {
        b.push(
            format!("{label}: route"),
            Edit::SetChannelTrack {
                channel: id,
                track: t,
            },
        );
        b.diff.push(format!("I{id} ->T{t}"));
    }
    if let Some(g) = a.choke_group {
        b.push(
            format!("{label}: choke group"),
            Edit::SetChokeGroup {
                channel: id,
                group: g,
            },
        );
        b.diff.push(format!("I{id} choke group {g}"));
    }
    let kind_err = |what: &str| {
        format!(
            "{label}: `{what}` does not apply to a {} instrument",
            super::summary::instrument_kind(&ch.instrument)
        )
    };
    if let Some(waves) = &a.wave {
        if !matches!(ch.instrument, Instrument::Synth(_)) {
            return Err(kind_err("wave"));
        }
        for w in waves {
            b.push(
                format!("{label}: wave"),
                Edit::SetSynthWave {
                    channel: id,
                    osc: w.osc,
                    wave: w.wave,
                },
            );
            b.diff
                .push(format!("I{id} osc{} {}", w.osc, serde_name(&w.wave)));
        }
    }
    if a.mode.is_some() || a.reverse.is_some() {
        let Instrument::Sampler(s) = &ch.instrument else {
            return Err(kind_err("mode"));
        };
        b.push(
            format!("{label}: sample mode"),
            Edit::SetSamplerMode {
                channel: id,
                mode: a.mode.unwrap_or(s.mode),
                reverse: a.reverse.unwrap_or(s.reverse),
            },
        );
        b.diff.push(format!("I{id} sampler mode"));
    }
    if let Some(sample) = &a.sample {
        if !matches!(ch.instrument, Instrument::Sampler(_)) {
            return Err(kind_err("sample"));
        }
        b.push(
            format!("{label}: sample"),
            Edit::SetSamplerSample {
                channel: id,
                sample: sample.clone(),
            },
        );
        b.diff.push(format!("I{id} sample changed"));
    }
    if let Some(m) = a.mono {
        if !matches!(ch.instrument, Instrument::Bass808(_)) {
            return Err(kind_err("mono"));
        }
        b.push(
            format!("{label}: mono"),
            Edit::SetBass808Mono {
                channel: id,
                mono: m,
            },
        );
        b.diff.push(format!("I{id} mono {m}"));
    }
    for (name, value) in a.params.iter().flatten() {
        let value = *value;
        let unknown = || {
            format!(
                "{label}: no parameter \"{}\"; this instrument has: {}",
                name.chars()
                    .filter(|c| !c.is_control())
                    .take(40)
                    .collect::<String>(),
                param_names(&ch.instrument)
            )
        };
        let e = match &ch.instrument {
            Instrument::Synth(_) => {
                let p = SynthParam::ALL
                    .iter()
                    .find(|p| serde_name(*p) == *name)
                    .ok_or_else(unknown)?;
                in_range(name, value, p.range()).map_err(|m| format!("{label}: {m}"))?;
                Edit::SetSynthParam {
                    channel: id,
                    param: *p,
                    value,
                }
            }
            Instrument::Sampler(_) => {
                let p = SamplerParam::ALL
                    .iter()
                    .find(|p| p.name() == name)
                    .ok_or_else(unknown)?;
                in_range(name, value, p.range()).map_err(|m| format!("{label}: {m}"))?;
                Edit::SetSamplerParam {
                    channel: id,
                    param: *p,
                    value,
                }
            }
            Instrument::Bass808(_) => {
                let p = Bass808Param::ALL
                    .iter()
                    .find(|p| p.name() == name)
                    .ok_or_else(unknown)?;
                in_range(name, value, p.range()).map_err(|m| format!("{label}: {m}"))?;
                Edit::SetBass808Param {
                    channel: id,
                    param: *p,
                    value,
                }
            }
            Instrument::Clap(r) => {
                let param_id: u32 = name.parse().map_err(|_| unknown())?;
                Edit::SetPluginParam {
                    instance: r.instance,
                    param_id,
                    value,
                }
            }
        };
        b.push(format!("{label}: {name}"), e);
        b.diff.push(format!("I{id} {name} = {value}"));
    }
    if b.edits.is_empty() {
        return Err("nothing to change: give at least one of name, root_key, track, choke_group, params, wave, mode, reverse, mono, sample".into());
    }
    Ok(b)
}

// ---- mixer ----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum FxChoice {
    Builtin(BuiltinFxKind),
    Plugin { plugin_id: String },
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FxAddArgs {
    pub track: TrackId,
    pub fx: FxChoice,
    pub index: Option<u8>,
    /// A named setting for the new effect (see fxpresets).
    pub preset: Option<String>,
}

pub fn fx_add(project: &Project, ids: &mut IdGen, a: &FxAddArgs) -> BuildResult {
    let t = project
        .track(a.track)
        .ok_or_else(|| format!("mixer track {} does not exist", a.track))?;
    let index = a.index.unwrap_or(t.inserts.len() as u8);
    let mut b = Built::default();
    match &a.fx {
        FxChoice::Builtin(kind) => {
            let preset = match &a.preset {
                Some(name) => Some(crate::fxpresets::find(*kind, name).ok_or_else(|| {
                    format!(
                        "no sound \"{}\" for this effect; it has: {}",
                        name.chars().take(40).collect::<String>(),
                        crate::fxpresets::names(*kind)
                    )
                })?),
                None => None,
            };
            b.diff
                .push(format!("T{} + {} at {index}", a.track, serde_name(kind)));
            b.push(
                "add the effect",
                Edit::AddBuiltinInsert {
                    track: a.track,
                    index,
                    fx: *kind,
                },
            );
            if let Some(p) = preset {
                let instance = InstanceId(ids.alloc());
                for e in crate::fxpresets::edits(a.track, instance, &BuiltinFx::new(*kind), p) {
                    b.push("apply the sound", e);
                }
                b.diff.push(format!("insert {instance} sound {}", p.name));
            }
        }
        FxChoice::Plugin { plugin_id } => {
            if a.preset.is_some() {
                return Err("preset applies to a built-in effect only".into());
            }
            b.diff.push(format!(
                "T{} + plugin {} at {index}",
                a.track,
                quoted(plugin_id)
            ));
            b.push(
                "add the plugin",
                Edit::AddInsert {
                    track: a.track,
                    index,
                    plugin_id: plugin_id.clone(),
                },
            );
        }
    }
    b.predicted = ids.predicted.clone();
    Ok(b)
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FxSetArgs {
    pub insert: InstanceId,
    /// A named setting (see fxpresets); explicit params still win.
    pub preset: Option<String>,
    pub params: Option<BTreeMap<String, f64>>,
    pub curve: Option<SaturatorCurve>,
    pub ping_pong: Option<bool>,
    /// Compressor key input; `null` clears it.
    pub sidechain: Option<Option<TrackId>>,
    /// New position on the track.
    pub index: Option<u8>,
    pub remove: Option<bool>,
}

fn fx_param_names(fx: &BuiltinFx) -> String {
    (0..fx.param_count())
        .filter_map(|i| {
            let (lo, hi) = fx.param_range(i)?;
            Some(format!("{} {lo}..{hi}", fx.param_name(i)?))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn fx_set(project: &Project, a: &FxSetArgs) -> BuildResult {
    let (track, insert) = project
        .tracks
        .iter()
        .find_map(|t| t.inserts.iter().find(|i| i.instance() == a.insert).map(|i| (t.id, i)))
        .ok_or_else(|| {
            format!("insert {} does not exist; project_summary lists them as <insert id>:<kind> after fx", a.insert)
        })?;
    let mut b = Built::default();
    let instance = a.insert;
    let label = format!("insert {instance} on T{track}");
    if a.remove == Some(true) {
        b.push(
            format!("{label}: remove"),
            Edit::RemoveInsert { track, instance },
        );
        b.diff.push(format!("T{track} - insert {instance}"));
        return Ok(b);
    }
    match insert {
        Insert::Builtin { fx, .. } => {
            if let Some(name) = &a.preset {
                let p = crate::fxpresets::find(fx.kind(), name).ok_or_else(|| {
                    format!(
                        "{label}: no sound \"{}\"; this effect has: {}",
                        name.chars().take(40).collect::<String>(),
                        crate::fxpresets::names(fx.kind())
                    )
                })?;
                for e in crate::fxpresets::edits(track, instance, fx, p) {
                    b.push(format!("{label}: sound {}", p.name), e);
                }
                b.diff.push(format!("insert {instance} sound {}", p.name));
            }
            for (name, value) in a.params.iter().flatten() {
                let i = (0..fx.param_count())
                    .find(|i| fx.param_name(*i) == Some(name.as_str()))
                    .ok_or_else(|| {
                        format!(
                            "{label}: no parameter \"{}\"; it has: {}",
                            name.chars().take(40).collect::<String>(),
                            fx_param_names(fx)
                        )
                    })?;
                in_range(
                    name,
                    *value,
                    fx.param_range(i).unwrap_or((f64::MIN, f64::MAX)),
                )
                .map_err(|m| format!("{label}: {m}"))?;
                b.push(
                    format!("{label}: {name}"),
                    Edit::SetFxParam {
                        track,
                        instance,
                        param: i as u8,
                        value: *value,
                    },
                );
                b.diff.push(format!("insert {instance} {name} = {value}"));
            }
            if let Some(curve) = a.curve {
                if !matches!(fx, BuiltinFx::Saturator { .. }) {
                    return Err(format!("{label}: curve applies to a saturator only"));
                }
                b.push(
                    format!("{label}: curve"),
                    Edit::SetSaturatorCurve {
                        track,
                        instance,
                        curve,
                    },
                );
                b.diff
                    .push(format!("insert {instance} curve {}", serde_name(&curve)));
            }
            if let Some(pp) = a.ping_pong {
                if !matches!(fx, BuiltinFx::Delay { .. }) {
                    return Err(format!("{label}: ping_pong applies to a delay only"));
                }
                b.push(
                    format!("{label}: ping pong"),
                    Edit::SetDelayPingPong {
                        track,
                        instance,
                        ping_pong: pp,
                    },
                );
                b.diff.push(format!("insert {instance} ping_pong {pp}"));
            }
            if let Some(src) = a.sidechain {
                if !matches!(fx, BuiltinFx::Compressor { .. }) {
                    return Err(format!("{label}: sidechain applies to a compressor only"));
                }
                b.push(
                    format!("{label}: sidechain"),
                    Edit::SetSidechain {
                        track,
                        instance,
                        source: src,
                    },
                );
                b.diff.push(match src {
                    Some(s) => format!("insert {instance} keyed by T{s}"),
                    None => format!("insert {instance} sidechain off"),
                });
            }
        }
        Insert::Clap(r) => {
            if a.curve.is_some() || a.ping_pong.is_some() || a.sidechain.is_some() {
                return Err(format!(
                    "{label}: a plugin insert only takes params (by parameter id), index and remove"
                ));
            }
            for (name, value) in a.params.iter().flatten() {
                let param_id: u32 = name.parse().map_err(|_| {
                    format!("{label}: plugin parameters are named by id, for example \"12\"")
                })?;
                b.push(
                    format!("{label}: param {param_id}"),
                    Edit::SetPluginParam {
                        instance: r.instance,
                        param_id,
                        value: *value,
                    },
                );
                b.diff
                    .push(format!("insert {instance} param {param_id} = {value}"));
            }
        }
    }
    if let Some(index) = a.index {
        b.push(
            format!("{label}: move"),
            Edit::MoveInsert {
                track,
                instance,
                index,
            },
        );
        b.diff.push(format!("insert {instance} moved to {index}"));
    }
    if b.edits.is_empty() {
        return Err(
            "nothing to change: give params, curve, ping_pong, sidechain, index or remove".into(),
        );
    }
    Ok(b)
}

// ---- clips ------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipArg {
    pub instrument: ChannelId,
    pub start: Value,
    pub length: Option<Value>,
    /// Existing content of the same instrument: a linked copy.
    pub content: Option<PatternId>,
    pub grid: Option<String>,
    pub vel: Option<u8>,
    pub ratchet: Option<u8>,
    pub notes: Option<String>,
}

pub fn clips_add(project: &Project, ids: &mut IdGen, list: &[ClipArg]) -> BuildResult {
    if list.is_empty() {
        return Err("clips is empty: give at least one {instrument, start}".into());
    }
    let t = tpb(project);
    let mut b = Built::default();
    let mut made = Vec::new();
    let mut out = Vec::new();
    for (i, c) in list.iter().enumerate() {
        let label = format!("clip {i} (instrument {})", c.instrument);
        let ctx = |m: String| format!("{label}: {m}");
        let ch = project.channel(c.instrument).ok_or_else(|| {
            ctx("the instrument does not exist; call project_summary for the ids".into())
        })?;
        let start = ticks_of(&c.start, t, "start").map_err(ctx)?;
        let (content, steps, parsed) = match c.content {
            Some(p) => {
                if c.grid.is_some() || c.notes.is_some() {
                    return Err(ctx("give either `content` (a linked copy) or `grid`/`notes` for new content, not both; edit linked content with beat_grid_set or notes_write".into()));
                }
                let pat = project
                    .pattern(p)
                    .ok_or_else(|| ctx(format!("content {p} does not exist")))?;
                if pat.instrument != c.instrument {
                    return Err(ctx(format!(
                        "content {p} belongs to instrument {}; a clip plays content of its own row only (move it with clips_change instrument)",
                        pat.instrument
                    )));
                }
                (Some(p), pat.length_steps, None)
            }
            None => {
                let (steps, parsed) =
                    plan_content(project, c.grid.as_deref(), c.notes.as_deref()).map_err(ctx)?;
                (None, steps, parsed)
            }
        };
        let natural = match content.and_then(|p| project.pattern(p)) {
            Some(p) => p.length_ticks(),
            None => steps as u32 * protocol::consts::DEFAULT_STEP_TICKS,
        };
        let len = match &c.length {
            Some(v) => ticks_of(v, t, "length").map_err(ctx)?,
            None => natural,
        };
        b.push(
            format!("{label}: add"),
            Edit::AddClip {
                instrument: c.instrument,
                pattern: content,
                start,
                len,
            },
        );
        let new_content = content.is_none().then(|| ids.alloc());
        let clip = ids.alloc();
        b.report_clips.push(ClipId(clip));
        let pid = content.map_or_else(|| new_content.unwrap_or(0), |p| p.0);
        b.diff.push(format!(
            "C{clip} on I{} {} @{}+{} plays P{pid}{}",
            c.instrument,
            quoted(&ch.name),
            notes::fraction(start, t),
            notes::fraction(len, t),
            if content.is_some() {
                " (linked)"
            } else {
                " (new)"
            }
        ));
        out.push(json!({"clip": clip, "content": pid, "instrument": c.instrument.0}));
        if let Some(nc) = new_content {
            if c.grid.is_some() || c.vel.is_some() || c.ratchet.is_some() {
                grid::check_row_args(c.vel, c.ratchet).map_err(ctx)?;
            }
            made.push(NewContent {
                content: nc,
                steps,
                grid: c.grid.as_deref().map(|g| GridRow {
                    pattern: PatternId(nc),
                    grid: g,
                    vel: c.vel,
                    ratchet: c.ratchet,
                }),
                notes: parsed,
                label: label.clone(),
            });
        }
    }
    write_new_contents(project, &mut b, made)?;
    b.reply.insert("clips".into(), json!(out));
    b.predicted = ids.predicted.clone();
    Ok(b)
}

fn find_clips(project: &Project, clips: &[ClipId]) -> Result<Vec<protocol::model::Clip>, String> {
    if clips.is_empty() {
        return Err("clips is empty: give clip ids (C<id> in project_summary)".into());
    }
    clips
        .iter()
        .map(|id| {
            project
                .clips
                .iter()
                .find(|c| c.id == *id)
                .copied()
                .ok_or_else(|| {
                    format!("clip {id} does not exist; call project_summary for the clip ids")
                })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipsCopyArgs {
    pub clips: Vec<ClipId>,
    /// Where the earliest copy starts; default right after the selection.
    pub to: Option<Value>,
    pub times: Option<u32>,
    /// Linked (default): copies share content. False: independent copies.
    pub linked: Option<bool>,
}

pub fn clips_copy(project: &Project, a: &ClipsCopyArgs) -> BuildResult {
    let found = find_clips(project, &a.clips)?;
    let t = tpb(project);
    let first = found.iter().map(|c| c.start).min().unwrap_or(0) as i64;
    let last = found.iter().map(|c| c.end()).max().unwrap_or(0) as i64;
    let dt = match &a.to {
        Some(v) => ticks_of(v, t, "to")? as i64 - first,
        None => last - first,
    };
    if dt == 0 {
        return Err(
            "the copy would land on the clips themselves: give `to` (where the first copy starts)"
                .into(),
        );
    }
    let times = a.times.unwrap_or(1);
    if !(1..=64).contains(&times) {
        return Err("times must be 1 to 64".into());
    }
    let linked = a.linked.unwrap_or(true);
    let mut b = Built::default();
    for k in 1..=times as i64 {
        b.push(
            format!("copy {k}"),
            Edit::DuplicateClips {
                clips: a.clips.clone(),
                dt: dt * k,
                linked,
            },
        );
    }
    b.diff.push(format!(
        "{} clip(s) copied {times}x every {} bar(s){}",
        a.clips.len(),
        notes::fraction(dt.unsigned_abs() as u32, t),
        if linked {
            " (linked)"
        } else {
            " (independent copies)"
        }
    ));
    Ok(b)
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipsChangeArgs {
    pub clips: Vec<ClipId>,
    pub move_by: Option<Value>,
    pub move_to: Option<Value>,
    pub length: Option<Value>,
    pub resize_by: Option<Value>,
    #[serde(default)]
    pub from_start: bool,
    pub muted: Option<bool>,
    #[serde(default)]
    pub make_unique: bool,
    /// Move the clip (one only) to another instrument's row.
    pub instrument: Option<ChannelId>,
}

pub fn clips_change(project: &Project, a: &ClipsChangeArgs) -> BuildResult {
    let found = find_clips(project, &a.clips)?;
    let t = tpb(project);
    let mut b = Built::default();
    let first = found.iter().map(|c| c.start).min().unwrap_or(0) as i64;
    let dt = match (&a.move_by, &a.move_to) {
        (Some(_), Some(_)) => return Err("give move_by or move_to, not both".into()),
        (Some(v), None) => signed_ticks_of(v, t, "move_by")?,
        (None, Some(v)) => ticks_of(v, t, "move_to")? as i64 - first,
        (None, None) => 0,
    };
    if dt != 0 {
        b.push(
            "move",
            Edit::MoveClips {
                clips: a.clips.clone(),
                dt,
            },
        );
        b.diff.push(format!(
            "moved {} clip(s) by {}{} bar(s)",
            a.clips.len(),
            if dt < 0 { "-" } else { "" },
            notes::fraction(dt.unsigned_abs() as u32, t)
        ));
    }
    match (&a.length, &a.resize_by) {
        (Some(_), Some(_)) => return Err("give length or resize_by, not both".into()),
        (Some(v), None) => {
            let len = ticks_of(v, t, "length")? as i64;
            for c in &found {
                if len != c.len as i64 {
                    b.push(
                        format!("resize clip {}", c.id),
                        Edit::ResizeClips {
                            clips: vec![c.id],
                            dlen: len - c.len as i64,
                            from_start: a.from_start,
                        },
                    );
                }
            }
            b.diff.push(format!(
                "{} clip(s) now {} bar(s) long",
                a.clips.len(),
                notes::fraction(len as u32, t)
            ));
        }
        (None, Some(v)) => {
            let dlen = signed_ticks_of(v, t, "resize_by")?;
            b.push(
                "resize",
                Edit::ResizeClips {
                    clips: a.clips.clone(),
                    dlen,
                    from_start: a.from_start,
                },
            );
            b.diff.push(format!("{} clip(s) resized", a.clips.len()));
        }
        (None, None) => {}
    }
    if let Some(m) = a.muted {
        b.push(
            "mute",
            Edit::SetClipMuted {
                clips: a.clips.clone(),
                muted: m,
            },
        );
        b.diff.push(format!(
            "{} clip(s) {}",
            a.clips.len(),
            if m { "muted" } else { "unmuted" }
        ));
    }
    if let Some(i) = a.instrument {
        let [c] = found.as_slice() else {
            return Err("instrument moves one clip at a time: give exactly one clip".into());
        };
        if project.channel(i).is_none() {
            return Err(format!("instrument {i} does not exist"));
        }
        b.push(
            "move to another instrument",
            Edit::MoveClipToInstrument {
                clip: c.id,
                instrument: i,
            },
        );
        b.diff.push(format!(
            "C{} moved to I{i} (its content is copied for that instrument)",
            c.id
        ));
    }
    if a.make_unique {
        for c in &found {
            b.push(
                format!("make clip {} unique", c.id),
                Edit::MakeUnique { clip: c.id },
            );
        }
        b.diff
            .push(format!("{} clip(s) got their own content", a.clips.len()));
    }
    if b.edits.is_empty() {
        return Err("nothing to change: give move_by, move_to, length, resize_by, muted, make_unique or instrument".into());
    }
    b.report_clips = a.clips.clone();
    Ok(b)
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipsSplitArgs {
    pub clip: ClipId,
    /// Song positions inside the clip, in bars.
    pub at: Vec<Value>,
}

pub fn clips_split(project: &Project, a: &ClipsSplitArgs) -> BuildResult {
    let c = find_clips(project, &[a.clip])?[0];
    let t = tpb(project);
    let mut points =
        a.at.iter()
            .map(|v| ticks_of(v, t, "at"))
            .collect::<Result<Vec<_>, _>>()?;
    if points.is_empty() {
        return Err("at is empty: give the song positions to split at".into());
    }
    points.sort_unstable();
    points.dedup();
    let mut b = Built::default();
    for p in points.iter().rev() {
        if *p <= c.start || *p >= c.end() {
            return Err(format!(
                "split point {} is not inside clip {} ({}..{})",
                notes::fraction(*p, t),
                c.id,
                notes::fraction(c.start, t),
                notes::fraction(c.end(), t)
            ));
        }
        b.push(
            format!("split at {}", notes::fraction(*p, t)),
            Edit::SplitClip { clip: c.id, at: *p },
        );
    }
    b.diff
        .push(format!("C{} split into {} clips", c.id, points.len() + 1));
    b.report_clips = vec![c.id];
    Ok(b)
}

// ---- contents -----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowArg {
    pub clip: Option<ClipId>,
    pub content: Option<PatternId>,
    pub instrument: Option<ChannelId>,
    pub grid: String,
    pub vel: Option<u8>,
    pub ratchet: Option<u8>,
}

impl RowArg {
    pub fn target(&self) -> Target {
        Target {
            clip: self.clip,
            content: self.content,
            instrument: self.instrument,
        }
    }
}

pub fn grid_set(project: &Project, rows: &[RowArg]) -> BuildResult {
    if rows.is_empty() {
        return Err(
            "rows is empty: give at least one {clip or content or instrument, grid}".into(),
        );
    }
    let mut resolved = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        grid::check_row_args(r.vel, r.ratchet).map_err(|m| format!("row {i}: {m}"))?;
        let pattern = build::resolve(project, r.target()).map_err(|m| format!("row {i}: {m}"))?;
        resolved.push(GridRow {
            pattern,
            grid: &r.grid,
            vel: r.vel,
            ratchet: r.ratchet,
        });
    }
    let (edits, diff) = build::grid_edits(project, &resolved)?;
    let mut b = Built::default();
    for e in edits {
        b.push("steps", e);
    }
    b.diff = diff;
    Ok(b)
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotesPart {
    pub clip: Option<ClipId>,
    pub content: Option<PatternId>,
    pub instrument: Option<ChannelId>,
    pub notes: String,
    #[serde(default)]
    pub replace: bool,
}

impl NotesPart {
    pub fn target(&self) -> Target {
        Target {
            clip: self.clip,
            content: self.content,
            instrument: self.instrument,
        }
    }
}

pub fn notes_write(project: &Project, parts: &[NotesPart]) -> BuildResult {
    if parts.is_empty() {
        return Err(
            "parts is empty: give at least one {clip or content or instrument, notes}".into(),
        );
    }
    let mut b = Built::default();
    for (i, p) in parts.iter().enumerate() {
        let pattern = build::resolve(project, p.target()).map_err(|m| format!("part {i}: {m}"))?;
        let (edits, diff) = build::notes_edits(project, pattern, &p.notes, p.replace)
            .map_err(|m| format!("part {i}: {m}"))?;
        for e in edits {
            b.push(format!("part {i}"), e);
        }
        b.diff.extend(diff);
    }
    Ok(b)
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum NoteSelection {
    All(String),
    Ids(Vec<NoteId>),
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotesEditArgs {
    pub clip: Option<ClipId>,
    pub content: Option<PatternId>,
    pub instrument: Option<ChannelId>,
    pub notes: NoteSelection,
    pub move_by: Option<Value>,
    pub transpose: Option<i16>,
    pub resize_by: Option<Value>,
    pub velocity: Option<u8>,
    pub quantize: Option<Value>,
    #[serde(default)]
    pub remove: bool,
}

pub fn notes_edit(project: &Project, a: &NotesEditArgs) -> BuildResult {
    let pid = build::resolve(
        project,
        Target {
            clip: a.clip,
            content: a.content,
            instrument: a.instrument,
        },
    )?;
    let pat = project.pattern(pid).expect("resolved");
    let t = tpb(project);
    let ids: Vec<NoteId> = match &a.notes {
        NoteSelection::All(w) if w == "all" => pat.notes.iter().map(|n| n.id).collect(),
        NoteSelection::All(_) => {
            return Err("notes must be a list of note ids (from content_get) or \"all\"".into());
        }
        NoteSelection::Ids(v) => {
            for id in v {
                if !pat.notes.iter().any(|n| n.id == *id) {
                    return Err(format!(
                        "note {id} is not in content {pid}; content_get lists the note ids"
                    ));
                }
            }
            v.clone()
        }
    };
    if ids.is_empty() {
        return Err(format!("content {pid} has no notes to change"));
    }
    let mut b = Built::default();
    let n = ids.len();
    if a.remove {
        b.push(
            "remove",
            Edit::RemoveNotes {
                pattern: pid,
                notes: ids,
            },
        );
        b.diff.push(format!("P{pid}: removed {n} note(s)"));
        return Ok(b);
    }
    if let Some(q) = &a.quantize {
        let grid = ticks_of(q, t, "quantize")?;
        if grid == 0 {
            return Err("quantize needs a grid like \"1/16\"".into());
        }
        let mut moved = 0;
        for id in &ids {
            let note = pat.notes.iter().find(|x| x.id == *id).expect("checked");
            let target = ((note.start + grid / 2) / grid) * grid;
            let target = if target >= pat.length_ticks() {
                note.start - note.start % grid
            } else {
                target
            };
            if target != note.start {
                moved += 1;
                b.push(
                    format!("quantize note {id}"),
                    Edit::MoveNotes {
                        pattern: pid,
                        notes: vec![*id],
                        dt: target as i64 - note.start as i64,
                        dkey: 0,
                    },
                );
            }
        }
        b.diff.push(format!(
            "P{pid}: quantized {moved} of {n} note(s) to {}",
            notes::fraction(grid, t)
        ));
    }
    let dt = a
        .move_by
        .as_ref()
        .map(|v| signed_ticks_of(v, t, "move_by"))
        .transpose()?
        .unwrap_or(0);
    let dkey = a.transpose.unwrap_or(0);
    if dt != 0 || dkey != 0 {
        b.push(
            "move",
            Edit::MoveNotes {
                pattern: pid,
                notes: ids.clone(),
                dt,
                dkey,
            },
        );
        b.diff.push(format!(
            "P{pid}: moved {n} note(s){}{}",
            if dt != 0 {
                format!(
                    " by {}{} bar",
                    if dt < 0 { "-" } else { "" },
                    notes::fraction(dt.unsigned_abs() as u32, t)
                )
            } else {
                String::new()
            },
            if dkey != 0 {
                format!(" {dkey:+} semitones")
            } else {
                String::new()
            }
        ));
    }
    if let Some(v) = &a.resize_by {
        let dlen = signed_ticks_of(v, t, "resize_by")?;
        b.push(
            "resize",
            Edit::ResizeNotes {
                pattern: pid,
                notes: ids.clone(),
                dlen,
            },
        );
        b.diff.push(format!("P{pid}: resized {n} note(s)"));
    }
    if let Some(vel) = a.velocity {
        if !(1..=127).contains(&vel) {
            return Err("velocity must be 1 to 127".into());
        }
        b.push(
            "velocity",
            Edit::SetNoteVelocity {
                pattern: pid,
                notes: ids,
                vel,
            },
        );
        b.diff
            .push(format!("P{pid}: {n} note(s) at velocity {vel}"));
    }
    if b.edits.is_empty() {
        return Err(
            "nothing to change: give move_by, transpose, resize_by, velocity, quantize or remove"
                .into(),
        );
    }
    Ok(b)
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentSetArgs {
    pub clip: Option<ClipId>,
    pub content: Option<PatternId>,
    pub instrument: Option<ChannelId>,
    pub name: Option<String>,
    pub steps: Option<u8>,
    pub step_length: Option<Value>,
    pub swing: Option<u16>,
    /// Pitch lane, velocity or ratchet of single steps.
    pub lanes: Option<Vec<LaneArg>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneArg {
    pub step: u8,
    pub vel: Option<u8>,
    pub pitch: Option<i8>,
    pub ratchet: Option<u8>,
}

pub fn content_set(project: &Project, a: &ContentSetArgs) -> BuildResult {
    let pid = build::resolve(
        project,
        Target {
            clip: a.clip,
            content: a.content,
            instrument: a.instrument,
        },
    )?;
    let t = tpb(project);
    let mut b = Built::default();
    if let Some(n) = &a.name {
        b.push(
            "rename",
            Edit::RenamePattern {
                pattern: pid,
                name: n.clone(),
            },
        );
        b.diff.push(format!("P{pid} renamed to {}", quoted(n)));
    }
    if let Some(v) = &a.step_length {
        let st = ticks_of(v, t, "step_length")?;
        b.push(
            "step length",
            Edit::SetStepTicks {
                pattern: pid,
                step_ticks: st,
            },
        );
        b.diff
            .push(format!("P{pid} steps are {} bar", notes::fraction(st, t)));
    }
    if let Some(s) = a.steps {
        b.push(
            "length",
            Edit::SetPatternLength {
                pattern: pid,
                length_steps: s,
            },
        );
        b.diff.push(format!("P{pid} has {s} steps"));
    }
    if let Some(s) = a.swing {
        if s > MAX_SWING {
            return Err(format!(
                "swing must be 0 to {MAX_SWING} (thousandths of a step)"
            ));
        }
        b.push(
            "swing",
            Edit::SetSwing {
                pattern: pid,
                swing: s,
            },
        );
        b.diff.push(format!("P{pid} swing {s}"));
    }
    for l in a.lanes.iter().flatten() {
        if let Some(r) = l.ratchet
            && !RATCHETS.contains(&r)
        {
            return Err(format!(
                "step {}: ratchet must be one of 1, 2, 3, 4, 6, 8",
                l.step
            ));
        }
        b.push(
            format!("step {} lanes", l.step),
            Edit::SetStepLanes {
                pattern: pid,
                step: l.step,
                vel: l.vel,
                off: l.pitch,
                repeat: l.ratchet,
            },
        );
        b.diff.push(format!("P{pid} step {} lanes", l.step));
    }
    if b.edits.is_empty() {
        return Err("nothing to change: give name, steps, step_length, swing or lanes".into());
    }
    Ok(b)
}

// ---- song -----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopArgs {
    pub start: Option<Value>,
    pub end: Option<Value>,
    pub clip: Option<ClipId>,
    pub enabled: Option<bool>,
}

pub fn loop_set(project: &Project, a: &LoopArgs) -> BuildResult {
    let t = tpb(project);
    let cur = project.loop_region;
    let (start, end) = match a.clip {
        Some(id) => {
            if a.start.is_some() || a.end.is_some() {
                return Err("give clip, or start and end, not both".into());
            }
            let c = find_clips(project, &[id])?[0];
            (c.start, c.end())
        }
        None => {
            let start = a
                .start
                .as_ref()
                .map(|v| ticks_of(v, t, "start"))
                .transpose()?
                .unwrap_or(cur.start);
            let end = match &a.end {
                Some(v) => ticks_of(v, t, "end")?,
                None if cur.end > start => cur.end,
                None => project
                    .clips
                    .iter()
                    .map(|c| c.end())
                    .max()
                    .unwrap_or(0)
                    .max(start + t),
            };
            (start, end)
        }
    };
    if end <= start {
        return Err(format!(
            "the loop end ({}) must be after its start ({})",
            notes::fraction(end, t),
            notes::fraction(start, t)
        ));
    }
    let enabled = a.enabled.unwrap_or(true);
    let mut b = Built::default();
    b.push(
        "loop",
        Edit::SetLoopRegion {
            start,
            end,
            enabled,
        },
    );
    b.diff.push(format!(
        "loop {}..{} {}",
        notes::fraction(start, t),
        notes::fraction(end, t),
        if enabled { "on" } else { "off" }
    ));
    Ok(b)
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SongArgs {
    pub tempo: Option<f64>,
    pub beats_per_bar: Option<u8>,
    pub metronome: Option<bool>,
    pub metronome_db: Option<f64>,
}

pub fn song_set(project: &Project, a: &SongArgs) -> BuildResult {
    let mut b = Built::default();
    if let Some(bpm) = a.tempo {
        b.push("tempo", Edit::SetTempo { bpm });
        b.diff.push(format!("tempo {bpm} BPM"));
    }
    if let Some(num) = a.beats_per_bar {
        b.push("time signature", Edit::SetTimeSigNum { num });
        b.diff.push(format!("{num}/4"));
    }
    if a.metronome.is_some() || a.metronome_db.is_some() {
        let enabled = a.metronome.unwrap_or(project.metronome.enabled);
        let gain_db = a.metronome_db.unwrap_or(project.metronome.gain_db);
        b.push("metronome", Edit::SetMetronome { enabled, gain_db });
        b.diff.push(format!(
            "metronome {} {gain_db:+.1}dB",
            if enabled { "on" } else { "off" }
        ));
    }
    if b.edits.is_empty() {
        return Err(
            "nothing to change: give tempo, beats_per_bar, metronome or metronome_db".into(),
        );
    }
    Ok(b)
}

// ---- mix --------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MixChange {
    pub track: Option<TrackId>,
    pub instrument: Option<ChannelId>,
    pub volume_db: Option<f64>,
    pub pan: Option<f64>,
    pub mute: Option<bool>,
    pub solo: Option<bool>,
}

/// `mix_set` needs no project: plain edits.
pub fn mix_edits(changes: &[MixChange]) -> Result<Vec<Edit>, String> {
    if changes.is_empty() {
        return Err(
            "changes is empty: give at least one {track or instrument, volume_db/pan/mute/solo}"
                .into(),
        );
    }
    let mut list = Vec::new();
    for (i, c) in changes.iter().enumerate() {
        let mut values = Vec::new();
        if let Some(v) = c.volume_db {
            values.push(MixValue::VolumeDb(v));
        }
        if let Some(v) = c.pan {
            values.push(MixValue::Pan(v));
        }
        if let Some(v) = c.mute {
            values.push(MixValue::Mute(v));
        }
        if let Some(v) = c.solo {
            values.push(MixValue::Solo(v));
        }
        if values.is_empty() {
            return Err(format!(
                "change {i} has no value: give at least one of volume_db, pan, mute, solo"
            ));
        }
        match (c.track, c.instrument) {
            (Some(track), None) => list.extend(
                values
                    .into_iter()
                    .map(|value| Edit::SetTrackMix { track, value }),
            ),
            (None, Some(channel)) => list.extend(
                values
                    .into_iter()
                    .map(|value| Edit::SetChannelMix { channel, value }),
            ),
            _ => {
                return Err(format!(
                    "change {i} must name exactly one of track or instrument"
                ));
            }
        }
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::model::{Channel, Mix, Pattern};
    use std::sync::Arc;

    fn project() -> Project {
        let mut p = Project::empty();
        p.channels.push(Arc::new(Channel {
            id: ChannelId(1),
            name: "kick".into(),
            root_key: 36,
            track: TrackId::MASTER,
            mix: Mix::default(),
            instrument: Instrument::Synth(Default::default()),
            choke_group: 0,
        }));
        p.patterns.push(Arc::new(Pattern::new(
            PatternId(2),
            "Kick 1".into(),
            ChannelId(1),
        )));
        p.clips.push(protocol::model::Clip {
            id: ClipId(3),
            instrument: ChannelId(1),
            pattern: PatternId(2),
            start: 0,
            len: 3840,
            offset: 0,
            muted: false,
        });
        p
    }

    fn arg(v: Value) -> InstrumentArg {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_plugin_sound_is_passed_on_with_its_key_and_level() {
        let p = project();
        let mut ids = IdGen::new(&p, None);
        let id = "org.surge-synth-team.surge-xt";
        let b = instruments_add(
            &p,
            &mut ids,
            &[
                arg(json!({"name": "Lead", "plugin_id": id, "preset": "Leads/Classic Lead 1.fxp"})),
                arg(json!({"name": "Sub", "plugin_id": id, "preset": "Basses/Sub 1.fxp"})),
            ],
        )
        .unwrap();
        let clap = |e: &Edit| match e {
            Edit::AddChannel {
                instrument: NewInstrument::Clap { preset, .. },
                root_key,
                ..
            } => Some((preset.clone(), *root_key)),
            _ => None,
        };
        let added: Vec<_> = b.edits.iter().filter_map(clap).collect();
        assert_eq!(
            added,
            vec![
                (Some("Leads/Classic Lead 1.fxp".into()), 60),
                (Some("Basses/Sub 1.fxp".into()), 36)
            ]
        );
        // Only the loud sound turns its new track down.
        let turned: Vec<_> = b
            .edits
            .iter()
            .filter(|e| matches!(e, Edit::SetTrackMix { .. }))
            .collect();
        assert_eq!(turned.len(), 1);
        assert!(
            instruments_add(
                &p,
                &mut ids,
                &[arg(json!({"name": "x", "kind": "808", "preset": "a"}))]
            )
            .is_err()
        );
    }

    #[test]
    fn an_instrument_gets_its_own_track_and_a_clip_in_one_batch() {
        let p = project();
        let mut ids = IdGen::new(&p, None);
        let b = instruments_add(
            &p,
            &mut ids,
            &[arg(
                json!({"name": "808", "kind": "808", "clip": {"notes": "C2:0:1/4 C2:1:1/4"}}),
            )],
        )
        .unwrap();
        // track 4, channel 5, content 6, clip 7.
        assert_eq!(b.predicted, vec![4, 5, 6, 7]);
        assert!(matches!(b.edits[0], Edit::AddTrack { .. }));
        assert!(matches!(
            b.edits[1],
            Edit::AddChannel {
                track: TrackId(4),
                ..
            }
        ));
        assert!(matches!(
            b.edits[2],
            Edit::AddClip {
                instrument: ChannelId(5),
                pattern: None,
                start: 0,
                len: 7680
            }
        ));
        // Two bars of notes: the content is made 32 steps long first.
        assert!(matches!(
            b.edits[3],
            Edit::SetPatternLength {
                pattern: PatternId(6),
                length_steps: 32
            }
        ));
        assert!(matches!(
            b.edits[4],
            Edit::AddNotes {
                pattern: PatternId(6),
                ..
            }
        ));
        assert_eq!(b.reply["instruments"][0]["clip"], json!(7));
        assert_eq!(b.labels.len(), b.edits.len());
    }

    #[test]
    fn instrument_arguments_are_checked() {
        let p = project();
        let mut ids = IdGen::new(&p, None);
        let e = instruments_add(
            &p,
            &mut ids,
            &[arg(json!({"name": "x", "kind": "808", "synth": {}}))],
        )
        .unwrap_err();
        assert!(e.contains("`synth` does not apply to kind \"808\""), "{e}");
        let e = instruments_add(&p, &mut ids, &[arg(json!({"name": "x", "kind": "plugin"}))])
            .unwrap_err();
        assert!(e.contains("plugin_id"), "{e}");
        let e =
            instruments_add(&p, &mut ids, &[arg(json!({"name": "x", "track": 99}))]).unwrap_err();
        assert!(e.contains("mixer track 99"), "{e}");
        let b = instruments_add(
            &p,
            &mut IdGen::new(&p, None),
            &[arg(json!({"name": "x", "track": "master", "copy_of": 1}))],
        )
        .unwrap();
        assert!(matches!(
            b.edits[0],
            Edit::AddChannel {
                track: TrackId(0),
                root_key: 36,
                ..
            }
        ));
    }

    #[test]
    fn clips_add_links_or_writes_new_content() {
        let p = project();
        let mut ids = IdGen::new(&p, None);
        let list: Vec<ClipArg> = serde_json::from_value(json!([
            {"instrument": 1, "start": 1, "content": 2},
            {"instrument": 1, "start": "2", "grid": "x...x...x...x..."}
        ]))
        .unwrap();
        let b = clips_add(&p, &mut ids, &list).unwrap();
        assert_eq!(b.predicted, vec![4, 5, 6]);
        assert!(matches!(
            b.edits[0],
            Edit::AddClip {
                pattern: Some(PatternId(2)),
                start: 3840,
                len: 3840,
                ..
            }
        ));
        assert!(matches!(
            b.edits[1],
            Edit::AddClip {
                pattern: None,
                start: 7680,
                ..
            }
        ));
        assert_eq!(
            b.edits
                .iter()
                .filter(|e| matches!(
                    e,
                    Edit::SetStep {
                        pattern: PatternId(5),
                        on: true,
                        ..
                    }
                ))
                .count(),
            4
        );
        let bad: Vec<ClipArg> = serde_json::from_value(
            json!([{"instrument": 1, "start": 0, "content": 2, "grid": "x"}]),
        )
        .unwrap();
        assert!(
            clips_add(&p, &mut IdGen::new(&p, None), &bad)
                .unwrap_err()
                .contains("not both")
        );
    }

    #[test]
    fn copies_go_right_after_and_repeat() {
        let p = project();
        let a: ClipsCopyArgs = serde_json::from_value(json!({"clips": [3], "times": 3})).unwrap();
        let b = clips_copy(&p, &a).unwrap();
        let dts: Vec<i64> = b
            .edits
            .iter()
            .map(|e| match e {
                Edit::DuplicateClips {
                    dt, linked: true, ..
                } => *dt,
                _ => 0,
            })
            .collect();
        assert_eq!(dts, vec![3840, 7680, 11520]);
    }

    #[test]
    fn changes_moves_and_lengths() {
        let p = project();
        let a: ClipsChangeArgs = serde_json::from_value(
            json!({"clips": [3], "move_by": "-0", "length": 2, "muted": true}),
        )
        .unwrap();
        let b = clips_change(&p, &a).unwrap();
        assert!(matches!(b.edits[0], Edit::ResizeClips { dlen: 3840, .. }));
        assert!(matches!(b.edits[1], Edit::SetClipMuted { muted: true, .. }));
        let a: ClipsChangeArgs =
            serde_json::from_value(json!({"clips": [3], "move_by": "-1/4"})).unwrap();
        assert!(matches!(
            clips_change(&p, &a).unwrap().edits[0],
            Edit::MoveClips { dt: -960, .. }
        ));
        let a: ClipsChangeArgs = serde_json::from_value(json!({"clips": [3]})).unwrap();
        assert!(
            clips_change(&p, &a)
                .unwrap_err()
                .contains("nothing to change")
        );
    }

    #[test]
    fn splits_happen_right_to_left() {
        let mut p = project();
        p.clips[0].len = 4 * 3840;
        let a: ClipsSplitArgs =
            serde_json::from_value(json!({"clip": 3, "at": [1, 3, 2]})).unwrap();
        let b = clips_split(&p, &a).unwrap();
        let at: Vec<u32> = b
            .edits
            .iter()
            .map(|e| match e {
                Edit::SplitClip { at, .. } => *at,
                _ => 0,
            })
            .collect();
        assert_eq!(at, vec![11520, 7680, 3840]);
        let a: ClipsSplitArgs = serde_json::from_value(json!({"clip": 3, "at": [9]})).unwrap();
        assert!(clips_split(&p, &a).unwrap_err().contains("not inside"));
    }

    #[test]
    fn instrument_set_maps_params_by_name_and_checks_ranges() {
        let p = project();
        let a: InstrumentSetArgs =
            serde_json::from_value(json!({"instrument": 1, "params": {"cutoff_hz": 400}})).unwrap();
        assert!(matches!(
            instrument_set(&p, &a).unwrap().edits[0],
            Edit::SetSynthParam {
                param: SynthParam::CutoffHz,
                ..
            }
        ));
        let a: InstrumentSetArgs =
            serde_json::from_value(json!({"instrument": 1, "params": {"cutoff_hz": 1e9}})).unwrap();
        assert!(instrument_set(&p, &a).unwrap_err().contains("out of range"));
        let a: InstrumentSetArgs =
            serde_json::from_value(json!({"instrument": 1, "params": {"drive": 1}})).unwrap();
        let e = instrument_set(&p, &a).unwrap_err();
        assert!(
            e.contains("no parameter \"drive\"") && e.contains("cutoff_hz"),
            "{e}"
        );
        let a: InstrumentSetArgs =
            serde_json::from_value(json!({"instrument": 1, "mono": true})).unwrap();
        assert!(
            instrument_set(&p, &a)
                .unwrap_err()
                .contains("does not apply")
        );
    }

    #[test]
    fn loop_follows_a_clip_or_the_song() {
        let p = project();
        let a: LoopArgs = serde_json::from_value(json!({"clip": 3})).unwrap();
        assert!(matches!(
            loop_set(&p, &a).unwrap().edits[0],
            Edit::SetLoopRegion {
                start: 0,
                end: 3840,
                enabled: true
            }
        ));
        let a: LoopArgs = serde_json::from_value(json!({"start": 2, "end": 1})).unwrap();
        assert!(loop_set(&p, &a).unwrap_err().contains("after its start"));
    }

    #[test]
    fn notes_edit_quantizes_and_selects() {
        let mut p = project();
        Arc::make_mut(&mut p.patterns[0]).notes = vec![protocol::model::Note {
            id: NoteId(9),
            start: 250,
            len: 100,
            key: 40,
            vel: 100,
            off: 0,
            repeat: 1,
        }];
        let a: NotesEditArgs = serde_json::from_value(
            json!({"content": 2, "notes": "all", "quantize": "1/16", "velocity": 90}),
        )
        .unwrap();
        let b = notes_edit(&p, &a).unwrap();
        assert!(matches!(b.edits[0], Edit::MoveNotes { dt: -10, .. }));
        assert!(matches!(b.edits[1], Edit::SetNoteVelocity { vel: 90, .. }));
        let a: NotesEditArgs =
            serde_json::from_value(json!({"content": 2, "notes": [77], "remove": true})).unwrap();
        assert!(notes_edit(&p, &a).unwrap_err().contains("note 77"));
    }

    #[test]
    fn mix_edits_name_one_target() {
        let c: Vec<MixChange> =
            serde_json::from_value(json!([{"instrument": 1, "volume_db": -6, "pan": 0.2}]))
                .unwrap();
        assert_eq!(mix_edits(&c).unwrap().len(), 2);
        let c: Vec<MixChange> = serde_json::from_value(json!([{"volume_db": -6}])).unwrap();
        assert!(mix_edits(&c).unwrap_err().contains("exactly one"));
    }
}
