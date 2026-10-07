// SPDX-License-Identifier: GPL-3.0-or-later
//! The timeline edits (SPEC 20.2, 20.3): clips on instrument rows, linked
//! copies, and the loop region.
//!
//! Choices where the spec leaves room:
//! - A clip plays its content at position `(offset + t - start) mod
//!   content_len`. Edits keep that mapping: splitting and start-resizing
//!   adjust `offset` modulo the content length.
//! - Overlap on a row is an `Invalid(Overlap)` error naming both clip ids
//!   (`a` is 0 for a clip that has no id yet). Nothing is
//!   clamped, moved aside, or trimmed.
//! - `created` order: new contents first (in the order of their first
//!   source clip), then new clips (in the order of the request). Notes of
//!   copied content get ids too, but they are not reported.
//! - Content made by `AddClip` with `pattern: None` is named
//!   `<instrument name> <n>` with the smallest `n` not used by that
//!   instrument. Copies are named `<name> copy`, then `<name> copy 2`, ...
//!   A content moved to another instrument that still has an automatic
//!   name gets an automatic name of the new instrument.
//! - `MoveClipToInstrument` to the instrument the clip is already on does
//!   nothing.
//! - Contents that no clip uses are removed by `RemoveClips` and
//!   `MoveClipToInstrument`, but only the ones those edits orphaned:
//!   content made by `AddPattern` and never placed stays.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use protocol::consts::*;
use protocol::edit::{Edit, EditError};
use protocol::ids::{ChannelId, ClipId, NoteId, PatternId};
use protocol::model::{AudioSource, Clip, ClipGroup, Instrument, Note, Pattern, Project};
use protocol::validate::ValidationError;

use super::{Work, bad, channel_idx, not_found, out_of_range, pattern_idx, too_many};

/// A clip-to-be on a row: `(instrument, start, end, id if it has one)`.
pub(super) type Placement = (ChannelId, u32, u32, Option<ClipId>);

fn overlap(a: Option<ClipId>, b: ClipId) -> EditError {
    EditError::Invalid {
        reason: ValidationError::Overlap {
            a: a.map_or(0, |id| id.0),
            b: b.0,
        },
    }
}

/// Fails if any placement overlaps a clip of the project (other than
/// those in `ignore`) or another placement on the same row.
pub(super) fn check_placements(
    p: &Project,
    planned: &[Placement],
    ignore: &HashSet<ClipId>,
) -> Result<(), EditError> {
    for (i, a) in planned.iter().enumerate() {
        if let Some(o) = p.clips.iter().find(|c| {
            !ignore.contains(&c.id) && c.instrument == a.0 && c.start < a.2 && a.1 < c.end()
        }) {
            return Err(overlap(a.3, o.id));
        }
        for b in &planned[i + 1..] {
            if a.0 == b.0 && a.1 < b.2 && b.1 < a.2 {
                return Err(overlap(a.3, b.3.unwrap_or(ClipId(0))));
            }
        }
    }
    Ok(())
}

/// The named clips in request order. Duplicates count once; `NotFound`
/// for the first missing id.
pub(super) fn locate_clips(p: &Project, ids: &[ClipId]) -> Result<Vec<Clip>, EditError> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for id in ids {
        if !seen.insert(*id) {
            continue;
        }
        let c = p
            .clips
            .iter()
            .find(|c| c.id == *id)
            .ok_or_else(|| not_found("clip", id.0))?;
        out.push(*c);
    }
    Ok(out)
}

fn clip_mut(p: &mut Project, id: ClipId) -> &mut Clip {
    p.clips
        .iter_mut()
        .find(|c| c.id == id)
        .expect("clip was located")
}

pub(super) fn check_span(start: i64, len: i64) -> Result<(u32, u32), EditError> {
    if start < 0 {
        return Err(out_of_range("clip.start", start as f64));
    }
    if len < 1 {
        return Err(out_of_range("clip.len", len as f64));
    }
    let end = start
        .checked_add(len)
        .ok_or_else(|| out_of_range("clip.end", f64::INFINITY))?;
    if end > MAX_TICK as i64 {
        return Err(out_of_range("clip.end", end as f64));
    }
    Ok((start as u32, len as u32))
}

pub(super) fn check_clip_room(p: &Project, extra: usize) -> Result<(), EditError> {
    if p.clips.len() + extra > MAX_CLIPS {
        return Err(too_many("clips", MAX_CLIPS));
    }
    Ok(())
}

/// `base` + `suffix` cut so the result is a valid name.
fn fit(base: &str, suffix: &str) -> String {
    let keep = MAX_NAME_CHARS.saturating_sub(suffix.chars().count());
    let mut s: String = base.chars().take(keep).collect();
    s.push_str(suffix);
    s
}

fn name_in_use(p: &Project, instrument: ChannelId, name: &str) -> bool {
    p.patterns
        .iter()
        .any(|x| x.instrument == instrument && x.name == name)
}

/// `<instrument name> <n>` with the smallest unused `n`.
fn auto_name(p: &Project, instrument: ChannelId) -> String {
    let inst = p
        .channel(instrument)
        .map_or("Clip", |c| c.name.as_str())
        .to_string();
    (1..)
        .map(|n| fit(&inst, &format!(" {n}")))
        .find(|s| !name_in_use(p, instrument, s))
        .expect("endless range")
}

/// `<name> copy`, `<name> copy 2`, ... unused on this instrument.
fn copy_name(p: &Project, instrument: ChannelId, name: &str) -> String {
    let first = fit(name, " copy");
    if !name_in_use(p, instrument, &first) {
        return first;
    }
    (2..)
        .map(|n| fit(name, &format!(" copy {n}")))
        .find(|s| !name_in_use(p, instrument, s))
        .expect("endless range")
}

/// True for names made by `auto_name` for this instrument name.
fn is_auto_name(name: &str, instrument_name: &str) -> bool {
    name.strip_prefix(instrument_name)
        .and_then(|r| r.strip_prefix(' '))
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

/// Copies a content for `instrument` under a new id (reported in
/// `created`). Step notes follow the new instrument's root key, keeping
/// `off`; every note gets a new id.
fn copy_content(
    w: &mut Work,
    src: PatternId,
    instrument: ChannelId,
    name: String,
) -> Result<PatternId, EditError> {
    let si = pattern_idx(&w.p, src)?;
    let new_root = w.p.channel(instrument).map(|c| c.root_key);
    let new_root = new_root.ok_or_else(|| not_found("channel", instrument.0))?;
    if w.p.patterns.len() >= MAX_PATTERNS {
        return Err(too_many("patterns", MAX_PATTERNS));
    }
    let extra = w.p.patterns[si].note_count();
    if w.p.patterns[si].note_count() + extra > MAX_NOTES_PER_PATTERN
        || w.p.note_count() + extra > MAX_NOTES_PER_PROJECT
    {
        return Err(too_many("notes in project", MAX_NOTES_PER_PROJECT));
    }
    let old_root =
        w.p.channel(w.p.patterns[si].instrument)
            .map(|c| c.root_key)
            .unwrap_or(new_root);
    let id = PatternId(w.alloc()?);
    let src_pat = w.p.patterns[si].clone();
    let mut notes: Vec<Note> = Vec::with_capacity(src_pat.notes.len());
    for n in &src_pat.notes {
        let mut m = *n;
        m.id = NoteId(w.alloc_quiet()?);
        if instrument != src_pat.instrument && n.is_step_note(old_root, &src_pat) {
            let k = new_root as i16 + n.off as i16;
            if !(0..=127).contains(&k) {
                return Err(out_of_range("note.key", k as f64));
            }
            m.key = k as u8;
        }
        notes.push(m);
    }
    notes.sort_by_key(|n| (n.start, n.key, n.id));
    let mut pat = Pattern::new(id, name, instrument);
    pat.length_steps = src_pat.length_steps;
    pat.step_ticks = src_pat.step_ticks;
    pat.swing = src_pat.swing;
    pat.notes = notes;
    w.p.patterns.push(Arc::new(pat));
    Ok(id)
}

/// Removes the listed contents that no clip uses any more.
fn collect_unused(p: &mut Project, candidates: &HashSet<PatternId>) {
    let used: HashSet<PatternId> = p.clips.iter().map(|c| c.pattern).collect();
    p.patterns
        .retain(|x| !candidates.contains(&x.id) || used.contains(&x.id));
}

pub(super) fn apply(w: &mut Work, e: &Edit) -> Result<(), EditError> {
    match e {
        Edit::AddClip {
            instrument,
            pattern,
            start,
            len,
        } => add_clip(w, *instrument, *pattern, *start, *len),
        Edit::DuplicateClips { clips, dt, linked } => duplicate(w, clips, *dt, *linked),
        Edit::RemoveClips { clips } => {
            let found = locate_clips(&w.p, clips)?;
            let gone: HashSet<ClipId> = found.iter().map(|c| c.id).collect();
            let touched: HashSet<PatternId> = found.iter().map(|c| c.pattern).collect();
            w.p.clips.retain(|c| !gone.contains(&c.id));
            collect_unused(&mut w.p, &touched);
            Ok(())
        }
        Edit::MoveClips { clips, dt } => move_clips(w, clips, *dt),
        Edit::MoveClipToInstrument { clip, instrument } => {
            move_to_instrument(w, *clip, *instrument)
        }
        Edit::ResizeClips {
            clips,
            dlen,
            from_start,
        } => resize(w, clips, *dlen, *from_start),
        Edit::SplitClip { clip, at } => split(w, *clip, *at),
        Edit::MakeUnique { clip } => {
            let c = locate_clips(&w.p, &[*clip])?[0];
            if c.audio.is_some()
                || w.p.clips.iter().filter(|x| x.pattern == c.pattern).count() <= 1
            {
                return Ok(());
            }
            let src = &w.p.patterns[pattern_idx(&w.p, c.pattern)?];
            let name = copy_name(&w.p, c.instrument, &src.name);
            let new = copy_content(w, c.pattern, c.instrument, name)?;
            clip_mut(&mut w.p, c.id).pattern = new;
            Ok(())
        }
        Edit::SetClipMuted { clips, muted } => {
            let found = locate_clips(&w.p, clips)?;
            for c in found {
                clip_mut(&mut w.p, c.id).muted = *muted;
            }
            Ok(())
        }
        Edit::SetLoopRegion {
            start,
            end,
            enabled,
        } => {
            if *start == 0 && *end == 0 && !*enabled {
                w.p.loop_region = Default::default();
                return Ok(());
            }
            if *end == 0 || *end > MAX_TICK {
                return Err(out_of_range("loop.end", *end as f64));
            }
            if start >= end {
                return Err(out_of_range("loop.start", *start as f64));
            }
            w.p.loop_region = protocol::model::LoopRegion {
                start: *start,
                end: *end,
                enabled: *enabled,
            };
            Ok(())
        }
        _ => Err(bad("edit is not a timeline edit")),
    }
}

fn add_clip(
    w: &mut Work,
    instrument: ChannelId,
    pattern: Option<PatternId>,
    start: u32,
    len: u32,
) -> Result<(), EditError> {
    let ci = channel_idx(&w.p, instrument)?;
    if matches!(w.p.channels[ci].instrument, Instrument::Audio) {
        return Err(bad("an audio row holds audio clips, not note clips"));
    }
    let (start, len) = check_span(start as i64, len as i64)?;
    check_clip_room(&w.p, 1)?;
    check_placements(
        &w.p,
        &[(instrument, start, start + len, None)],
        &HashSet::new(),
    )?;
    let content = match pattern {
        Some(pid) => {
            let pi = pattern_idx(&w.p, pid)?;
            if w.p.patterns[pi].instrument != instrument {
                return Err(bad("content belongs to another instrument"));
            }
            pid
        }
        None => {
            if w.p.patterns.len() >= MAX_PATTERNS {
                return Err(too_many("patterns", MAX_PATTERNS));
            }
            let name = auto_name(&w.p, instrument);
            let id = PatternId(w.alloc()?);
            w.p.patterns
                .push(Arc::new(Pattern::new(id, name, instrument)));
            id
        }
    };
    let id = ClipId(w.alloc()?);
    w.p.clips.push(Clip {
        id,
        instrument,
        pattern: content,
        start,
        len,
        offset: 0,
        muted: false,
        audio: None,
        group: None,
    });
    Ok(())
}

fn duplicate(w: &mut Work, clips: &[ClipId], dt: i64, linked: bool) -> Result<(), EditError> {
    let src = locate_clips(&w.p, clips)?;
    if src.is_empty() {
        return Ok(());
    }
    check_clip_room(&w.p, src.len())?;
    let mut planned: Vec<Placement> = Vec::with_capacity(src.len());
    for c in &src {
        let s = (c.start as i64)
            .checked_add(dt)
            .ok_or_else(|| out_of_range("clip.start", f64::INFINITY))?;
        let (s, l) = check_span(s, c.len as i64)?;
        planned.push((c.instrument, s, s + l, None));
    }
    check_placements(&w.p, &planned, &HashSet::new())?;
    // Copies of clips that share content share one new content.
    let mut mapped: HashMap<PatternId, PatternId> = HashMap::new();
    if !linked {
        for c in src.iter().filter(|c| c.audio.is_none()) {
            if mapped.contains_key(&c.pattern) {
                continue;
            }
            let name = copy_name(
                &w.p,
                c.instrument,
                &w.p.patterns[pattern_idx(&w.p, c.pattern)?].name,
            );
            let new = copy_content(w, c.pattern, c.instrument, name)?;
            mapped.insert(c.pattern, new);
        }
    }
    // Copies of grouped clips are a new instance of their pattern (20.7);
    // clips that were one instance stay one.
    let mut instances: HashMap<(u32, u32), u32> = HashMap::new();
    let mut top: HashMap<protocol::ids::GroupId, u32> = HashMap::new();
    for c in &w.p.clips {
        if let Some(g) = c.group {
            let t = top.entry(g.group).or_insert(0);
            *t = (*t).max(g.instance);
        }
    }
    for (c, pl) in src.iter().zip(&planned) {
        let id = ClipId(w.alloc()?);
        let group = c.group.map(|g| ClipGroup {
            group: g.group,
            instance: *instances.entry((g.group.0, g.instance)).or_insert_with(|| {
                let t = top.get_mut(&g.group).expect("counted");
                *t += 1;
                *t
            }),
        });
        w.p.clips.push(Clip {
            id,
            pattern: mapped.get(&c.pattern).copied().unwrap_or(c.pattern),
            start: pl.1,
            group,
            ..*c
        });
    }
    Ok(())
}

fn move_clips(w: &mut Work, clips: &[ClipId], dt: i64) -> Result<(), EditError> {
    let found = locate_clips(&w.p, clips)?;
    let mut planned: Vec<Placement> = Vec::with_capacity(found.len());
    for c in &found {
        let s = (c.start as i64)
            .checked_add(dt)
            .ok_or_else(|| out_of_range("clip.start", f64::INFINITY))?;
        let (s, l) = check_span(s, c.len as i64)?;
        planned.push((c.instrument, s, s + l, Some(c.id)));
    }
    let moving: HashSet<ClipId> = found.iter().map(|c| c.id).collect();
    check_placements(&w.p, &planned, &moving)?;
    for pl in planned {
        if let Some(id) = pl.3 {
            clip_mut(&mut w.p, id).start = pl.1;
        }
    }
    Ok(())
}

fn move_to_instrument(w: &mut Work, clip: ClipId, instrument: ChannelId) -> Result<(), EditError> {
    let c = locate_clips(&w.p, &[clip])?[0];
    let to = channel_idx(&w.p, instrument)?;
    if c.instrument == instrument {
        return Ok(());
    }
    if matches!(w.p.channels[to].instrument, Instrument::Audio) != c.audio.is_some() {
        return Err(bad("audio clips stay on audio rows, note clips on note rows"));
    }
    check_placements(
        &w.p,
        &[(instrument, c.start, c.end(), Some(c.id))],
        &HashSet::from([c.id]),
    )?;
    if c.audio.is_some() {
        clip_mut(&mut w.p, c.id).instrument = instrument;
        return Ok(());
    }
    let src_name = w.p.patterns[pattern_idx(&w.p, c.pattern)?].name.clone();
    let old_inst =
        w.p.channel(c.instrument)
            .map(|x| x.name.clone())
            .unwrap_or_default();
    let name = if is_auto_name(&src_name, &old_inst) {
        auto_name(&w.p, instrument)
    } else {
        let first = fit(&src_name, "");
        if name_in_use(&w.p, instrument, &first) {
            copy_name(&w.p, instrument, &src_name)
        } else {
            first
        }
    };
    let new = copy_content(w, c.pattern, instrument, name)?;
    let m = clip_mut(&mut w.p, c.id);
    m.instrument = instrument;
    m.pattern = new;
    collect_unused(&mut w.p, &HashSet::from([c.pattern]));
    Ok(())
}

fn resize(w: &mut Work, clips: &[ClipId], dlen: i64, from_start: bool) -> Result<(), EditError> {
    let found = locate_clips(&w.p, clips)?;
    // (id, start, len, offset)
    let mut out: Vec<(ClipId, u32, u32, u32)> = Vec::with_capacity(found.len());
    let mut planned: Vec<Placement> = Vec::with_capacity(found.len());
    for c in &found {
        let len = (c.len as i64)
            .checked_add(dlen)
            .ok_or_else(|| out_of_range("clip.len", f64::INFINITY))?;
        let (start, offset) = if from_start && c.audio.is_some() {
            // An audio clip trims into the sample: growing at the start
            // uncovers earlier audio, so it cannot go below 0.
            let offset = c.offset as i64 - dlen;
            if !(0..=MAX_TICK as i64).contains(&offset) {
                return Err(out_of_range("clip.offset", offset as f64));
            }
            (
                (c.start as i64)
                    .checked_sub(dlen)
                    .ok_or_else(|| out_of_range("clip.start", f64::INFINITY))?,
                offset as u32,
            )
        } else if from_start {
            let content = w.p.patterns[pattern_idx(&w.p, c.pattern)?]
                .length_ticks()
                .max(1) as i64;
            (
                (c.start as i64)
                    .checked_sub(dlen)
                    .ok_or_else(|| out_of_range("clip.start", f64::INFINITY))?,
                (c.offset as i64 - dlen % content).rem_euclid(content) as u32,
            )
        } else {
            (c.start as i64, c.offset)
        };
        let (start, len) = check_span(start, len)?;
        planned.push((c.instrument, start, start + len, Some(c.id)));
        out.push((c.id, start, len, offset));
    }
    let moving: HashSet<ClipId> = found.iter().map(|c| c.id).collect();
    check_placements(&w.p, &planned, &moving)?;
    for (id, start, len, offset) in out {
        let m = clip_mut(&mut w.p, id);
        m.start = start;
        m.len = len;
        m.offset = offset;
        if let Some(a) = &mut m.audio {
            a.fade_in = a.fade_in.min(len);
            a.fade_out = a.fade_out.min(len);
        }
    }
    Ok(())
}

fn split(w: &mut Work, clip: ClipId, at: u32) -> Result<(), EditError> {
    let c = locate_clips(&w.p, &[clip])?[0];
    if at <= c.start || at >= c.end() {
        return Err(bad("split point must be inside the clip"));
    }
    check_clip_room(&w.p, 1)?;
    let left = at - c.start;
    if let Some(a) = c.audio {
        // The right part plays on from where the left stops; the fade-in
        // stays with the left part and the fade-out with the right.
        let right_len = c.end() - at;
        let id = ClipId(w.alloc()?);
        let m = clip_mut(&mut w.p, c.id);
        m.len = left;
        if let Some(x) = &mut m.audio {
            x.fade_in = a.fade_in.min(left);
            x.fade_out = 0;
        }
        w.p.clips.push(Clip {
            id,
            start: at,
            len: right_len,
            offset: c
                .offset
                .checked_add(left)
                .filter(|o| *o <= MAX_TICK)
                .ok_or_else(|| out_of_range("clip.offset", c.offset as f64 + left as f64))?,
            audio: Some(AudioSource {
                fade_in: 0,
                fade_out: a.fade_out.min(right_len),
                ..a
            }),
            ..c
        });
        return Ok(());
    }
    let content = w.p.patterns[pattern_idx(&w.p, c.pattern)?]
        .length_ticks()
        .max(1) as u64;
    let id = ClipId(w.alloc()?);
    clip_mut(&mut w.p, c.id).len = left;
    w.p.clips.push(Clip {
        id,
        start: at,
        len: c.end() - at,
        offset: ((c.offset as u64 + left as u64) % content) as u32,
        ..c
    });
    Ok(())
}
