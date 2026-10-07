// SPDX-License-Identifier: GPL-3.0-or-later
//! Whole-project validation. `apply()` (in `ui`) and the file loader both
//! use it, so every document the program holds can be saved (6, 7.2).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

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
        for Insert::Clap(r) in &t.inserts {
            check_clap(r, &mut ids)?;
        }
    }

    let mut channel_ids = HashSet::new();
    for (i, c) in p.channels.iter().enumerate() {
        unique(&mut ids, c.id.0)?;
        if i > 0 && c.id <= p.channels[i - 1].id {
            return Err(ValidationError::NotSorted {
                what: "channels".into(),
            });
        }
        channel_ids.insert(c.id);
        check_name("channel.name", &c.name)?;
        int_range("channel.root_key", c.root_key as u64, 0, 127)?;
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
        }
    }

    let mut total_notes = 0usize;
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
        let count = pat.note_count();
        if count > MAX_NOTES_PER_PATTERN {
            return Err(ValidationError::TooMany {
                what: "notes in pattern".into(),
                max: MAX_NOTES_PER_PATTERN,
            });
        }
        total_notes += count;
        for (j, cn) in pat.notes.iter().enumerate() {
            if !channel_ids.contains(&cn.channel) {
                return Err(ValidationError::MissingRef {
                    what: "channel".into(),
                    id: cn.channel.0,
                });
            }
            if j > 0 && cn.channel <= pat.notes[j - 1].channel {
                return Err(ValidationError::NotSorted {
                    what: "pattern channels".into(),
                });
            }
            for (k, n) in cn.notes.iter().enumerate() {
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
                if k > 0 {
                    let a = &cn.notes[k - 1];
                    if (a.start, a.key, a.id) >= (n.start, n.key, n.id) {
                        return Err(ValidationError::NotSorted {
                            what: "notes".into(),
                        });
                    }
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
    Ok(())
}

/// Puts every collection into the canonical order `validate` requires:
/// tracks (master first), channels, and patterns by id; pattern channel
/// lists by channel id; notes by `(start, key, id)`; plugin params by id.
/// `apply()` may call this after an edit instead of keeping order by hand.
pub fn sort_canonical(p: &mut Project) {
    use std::sync::Arc;
    p.tracks.sort_by_key(|t| t.id);
    for t in &mut p.tracks {
        if t.inserts
            .iter()
            .any(|Insert::Clap(r)| !r.params.is_sorted_by_key(|v| v.id))
        {
            for Insert::Clap(r) in &mut Arc::make_mut(t).inserts {
                r.params.sort_by_key(|v| v.id);
            }
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
        let sorted = pat.notes.is_sorted_by_key(|cn| cn.channel)
            && pat
                .notes
                .iter()
                .all(|cn| cn.notes.is_sorted_by_key(|n| (n.start, n.key, n.id)));
        if !sorted {
            let pat = Arc::make_mut(pat);
            pat.notes.sort_by_key(|cn| cn.channel);
            for cn in &mut pat.notes {
                cn.notes.sort_by_key(|n| (n.start, n.key, n.id));
            }
        }
    }
}
