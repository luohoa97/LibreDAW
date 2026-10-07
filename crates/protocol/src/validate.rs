// SPDX-License-Identifier: GPL-3.0-or-later
//! Whole-project validation. `apply()` (in `ui`) and the file loader both
//! use it, so every document the program holds can be saved (6, 7.2).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::beats::{Bass808Param, BuiltinFx, SamplerParam};
use crate::consts::*;
use crate::ids::TrackId;
use crate::model::{ClapRef, Insert, Instrument, Mix, Project, SynthParam, SynthParams};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum ValidationError {
    /// A number is NaN, infinite, or outside its range.
    OutOfRange { field: String, value: f64 },
    /// A count exceeds a fixed maximum.
    TooMany { what: String, max: usize },
    /// An id appears twice.
    DuplicateId { id: u32 },
    /// A reference points at an entity that does not exist.
    MissingRef { what: String, id: u32 },
    /// A name or string field is empty, too long, or has control characters.
    BadString { field: String },
    /// Notes or entities are not in canonical order.
    NotSorted { what: String },
    /// The master track is missing or not first.
    BadMaster,
    /// Sends and sidechains form a loop (17.2).
    RoutingCycle,
    /// Two clips on one instrument's row overlap.
    Overlap { what: String, id: u32 },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::OutOfRange { field, value } => {
                write!(f, "{field} out of range: {value}")
            }
            ValidationError::TooMany { what, max } => write!(f, "too many {what} (max {max})"),
            ValidationError::DuplicateId { id } => write!(f, "duplicate id {id}"),
            ValidationError::MissingRef { what, id } => write!(f, "missing {what} {id}"),
            ValidationError::BadString { field } => write!(f, "invalid text in {field}"),
            ValidationError::NotSorted { what } => write!(f, "{what} not sorted"),
            ValidationError::BadMaster => write!(f, "master track must be track 0 and first"),
            ValidationError::RoutingCycle => write!(f, "sends and sidechains form a loop"),
            ValidationError::Overlap { what, id } => write!(f, "{what} overlap at {id}"),
        }
    }
}

impl std::error::Error for ValidationError {}

fn range(field: &str, v: f64, lo: f64, hi: f64) -> Result<(), ValidationError> {
    if v.is_finite() && v >= lo && v <= hi {
        Ok(())
    } else {
        Err(ValidationError::OutOfRange {
            field: field.to_string(),
            value: v,
        })
    }
}

fn int_range(field: &str, v: u64, lo: u64, hi: u64) -> Result<(), ValidationError> {
    range(field, v as f64, lo as f64, hi as f64)
}

/// Names: 1 to `MAX_NAME_CHARS` characters, no control characters.
pub fn check_name(field: &str, s: &str) -> Result<(), ValidationError> {
    let n = s.chars().count();
    if n == 0 || n > MAX_NAME_CHARS || s.chars().any(char::is_control) {
        return Err(ValidationError::BadString {
            field: field.to_string(),
        });
    }
    Ok(())
}

/// A file name inside the bundle: `[A-Za-z0-9._-]`, no leading dot, no
/// path separators, at most 128 bytes.
pub fn check_file_name(field: &str, s: &str) -> Result<(), ValidationError> {
    let ok = !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(ValidationError::BadString {
            field: field.to_string(),
        })
    }
}

fn check_mix(what: &str, m: &Mix) -> Result<(), ValidationError> {
    range(
        &format!("{what}.volume_db"),
        m.volume_db,
        MIN_GAIN_DB,
        MAX_GAIN_DB,
    )?;
    range(&format!("{what}.pan"), m.pan, -1.0, 1.0)
}

pub fn check_synth(p: &SynthParams) -> Result<(), ValidationError> {
    for sp in SynthParam::ALL {
        let (lo, hi) = sp.range();
        range(&format!("synth.{sp:?}"), p.get(sp), lo, hi)?;
    }
    Ok(())
}

fn check_clap(r: &ClapRef, ids: &mut HashSet<u32>) -> Result<(), ValidationError> {
    unique(ids, r.instance.0)?;
    check_name("plugin_id", &r.plugin_id)?;
    if r.plugin_version.chars().count() > MAX_NAME_CHARS
        || r.plugin_version.chars().any(char::is_control)
    {
        return Err(ValidationError::BadString {
            field: "plugin_version".into(),
        });
    }
    if let Some(f) = &r.state_file {
        check_file_name("state_file", f)?;
    }
    for w in r.params.windows(2) {
        if w[0].id >= w[1].id {
            return Err(ValidationError::NotSorted {
                what: "plugin params".into(),
            });
        }
    }
    for p in &r.params {
        if !p.value.is_finite() {
            return Err(ValidationError::OutOfRange {
                field: "plugin param".into(),
                value: p.value,
            });
        }
    }
    Ok(())
}

fn unique(ids: &mut HashSet<u32>, id: u32) -> Result<(), ValidationError> {
    if id == 0 || !ids.insert(id) {
        return Err(ValidationError::DuplicateId { id });
    }
    Ok(())
}

/// Checks every rule a saved project must satisfy.
pub fn validate(p: &Project) -> Result<(), ValidationError> {
    range("tempo_bpm", p.tempo_bpm, MIN_TEMPO_BPM, MAX_TEMPO_BPM)?;
    int_range(
        "time_sig_num",
        p.time_sig_num as u64,
        MIN_TIME_SIG_NUM as u64,
        MAX_TIME_SIG_NUM as u64,
    )?;
    range(
        "metronome.gain_db",
        p.metronome.gain_db,
        MIN_GAIN_DB,
        MAX_GAIN_DB,
    )?;

    if p.channels.len() > MAX_CHANNELS {
        return Err(ValidationError::TooMany {
            what: "channels".into(),
            max: MAX_CHANNELS,
        });
    }
    if p.tracks.len() > TRACK_SLOTS {
        return Err(ValidationError::TooMany {
            what: "tracks".into(),
            max: MAX_TRACKS,
        });
    }
    if p.patterns.len() > MAX_PATTERNS {
        return Err(ValidationError::TooMany {
            what: "patterns".into(),
            max: MAX_PATTERNS,
        });
    }
    match p.tracks.first() {
        Some(t) if t.id == TrackId::MASTER => {}
        _ => return Err(ValidationError::BadMaster),
    }

    let mut ids = HashSet::new();

    // Samples (17.2): unique hashes, sorted.
    if p.samples.len() > MAX_SAMPLES {
        return Err(ValidationError::TooMany {
            what: "samples".into(),
            max: MAX_SAMPLES,
        });
    }
    let mut sample_hashes = HashSet::new();
    for (i, s) in p.samples.iter().enumerate() {
        check_hash("sample.hash", &s.hash)?;
        check_name("sample.orig_name", &s.orig_name)?;
        if i > 0 && s.hash <= p.samples[i - 1].hash {
            return Err(ValidationError::NotSorted {
                what: "samples".into(),
            });
        }
        sample_hashes.insert(s.hash.as_str());
    }

    let mut track_ids = HashSet::new();
    for (i, t) in p.tracks.iter().enumerate() {
        if i > 0 {
            unique(&mut ids, t.id.0)?;
            if t.id <= p.tracks[i - 1].id {
                return Err(ValidationError::NotSorted {
                    what: "tracks".into(),
                });
            }
        }
        track_ids.insert(t.id);
        check_name("track.name", &t.name)?;
        check_mix("track.mix", &t.mix)?;
        if t.inserts.len() > MAX_INSERTS {
            return Err(ValidationError::TooMany {
                what: "inserts".into(),
                max: MAX_INSERTS,
            });
        }
        for ins in &t.inserts {
            match ins {
                Insert::Clap(r) => check_clap(r, &mut ids)?,
                Insert::Builtin { instance, fx } => {
                    unique(&mut ids, instance.0)?;
                    check_fx(fx)?;
                }
            }
        }
        if t.sends.len() > MAX_SENDS {
            return Err(ValidationError::TooMany {
                what: "sends".into(),
                max: MAX_SENDS,
            });
        }
        for (j, s) in t.sends.iter().enumerate() {
            range("send.level_db", s.level_db, MIN_GAIN_DB, MAX_GAIN_DB)?;
            if j > 0 && s.to <= t.sends[j - 1].to {
                return Err(ValidationError::NotSorted {
                    what: "sends".into(),
                });
            }
        }
    }
    // References from sends and sidechains, and an acyclic routing graph
    // (17.2): every track feeds the master; sends and sidechains add edges.
    let mut edges: Vec<(TrackId, TrackId)> = Vec::new();
    for t in &p.tracks {
        if t.id != TrackId::MASTER {
            edges.push((t.id, TrackId::MASTER));
        }
        for s in &t.sends {
            if s.to == t.id || s.to == TrackId::MASTER || !track_ids.contains(&s.to) {
                return Err(ValidationError::MissingRef {
                    what: "send target".into(),
                    id: s.to.0,
                });
            }
            edges.push((t.id, s.to));
        }
        for ins in &t.inserts {
            if let Insert::Builtin {
                fx:
                    BuiltinFx::Compressor {
                        sidechain: Some(src),
                        ..
                    },
                ..
            } = ins
            {
                if *src == t.id || !track_ids.contains(src) {
                    return Err(ValidationError::MissingRef {
                        what: "sidechain source".into(),
                        id: src.0,
                    });
                }
                edges.push((*src, t.id));
            }
        }
    }
    if has_cycle(&edges) {
        return Err(ValidationError::RoutingCycle);
    }
    // Effect pools (17.2).
    let mut counts = [0usize; 6];
    for t in &p.tracks {
        for ins in &t.inserts {
            if let Insert::Builtin { fx, .. } = ins {
                counts[fx.kind() as usize] += 1;
            }
        }
    }
    let pools = [
        ("EQ", FX_POOL_EQ),
        ("compressors", FX_POOL_COMPRESSOR),
        ("saturators", FX_POOL_SATURATOR),
        ("reverbs", FX_POOL_REVERB),
        ("delays", FX_POOL_DELAY),
        ("limiters", FX_POOL_LIMITER),
    ];
    for (n, (what, max)) in counts.iter().zip(pools) {
        if *n > max {
            return Err(ValidationError::TooMany {
                what: what.into(),
                max,
            });
        }
    }

    let mut channel_roots = std::collections::HashMap::new();
    for (i, c) in p.channels.iter().enumerate() {
        unique(&mut ids, c.id.0)?;
        if i > 0 && c.id <= p.channels[i - 1].id {
            return Err(ValidationError::NotSorted {
                what: "channels".into(),
            });
        }
        channel_roots.insert(c.id, c.root_key);
        check_name("channel.name", &c.name)?;
        int_range("channel.root_key", c.root_key as u64, 0, 127)?;
        int_range(
            "channel.choke_group",
            c.choke_group as u64,
            0,
            MAX_CHOKE_GROUP as u64,
        )?;
        check_mix("channel.mix", &c.mix)?;
        if !track_ids.contains(&c.track) {
            return Err(ValidationError::MissingRef {
                what: "track".into(),
                id: c.track.0,
            });
        }
        match &c.instrument {
            Instrument::Synth(s) => check_synth(s)?,
            Instrument::Clap(r) => check_clap(r, &mut ids)?,
            Instrument::Sampler(s) => {
                for sp in SamplerParam::ALL {
                    let (lo, hi) = sp.range();
                    range(&format!("sampler.{sp:?}"), s.params.get(*sp), lo, hi)?;
                }
                if s.params.end <= s.params.start {
                    return Err(ValidationError::OutOfRange {
                        field: "sampler.end".into(),
                        value: s.params.end,
                    });
                }
                if let Some(h) = &s.sample
                    && !sample_hashes.contains(h.as_str())
                {
                    return Err(ValidationError::BadString {
                        field: "sampler.sample (not in samples)".into(),
                    });
                }
            }
            Instrument::Bass808(b) => {
                for bp in Bass808Param::ALL {
                    let (lo, hi) = bp.range();
                    range(&format!("bass808.{bp:?}"), b.params.get(*bp), lo, hi)?;
                }
            }
        }
    }

    let mut total_notes = 0usize;
    let mut pattern_owner = std::collections::HashMap::new();
    for (i, pat) in p.patterns.iter().enumerate() {
        unique(&mut ids, pat.id.0)?;
        if i > 0 && pat.id <= p.patterns[i - 1].id {
            return Err(ValidationError::NotSorted {
                what: "patterns".into(),
            });
        }
        check_name("pattern.name", &pat.name)?;
        int_range(
            "pattern.length_steps",
            pat.length_steps as u64,
            MIN_STEPS as u64,
            MAX_STEPS as u64,
        )?;
        int_range(
            "pattern.step_ticks",
            pat.step_ticks as u64,
            1,
            (PPQ * 4) as u64,
        )?;
        int_range("pattern.swing", pat.swing as u64, 0, MAX_SWING as u64)?;
        let count = pat.note_count();
        if count > MAX_NOTES_PER_PATTERN {
            return Err(ValidationError::TooMany {
                what: "notes in pattern".into(),
                max: MAX_NOTES_PER_PATTERN,
            });
        }
        total_notes += count;
        let Some(&root) = channel_roots.get(&pat.instrument) else {
            return Err(ValidationError::MissingRef {
                what: "instrument".into(),
                id: pat.instrument.0,
            });
        };
        pattern_owner.insert(pat.id, (pat.instrument, pat.length_ticks()));
        for (k, n) in pat.notes.iter().enumerate() {
            unique(&mut ids, n.id.0)?;
            int_range("note.len", n.len as u64, 1, MAX_TICK as u64)?;
            int_range(
                "note.end",
                n.start as u64 + n.len as u64,
                1,
                MAX_TICK as u64,
            )?;
            int_range("note.key", n.key as u64, 0, 127)?;
            int_range("note.vel", n.vel as u64, 1, 127)?;
            range(
                "note.off",
                n.off as f64,
                -(MAX_STEP_OFFSET as f64),
                MAX_STEP_OFFSET as f64,
            )?;
            if !RATCHETS.contains(&n.repeat) || !n.len.is_multiple_of(n.repeat as u32) {
                return Err(ValidationError::OutOfRange {
                    field: "note.repeat".into(),
                    value: n.repeat as f64,
                });
            }
            if n.off != 0 && n.key as i16 != root as i16 + n.off as i16 {
                return Err(ValidationError::OutOfRange {
                    field: "note.off".into(),
                    value: n.off as f64,
                });
            }
            if k > 0 {
                let a = &pat.notes[k - 1];
                if (a.start, a.key, a.id) >= (n.start, n.key, n.id) {
                    return Err(ValidationError::NotSorted {
                        what: "notes".into(),
                    });
                }
            }
        }
    }
    if total_notes > MAX_NOTES_PER_PROJECT {
        return Err(ValidationError::TooMany {
            what: "notes in project".into(),
            max: MAX_NOTES_PER_PROJECT,
        });
    }

    // Clips on the timeline (20.2).
    if p.clips.len() > MAX_CLIPS {
        return Err(ValidationError::TooMany {
            what: "clips".into(),
            max: MAX_CLIPS,
        });
    }
    for (j, c) in p.clips.iter().enumerate() {
        unique(&mut ids, c.id.0)?;
        let Some(&(owner, content_len)) = pattern_owner.get(&c.pattern) else {
            return Err(ValidationError::MissingRef {
                what: "clip content".into(),
                id: c.pattern.0,
            });
        };
        if owner != c.instrument {
            return Err(ValidationError::MissingRef {
                what: "clip content of this instrument".into(),
                id: c.pattern.0,
            });
        }
        int_range("clip.len", c.len as u64, 1, MAX_TICK as u64)?;
        int_range(
            "clip.end",
            c.start as u64 + c.len as u64,
            1,
            MAX_TICK as u64,
        )?;
        if c.offset >= content_len.max(1) {
            return Err(ValidationError::OutOfRange {
                field: "clip.offset".into(),
                value: c.offset as f64,
            });
        }
        if j > 0 {
            let a = &p.clips[j - 1];
            if (a.instrument, a.start, a.id) >= (c.instrument, c.start, c.id) {
                return Err(ValidationError::NotSorted {
                    what: "clips".into(),
                });
            }
            if a.instrument == c.instrument && a.end() > c.start {
                return Err(ValidationError::Overlap {
                    what: "clips".into(),
                    id: c.id.0,
                });
            }
        }
    }
    let lr = p.loop_region;
    if lr.enabled || lr.end != 0 || lr.start != 0 {
        int_range("loop.end", lr.end as u64, 1, MAX_TICK as u64)?;
        if lr.start >= lr.end {
            return Err(ValidationError::OutOfRange {
                field: "loop.start".into(),
                value: lr.start as f64,
            });
        }
    }
    Ok(())
}

/// SHA-256 as 64 lowercase hex digits.
pub fn check_hash(field: &str, s: &str) -> Result<(), ValidationError> {
    if s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(ValidationError::BadString {
            field: field.to_string(),
        })
    }
}

pub fn check_fx(fx: &BuiltinFx) -> Result<(), ValidationError> {
    for i in 0..fx.param_count() {
        let (lo, hi) = fx.param_range(i).expect("index below param_count");
        let v = fx.param(i).expect("index below param_count");
        range(&format!("fx.{:?}.{i}", fx.kind()), v, lo, hi)?;
    }
    Ok(())
}

/// Depth-first search for a cycle in a small edge list.
fn has_cycle(edges: &[(TrackId, TrackId)]) -> bool {
    use std::collections::HashMap;
    let mut adj: HashMap<TrackId, Vec<TrackId>> = HashMap::new();
    for &(a, b) in edges {
        adj.entry(a).or_default().push(b);
    }
    // 0 = unvisited, 1 = on stack, 2 = done
    let mut state: HashMap<TrackId, u8> = HashMap::new();
    fn visit(
        n: TrackId,
        adj: &HashMap<TrackId, Vec<TrackId>>,
        state: &mut HashMap<TrackId, u8>,
    ) -> bool {
        match state.get(&n) {
            Some(1) => return true,
            Some(2) => return false,
            _ => {}
        }
        state.insert(n, 1);
        for &m in adj.get(&n).map(Vec::as_slice).unwrap_or(&[]) {
            if visit(m, adj, state) {
                return true;
            }
        }
        state.insert(n, 2);
        false
    }
    let nodes: Vec<TrackId> = adj.keys().copied().collect();
    nodes.into_iter().any(|n| visit(n, &adj, &mut state))
}

/// Puts every collection into the canonical order `validate` requires:
/// tracks (master first), channels, and patterns by id; pattern channel
/// lists by channel id; notes by `(start, key, id)`; plugin params by id.
/// `apply()` may call this after an edit instead of keeping order by hand.
pub fn sort_canonical(p: &mut Project) {
    use std::sync::Arc;
    p.tracks.sort_by_key(|t| t.id);
    for t in &mut p.tracks {
        let unsorted = t.inserts.iter().any(|i| match i {
            Insert::Clap(r) => !r.params.is_sorted_by_key(|v| v.id),
            Insert::Builtin { .. } => false,
        }) || !t.sends.is_sorted_by_key(|s| s.to);
        if unsorted {
            let t = Arc::make_mut(t);
            for i in &mut t.inserts {
                if let Insert::Clap(r) = i {
                    r.params.sort_by_key(|v| v.id);
                }
            }
            t.sends.sort_by_key(|s| s.to);
        }
    }
    p.channels.sort_by_key(|c| c.id);
    for c in &mut p.channels {
        if let Instrument::Clap(r) = &c.instrument
            && !r.params.is_sorted_by_key(|v| v.id)
            && let Instrument::Clap(r) = &mut Arc::make_mut(c).instrument
        {
            r.params.sort_by_key(|v| v.id);
        }
    }
    p.patterns.sort_by_key(|pat| pat.id);
    for pat in &mut p.patterns {
        if !pat.notes.is_sorted_by_key(|n| (n.start, n.key, n.id)) {
            Arc::make_mut(pat)
                .notes
                .sort_by_key(|n| (n.start, n.key, n.id));
        }
    }
    p.samples.sort_by(|a, b| a.hash.cmp(&b.hash));
    p.clips.sort_by_key(|c| (c.instrument, c.start, c.id));
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::beats::{BuiltinFx, BuiltinFxKind};
    use crate::ids::{ChannelId, ClipId, InstanceId, NoteId, PatternId};
    use crate::model::{Channel, Clip, Note, Pattern, Send, Track};

    fn track(id: u32) -> Arc<Track> {
        Arc::new(Track {
            id: TrackId(id),
            name: "T".into(),
            mix: Mix::default(),
            inserts: Vec::new(),
            sends: Vec::new(),
        })
    }

    fn base() -> Project {
        let mut p = Project::empty();
        p.tracks.push(track(1));
        p.tracks.push(track(2));
        p.channels.push(Arc::new(Channel {
            id: ChannelId(3),
            name: "Hat".into(),
            root_key: 42,
            track: TrackId(1),
            mix: Mix::default(),
            instrument: Instrument::Synth(SynthParams::default()),
            choke_group: 1,
        }));
        let mut pat = Pattern::new(PatternId(4), "P".into(), ChannelId(3));
        pat.notes.push(Note {
            id: NoteId(5),
            start: 0,
            len: 240,
            key: 45,
            vel: 100,
            off: 3,
            repeat: 4,
        });
        p.patterns.push(Arc::new(pat));
        let clip = |id, start| Clip {
            id: ClipId(id),
            instrument: ChannelId(3),
            pattern: PatternId(4),
            start,
            len: 3840,
            offset: 0,
            muted: false,
        };
        p.clips = vec![clip(7, 0), clip(8, 3840)];
        p.loop_region = crate::model::LoopRegion {
            start: 0,
            end: 7680,
            enabled: true,
        };
        p
    }

    #[test]
    fn milestone_b_base_is_valid() {
        validate(&base()).unwrap();
    }

    #[test]
    fn rejects_send_and_sidechain_loops() {
        let mut p = base();
        Arc::make_mut(&mut p.tracks[1]).sends.push(Send {
            to: TrackId(2),
            level_db: 0.0,
            pre_fader: false,
        });
        validate(&p).unwrap();
        let mut fx = BuiltinFx::new(BuiltinFxKind::Compressor);
        if let BuiltinFx::Compressor { sidechain, .. } = &mut fx {
            *sidechain = Some(TrackId(2));
        }
        Arc::make_mut(&mut p.tracks[1])
            .inserts
            .push(Insert::Builtin {
                instance: InstanceId(9),
                fx,
            });
        assert_eq!(validate(&p), Err(ValidationError::RoutingCycle));
    }

    #[test]
    fn rejects_overlapping_clips_bad_ratchets_and_bad_offsets() {
        let mut p = base();
        p.clips[1].start = 3000;
        assert!(matches!(validate(&p), Err(ValidationError::Overlap { .. })));

        let mut p = base();
        Arc::make_mut(&mut p.patterns[0]).notes[0].repeat = 5;
        assert!(matches!(
            validate(&p),
            Err(ValidationError::OutOfRange { .. })
        ));

        let mut p = base();
        Arc::make_mut(&mut p.patterns[0]).notes[0].key = 46;
        assert!(matches!(
            validate(&p),
            Err(ValidationError::OutOfRange { .. })
        ));
    }

    #[test]
    fn clips_must_use_their_own_instruments_content() {
        let mut p = base();
        p.channels.push(Arc::new(Channel {
            id: ChannelId(10),
            name: "Kick".into(),
            root_key: 36,
            track: TrackId(1),
            mix: Mix::default(),
            instrument: Instrument::Synth(SynthParams::default()),
            choke_group: 0,
        }));
        p.clips[1].instrument = ChannelId(10);
        crate::validate::sort_canonical(&mut p);
        assert!(matches!(
            validate(&p),
            Err(ValidationError::MissingRef { .. })
        ));

        let mut p = base();
        p.clips[0].offset = 99_999;
        assert!(matches!(
            validate(&p),
            Err(ValidationError::OutOfRange { .. })
        ));

        let mut p = base();
        p.loop_region.start = 7680;
        assert!(matches!(
            validate(&p),
            Err(ValidationError::OutOfRange { .. })
        ));
    }

    #[test]
    fn step_predicate_follows_the_pitch_lane() {
        let p = base();
        let n = p.patterns[0].notes[0];
        assert!(n.is_step_note(42, &p.patterns[0]));
        assert!(!n.is_step_note(41, &p.patterns[0]));
    }
}
