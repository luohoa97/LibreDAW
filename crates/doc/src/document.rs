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
//!
//! Milestone B (SPEC 15, 17.2):
//! - A step note may carry a pitch offset `off` and a ratchet `repeat`.
//!   Toggling a step off removes step notes at that step whatever their
//!   `off`. `SetRootKey` rewrites `key = new_root + off` and fails if that
//!   leaves 0 to 127. Moving or resizing a note clears its `off`.
//! - `RemovePattern` removes the pattern's clips. `RemoveTrack` removes the
//!   sends to it and clears sidechains that read from it.
//! - The Milestone B edits are in `document/beats.rs`.

use std::collections::HashSet;
use std::sync::Arc;

use protocol::beats::{Bass808, Sampler, SamplerParams};
use protocol::consts::*;
use protocol::edit::{Edit, EditError, MixValue, NewInstrument, NewNote, set_mix};
use protocol::ids::{ChannelId, FIRST_ID, InstanceId, NoteId, PatternId, TrackId};
use protocol::model::{
    Channel, ClapRef, Insert, Instrument, Note, ParamValue, Pattern, Project, Track, Wave,
};
use protocol::validate::{ValidationError, check_synth, sort_canonical, validate};

mod beats;
mod clips;
mod v4;

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
    apply_batch_indexed(doc, edits).map_err(|(_, e)| e)
}

/// Like `apply_batch`; on failure also returns the position of the edit
/// that failed (for the control API, 17.1). A failure that only shows in
/// the final validation is traced to the first edit after which the
/// project is invalid.
pub fn apply_batch_indexed(
    doc: &Document,
    edits: &[Edit],
) -> Result<(Document, Vec<u32>), (Option<u32>, EditError)> {
    let mut w = Work {
        p: (*doc.project).clone(),
        next_id: doc.next_id,
        created: Vec::new(),
    };
    for (i, e) in edits.iter().enumerate() {
        apply_one(&mut w, e).map_err(|err| (Some(i as u32), err))?;
    }
    v4::collect_groups(&mut w.p);
    sort_canonical(&mut w.p);
    if let Err(err) = validate(&w.p) {
        return Err((first_invalid_edit(doc, edits), err.into()));
    }
    let out = Document {
        project: Arc::new(w.p),
        next_id: w.next_id,
        revision: doc.revision + 1,
    };
    Ok((out, w.created))
}

/// Slow path for a failed batch: replays it validating after each edit.
fn first_invalid_edit(doc: &Document, edits: &[Edit]) -> Option<u32> {
    let mut w = Work {
        p: (*doc.project).clone(),
        next_id: doc.next_id,
        created: Vec::new(),
    };
    for (i, e) in edits.iter().enumerate() {
        apply_one(&mut w, e).ok()?;
        sort_canonical(&mut w.p);
        if validate(&w.p).is_err() {
            return Some(i as u32);
        }
    }
    None
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

    /// An id that is not reported in `created` (the notes of copied
    /// content).
    fn alloc_quiet(&mut self) -> Result<u32, EditError> {
        if self.next_id == u32::MAX {
            return Err(bad("id space exhausted"));
        }
        let id = self.next_id;
        self.next_id += 1;
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
    let at = p.tracks.iter().enumerate().find_map(|(ti, t)| {
        t.inserts
            .iter()
            .position(|i| matches!(i, Insert::Clap(r) if r.instance == inst))
            .map(|ii| (ti, ii))
    });
    if let Some((ti, ii)) = at
        && let Insert::Clap(r) = &mut Arc::make_mut(&mut p.tracks[ti]).inserts[ii]
    {
        return Some(r);
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

fn insert_notes(pat: &mut Pattern, notes: Vec<Note>) {
    if notes.is_empty() {
        return;
    }
    pat.notes.extend(notes);
    pat.notes.sort_by_key(|n| (n.start, n.key, n.id));
}

/// The named notes of a pattern, or `NotFound` for the first id that is
/// missing. Duplicate ids in the request count once.
fn locate(pat: &Pattern, ids: &[NoteId]) -> Result<HashSet<NoteId>, EditError> {
    let want: HashSet<NoteId> = ids.iter().copied().collect();
    let have: HashSet<NoteId> = pat.notes.iter().map(|n| n.id).collect();
    if let Some(missing) = ids.iter().find(|i| !have.contains(i)) {
        return Err(not_found("note", missing.0));
    }
    Ok(want)
}

/// Root key of the instrument that owns a pattern's notes.
fn root_of(p: &Project, pat: &Pattern) -> Result<u8, EditError> {
    p.channel(pat.instrument)
        .map(|c| c.root_key)
        .ok_or_else(|| not_found("channel", pat.instrument.0))
}

/// After a content's length changed, keeps every clip offset inside it by
/// wrapping (the content repeats, so the same musical position).
fn wrap_offsets(p: &mut Project, pattern: PatternId) {
    let Some(len) = p.pattern(pattern).map(|x| x.length_ticks().max(1)) else {
        return;
    };
    for c in &mut p.clips {
        if c.pattern == pattern && c.offset >= len {
            c.offset %= len;
        }
    }
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
            match instrument {
                NewInstrument::Synth { params } => check_synth(params)?,
                NewInstrument::Sampler {
                    sample: Some(h), ..
                } => beats::require_sample(&w.p, h)?,
                _ => {}
            }
            let id = ChannelId(w.alloc()?);
            let instrument = match instrument {
                NewInstrument::Synth { params } => Instrument::Synth(*params),
                NewInstrument::Clap { plugin_id, .. } => Instrument::Clap(new_clap(w, plugin_id)?),
                NewInstrument::Sampler { sample, mode } => Instrument::Sampler(Sampler {
                    sample: sample.clone(),
                    mode: *mode,
                    reverse: false,
                    params: SamplerParams::default(),
                }),
                NewInstrument::Bass808 { mono } => Instrument::Bass808(Bass808 {
                    mono: *mono,
                    ..Bass808::default()
                }),
                NewInstrument::Audio => Instrument::Audio,
            };
            w.p.channels.push(Arc::new(Channel {
                id,
                name: name.clone(),
                root_key: *root_key,
                track: *track,
                mix: Default::default(),
                instrument,
                choke_group: 0,
            }));
        }
        Edit::RemoveChannel { channel } => {
            let i = channel_idx(&w.p, *channel)?;
            w.p.channels.remove(i);
            // Its contents and clips go with it (20.3).
            w.p.patterns.retain(|pat| pat.instrument != *channel);
            w.p.clips.retain(|c| c.instrument != *channel);
            v4::forget_channel(&mut w.p, *channel);
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

        // Clip contents (20.2)
        Edit::AddPattern {
            instrument,
            name,
            length_steps,
        } => {
            channel_idx(&w.p, *instrument)?;
            if w.p.patterns.len() >= MAX_PATTERNS {
                return Err(too_many("patterns", MAX_PATTERNS));
            }
            if !(MIN_STEPS..=MAX_STEPS).contains(length_steps) {
                return Err(out_of_range("pattern.length_steps", *length_steps as f64));
            }
            let id = PatternId(w.alloc()?);
            let mut pat = Pattern::new(id, name.clone(), *instrument);
            pat.length_steps = *length_steps;
            w.p.patterns.push(Arc::new(pat));
        }
        Edit::RemovePattern { pattern } => {
            let i = pattern_idx(&w.p, *pattern)?;
            w.p.patterns.remove(i);
            w.p.clips.retain(|c| c.pattern != *pattern);
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
            pat.notes.retain(|n| n.start < end);
            wrap_offsets(&mut w.p, *pattern);
        }
        Edit::SetStepTicks {
            pattern,
            step_ticks,
        } => set_step_ticks(w, *pattern, *step_ticks)?,

        // Steps and notes
        Edit::SetStep {
            pattern,
            step,
            on,
            vel,
        } => set_step(w, *pattern, *step, *on, *vel)?,
        Edit::AddNotes { pattern, notes } => {
            let pi = pattern_idx(&w.p, *pattern)?;
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
                    off: 0,
                    repeat: 1,
                });
            }
            insert_notes(Arc::make_mut(&mut w.p.patterns[pi]), made);
        }
        Edit::RemoveNotes { pattern, notes } => {
            let pi = pattern_idx(&w.p, *pattern)?;
            let want = locate(&w.p.patterns[pi], notes)?;
            if want.is_empty() {
                return Ok(());
            }
            Arc::make_mut(&mut w.p.patterns[pi])
                .notes
                .retain(|n| !want.contains(&n.id));
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
            for n in w.p.patterns[pi]
                .notes
                .iter()
                .filter(|n| want.contains(&n.id))
            {
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
            if want.is_empty() {
                return Ok(());
            }
            let pat = Arc::make_mut(&mut w.p.patterns[pi]);
            for n in pat.notes.iter_mut().filter(|n| want.contains(&n.id)) {
                n.start = (n.start as i64 + *dt) as u32;
                n.key = (n.key as i64 + *dkey as i64) as u8;
                if *dt != 0 || *dkey != 0 {
                    // A moved step note becomes a piano-roll note (17.2).
                    n.off = 0;
                }
            }
            pat.notes.sort_by_key(|n| (n.start, n.key, n.id));
        }
        Edit::ResizeNotes {
            pattern,
            notes,
            dlen,
        } => {
            let pi = pattern_idx(&w.p, *pattern)?;
            let want = locate(&w.p.patterns[pi], notes)?;
            for n in w.p.patterns[pi]
                .notes
                .iter()
                .filter(|n| want.contains(&n.id))
            {
                let l = (n.len as i64)
                    .checked_add(*dlen)
                    .ok_or_else(|| out_of_range("note.len", f64::INFINITY))?;
                if l < 1 {
                    return Err(out_of_range("note.len", l as f64));
                }
                if n.start as i64 + l > MAX_TICK as i64 {
                    return Err(out_of_range("note.end", (n.start as i64 + l) as f64));
                }
                if l % n.repeat as i64 != 0 {
                    return Err(bad("note length must stay divisible by its repeat"));
                }
            }
            if want.is_empty() {
                return Ok(());
            }
            let pat = Arc::make_mut(&mut w.p.patterns[pi]);
            for n in pat.notes.iter_mut().filter(|n| want.contains(&n.id)) {
                n.len = (n.len as i64 + *dlen) as u32;
                if *dlen != 0 {
                    n.off = 0;
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
            for n in pat.notes.iter_mut().filter(|n| want.contains(&n.id)) {
                n.vel = *vel;
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
                sends: Vec::new(),
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
            beats::forget_track(&mut w.p, *track);
            v4::forget_track(&mut w.p, *track);
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
                .position(|i| i.instance() == *instance)
                .ok_or_else(|| not_found("insert", instance.0))?;
            Arc::make_mut(&mut w.p.tracks[ti]).inserts.remove(ii);
            v4::forget_insert(&mut w.p, *track, *instance);
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

        // Milestone B (15, 17.2)
        Edit::SetStepLanes { .. }
        | Edit::SetNoteRepeat { .. }
        | Edit::SetSwing { .. }
        | Edit::SetChokeGroup { .. }
        | Edit::AddSample { .. }
        | Edit::RemoveSample { .. }
        | Edit::SetSamplerSample { .. }
        | Edit::SetSamplerMode { .. }
        | Edit::SetSamplerParam { .. }
        | Edit::SetBass808Mono { .. }
        | Edit::SetBass808Param { .. }
        | Edit::AddBuiltinInsert { .. }
        | Edit::SetFxParam { .. }
        | Edit::SetSaturatorCurve { .. }
        | Edit::SetDelayPingPong { .. }
        | Edit::SetSidechain { .. }
        | Edit::MoveInsert { .. }
        | Edit::SetInsertBypass { .. }
        | Edit::SetSend { .. }
        | Edit::RemoveSend { .. } => beats::apply(w, e)?,

        // Timeline (20.2, 20.3)
        Edit::AddClip { .. }
        | Edit::DuplicateClips { .. }
        | Edit::RemoveClips { .. }
        | Edit::MoveClips { .. }
        | Edit::MoveClipToInstrument { .. }
        | Edit::ResizeClips { .. }
        | Edit::SplitClip { .. }
        | Edit::MakeUnique { .. }
        | Edit::SetClipMuted { .. }
        | Edit::SetLoopRegion { .. } => clips::apply(w, e)?,

        // Audio clips, patterns and shapes (v4)
        Edit::AddAudioClip { .. }
        | Edit::SetClipAudio { .. }
        | Edit::MakePattern { .. }
        | Edit::PlacePattern { .. }
        | Edit::Ungroup { .. }
        | Edit::RenameGroup { .. }
        | Edit::AddShape { .. }
        | Edit::SetShapePoints { .. }
        | Edit::RemoveShape { .. } => v4::apply(w, e)?,
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
    step: u8,
    on: bool,
    vel: Option<u8>,
) -> Result<(), EditError> {
    let pi = pattern_idx(&w.p, pattern)?;
    let root = root_of(&w.p, &w.p.patterns[pi])?;
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
    // Off removes notes at the step whose key is `root + off`, whatever
    // their `off` (17.2); on keeps an existing step note, with any `off`.
    let at = |n: &Note| n.start == start && n.key as i16 == root as i16 + n.off as i16;
    if on {
        let exists = w.p.patterns[pi].notes.iter().any(|n| at(n) && n.len == len);
        if exists {
            if let Some(v) = vel {
                let pat = Arc::make_mut(&mut w.p.patterns[pi]);
                for n in pat.notes.iter_mut() {
                    if at(n) && n.len == len {
                        n.vel = v;
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
            off: 0,
            repeat: 1,
        };
        insert_notes(Arc::make_mut(&mut w.p.patterns[pi]), vec![note]);
    } else if w.p.patterns[pi].notes.iter().any(at) {
        Arc::make_mut(&mut w.p.patterns[pi])
            .notes
            .retain(|n| !at(n));
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
    // Check first so a failure changes nothing: every step note keeps
    // `key = new_root + off` inside 0 to 127.
    for pat in w.p.patterns.iter().filter(|p| p.instrument == channel) {
        for n in &pat.notes {
            if n.is_step_note(old, pat) {
                let k = key as i16 + n.off as i16;
                if !(0..=127).contains(&k) {
                    return Err(out_of_range("note.key", k as f64));
                }
            }
        }
    }
    for pat in w.p.patterns.iter_mut().filter(|p| p.instrument == channel) {
        if !pat.notes.iter().any(|n| n.is_step_note(old, pat)) {
            continue;
        }
        let (step, end) = (pat.step_ticks, pat.length_ticks());
        let pat = Arc::make_mut(pat);
        for n in pat.notes.iter_mut() {
            if n.start % step == 0
                && n.len == step
                && n.key as i16 == old as i16 + n.off as i16
                && n.start < end
            {
                n.key = (key as i16 + n.off as i16) as u8;
            }
        }
        pat.notes.sort_by_key(|n| (n.start, n.key, n.id));
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
    let root = root_of(&w.p, &w.p.patterns[pi])?;
    let pat = Arc::make_mut(&mut w.p.patterns[pi]);
    let old_len = pat.length_ticks();
    for n in &mut pat.notes {
        let is_step = n.key as i16 == root as i16 + n.off as i16
            && n.start % old == 0
            && n.len == old
            && n.start < old_len;
        if is_step {
            n.start = n.start / old * new;
            n.len = new;
        }
    }
    pat.step_ticks = new;
    let end = pat.length_ticks();
    pat.notes.retain(|n| n.start < end);
    pat.notes.sort_by_key(|n| (n.start, n.key, n.id));
    wrap_offsets(&mut w.p, pattern);
    Ok(())
}

#[cfg(test)]
mod beats_tests;
#[cfg(test)]
mod v4_tests;
#[cfg(test)]
mod clips_props;
#[cfg(test)]
mod clips_tests;
#[cfg(test)]
pub(crate) mod tests;
