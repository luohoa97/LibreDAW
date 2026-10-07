// SPDX-License-Identifier: GPL-3.0-or-later
//! The project file `project.toml` (SPEC 7.2, 7.3).
//!
//! Saving uses a hand-written emitter that writes one canonical text: fixed
//! key order, collections in id order, one note per line as an inline
//! table. Loading parses with `toml` and `serde`, checks the version first,
//! then validates. `parse(emit(p)) == p` and `emit(parse(t)) == t` for any
//! canonical text `t`.
//!
//! Plugin state blobs and the bundle layout (7.1, 7.4) are handled by the
//! `ui` crate; this module only reads and writes the TOML text.

use std::fmt::Write;
use std::sync::Arc;

use serde::Deserialize;

use crate::beats::{
    Bass808, Bass808Param, BuiltinFx, BuiltinFxKind, SampleMode, Sampler, SamplerParam,
    SaturatorCurve,
};
use crate::consts::FORMAT_VERSION;
use crate::ids::InstanceId;
use crate::model::{
    Adsr, Channel, ClapRef, Clip, Curve, Insert, Instrument, LoopRegion, Metronome, Mix, Osc, Pattern,
    PatternGroup, Project, SampleRef, Shape, ShapeTarget, SynthParams, Track, Wave,
};
use crate::validate::{ValidationError, validate};

#[derive(Debug, PartialEq)]
pub enum FormatError {
    /// No `format_version` key.
    MissingVersion,
    /// Written by a newer LibreDAW (7.3).
    TooNew { found: u32, supported: u32 },
    /// Not valid TOML, or keys and types do not match the schema.
    Syntax(String),
    /// Parsed, but breaks a project rule.
    Invalid(ValidationError),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::MissingVersion => {
                write!(f, "not a LibreDAW project: format_version missing")
            }
            FormatError::TooNew { found, supported } => write!(
                f,
                "written by a newer LibreDAW (format {found}; this version reads up to {supported})"
            ),
            FormatError::Syntax(e) => write!(f, "project file error: {e}"),
            FormatError::Invalid(e) => write!(f, "invalid project: {e}"),
        }
    }
}

impl std::error::Error for FormatError {}

// ---------------------------------------------------------------------------
// Emitter

/// Writes the canonical project text. `next_id` is the document's id
/// counter (17.1). Fails only if the project does not validate.
pub fn emit(p: &Project, next_id: u32) -> Result<String, ValidationError> {
    validate(p)?;
    let mut o = String::with_capacity(4096 + p.note_count() * 64);
    let w = &mut o;
    line(w, format_args!("format_version = {FORMAT_VERSION}"));
    line(w, format_args!("next_id = {}", next_id.max(p.max_id() + 1)));
    line(w, format_args!("tempo_bpm = {}", float(p.tempo_bpm)));
    line(w, format_args!("time_sig_num = {}", p.time_sig_num));
    w.push_str("\n[metronome]\n");
    line(w, format_args!("enabled = {}", p.metronome.enabled));
    line(w, format_args!("gain_db = {}", float(p.metronome.gain_db)));

    for s in &p.samples {
        w.push_str("\n[[samples]]\n");
        line(w, format_args!("hash = {}", string(&s.hash)));
        line(w, format_args!("orig_name = {}", string(&s.orig_name)));
        line(w, format_args!("size = {}", s.size));
        line(w, format_args!("local_only = {}", s.local_only));
    }

    for c in &p.channels {
        w.push_str("\n[[channels]]\n");
        line(w, format_args!("id = {}", c.id));
        line(w, format_args!("name = {}", string(&c.name)));
        line(w, format_args!("root_key = {}", c.root_key));
        line(w, format_args!("track = {}", c.track));
        line(w, format_args!("mix = {}", mix(&c.mix)));
        if c.choke_group != 0 {
            line(w, format_args!("choke_group = {}", c.choke_group));
        }
        w.push_str("\n[channels.instrument]\n");
        match &c.instrument {
            Instrument::Synth(s) => emit_synth(w, s),
            Instrument::Clap(r) => emit_clap(w, r),
            Instrument::Sampler(s) => emit_sampler(w, s),
            Instrument::Bass808(b) => emit_808(w, b),
            Instrument::Audio => line(w, format_args!("kind = \"audio\"")),
        }
    }

    for pat in &p.patterns {
        w.push_str("\n[[patterns]]\n");
        line(w, format_args!("id = {}", pat.id));
        line(w, format_args!("name = {}", string(&pat.name)));
        line(w, format_args!("instrument = {}", pat.instrument));
        line(w, format_args!("length_steps = {}", pat.length_steps));
        line(w, format_args!("step_ticks = {}", pat.step_ticks));
        if pat.swing != 0 {
            line(w, format_args!("swing = {}", pat.swing));
        }
        if !pat.notes.is_empty() {
            w.push_str("notes = [\n");
            for n in &pat.notes {
                let mut extra = String::new();
                if n.off != 0 {
                    let _ = write!(extra, ", off = {}", n.off);
                }
                if n.repeat != 1 {
                    let _ = write!(extra, ", repeat = {}", n.repeat);
                }
                line(
                    w,
                    format_args!(
                        "  {{ id = {}, start = {}, len = {}, key = {}, vel = {}{extra} }},",
                        n.id, n.start, n.len, n.key, n.vel
                    ),
                );
            }
            w.push_str("]\n");
        }
    }

    for t in &p.tracks {
        w.push_str("\n[[tracks]]\n");
        line(w, format_args!("id = {}", t.id));
        line(w, format_args!("name = {}", string(&t.name)));
        line(w, format_args!("mix = {}", mix(&t.mix)));
        if !t.sends.is_empty() {
            w.push_str("sends = [\n");
            for s in &t.sends {
                line(
                    w,
                    format_args!(
                        "  {{ to = {}, level_db = {}, pre_fader = {} }},",
                        s.to,
                        float(s.level_db),
                        s.pre_fader
                    ),
                );
            }
            w.push_str("]\n");
        }
        for ins in &t.inserts {
            w.push_str("\n[[tracks.inserts]]\n");
            match ins {
                Insert::Clap(r) => emit_clap(w, r),
                Insert::Builtin {
                    instance,
                    fx,
                    bypass,
                } => {
                    emit_builtin(w, *instance, fx);
                    if *bypass {
                        line(w, format_args!("bypass = true"));
                    }
                }
            }
        }
    }

    for g in &p.groups {
        w.push_str("\n[[groups]]\n");
        line(w, format_args!("id = {}", g.id));
        line(w, format_args!("name = {}", string(&g.name)));
        line(w, format_args!("color = {}", g.color));
    }

    for s in &p.shapes {
        w.push_str("\n[[shapes]]\n");
        line(w, format_args!("id = {}", s.id));
        line(w, format_args!("target = {}", shape_target(&s.target)));
        if s.points.is_empty() {
            w.push_str("points = []\n");
        } else {
            w.push_str("points = [\n");
            for pt in &s.points {
                line(
                    w,
                    format_args!(
                        "  {{ tick = {}, value = {}, curve = \"{}\" }},",
                        pt.tick,
                        float(f64::from(pt.value)),
                        curve(pt.curve)
                    ),
                );
            }
            w.push_str("]\n");
        }
    }

    w.push_str("\n[timeline]\n");
    let lr = p.loop_region;
    line(
        w,
        format_args!(
            "loop = {{ start = {}, end = {}, enabled = {} }}",
            lr.start, lr.end, lr.enabled
        ),
    );
    if !p.clips.is_empty() {
        w.push_str("clips = [\n");
        for c in &p.clips {
            let mut extra = String::new();
            if c.offset != 0 {
                let _ = write!(extra, ", offset = {}", c.offset);
            }
            if c.muted {
                extra.push_str(", muted = true");
            }
            if let Some(a) = &c.audio {
                let _ = write!(extra, ", audio = {{ sample = \"{}\"", a.sample.to_hex());
                if a.gain_mdb != 0 {
                    let _ = write!(extra, ", gain_mdb = {}", a.gain_mdb);
                }
                if a.fade_in != 0 {
                    let _ = write!(extra, ", fade_in = {}", a.fade_in);
                }
                if a.fade_out != 0 {
                    let _ = write!(extra, ", fade_out = {}", a.fade_out);
                }
                extra.push_str(" }");
            }
            if let Some(g) = &c.group {
                let _ = write!(
                    extra,
                    ", group = {{ group = {}, instance = {} }}",
                    g.group, g.instance
                );
            }
            line(
                w,
                format_args!(
                    "  {{ id = {}, instrument = {}, pattern = {}, start = {}, len = {}{extra} }},",
                    c.id, c.instrument, c.pattern, c.start, c.len
                ),
            );
        }
        w.push_str("]\n");
    }
    Ok(o)
}

fn line(w: &mut String, args: std::fmt::Arguments<'_>) {
    w.write_fmt(args).expect("writing to a String cannot fail");
    w.push('\n');
}

/// Shortest round-trip form, always with a decimal point or exponent so
/// TOML reads it back as a float. `-0.0` is written as `0.0`. Callers
/// validate first, so NaN and infinity never reach here.
fn float(v: f64) -> String {
    debug_assert!(v.is_finite());
    let v = if v == 0.0 { 0.0 } else { v };
    let s = format!("{v:?}");
    if s.contains(['.', 'e', 'E']) {
        s
    } else {
        format!("{s}.0")
    }
}

/// TOML basic string with escapes.
fn string(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if c.is_control() => {
                let _ = write!(o, "\\u{:04X}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn curve(c: Curve) -> &'static str {
    match c {
        Curve::Smooth => "smooth",
        Curve::Linear => "linear",
        Curve::Hold => "hold",
        Curve::Stairs => "stairs",
        Curve::Pulse => "pulse",
        Curve::Wave => "wave",
    }
}

fn shape_target(t: &ShapeTarget) -> String {
    match t {
        ShapeTarget::Volume { track } => format!("{{ kind = \"volume\", track = {track} }}"),
        ShapeTarget::Pan { track } => format!("{{ kind = \"pan\", track = {track} }}"),
        ShapeTarget::Pitch { instrument } => {
            format!("{{ kind = \"pitch\", instrument = {instrument} }}")
        }
        ShapeTarget::Filter { instrument } => {
            format!("{{ kind = \"filter\", instrument = {instrument} }}")
        }
        ShapeTarget::FxParam {
            track,
            instance,
            param,
        } => format!(
            "{{ kind = \"fx_param\", track = {track}, instance = {instance}, param = {param} }}"
        ),
    }
}

fn mix(m: &Mix) -> String {
    format!(
        "{{ volume_db = {}, pan = {}, mute = {}, solo = {} }}",
        float(m.volume_db),
        float(m.pan),
        m.mute,
        m.solo
    )
}

fn wave(w: Wave) -> &'static str {
    match w {
        Wave::Sine => "sine",
        Wave::Saw => "saw",
        Wave::Square => "square",
        Wave::Triangle => "triangle",
    }
}

fn osc(o: &Osc) -> String {
    format!(
        "{{ wave = \"{}\", semitones = {}, cents = {} }}",
        wave(o.wave),
        float(o.semitones),
        float(o.cents)
    )
}

fn adsr(a: &Adsr) -> String {
    format!(
        "{{ attack_ms = {}, decay_ms = {}, sustain = {}, release_ms = {} }}",
        float(a.attack_ms),
        float(a.decay_ms),
        float(a.sustain),
        float(a.release_ms)
    )
}

fn emit_synth(w: &mut String, s: &SynthParams) {
    line(w, format_args!("kind = \"synth\""));
    line(w, format_args!("osc1 = {}", osc(&s.osc1)));
    line(w, format_args!("osc2 = {}", osc(&s.osc2)));
    line(w, format_args!("osc_mix = {}", float(s.osc_mix)));
    line(w, format_args!("cutoff_hz = {}", float(s.cutoff_hz)));
    line(w, format_args!("resonance = {}", float(s.resonance)));
    line(
        w,
        format_args!("filter_env_octaves = {}", float(s.filter_env_octaves)),
    );
    line(w, format_args!("amp_env = {}", adsr(&s.amp_env)));
    line(w, format_args!("filter_env = {}", adsr(&s.filter_env)));
    line(w, format_args!("gain_db = {}", float(s.gain_db)));
}

/// `{ a = 1.0, b = 2.0 }` from parameter names and values.
fn params_table(it: impl Iterator<Item = (&'static str, f64)>) -> String {
    let mut o = String::from("{ ");
    for (i, (k, v)) in it.enumerate() {
        if i > 0 {
            o.push_str(", ");
        }
        let _ = write!(o, "{k} = {}", float(v));
    }
    o.push_str(" }");
    o
}

fn emit_sampler(w: &mut String, s: &Sampler) {
    line(w, format_args!("kind = \"sampler\""));
    if let Some(h) = &s.sample {
        line(w, format_args!("sample = {}", string(h)));
    }
    let mode = match s.mode {
        SampleMode::OneShot => "one_shot",
        SampleMode::Pitched => "pitched",
    };
    line(w, format_args!("mode = \"{mode}\""));
    line(w, format_args!("reverse = {}", s.reverse));
    let t = params_table(
        SamplerParam::ALL
            .iter()
            .map(|p| (p.name(), s.params.get(*p))),
    );
    line(w, format_args!("params = {t}"));
}

fn emit_808(w: &mut String, b: &Bass808) {
    line(w, format_args!("kind = \"bass808\""));
    line(w, format_args!("mono = {}", b.mono));
    let t = params_table(
        Bass808Param::ALL
            .iter()
            .map(|p| (p.name(), b.params.get(*p))),
    );
    line(w, format_args!("params = {t}"));
}

fn emit_builtin(w: &mut String, instance: InstanceId, fx: &BuiltinFx) {
    line(w, format_args!("kind = \"builtin\""));
    line(w, format_args!("instance = {instance}"));
    let ty = match fx.kind() {
        BuiltinFxKind::Eq => "eq",
        BuiltinFxKind::Compressor => "compressor",
        BuiltinFxKind::Saturator => "saturator",
        BuiltinFxKind::Reverb => "reverb",
        BuiltinFxKind::Delay => "delay",
        BuiltinFxKind::Limiter => "limiter",
    };
    let params = params_table((0..fx.param_count()).map(|i| {
        (
            fx.param_name(i).expect("in range"),
            fx.param(i).expect("in range"),
        )
    }));
    let mut extra = String::new();
    match fx {
        BuiltinFx::Compressor {
            sidechain: Some(t), ..
        } => {
            let _ = write!(extra, ", sidechain = {t}");
        }
        BuiltinFx::Saturator { curve, .. } => {
            let c = match curve {
                SaturatorCurve::Soft => "soft",
                SaturatorCurve::Hard => "hard",
                SaturatorCurve::Fold => "fold",
            };
            let _ = write!(extra, ", curve = \"{c}\"");
        }
        BuiltinFx::Delay { ping_pong, .. } => {
            let _ = write!(extra, ", ping_pong = {ping_pong}");
        }
        _ => {}
    }
    line(
        w,
        format_args!("fx = {{ type = \"{ty}\"{extra}, params = {params} }}"),
    );
}

fn emit_clap(w: &mut String, r: &ClapRef) {
    line(w, format_args!("kind = \"clap\""));
    line(w, format_args!("instance = {}", r.instance));
    line(w, format_args!("plugin_id = {}", string(&r.plugin_id)));
    line(
        w,
        format_args!("plugin_version = {}", string(&r.plugin_version)),
    );
    if let Some(f) = &r.state_file {
        line(w, format_args!("state_file = {}", string(f)));
    }
    if !r.params.is_empty() {
        w.push_str("params = [\n");
        for p in &r.params {
            line(
                w,
                format_args!("  {{ id = {}, value = {} }},", p.id, float(p.value)),
            );
        }
        w.push_str("]\n");
    }
}

// ---------------------------------------------------------------------------
// Loader

#[derive(Deserialize)]
struct VersionOnly {
    format_version: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Timeline {
    #[serde(rename = "loop", default)]
    loop_region: LoopRegion,
    #[serde(default)]
    clips: Vec<Clip>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileV4 {
    #[allow(dead_code)]
    format_version: u32,
    next_id: u32,
    tempo_bpm: f64,
    time_sig_num: u8,
    metronome: Metronome,
    #[serde(default)]
    samples: Vec<SampleRef>,
    #[serde(default)]
    channels: Vec<Channel>,
    #[serde(default)]
    patterns: Vec<Pattern>,
    tracks: Vec<Track>,
    #[serde(default)]
    groups: Vec<PatternGroup>,
    #[serde(default)]
    shapes: Vec<Shape>,
    timeline: Option<Timeline>,
}

/// Parses project text. Returns the project and its id counter, which is
/// at least the largest id in the file plus one (17.1).
pub fn parse(text: &str) -> Result<(Project, u32), FormatError> {
    let v: VersionOnly = toml::from_str(text).map_err(|e| FormatError::Syntax(e.to_string()))?;
    match v.format_version {
        None => Err(FormatError::MissingVersion),
        Some(found) if found > FORMAT_VERSION => Err(FormatError::TooNew {
            found,
            supported: FORMAT_VERSION,
        }),
        // Versions 1 and 2 used multi-channel patterns and a playlist
        // (15.6); they are converted to the timeline model (20.5).
        Some(1 | 2) => legacy::parse_v2(text),
        Some(_) => parse_v4(text),
    }
}

fn parse_v4(text: &str) -> Result<(Project, u32), FormatError> {
    let f: FileV4 = toml::from_str(text).map_err(|e| FormatError::Syntax(e.to_string()))?;
    let (clips, loop_region) = match f.timeline {
        Some(t) => (t.clips, t.loop_region),
        None => (Vec::new(), LoopRegion::default()),
    };
    let p = Project {
        tempo_bpm: f.tempo_bpm,
        time_sig_num: f.time_sig_num,
        metronome: f.metronome,
        channels: f.channels.into_iter().map(Arc::new).collect(),
        patterns: f.patterns.into_iter().map(Arc::new).collect(),
        tracks: f.tracks.into_iter().map(Arc::new).collect(),
        samples: f.samples,
        clips,
        loop_region,
        groups: f.groups,
        shapes: f.shapes,
    };
    validate(&p).map_err(FormatError::Invalid)?;
    let next_id = f.next_id.max(p.max_id() + 1);
    Ok((p, next_id))
}

/// Versions 1 and 2 (20.5): patterns held notes for several channels and a
/// playlist placed patterns. Each (pattern, channel) becomes one clip
/// content; each playlist clip becomes one clip per channel with notes.
/// Clips that would overlap on one instrument's row after the split are
/// dropped (the later one by start, then id).
mod legacy {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;

    use serde::Deserialize;

    use super::FormatError;
    use crate::ids::{ChannelId, ClipId, PatternId};
    use crate::model::{
        Channel, Clip, LoopRegion, Metronome, Note, Pattern, Project, SampleRef, Track,
    };
    use crate::validate::{sort_canonical, validate};

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ChannelNotesV2 {
        channel: ChannelId,
        notes: Vec<Note>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PatternV2 {
        id: PatternId,
        name: String,
        length_steps: u8,
        step_ticks: u32,
        #[serde(default)]
        swing: u16,
        #[serde(default)]
        notes: Vec<ChannelNotesV2>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ClipV2 {
        #[allow(dead_code)]
        id: u32,
        pattern: PatternId,
        start: u32,
        len: u32,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PlaylistTrackV2 {
        #[allow(dead_code)]
        id: u32,
        #[allow(dead_code)]
        name: String,
        #[serde(default)]
        clips: Vec<ClipV2>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FileV2 {
        #[allow(dead_code)]
        format_version: u32,
        next_id: u32,
        tempo_bpm: f64,
        time_sig_num: u8,
        metronome: Metronome,
        #[serde(default)]
        channels: Vec<Channel>,
        #[serde(default)]
        patterns: Vec<PatternV2>,
        tracks: Vec<Track>,
        #[serde(default)]
        samples: Vec<SampleRef>,
        #[serde(default)]
        playlist: Vec<PlaylistTrackV2>,
    }

    pub(super) fn parse_v2(text: &str) -> Result<(Project, u32), FormatError> {
        let f: FileV2 = toml::from_str(text).map_err(|e| FormatError::Syntax(e.to_string()))?;
        // Ids already used, so new ids start above all of them.
        let mut max = f.next_id.saturating_sub(1);
        for c in &f.channels {
            max = max.max(c.id.0);
        }
        for t in &f.tracks {
            max = max.max(t.id.0);
            for i in &t.inserts {
                max = max.max(i.instance().0);
            }
        }
        for p in &f.patterns {
            max = max.max(p.id.0);
            for cn in &p.notes {
                for n in &cn.notes {
                    max = max.max(n.id.0);
                }
            }
        }
        for pt in &f.playlist {
            max = max.max(pt.id);
            for c in &pt.clips {
                max = max.max(c.id);
            }
        }
        let mut next = max + 1;
        let mut fresh = || {
            let id = next;
            next += 1;
            id
        };

        // (old pattern, channel) -> new content id; the first channel of a
        // pattern keeps the pattern's own id.
        let mut contents: BTreeMap<(PatternId, ChannelId), PatternId> = BTreeMap::new();
        let mut patterns = Vec::new();
        let mut lengths: HashMap<PatternId, u32> = HashMap::new();
        for p in &f.patterns {
            let mut first = true;
            for cn in &p.notes {
                let id = if first { p.id } else { PatternId(fresh()) };
                first = false;
                contents.insert((p.id, cn.channel), id);
                let name = if p.notes.len() > 1 {
                    let ch = f
                        .channels
                        .iter()
                        .find(|c| c.id == cn.channel)
                        .map(|c| c.name.as_str())
                        .unwrap_or("Instrument");
                    format!("{} {}", p.name, ch)
                        .chars()
                        .take(crate::consts::MAX_NAME_CHARS)
                        .collect()
                } else {
                    p.name.clone()
                };
                let mut notes: Vec<Note> = cn.notes.clone();
                notes.sort_by_key(|n| (n.start, n.key, n.id));
                let pat = Pattern {
                    id,
                    name,
                    instrument: cn.channel,
                    length_steps: p.length_steps,
                    step_ticks: p.step_ticks,
                    swing: p.swing,
                    notes,
                };
                lengths.insert(id, pat.length_ticks());
                patterns.push(Arc::new(pat));
            }
        }

        let mut clips: Vec<Clip> = Vec::new();
        let mut placed: Vec<(u32, u32, PatternId)> = f
            .playlist
            .iter()
            .flat_map(|pt| pt.clips.iter().map(|c| (c.start, c.len, c.pattern)))
            .collect();
        placed.sort();
        for (start, len, old) in &placed {
            for ((p, ch), id) in contents.range((*old, ChannelId(0))..=(*old, ChannelId(u32::MAX)))
            {
                debug_assert_eq!(p, old);
                let overlaps = clips
                    .iter()
                    .any(|c| c.instrument == *ch && c.start < start + len && *start < c.end());
                if !overlaps {
                    clips.push(Clip {
                        id: ClipId(fresh()),
                        instrument: *ch,
                        pattern: *id,
                        start: *start,
                        len: *len,
                        offset: 0,
                        muted: false,
                        audio: None,
                        group: None,
                    });
                }
            }
        }
        let mut loop_region = LoopRegion::default();
        if placed.is_empty() {
            // No arrangement: each content once at the start, looped.
            let mut longest = 0;
            for (_, id) in contents.iter() {
                let instrument = patterns
                    .iter()
                    .find(|p| p.id == *id)
                    .expect("made above")
                    .instrument;
                if clips.iter().any(|c| c.instrument == instrument) {
                    continue;
                }
                let len = lengths[id].max(1);
                longest = longest.max(len);
                clips.push(Clip {
                    id: ClipId(fresh()),
                    instrument,
                    pattern: *id,
                    start: 0,
                    len,
                    offset: 0,
                    muted: false,
                    audio: None,
                    group: None,
                });
            }
            if longest > 0 {
                loop_region = LoopRegion {
                    start: 0,
                    end: longest,
                    enabled: true,
                };
            }
        }
        let next_id = next;
        let mut p = Project {
            tempo_bpm: f.tempo_bpm,
            time_sig_num: f.time_sig_num,
            metronome: f.metronome,
            channels: f.channels.into_iter().map(Arc::new).collect(),
            patterns,
            tracks: f.tracks.into_iter().map(Arc::new).collect(),
            samples: f.samples,
            clips,
            loop_region,
            groups: Vec::new(),
            shapes: Vec::new(),
        };
        sort_canonical(&mut p);
        validate(&p).map_err(FormatError::Invalid)?;
        let next_id = next_id.max(p.max_id() + 1);
        Ok((p, next_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beats::*;
    use crate::consts::*;
    use crate::ids::*;
    use crate::model::*;

    /// xorshift64*, so tests need no random-number crate and are repeatable.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
        fn float(&mut self, lo: f64, hi: f64) -> f64 {
            let unit = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
            lo + unit * (hi - lo)
        }
        fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
            xs[self.below(xs.len() as u64) as usize]
        }
    }

    const NAMES: &[&str] = &[
        "Kick",
        "808 \"sub\"",
        "Hats \\ open",
        "Pad ✨",
        "Lead",
        "Bass",
    ];

    fn synth(r: &mut Rng) -> SynthParams {
        let mut s = SynthParams::default();
        for sp in SynthParam::ALL {
            let (lo, hi) = sp.range();
            // Mix exact values with arbitrary ones to test float formatting.
            let v = match r.below(4) {
                0 => lo,
                1 => hi,
                2 => 0.0f64.clamp(lo, hi),
                _ => r.float(lo, hi),
            };
            s.set(sp, v);
        }
        s.osc1.wave = r.pick(&[Wave::Sine, Wave::Saw, Wave::Square, Wave::Triangle]);
        s.osc2.wave = r.pick(&[Wave::Sine, Wave::Saw, Wave::Square, Wave::Triangle]);
        s
    }

    fn clap(r: &mut Rng, next: &mut u32) -> ClapRef {
        let instance = InstanceId(*next);
        *next += 1;
        let mut params: Vec<ParamValue> = (0..r.below(4))
            .map(|i| ParamValue {
                id: (i * 7 + r.below(5)) as u32,
                value: r.float(-1e6, 1e6),
            })
            .collect();
        params.sort_by_key(|p| p.id);
        params.dedup_by_key(|p| p.id);
        ClapRef {
            instance,
            plugin_id: "org.example.plugin".into(),
            plugin_version: "1.2.3".into(),
            state_file: if r.below(2) == 0 {
                Some(format!("{}-1.bin", instance.0))
            } else {
                None
            },
            state_bytes: None,
            params,
        }
    }

    fn mix(r: &mut Rng) -> Mix {
        Mix {
            volume_db: r.float(MIN_GAIN_DB, MAX_GAIN_DB),
            pan: r.float(-1.0, 1.0),
            mute: r.below(2) == 0,
            solo: r.below(2) == 0,
        }
    }

    fn random_project(seed: u64) -> (Project, u32) {
        let r = &mut Rng(seed | 1);
        let mut next = FIRST_ID;
        let mut p = Project::empty();
        let any_tempo = r.float(20.0, 999.0);
        p.tempo_bpm = r.pick(&[120.0, 133.33, MIN_TEMPO_BPM, MAX_TEMPO_BPM, any_tempo]);
        p.time_sig_num = 1 + r.below(16) as u8;
        p.metronome = Metronome {
            enabled: r.below(2) == 0,
            gain_db: r.float(-96.0, 12.0),
        };
        for _ in 0..r.below(4) {
            let id = TrackId(next);
            next += 1;
            let mut inserts = Vec::new();
            for _ in 0..r.below(4) {
                if r.below(2) == 0 {
                    inserts.push(Insert::Clap(clap(r, &mut next)));
                } else {
                    let kinds = [
                        BuiltinFxKind::Eq,
                        BuiltinFxKind::Compressor,
                        BuiltinFxKind::Saturator,
                        BuiltinFxKind::Reverb,
                        BuiltinFxKind::Delay,
                        BuiltinFxKind::Limiter,
                    ];
                    let mut fx = BuiltinFx::new(r.pick(&kinds));
                    for i in 0..fx.param_count() {
                        let (lo, hi) = fx.param_range(i).unwrap();
                        let v = r.float(lo, hi);
                        fx.set_param(i, v);
                    }
                    if let BuiltinFx::Delay { ping_pong, .. } = &mut fx {
                        *ping_pong = r.below(2) == 0;
                    }
                    if let BuiltinFx::Saturator { curve, .. } = &mut fx {
                        *curve = r.pick(&[
                            SaturatorCurve::Soft,
                            SaturatorCurve::Hard,
                            SaturatorCurve::Fold,
                        ]);
                    }
                    inserts.push(Insert::Builtin {
                        instance: InstanceId(next),
                        fx,
                        bypass: r.below(4) == 0,
                    });
                    next += 1;
                }
            }
            p.tracks.push(Arc::new(Track {
                id,
                name: r.pick(NAMES).into(),
                mix: mix(r),
                inserts,
                sends: Vec::new(),
            }));
        }
        // Sends and a sidechain from lower to higher track ids: never a loop.
        let ids: Vec<TrackId> = p.tracks.iter().map(|t| t.id).collect();
        for i in 1..p.tracks.len() {
            let t = Arc::make_mut(&mut p.tracks[i]);
            for &to in &ids[i + 1..] {
                if r.below(2) == 0 {
                    t.sends.push(Send {
                        to,
                        level_db: r.float(-96.0, 12.0),
                        pre_fader: r.below(2) == 0,
                    });
                }
            }
            if i > 1
                && let Some(Insert::Builtin {
                    fx: BuiltinFx::Compressor { sidechain, .. },
                    ..
                }) = t.inserts.iter_mut().find(|x| {
                    matches!(
                        x,
                        Insert::Builtin {
                            fx: BuiltinFx::Compressor { .. },
                            ..
                        }
                    )
                })
            {
                *sidechain = Some(ids[1]);
            }
        }
        for _ in 0..r.below(3) {
            let mut h = String::new();
            for _ in 0..4 {
                h.push_str(&format!("{:016x}", r.next()));
            }
            p.samples.push(SampleRef {
                hash: h,
                orig_name: r.pick(NAMES).into(),
                size: r.below(1 << 40),
                local_only: r.below(2) == 0,
            });
        }
        let hashes: Vec<String> = p.samples.iter().map(|s| s.hash.clone()).collect();
        {}
        let track_ids: Vec<TrackId> = p.tracks.iter().map(|t| t.id).collect();
        for _ in 0..r.below(6) {
            let id = ChannelId(next);
            next += 1;
            let instrument = match r.below(4) {
                0 => Instrument::Clap(clap(r, &mut next)),
                1 => {
                    let mut params = SamplerParams::default();
                    for sp in SamplerParam::ALL {
                        let (lo, hi) = sp.range();
                        let v = r.float(lo, hi);
                        params.set(*sp, v);
                    }
                    params.start = r.float(0.0, 0.4);
                    params.end = r.float(0.5, 1.0);
                    let sample = if hashes.is_empty() || r.below(3) == 0 {
                        None
                    } else {
                        Some(hashes[r.below(hashes.len() as u64) as usize].clone())
                    };
                    Instrument::Sampler(Sampler {
                        sample,
                        mode: r.pick(&[SampleMode::OneShot, SampleMode::Pitched]),
                        reverse: r.below(2) == 0,
                        params,
                    })
                }
                2 => {
                    let mut b = Bass808::default();
                    for bp in Bass808Param::ALL {
                        let (lo, hi) = bp.range();
                        let v = r.float(lo, hi);
                        b.params.set(*bp, v);
                    }
                    b.mono = r.below(2) == 0;
                    Instrument::Bass808(b)
                }
                _ => Instrument::Synth(synth(r)),
            };
            p.channels.push(Arc::new(Channel {
                id,
                name: r.pick(NAMES).into(),
                root_key: r.below(128) as u8,
                track: r.pick(&track_ids),
                mix: mix(r),
                instrument,
                choke_group: r.below(17) as u8,
            }));
        }
        let channel_ids: Vec<ChannelId> = p.channels.iter().map(|c| c.id).collect();
        for &ch in &channel_ids {
            for _ in 0..r.below(3) {
                let mut pat = Pattern::new(PatternId(next), r.pick(NAMES).into(), ch);
                next += 1;
                pat.length_steps = 1 + r.below(64) as u8;
                let any_step = 1 + r.below(3840) as u32;
                pat.step_ticks = r.pick(&[60, 120, 240, 480, any_step]);
                pat.swing = r.below(751) as u16;
                let root = p.channels.iter().find(|c| c.id == ch).unwrap().root_key;
                for _ in 0..r.below(20) {
                    let repeat = r.pick(&RATCHETS);
                    let len = repeat as u32 * (1 + r.below(500) as u32);
                    let off = if r.below(3) == 0 {
                        r.below(49) as i16 - 24
                    } else {
                        0
                    };
                    let key_with_off = root as i16 + off;
                    let (key, off) = if off != 0 && (0..=127).contains(&key_with_off) {
                        (key_with_off as u8, off as i8)
                    } else {
                        (r.below(128) as u8, 0)
                    };
                    pat.notes.push(Note {
                        id: NoteId(next),
                        start: r.below(1 << 20) as u32,
                        len,
                        key,
                        vel: 1 + r.below(127) as u8,
                        off,
                        repeat,
                    });
                    next += 1;
                }
                pat.notes.sort_by_key(|n| (n.start, n.key, n.id));
                p.patterns.push(Arc::new(pat));
            }
        }
        // Clips: per instrument, non-overlapping, using that instrument's
        // contents, some linked (same content twice), some trimmed.
        for &ch in &channel_ids {
            let own: Vec<(PatternId, u32)> = p
                .patterns
                .iter()
                .filter(|x| x.instrument == ch)
                .map(|x| (x.id, x.length_ticks()))
                .collect();
            if own.is_empty() {
                continue;
            }
            let mut t = 0u32;
            for _ in 0..r.below(6) {
                t += r.below(4000) as u32;
                let (pattern, content_len) = r.pick(&own);
                let len = 1 + r.below(8000) as u32;
                p.clips.push(Clip {
                    id: ClipId(next),
                    instrument: ch,
                    pattern,
                    start: t,
                    len,
                    offset: r.below(content_len.max(1) as u64) as u32,
                    muted: r.below(5) == 0,
                    audio: None,
                    group: None,
                });
                next += 1;
                t += len;
            }
        }
        // An audio row with audio clips (21.1).
        if !p.samples.is_empty() && r.below(2) == 0 {
            let row = ChannelId(next);
            next += 1;
            p.channels.push(Arc::new(Channel {
                id: row,
                name: "Audio".into(),
                root_key: 60,
                track: r.pick(&track_ids),
                mix: mix(r),
                instrument: Instrument::Audio,
                choke_group: 0,
            }));
            let mut t = 0u32;
            for _ in 0..1 + r.below(4) {
                t += r.below(4000) as u32;
                let len = 100 + r.below(8000) as u32;
                let h = &hashes[r.below(hashes.len() as u64) as usize];
                p.clips.push(Clip {
                    id: ClipId(next),
                    instrument: row,
                    pattern: PatternId::NONE,
                    start: t,
                    len,
                    offset: r.below(5000) as u32,
                    muted: r.below(5) == 0,
                    audio: Some(AudioSource {
                        sample: SampleHash::parse(h).unwrap(),
                        gain_mdb: r.below(40_000) as i32 - 20_000,
                        fade_in: r.below(len as u64 + 1) as u32,
                        fade_out: r.below(len as u64 + 1) as u32,
                    }),
                    group: None,
                });
                next += 1;
                t += len;
            }
        }
        // Patterns and shapes (20.7, 24.2-1).
        if r.below(2) == 0 {
            let gid = GroupId(next);
            next += 1;
            p.groups.push(PatternGroup {
                id: gid,
                name: r.pick(NAMES).into(),
                color: r.below(0x100_0000) as u32,
            });
            for (i, c) in p.clips.iter_mut().enumerate() {
                if i % 2 == 0 {
                    c.group = Some(ClipGroup {
                        group: gid,
                        instance: 1 + r.below(3) as u32,
                    });
                }
            }
        }
        for _ in 0..r.below(4) {
            let tr = r.pick(&track_ids);
            let (target, lo, hi) = match r.below(3) {
                0 => (ShapeTarget::Volume { track: tr }, -60.0, 6.0),
                1 => (ShapeTarget::Pan { track: tr }, -1.0, 1.0),
                _ => match p.channels.first() {
                    Some(c) => (ShapeTarget::Pitch { instrument: c.id }, -24.0, 24.0),
                    None => (ShapeTarget::Pan { track: tr }, -1.0, 1.0),
                },
            };
            let mut points = Vec::new();
            let mut tick = 0u32;
            for _ in 0..r.below(6) {
                tick += 1 + r.below(2000) as u32;
                points.push(ShapePoint {
                    tick,
                    value: r.float(lo, hi) as f32,
                    curve: r.pick(&[
                        Curve::Smooth,
                        Curve::Linear,
                        Curve::Hold,
                        Curve::Stairs,
                        Curve::Pulse,
                        Curve::Wave,
                    ]),
                });
            }
            p.shapes.push(Shape {
                id: ShapeId(next),
                target,
                points,
            });
            next += 1;
        }
        if r.below(2) == 0 {
            let start = r.below(10_000) as u32;
            p.loop_region = LoopRegion {
                start,
                end: start + 1 + r.below(10_000) as u32,
                enabled: r.below(2) == 0,
            };
        }
        crate::validate::sort_canonical(&mut p);
        (p, next)
    }

    #[test]
    fn round_trip_random_projects() {
        for seed in 1..=500u64 {
            let (p, next) = random_project(seed);
            let text = emit(&p, next).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
            let (back, back_next) =
                parse(&text).unwrap_or_else(|e| panic!("seed {seed}: {e}\n{text}"));
            assert_eq!(back, p, "seed {seed}");
            assert_eq!(back_next, next, "seed {seed}");
            assert_eq!(
                emit(&back, back_next).unwrap(),
                text,
                "seed {seed}: not canonical"
            );
        }
    }

    #[test]
    fn version_3_text_loads_unchanged() {
        let text = emit(&Project::empty(), FIRST_ID)
            .unwrap()
            .replace("format_version = 4", "format_version = 3");
        let (p, _) = parse(&text).unwrap();
        assert_eq!(p, Project::empty());
    }

    #[test]
    fn empty_project_text_is_stable() {
        let text = emit(&Project::empty(), FIRST_ID).unwrap();
        assert_eq!(
            text,
            "format_version = 4\nnext_id = 1\ntempo_bpm = 120.0\ntime_sig_num = 4\n\n\
             [metronome]\nenabled = false\ngain_db = -6.0\n\n\
             [[tracks]]\nid = 0\nname = \"Master\"\n\
             mix = { volume_db = 0.0, pan = 0.0, mute = false, solo = false }\n\n\
             [timeline]\nloop = { start = 0, end = 0, enabled = false }\n"
        );
    }

    #[test]
    fn one_note_per_line() {
        let (mut p, _) = random_project(7);
        let ch = ChannelId(100_000);
        p.channels.push(Arc::new(Channel {
            id: ch,
            name: "Kick".into(),
            root_key: 36,
            track: TrackId::MASTER,
            mix: Mix::default(),
            instrument: Instrument::Synth(SynthParams::default()),
            choke_group: 0,
        }));
        let mut pat = Pattern::new(PatternId(100_001), "P".into(), ch);
        pat.notes.extend([
            Note {
                id: NoteId(100_002),
                start: 0,
                len: 240,
                key: 36,
                vel: 100,
                off: 0,
                repeat: 1,
            },
            Note {
                id: NoteId(100_003),
                start: 960,
                len: 240,
                key: 36,
                vel: 100,
                off: 0,
                repeat: 1,
            },
        ]);
        p.patterns.push(Arc::new(pat.clone()));
        crate::validate::sort_canonical(&mut p);
        let before = emit(&p, 200_000).unwrap();

        // Move one note: exactly one line changes.
        let i = p
            .patterns
            .iter()
            .position(|x| x.id == PatternId(100_001))
            .unwrap();
        Arc::make_mut(&mut p.patterns[i]).notes[1].start = 1200;
        let after = emit(&p, 200_000).unwrap();
        let changed = before
            .lines()
            .zip(after.lines())
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(before.lines().count(), after.lines().count());
        assert_eq!(changed, 1);
    }

    #[test]
    fn negative_zero_and_floats_are_canonical() {
        assert_eq!(float(-0.0), "0.0");
        assert_eq!(float(1.0), "1.0");
        assert_eq!(float(133.33), "133.33");
        assert_eq!(float(1e-7), "1e-7");
        for v in [1e-7, 1e21, -96.0, 0.1, 1.0 / 3.0] {
            let t = format!("v = {}", float(v));
            let back: toml::Table = toml::from_str(&t).unwrap();
            assert_eq!(back["v"].as_float(), Some(v), "{t}");
        }
    }

    #[test]
    fn rejects_newer_version_missing_version_and_unknown_keys() {
        let text = emit(&Project::empty(), 1).unwrap();
        let newer = text.replace("format_version = 4", "format_version = 5");
        assert_eq!(
            parse(&newer).unwrap_err(),
            FormatError::TooNew {
                found: 5,
                supported: 4
            }
        );
        let missing = text.replace("format_version = 4\n", "");
        assert_eq!(parse(&missing).unwrap_err(), FormatError::MissingVersion);
        let unknown = text.replace("time_sig_num = 4", "time_sig_num = 4\nswing = 3");
        assert!(matches!(
            parse(&unknown).unwrap_err(),
            FormatError::Syntax(_)
        ));
        let unknown_inner = text.replace("enabled = false", "enabled = false\nvolume = 1");
        assert!(matches!(
            parse(&unknown_inner).unwrap_err(),
            FormatError::Syntax(_)
        ));
    }

    #[test]
    fn rejects_invalid_values_and_unknown_instrument_fields() {
        let (p, next) = random_project(42);
        let text = emit(&p, next).unwrap();
        let bad_tempo = text.replacen(
            &format!("tempo_bpm = {}", float(p.tempo_bpm)),
            "tempo_bpm = 5000.0",
            1,
        );
        assert!(matches!(
            parse(&bad_tempo).unwrap_err(),
            FormatError::Invalid(_)
        ));

        let mut q = Project::empty();
        q.channels.push(Arc::new(Channel {
            id: ChannelId(1),
            name: "S".into(),
            root_key: 60,
            track: TrackId::MASTER,
            mix: Mix::default(),
            instrument: Instrument::Synth(SynthParams::default()),
            choke_group: 0,
        }));
        let t = emit(&q, 2)
            .unwrap()
            .replace("kind = \"synth\"", "kind = \"synth\"\nbogus = 1");
        assert!(matches!(parse(&t).unwrap_err(), FormatError::Syntax(_)));
    }

    #[test]
    fn version_1_files_load_with_milestone_b_defaults() {
        let v1 = "format_version = 1\nnext_id = 5\ntempo_bpm = 120.0\ntime_sig_num = 4\n\n\
                  [metronome]\nenabled = false\ngain_db = -6.0\n\n\
                  [[channels]]\nid = 1\nname = \"Kick\"\nroot_key = 36\ntrack = 0\n\
                  mix = { volume_db = 0.0, pan = 0.0, mute = false, solo = false }\n\n\
                  [channels.instrument]\nkind = \"synth\"\n\
                  osc1 = { wave = \"sine\", semitones = 0.0, cents = 0.0 }\n\
                  osc2 = { wave = \"sine\", semitones = 0.0, cents = 0.0 }\n\
                  osc_mix = 0.0\ncutoff_hz = 2000.0\nresonance = 0.2\nfilter_env_octaves = 0.0\n\
                  amp_env = { attack_ms = 1.0, decay_ms = 100.0, sustain = 0.0, release_ms = 50.0 }\n\
                  filter_env = { attack_ms = 1.0, decay_ms = 100.0, sustain = 0.0, release_ms = 50.0 }\n\
                  gain_db = 0.0\n\n\
                  [[patterns]]\nid = 2\nname = \"P\"\nlength_steps = 16\nstep_ticks = 240\n\n\
                  [[patterns.notes]]\nchannel = 1\nnotes = [\n  { id = 3, start = 0, len = 240, key = 36, vel = 100 },\n]\n\n\
                  [[tracks]]\nid = 0\nname = \"Master\"\n\
                  mix = { volume_db = 0.0, pan = 0.0, mute = false, solo = false }\n";
        let (p, next) = parse(v1).unwrap();
        assert_eq!(next, 6); // the converted clip took id 5
        let n = p.patterns[0].notes[0];
        assert_eq!((n.off, n.repeat), (0, 1));
        assert_eq!(p.patterns[0].swing, 0);
        assert_eq!(p.channels[0].choke_group, 0);
        assert!(p.samples.is_empty() && p.tracks[0].sends.is_empty());
        // Converted to the timeline model (20.5): the pattern is the Kick's
        // content, placed once at 0 and looped.
        assert_eq!(p.patterns[0].instrument, ChannelId(1));
        assert_eq!(p.clips.len(), 1);
        assert_eq!(
            (p.clips[0].instrument, p.clips[0].start, p.clips[0].len),
            (ChannelId(1), 0, 3840)
        );
        assert_eq!(
            p.loop_region,
            LoopRegion {
                start: 0,
                end: 3840,
                enabled: true
            }
        );
        // Saving writes version 3 with the same note line as version 1.
        let out = emit(&p, next).unwrap();
        assert!(out.starts_with("format_version = 4\n"));
        assert!(out.contains("  { id = 3, start = 0, len = 240, key = 36, vel = 100 },\n"));
    }

    #[test]
    fn version_2_patterns_and_playlist_become_instrument_clips() {
        let synth = "[channels.instrument]\nkind = \"synth\"\n\
            osc1 = { wave = \"sine\", semitones = 0.0, cents = 0.0 }\n\
            osc2 = { wave = \"sine\", semitones = 0.0, cents = 0.0 }\n\
            osc_mix = 0.0\ncutoff_hz = 2000.0\nresonance = 0.2\nfilter_env_octaves = 0.0\n\
            amp_env = { attack_ms = 1.0, decay_ms = 100.0, sustain = 0.0, release_ms = 50.0 }\n\
            filter_env = { attack_ms = 1.0, decay_ms = 100.0, sustain = 0.0, release_ms = 50.0 }\n\
            gain_db = 0.0\n";
        let mix = "mix = { volume_db = 0.0, pan = 0.0, mute = false, solo = false }\n";
        let v2 = format!(
            "format_version = 2\nnext_id = 20\ntempo_bpm = 140.0\ntime_sig_num = 4\n\n\
             [metronome]\nenabled = false\ngain_db = -6.0\n\n\
             [[channels]]\nid = 1\nname = \"Kick\"\nroot_key = 36\ntrack = 0\n{mix}\n{synth}\n\
             [[channels]]\nid = 2\nname = \"Hat\"\nroot_key = 42\ntrack = 0\n{mix}\n{synth}\n\
             [[patterns]]\nid = 3\nname = \"Beat\"\nlength_steps = 16\nstep_ticks = 240\n\n\
             [[patterns.notes]]\nchannel = 1\nnotes = [\n  {{ id = 4, start = 0, len = 240, key = 36, vel = 100 }},\n]\n\n\
             [[patterns.notes]]\nchannel = 2\nnotes = [\n  {{ id = 5, start = 240, len = 240, key = 42, vel = 90 }},\n]\n\n\
             [[tracks]]\nid = 0\nname = \"Master\"\n{mix}\n\
             [[playlist]]\nid = 6\nname = \"Drums\"\nclips = [\n\
               {{ id = 7, pattern = 3, start = 0, len = 3840 }},\n\
               {{ id = 8, pattern = 3, start = 3840, len = 3840 }},\n]\n"
        );
        let (p, next) = parse(&v2).unwrap();
        // One content per instrument; the first keeps the old id.
        assert_eq!(p.patterns.len(), 2);
        assert_eq!(
            (p.patterns[0].id, p.patterns[0].instrument),
            (PatternId(3), ChannelId(1))
        );
        assert_eq!(p.patterns[1].instrument, ChannelId(2));
        assert_eq!(p.patterns[0].notes.len() + p.patterns[1].notes.len(), 2);
        // Two playlist clips x two instruments = four clips, linked per row.
        assert_eq!(p.clips.len(), 4);
        for c in &p.clips {
            let content = p.pattern(c.pattern).unwrap();
            assert_eq!(content.instrument, c.instrument);
        }
        assert!(next > p.max_id());
        // The converted project round-trips in version 4.
        let text = emit(&p, next).unwrap();
        assert_eq!(parse(&text).unwrap().0, p);
    }

    #[test]
    fn next_id_never_below_max_id_plus_one() {
        let (p, next) = random_project(99);
        let text = emit(&p, next)
            .unwrap()
            .replace(&format!("next_id = {next}"), "next_id = 1");
        let (_, n) = parse(&text).unwrap();
        assert_eq!(n, p.max_id() + 1);
    }
}
