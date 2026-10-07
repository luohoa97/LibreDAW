// SPDX-License-Identifier: GPL-3.0-or-later
//! `Document` and `apply()` (SPEC 5, 6, 17.1).
//!
//! `apply` is a pure function from a document and an edit to a new
//! document. It works on a cheap copy of the project (a few `Vec`s of
//! `Arc`s), clones only the touched path with `Arc::make_mut`, puts the
//! result in canonical order, and runs the full `protocol::validate`
//! before returning. So every document it returns can be saved, and a
//! failed edit leaves the input untouched.
//!
//! A batch (`apply_batch`) is atomic: either every edit applies or none
//! does. It is one undo group and one revision step.
//!
//! Choices where the spec leaves room:
//! - `AddChannel` with a CLAP instrument creates two ids, the channel id
//!   first and then the instance id. `created` lists them in that order.
//! - Notes must start inside the pattern (`start < length_ticks`). Moving a
//!   note out of range fails; nothing is clamped.
//! - `SetStep` works on any row, including one that also holds piano roll
//!   notes. The read-only rule of 5.2 is a display rule for the step grid.
//! - `SetStepTicks` rewrites step notes, and removes other notes that
//!   would start at or after the new pattern end (like shortening).

use std::collections::HashSet;
use std::sync::Arc;

use protocol::consts::*;
use protocol::edit::{Edit, EditError, MixValue, NewInstrument, NewNote, set_mix};
use protocol::ids::{ChannelId, FIRST_ID, InstanceId, NoteId, PatternId, TrackId};
use protocol::model::{
    Channel, ChannelNotes, ClapRef, Insert, Instrument, Note, ParamValue, Pattern, Project, Track,
    Wave,
};
use protocol::validate::{ValidationError, check_synth, sort_canonical, validate};

/// Velocity of a step turned on without an explicit velocity.
pub const DEFAULT_STEP_VEL: u8 = 100;

/// The open project plus the things that are not undoable (5.3, 6).
#[derive(Clone, Debug)]
pub struct Document {
    pub project: Arc<Project>,
    /// Next id to hand out. Only grows: undo never lowers it (5.3).
    pub next_id: u32,
    /// Bumped on every replacement of `project` (edit, undo, redo, load).
    pub revision: u64,
}

impl Document {
    /// A new empty project: master track only.
    pub fn new() -> Document {
        Document {
            project: Arc::new(Project::empty()),
            next_id: FIRST_ID,
            revision: 0,
        }
    }

    /// Wraps a loaded project. `next_id` is raised above every id in the
    /// project (17.1).
    pub fn from_project(project: Project, next_id: u32) -> Document {
        let floor = project.max_id().saturating_add(1);
        Document {
            project: Arc::new(project),
            next_id: next_id.max(floor).max(FIRST_ID),
            revision: 0,
        }
    }

    /// Replaces the project root (undo, redo, restore). `next_id` only
    /// grows: `max(current, max id in the new root + 1)` (17.1).
    pub fn with_project(&self, project: Arc<Project>) -> Document {
        let floor = project.max_id().saturating_add(1);
        Document {
            next_id: self.next_id.max(floor),
            revision: self.revision + 1,
            project,
        }
    }
}

impl Default for Document {
    fn default() -> Document {
        Document::new()
    }
}

/// Applies one edit. Returns the new document and the ids it created, in
/// creation order.
pub fn apply(doc: &Document, edit: &Edit) -> Result<(Document, Vec<u32>), EditError> {
    apply_batch(doc, std::slice::from_ref(edit))
}

/// Applies edits in order as one atomic group: one new document, one
/// revision step. If any edit fails, nothing is applied.
pub fn apply_batch(doc: &Document, edits: &[Edit]) -> Result<(Document, Vec<u32>), EditError> {
    let mut w = Work {
        p: (*doc.project).clone(),
        next_id: doc.next_id,
        created: Vec::new(),
    };
    for e in edits {
        apply_one(&mut w, e)?;
    }
    sort_canonical(&mut w.p);
    validate(&w.p)?;
    let out = Document {
        project: Arc::new(w.p),
        next_id: w.next_id,
        revision: doc.revision + 1,
    };
    Ok((out, w.created))
}

/// Records captured plugin state: the new blob name and its bytes, and
/// optionally the plugin version seen (7.5). Not an undo step: the caller
/// merges the result into the current snapshot (`Editor::merge`).
pub fn commit_plugin_state(
    doc: &Document,
    instance: InstanceId,
    state_file: &str,
    bytes: Arc<[u8]>,
    plugin_version: Option<&str>,
) -> Result<Document, EditError> {
    let mut p = (*doc.project).clone();
    let r = clap_mut(&mut p, instance).ok_or_else(|| not_found("plugin instance", instance.0))?;
    r.state_file = Some(state_file.to_string());
    r.state_bytes = Some(bytes);
    if let Some(v) = plugin_version {
        r.plugin_version = v.to_string();
    }
    validate(&p)?;
    Ok(Document {
        project: Arc::new(p),
        next_id: doc.next_id,
        revision: doc.revision + 1,
    })
}

/// Name of the blob file for an instance and generation (7.1).
pub fn state_file_name(instance: InstanceId, generation: u32) -> String {
    format!("{}-{}.bin", instance.0, generation)
}

/// Generation number of a blob file name, if it has our form.
pub fn parse_state_file_name(name: &str) -> Option<(InstanceId, u32)> {
    let stem = name.strip_suffix(".bin")?;
    let (i, g) = stem.split_once('-')?;
    Some((InstanceId(i.parse().ok()?), g.parse().ok()?))
}

/// Generation for the next capture of this instance.
pub fn next_generation(r: &ClapRef) -> u32 {
    r.state_file
        .as_deref()
        .and_then(parse_state_file_name)
        .map(|(_, g)| g + 1)
        .unwrap_or(1)
}

// ---------------------------------------------------------------------------

struct Work {
    p: Project,
    next_id: u32,
    created: Vec<u32>,
}

impl Work {
    fn alloc(&mut self) -> Result<u32, EditError> {
        if self.next_id == u32::MAX {
            return Err(bad("id space exhausted"));
        }
        let id = self.next_id;
        self.next_id += 1;
        self.created.push(id);
        Ok(id)
    }
}

fn bad(what: &str) -> EditError {
    EditError::BadArgument { what: what.into() }
}

fn not_found(what: &str, id: u32) -> EditError {
    EditError::NotFound {
        what: what.into(),
        id,
    }
}

fn out_of_range(field: &str, value: f64) -> EditError {
    EditError::Invalid {
        reason: ValidationError::OutOfRange {
            field: field.into(),
            value,
        },
    }
}

fn too_many(what: &str, max: usize) -> EditError {
    EditError::Invalid {
        reason: ValidationError::TooMany {
            what: what.into(),
            max,
        },
    }
}

fn channel_idx(p: &Project, id: ChannelId) -> Result<usize, EditError> {
    p.channels
        .iter()
        .position(|c| c.id == id)
        .ok_or_else(|| not_found("channel", id.0))
}

fn pattern_idx(p: &Project, id: PatternId) -> Result<usize, EditError> {
    p.patterns
        .iter()
        .position(|c| c.id == id)
        .ok_or_else(|| not_found("pattern", id.0))
}

fn track_idx(p: &Project, id: TrackId) -> Result<usize, EditError> {
    p.tracks
        .iter()
        .position(|c| c.id == id)
        .ok_or_else(|| not_found("track", id.0))
}

fn clap_mut(p: &mut Project, inst: InstanceId) -> Option<&mut ClapRef> {
    if let Some(i) = p
        .channels
        .iter()
        .position(|c| matches!(&c.instrument, Instrument::Clap(r) if r.instance == inst))
        && let Instrument::Clap(r) = &mut Arc::make_mut(&mut p.channels[i]).instrument
    {
        return Some(r);
    }
    for ti in 0..p.tracks.len() {
        if let Some(ii) = p.tracks[ti]
            .inserts
            .iter()
            .position(|Insert::Clap(r)| r.instance == inst)
        {
            let Insert::Clap(r) = &mut Arc::make_mut(&mut p.tracks[ti]).inserts[ii];
            return Some(r);
        }
    }
    None
}

fn check_vel(v: u8) -> Result<(), EditError> {
    if (1..=127).contains(&v) {
        Ok(())
    } else {
        Err(out_of_range("note.vel", v as f64))
    }
}

fn check_note(n: &NewNote, pat: &Pattern) -> Result<(), EditError> {
    if n.len == 0 {
        return Err(out_of_range("note.len", 0.0));
    }
    if n.start >= pat.length_ticks() {
        return Err(out_of_range("note.start", n.start as f64));
    }
    if n.start as u64 + n.len as u64 > MAX_TICK as u64 {
        return Err(out_of_range("note.end", n.start as f64 + n.len as f64));
    }
    if n.key > 127 {
        return Err(out_of_range("note.key", n.key as f64));
    }
    check_vel(n.vel)
}

/// Fails if adding `extra` notes to the pattern would pass a fixed
/// maximum (17.1). Checked before anything is cloned.
fn check_room(p: &Project, pi: usize, extra: usize) -> Result<(), EditError> {
    if p.patterns[pi].note_count() + extra > MAX_NOTES_PER_PATTERN {
        return Err(too_many("notes in pattern", MAX_NOTES_PER_PATTERN));
    }
    if p.note_count() + extra > MAX_NOTES_PER_PROJECT {
        return Err(too_many("notes in project", MAX_NOTES_PER_PROJECT));
    }
    Ok(())
}

fn insert_notes(pat: &mut Pattern, channel: ChannelId, notes: Vec<Note>) {
    if notes.is_empty() {
        return;
    }
    let i = match pat.notes.iter().position(|c| c.channel == channel) {
        Some(i) => i,
        None => {
            pat.notes.push(ChannelNotes {
                channel,
                notes: Vec::new(),
            });
            pat.notes.sort_by_key(|c| c.channel);
            pat.notes
                .iter()
                .position(|c| c.channel == channel)
                .expect("just inserted")
        }
    };
    let cn = &mut pat.notes[i];
    cn.notes.extend(notes);
    cn.notes.sort_by_key(|n| (n.start, n.key, n.id));
}

fn prune_empty(pat: &mut Pattern) {
    pat.notes.retain(|c| !c.notes.is_empty());
}

/// Positions of the named notes in a pattern, or `NotFound` for the first
/// id that is missing. Duplicate ids in the request count once.
fn locate(pat: &Pattern, ids: &[NoteId]) -> Result<HashSet<NoteId>, EditError> {
    let want: HashSet<NoteId> = ids.iter().copied().collect();
    let mut found = 0usize;
    for cn in &pat.notes {
        found += cn.notes.iter().filter(|n| want.contains(&n.id)).count();
    }
    if found != want.len() {
        let have: HashSet<NoteId> = pat
            .notes
            .iter()
            .flat_map(|c| c.notes.iter().map(|n| n.id))
            .collect();
        let missing = ids.iter().find(|i| !have.contains(i)).copied();
        return Err(not_found("note", missing.map(|m| m.0).unwrap_or(0)));
    }
    Ok(want)
}

fn apply_one(w: &mut Work, e: &Edit) -> Result<(), EditError> {
    match e {
        // Project
        Edit::SetTempo { bpm } => w.p.tempo_bpm = *bpm,
        Edit::SetTimeSigNum { num } => w.p.time_sig_num = *num,
        Edit::SetMetronome { enabled, gain_db } => {
            w.p.metronome.enabled = *enabled;
            w.p.metronome.gain_db = *gain_db;
        }

        // Channels
        Edit::AddChannel {
            name,
            instrument,
            root_key,
            track,
        } => {
            if w.p.channels.len() >= MAX_CHANNELS {
                return Err(too_many("channels", MAX_CHANNELS));
            }
            track_idx(&w.p, *track)?;
            if let NewInstrument::Synth { params } = instrument {
                check_synth(params)?;
            }
            let id = ChannelId(w.alloc()?);
            let instrument = match instrument {
                NewInstrument::Synth { params } => Instrument::Synth(*params),
                NewInstrument::Clap { plugin_id } => Instrument::Clap(new_clap(w, plugin_id)?),
            };
            w.p.channels.push(Arc::new(Channel {
                id,
                name: name.clone(),
                root_key: *root_key,
                track: *track,
                mix: Default::default(),
                instrument,
            }));
        }
        Edit::RemoveChannel { channel } => {
            let i = channel_idx(&w.p, *channel)?;
            w.p.channels.remove(i);
            for pat in &mut w.p.patterns {
                if pat.notes.iter().any(|c| c.channel == *channel) {
                    Arc::make_mut(pat).notes.retain(|c| c.channel != *channel);
                }
            }
        }
        Edit::RenameChannel { channel, name } => {
            let i = channel_idx(&w.p, *channel)?;
            Arc::make_mut(&mut w.p.channels[i]).name = name.clone();
        }
        Edit::SetChannelMix { channel, value } => {
            let i = channel_idx(&w.p, *channel)?;
            check_mix_value(value)?;
            set_mix(&mut Arc::make_mut(&mut w.p.channels[i]).mix, *value);
        }
        Edit::SetChannelTrack { channel, track } => {
            let i = channel_idx(&w.p, *channel)?;
            track_idx(&w.p, *track)?;
            Arc::make_mut(&mut w.p.channels[i]).track = *track;
        }
        Edit::SetRootKey { channel, key } => set_root_key(w, *channel, *key)?,
        Edit::SetSynthParam {
            channel,
            param,
            value,
        } => {
            let i = channel_idx(&w.p, *channel)?;
            if !matches!(w.p.channels[i].instrument, Instrument::Synth(_)) {
                return Err(bad("channel is not a built-in synth"));
            }
            let (lo, hi) = param.range();
            if !(value.is_finite() && *value >= lo && *value <= hi) {
                return Err(out_of_range(&format!("synth.{param:?}"), *value));
            }
            if let Instrument::Synth(s) = &mut Arc::make_mut(&mut w.p.channels[i]).instrument {
                s.set(*param, *value);
            }
        }
        Edit::SetSynthWave { channel, osc, wave } => {
            let i = channel_idx(&w.p, *channel)?;
            if !matches!(w.p.channels[i].instrument, Instrument::Synth(_)) {
                return Err(bad("channel is not a built-in synth"));
            }
            let wv: Wave = *wave;
            match (osc, &mut Arc::make_mut(&mut w.p.channels[i]).instrument) {
                (1, Instrument::Synth(s)) => s.osc1.wave = wv,
                (2, Instrument::Synth(s)) => s.osc2.wave = wv,
                _ => return Err(bad("oscillator must be 1 or 2")),
            }
        }

        // Patterns
        Edit::AddPattern { name, length_steps } => {
            if w.p.patterns.len() >= MAX_PATTERNS {
                return Err(too_many("patterns", MAX_PATTERNS));
            }
            if !(MIN_STEPS..=MAX_STEPS).contains(length_steps) {
                return Err(out_of_range("pattern.length_steps", *length_steps as f64));
            }
            let id = PatternId(w.alloc()?);
            let mut pat = Pattern::new(id, name.clone());
            pat.length_steps = *length_steps;
            w.p.patterns.push(Arc::new(pat));
        }
        Edit::RemovePattern { pattern } => {
            let i = pattern_idx(&w.p, *pattern)?;
            w.p.patterns.remove(i);
        }
        Edit::RenamePattern { pattern, name } => {
            let i = pattern_idx(&w.p, *pattern)?;
            Arc::make_mut(&mut w.p.patterns[i]).name = name.clone();
        }
        Edit::SetPatternLength {
            pattern,
            length_steps,
        } => {
            let i = pattern_idx(&w.p, *pattern)?;
            if !(MIN_STEPS..=MAX_STEPS).contains(length_steps) {
                return Err(out_of_range("pattern.length_steps", *length_steps as f64));
            }
            let pat = Arc::make_mut(&mut w.p.patterns[i]);
            pat.length_steps = *length_steps;
            let end = pat.length_ticks();
            for cn in &mut pat.notes {
                cn.notes.retain(|n| n.start < end);
            }
            prune_empty(pat);
        }
        Edit::SetStepTicks {
            pattern,
            step_ticks,
        } => set_step_ticks(w, *pattern, *step_ticks)?,

        // Steps and notes
        Edit::SetStep {
            pattern,
            channel,
            step,
            on,
            vel,
        } => set_step(w, *pattern, *channel, *step, *on, *vel)?,
        Edit::AddNotes {
            pattern,
            channel,
            notes,
        } => {
            let pi = pattern_idx(&w.p, *pattern)?;
            channel_idx(&w.p, *channel)?;
            check_room(&w.p, pi, notes.len())?;
            for n in notes {
                check_note(n, &w.p.patterns[pi])?;
            }
            let mut made = Vec::with_capacity(notes.len());
            for n in notes {
                made.push(Note {
                    id: NoteId(w.alloc()?),
                    start: n.start,
                    len: n.len,
                    key: n.key,
                    vel: n.vel,
                });
            }
            insert_notes(Arc::make_mut(&mut w.p.patterns[pi]), *channel, made);
        }
        Edit::RemoveNotes { pattern, notes } => {
            let pi = pattern_idx(&w.p, *pattern)?;
            let want = locate(&w.p.patterns[pi], notes)?;
            if want.is_empty() {
                return Ok(());
            }
            let pat = Arc::make_mut(&mut w.p.patterns[pi]);
            for cn in &mut pat.notes {
                cn.notes.retain(|n| !want.contains(&n.id));
            }
            prune_empty(pat);
        }
        Edit::MoveNotes {
            pattern,
            notes,
            dt,
            dkey,
        } => {
            let pi = pattern_idx(&w.p, *pattern)?;
            let want = locate(&w.p.patterns[pi], notes)?;
            let end = w.p.patterns[pi].length_ticks() as i64;
            for cn in &w.p.patterns[pi].notes {
                for n in cn.notes.iter().filter(|n| want.contains(&n.id)) {
                    let s = (n.start as i64)
                        .checked_add(*dt)
                        .ok_or_else(|| out_of_range("note.start", f64::INFINITY))?;
                    let k = n.key as i64 + *dkey as i64;
                    if s < 0 || s >= end {
                        return Err(out_of_range("note.start", s as f64));
                    }
                    if s + n.len as i64 > MAX_TICK as i64 {
                        return Err(out_of_range("note.end", (s + n.len as i64) as f64));
                    }
                    if !(0..=127).contains(&k) {
                        return Err(out_of_range("note.key", k as f64));
                    }
                }
            }
            if want.is_empty() {
                return Ok(());
            }
            let pat = Arc::make_mut(&mut w.p.patterns[pi]);
            for cn in &mut pat.notes {
                for n in cn.notes.iter_mut().filter(|n| want.contains(&n.id)) {
                    n.start = (n.start as i64 + *dt) as u32;
                    n.key = (n.key as i64 + *dkey as i64) as u8;
                }
                cn.notes.sort_by_key(|n| (n.start, n.key, n.id));
            }
        }
        Edit::ResizeNotes {
            pattern,
            notes,
            dlen,
        } => {
            let pi = pattern_idx(&w.p, *pattern)?;
            let want = locate(&w.p.patterns[pi], notes)?;
            for cn in &w.p.patterns[pi].notes {
                for n in cn.notes.iter().filter(|n| want.contains(&n.id)) {
                    let l = (n.len as i64)
                        .checked_add(*dlen)
                        .ok_or_else(|| out_of_range("note.len", f64::INFINITY))?;
                    if l < 1 {
                        return Err(out_of_range("note.len", l as f64));
                    }
                    if n.start as i64 + l > MAX_TICK as i64 {
                        return Err(out_of_range("note.end", (n.start as i64 + l) as f64));
                    }
                }
            }
            if want.is_empty() {
                return Ok(());
            }
            let pat = Arc::make_mut(&mut w.p.patterns[pi]);
            for cn in &mut pat.notes {
                for n in cn.notes.iter_mut().filter(|n| want.contains(&n.id)) {
                    n.len = (n.len as i64 + *dlen) as u32;
                }
            }
        }
        Edit::SetNoteVelocity {
            pattern,
            notes,
            vel,
        } => {
            let pi = pattern_idx(&w.p, *pattern)?;
            check_vel(*vel)?;
            let want = locate(&w.p.patterns[pi], notes)?;
            if want.is_empty() {
                return Ok(());
            }
            let pat = Arc::make_mut(&mut w.p.patterns[pi]);
            for cn in &mut pat.notes {
                for n in cn.notes.iter_mut().filter(|n| want.contains(&n.id)) {
                    n.vel = *vel;
                }
            }
        }

        // Mixer
        Edit::AddTrack { name } => {
            if w.p.tracks.len() >= TRACK_SLOTS {
                return Err(too_many("tracks", MAX_TRACKS));
            }
            let id = TrackId(w.alloc()?);
            w.p.tracks.push(Arc::new(Track {
                id,
                name: name.clone(),
                mix: Default::default(),
                inserts: Vec::new(),
            }));
        }
        Edit::RemoveTrack { track } => {
            if *track == TrackId::MASTER {
                return Err(bad("the master track cannot be removed"));
            }
            let i = track_idx(&w.p, *track)?;
            w.p.tracks.remove(i);
            for c in &mut w.p.channels {
                if c.track == *track {
                    Arc::make_mut(c).track = TrackId::MASTER;
                }
            }
        }
        Edit::RenameTrack { track, name } => {
            let i = track_idx(&w.p, *track)?;
            Arc::make_mut(&mut w.p.tracks[i]).name = name.clone();
        }
        Edit::SetTrackMix { track, value } => {
            let i = track_idx(&w.p, *track)?;
            check_mix_value(value)?;
            set_mix(&mut Arc::make_mut(&mut w.p.tracks[i]).mix, *value);
        }
        Edit::AddInsert {
            track,
            index,
            plugin_id,
        } => {
            let ti = track_idx(&w.p, *track)?;
            let n = w.p.tracks[ti].inserts.len();
            if n >= MAX_INSERTS {
                return Err(too_many("inserts", MAX_INSERTS));
            }
            if *index as usize > n {
                return Err(bad("insert index is past the end"));
            }
            let r = new_clap(w, plugin_id)?;
            Arc::make_mut(&mut w.p.tracks[ti])
                .inserts
                .insert(*index as usize, Insert::Clap(r));
        }
        Edit::RemoveInsert { track, instance } => {
            let ti = track_idx(&w.p, *track)?;
            let ii = w.p.tracks[ti]
                .inserts
                .iter()
                .position(|Insert::Clap(r)| r.instance == *instance)
                .ok_or_else(|| not_found("insert", instance.0))?;
            Arc::make_mut(&mut w.p.tracks[ti]).inserts.remove(ii);
        }

        // Plugins
        Edit::SetPluginParam {
            instance,
            param_id,
            value,
        } => {
            if !value.is_finite() {
                return Err(out_of_range("plugin param", *value));
            }
            let r = clap_mut(&mut w.p, *instance)
                .ok_or_else(|| not_found("plugin instance", instance.0))?;
            match r.params.binary_search_by_key(param_id, |v| v.id) {
                Ok(i) => r.params[i].value = *value,
                Err(i) => r.params.insert(
                    i,
                    ParamValue {
                        id: *param_id,
                        value: *value,
                    },
                ),
            }
        }
        Edit::CommitPluginState {
            instance,
            state_file,
        } => {
            let r = clap_mut(&mut w.p, *instance)
                .ok_or_else(|| not_found("plugin instance", instance.0))?;
            if r.state_file.as_deref() != Some(state_file.as_str()) {
                r.state_file = Some(state_file.clone());
                r.state_bytes = None;
            }
        }
    }
    Ok(())
}

fn check_mix_value(v: &MixValue) -> Result<(), EditError> {
    match v {
        MixValue::VolumeDb(x) if !(x.is_finite() && (MIN_GAIN_DB..=MAX_GAIN_DB).contains(x)) => {
            Err(out_of_range("mix.volume_db", *x))
        }
        MixValue::Pan(x) if !(x.is_finite() && (-1.0..=1.0).contains(x)) => {
            Err(out_of_range("mix.pan", *x))
        }
        _ => Ok(()),
    }
}

fn new_clap(w: &mut Work, plugin_id: &str) -> Result<ClapRef, EditError> {
    protocol::validate::check_name("plugin_id", plugin_id)?;
    Ok(ClapRef {
        instance: InstanceId(w.alloc()?),
        plugin_id: plugin_id.to_string(),
        plugin_version: String::new(),
        state_file: None,
        state_bytes: None,
        params: Vec::new(),
    })
}

fn set_step(
    w: &mut Work,
    pattern: PatternId,
    channel: ChannelId,
    step: u8,
    on: bool,
    vel: Option<u8>,
) -> Result<(), EditError> {
    let pi = pattern_idx(&w.p, pattern)?;
    let ci = channel_idx(&w.p, channel)?;
    let root = w.p.channels[ci].root_key;
    let (len, start) = {
        let pat = &w.p.patterns[pi];
        if step >= pat.length_steps {
            return Err(bad("step is outside the pattern"));
        }
        (pat.step_ticks, step as u32 * pat.step_ticks)
    };
    if let Some(v) = vel {
        check_vel(v)?;
    }
    let at = |n: &&Note| n.start == start && n.key == root;
    if on {
        let exists = w.p.patterns[pi]
            .notes_of(channel)
            .iter()
            .any(|n| at(&n) && n.len == len);
        if exists {
            if let Some(v) = vel {
                let pat = Arc::make_mut(&mut w.p.patterns[pi]);
                for cn in pat.notes.iter_mut().filter(|c| c.channel == channel) {
                    for n in cn.notes.iter_mut() {
                        if n.start == start && n.key == root && n.len == len {
                            n.vel = v;
                        }
                    }
                }
            }
            return Ok(());
        }
        check_room(&w.p, pi, 1)?;
        let note = Note {
            id: NoteId(w.alloc()?),
            start,
            len,
            key: root,
            vel: vel.unwrap_or(DEFAULT_STEP_VEL),
        };
        insert_notes(Arc::make_mut(&mut w.p.patterns[pi]), channel, vec![note]);
    } else if w.p.patterns[pi].notes_of(channel).iter().any(|n| at(&n)) {
        let pat = Arc::make_mut(&mut w.p.patterns[pi]);
        for cn in pat.notes.iter_mut().filter(|c| c.channel == channel) {
            cn.notes.retain(|n| !(n.start == start && n.key == root));
        }
        prune_empty(pat);
    }
    Ok(())
}

fn set_root_key(w: &mut Work, channel: ChannelId, key: u8) -> Result<(), EditError> {
    let ci = channel_idx(&w.p, channel)?;
    if key > 127 {
        return Err(out_of_range("channel.root_key", key as f64));
    }
    let old = w.p.channels[ci].root_key;
    if old == key {
        return Ok(());
    }
    for pat in &mut w.p.patterns {
        let has_step = pat
            .notes_of(channel)
            .iter()
            .any(|n| n.is_step_note(old, pat));
        if !has_step {
            continue;
        }
        let snapshot = (pat.step_ticks, pat.length_ticks());
        let pat = Arc::make_mut(pat);
        for cn in pat.notes.iter_mut().filter(|c| c.channel == channel) {
            for n in cn.notes.iter_mut() {
                if n.start % snapshot.0 == 0
                    && n.len == snapshot.0
                    && n.key == old
                    && n.start < snapshot.1
                {
                    n.key = key;
                }
            }
            cn.notes.sort_by_key(|n| (n.start, n.key, n.id));
        }
    }
    Arc::make_mut(&mut w.p.channels[ci]).root_key = key;
    Ok(())
}

fn set_step_ticks(w: &mut Work, pattern: PatternId, new: u32) -> Result<(), EditError> {
    let pi = pattern_idx(&w.p, pattern)?;
    if !(1..=PPQ * 4).contains(&new) {
        return Err(out_of_range("pattern.step_ticks", new as f64));
    }
    let old = w.p.patterns[pi].step_ticks;
    if old == new {
        return Ok(());
    }
    let roots: Vec<(ChannelId, u8)> = w.p.channels.iter().map(|c| (c.id, c.root_key)).collect();
    let pat = Arc::make_mut(&mut w.p.patterns[pi]);
    let old_len = pat.length_ticks();
    for cn in &mut pat.notes {
        let root = roots.iter().find(|r| r.0 == cn.channel).map(|r| r.1);
        for n in &mut cn.notes {
            let is_step =
                Some(n.key) == root && n.start % old == 0 && n.len == old && n.start < old_len;
            if is_step {
                n.start = n.start / old * new;
                n.len = new;
            }
        }
    }
    pat.step_ticks = new;
    let end = pat.length_ticks();
    for cn in &mut pat.notes {
        cn.notes.retain(|n| n.start < end);
        cn.notes.sort_by_key(|n| (n.start, n.key, n.id));
    }
    prune_empty(pat);
    Ok(())
}

#[cfg(test)]
mod tests;
