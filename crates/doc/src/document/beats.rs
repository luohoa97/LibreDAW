// SPDX-License-Identifier: GPL-3.0-or-later
//! The Milestone B edits (SPEC 15, 17.2): step lanes, samples, sampler and
//! 808, built-in effects, sends and sidechains, the playlist.
//!
//! Every function checks its arguments before it changes anything, and
//! reports the exact reason. Rules that need the whole project (routing
//! cycles, effect pools, clip overlap, sample references) are left to
//! `protocol::validate`, which `apply_batch` runs on the result.

use std::collections::HashSet;
use std::sync::Arc;

use protocol::beats::{BuiltinFx, SampleMode};
use protocol::consts::*;
use protocol::edit::{Edit, EditError};
use protocol::ids::{ChannelId, ClipId, InstanceId, NoteId, PatternId, PlaylistTrackId, TrackId};
use protocol::model::{Clip, Insert, Instrument, Note, PlaylistTrack, Project, Send};
use protocol::validate::ValidationError;

use super::{
    Work, bad, channel_idx, check_vel, locate, not_found, out_of_range, pattern_idx, too_many,
    track_idx,
};

/// Fails unless a sample with this hash is registered in the project.
pub(super) fn require_sample(p: &Project, hash: &str) -> Result<(), EditError> {
    if p.samples.iter().any(|s| s.hash == hash) {
        Ok(())
    } else {
        Err(EditError::NotFound {
            what: format!("sample {hash}"),
            id: 0,
        })
    }
}

/// Removes everything that points at a removed track: sends to it and
/// compressor sidechains reading from it (17.2).
pub(super) fn forget_track(p: &mut Project, gone: TrackId) {
    for t in &mut p.tracks {
        let has_send = t.sends.iter().any(|s| s.to == gone);
        let has_key = t.inserts.iter().any(|i| {
            matches!(
                i,
                Insert::Builtin {
                    fx: BuiltinFx::Compressor {
                        sidechain: Some(s), ..
                    },
                    ..
                } if *s == gone
            )
        });
        if !has_send && !has_key {
            continue;
        }
        let t = Arc::make_mut(t);
        t.sends.retain(|s| s.to != gone);
        for i in &mut t.inserts {
            if let Insert::Builtin {
                fx: BuiltinFx::Compressor { sidechain, .. },
                ..
            } = i
                && *sidechain == Some(gone)
            {
                *sidechain = None;
            }
        }
    }
}

fn check_range(field: &str, v: f64, (lo, hi): (f64, f64)) -> Result<(), EditError> {
    if v.is_finite() && v >= lo && v <= hi {
        Ok(())
    } else {
        Err(out_of_range(field, v))
    }
}

/// Index of the built-in insert with this instance id on a track.
fn builtin_idx(p: &Project, ti: usize, instance: InstanceId) -> Result<usize, EditError> {
    let ii = p.tracks[ti]
        .inserts
        .iter()
        .position(|i| i.instance() == instance)
        .ok_or_else(|| not_found("insert", instance.0))?;
    match p.tracks[ti].inserts[ii] {
        Insert::Builtin { .. } => Ok(ii),
        Insert::Clap(_) => Err(bad("insert is not a built-in effect")),
    }
}

fn fx_mut(p: &mut Project, ti: usize, ii: usize) -> &mut BuiltinFx {
    match &mut Arc::make_mut(&mut p.tracks[ti]).inserts[ii] {
        Insert::Builtin { fx, .. } => fx,
        Insert::Clap(_) => unreachable!("checked by builtin_idx"),
    }
}

fn playlist_idx(p: &Project, id: PlaylistTrackId) -> Result<usize, EditError> {
    p.playlist
        .iter()
        .position(|t| t.id == id)
        .ok_or_else(|| not_found("playlist track", id.0))
}

/// `(row, index in row)` of every named clip; `NotFound` for the first
/// missing id. Duplicates count once.
fn locate_clips(p: &Project, ids: &[ClipId]) -> Result<Vec<(usize, ClipId)>, EditError> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for id in ids {
        if !seen.insert(*id) {
            continue;
        }
        let row = p
            .playlist
            .iter()
            .position(|t| t.clips.iter().any(|c| c.id == *id))
            .ok_or_else(|| not_found("clip", id.0))?;
        out.push((row, *id));
    }
    Ok(out)
}

pub(super) fn apply(w: &mut Work, e: &Edit) -> Result<(), EditError> {
    match e {
        // Steps and groove
        Edit::SetStepLanes {
            pattern,
            channel,
            step,
            vel,
            off,
            repeat,
        } => set_step_lanes(w, *pattern, *channel, *step, *vel, *off, *repeat)?,
        Edit::SetNoteRepeat {
            pattern,
            notes,
            repeat,
        } => {
            let pi = pattern_idx(&w.p, *pattern)?;
            if !RATCHETS.contains(repeat) {
                return Err(out_of_range("note.repeat", *repeat as f64));
            }
            let want = locate(&w.p.patterns[pi], notes)?;
            for cn in &w.p.patterns[pi].notes {
                for n in cn.notes.iter().filter(|n| want.contains(&n.id)) {
                    if !n.len.is_multiple_of(*repeat as u32) {
                        return Err(bad("note length must be divisible by its repeat"));
                    }
                }
            }
            if want.is_empty() {
                return Ok(());
            }
            let pat = Arc::make_mut(&mut w.p.patterns[pi]);
            for cn in &mut pat.notes {
                for n in cn.notes.iter_mut().filter(|n| want.contains(&n.id)) {
                    n.repeat = *repeat;
                }
            }
        }
        Edit::SetSwing { pattern, swing } => {
            let pi = pattern_idx(&w.p, *pattern)?;
            if *swing > MAX_SWING {
                return Err(out_of_range("pattern.swing", *swing as f64));
            }
            Arc::make_mut(&mut w.p.patterns[pi]).swing = *swing;
        }
        Edit::SetChokeGroup { channel, group } => {
            let ci = channel_idx(&w.p, *channel)?;
            if *group > MAX_CHOKE_GROUP {
                return Err(out_of_range("channel.choke_group", *group as f64));
            }
            Arc::make_mut(&mut w.p.channels[ci]).choke_group = *group;
        }

        // Samples
        Edit::AddSample { sample } => {
            if w.p.samples.iter().any(|s| s.hash == sample.hash) {
                return Ok(());
            }
            if w.p.samples.len() >= MAX_SAMPLES {
                return Err(too_many("samples", MAX_SAMPLES));
            }
            protocol::validate::check_hash("sample.hash", &sample.hash)?;
            protocol::validate::check_name("sample.orig_name", &sample.orig_name)?;
            w.p.samples.push(sample.clone());
        }
        Edit::RemoveSample { hash } => {
            let i =
                w.p.samples
                    .iter()
                    .position(|s| s.hash == *hash)
                    .ok_or_else(|| EditError::NotFound {
                        what: format!("sample {hash}"),
                        id: 0,
                    })?;
            if let Some(c) = w.p.channels.iter().find(
                |c| matches!(&c.instrument, Instrument::Sampler(s) if s.sample.as_deref() == Some(hash)),
            ) {
                return Err(EditError::BadArgument {
                    what: format!("sample is used by channel {}", c.id),
                });
            }
            w.p.samples.remove(i);
        }

        // Sampler
        Edit::SetSamplerSample { channel, sample } => {
            let ci = sampler_channel(&w.p, *channel)?;
            if let Some(h) = sample {
                require_sample(&w.p, h)?;
            }
            if let Instrument::Sampler(s) = &mut Arc::make_mut(&mut w.p.channels[ci]).instrument {
                s.sample = sample.clone();
            }
        }
        Edit::SetSamplerMode {
            channel,
            mode,
            reverse,
        } => {
            let ci = sampler_channel(&w.p, *channel)?;
            let mode: SampleMode = *mode;
            if let Instrument::Sampler(s) = &mut Arc::make_mut(&mut w.p.channels[ci]).instrument {
                s.mode = mode;
                s.reverse = *reverse;
            }
        }
        Edit::SetSamplerParam {
            channel,
            param,
            value,
        } => {
            use protocol::beats::SamplerParam;
            let ci = sampler_channel(&w.p, *channel)?;
            check_range(&format!("sampler.{param:?}"), *value, param.range())?;
            if let Instrument::Sampler(s) = &mut Arc::make_mut(&mut w.p.channels[ci]).instrument {
                match param {
                    SamplerParam::Start if *value >= s.params.end => {
                        return Err(out_of_range("sampler.start", *value));
                    }
                    SamplerParam::End if *value <= s.params.start => {
                        return Err(out_of_range("sampler.end", *value));
                    }
                    _ => {}
                }
                s.params.set(*param, *value);
            }
        }

        // 808
        Edit::SetBass808Mono { channel, mono } => {
            let ci = bass_channel(&w.p, *channel)?;
            if let Instrument::Bass808(b) = &mut Arc::make_mut(&mut w.p.channels[ci]).instrument {
                b.mono = *mono;
            }
        }
        Edit::SetBass808Param {
            channel,
            param,
            value,
        } => {
            let ci = bass_channel(&w.p, *channel)?;
            check_range(&format!("bass808.{param:?}"), *value, param.range())?;
            if let Instrument::Bass808(b) = &mut Arc::make_mut(&mut w.p.channels[ci]).instrument {
                b.params.set(*param, *value);
            }
        }

        // Built-in effects and routing
        Edit::AddBuiltinInsert { track, index, fx } => {
            let ti = track_idx(&w.p, *track)?;
            let n = w.p.tracks[ti].inserts.len();
            if n >= MAX_INSERTS {
                return Err(too_many("inserts", MAX_INSERTS));
            }
            if *index as usize > n {
                return Err(bad("insert index is past the end"));
            }
            let instance = InstanceId(w.alloc()?);
            Arc::make_mut(&mut w.p.tracks[ti]).inserts.insert(
                *index as usize,
                Insert::Builtin {
                    instance,
                    fx: BuiltinFx::new(*fx),
                },
            );
        }
        Edit::SetFxParam {
            track,
            instance,
            param,
            value,
        } => {
            let ti = track_idx(&w.p, *track)?;
            let ii = builtin_idx(&w.p, ti, *instance)?;
            let Insert::Builtin { fx, .. } = &w.p.tracks[ti].inserts[ii] else {
                unreachable!("checked by builtin_idx");
            };
            let range = fx
                .param_range(*param as usize)
                .ok_or_else(|| bad("effect has no parameter with that index"))?;
            let name = fx.param_name(*param as usize).unwrap_or("param");
            check_range(&format!("fx.{:?}.{name}", fx.kind()), *value, range)?;
            fx_mut(&mut w.p, ti, ii).set_param(*param as usize, *value);
        }
        Edit::SetSaturatorCurve {
            track,
            instance,
            curve,
        } => {
            let ti = track_idx(&w.p, *track)?;
            let ii = builtin_idx(&w.p, ti, *instance)?;
            match fx_mut(&mut w.p, ti, ii) {
                BuiltinFx::Saturator { curve: c, .. } => *c = *curve,
                _ => return Err(bad("effect is not a saturator")),
            }
        }
        Edit::SetDelayPingPong {
            track,
            instance,
            ping_pong,
        } => {
            let ti = track_idx(&w.p, *track)?;
            let ii = builtin_idx(&w.p, ti, *instance)?;
            match fx_mut(&mut w.p, ti, ii) {
                BuiltinFx::Delay { ping_pong: p, .. } => *p = *ping_pong,
                _ => return Err(bad("effect is not a delay")),
            }
        }
        Edit::SetSidechain {
            track,
            instance,
            source,
        } => {
            let ti = track_idx(&w.p, *track)?;
            let ii = builtin_idx(&w.p, ti, *instance)?;
            if let Some(src) = source {
                track_idx(&w.p, *src)?;
                if src == track {
                    return Err(bad("a track cannot sidechain itself"));
                }
            }
            match fx_mut(&mut w.p, ti, ii) {
                BuiltinFx::Compressor { sidechain, .. } => *sidechain = *source,
                _ => return Err(bad("effect is not a compressor")),
            }
        }
        Edit::MoveInsert {
            track,
            instance,
            index,
        } => {
            let ti = track_idx(&w.p, *track)?;
            let n = w.p.tracks[ti].inserts.len();
            let from = w.p.tracks[ti]
                .inserts
                .iter()
                .position(|i| i.instance() == *instance)
                .ok_or_else(|| not_found("insert", instance.0))?;
            if *index as usize >= n {
                return Err(bad("insert index is past the end"));
            }
            if from != *index as usize {
                let t = Arc::make_mut(&mut w.p.tracks[ti]);
                let ins = t.inserts.remove(from);
                t.inserts.insert(*index as usize, ins);
            }
        }
        Edit::SetSend {
            track,
            to,
            level_db,
            pre_fader,
        } => {
            let ti = track_idx(&w.p, *track)?;
            track_idx(&w.p, *to)?;
            if *to == TrackId::MASTER || to == track {
                return Err(bad(
                    "a send needs a target track other than itself and the master",
                ));
            }
            check_range("send.level_db", *level_db, (MIN_GAIN_DB, MAX_GAIN_DB))?;
            let t = &w.p.tracks[ti];
            let existing = t.sends.iter().position(|s| s.to == *to);
            if existing.is_none() && t.sends.len() >= MAX_SENDS {
                return Err(too_many("sends", MAX_SENDS));
            }
            let send = Send {
                to: *to,
                level_db: *level_db,
                pre_fader: *pre_fader,
            };
            let t = Arc::make_mut(&mut w.p.tracks[ti]);
            match existing {
                Some(i) => t.sends[i] = send,
                None => t.sends.push(send),
            }
        }
        Edit::RemoveSend { track, to } => {
            let ti = track_idx(&w.p, *track)?;
            let i = w.p.tracks[ti]
                .sends
                .iter()
                .position(|s| s.to == *to)
                .ok_or_else(|| not_found("send", to.0))?;
            Arc::make_mut(&mut w.p.tracks[ti]).sends.remove(i);
        }

        // Playlist
        Edit::AddPlaylistTrack { name } => {
            if w.p.playlist.len() >= MAX_PLAYLIST_TRACKS {
                return Err(too_many("playlist tracks", MAX_PLAYLIST_TRACKS));
            }
            let id = PlaylistTrackId(w.alloc()?);
            w.p.playlist.push(Arc::new(PlaylistTrack {
                id,
                name: name.clone(),
                clips: Vec::new(),
            }));
        }
        Edit::RemovePlaylistTrack { track } => {
            let i = playlist_idx(&w.p, *track)?;
            w.p.playlist.remove(i);
        }
        Edit::RenamePlaylistTrack { track, name } => {
            let i = playlist_idx(&w.p, *track)?;
            Arc::make_mut(&mut w.p.playlist[i]).name = name.clone();
        }
        Edit::AddClip {
            track,
            pattern,
            start,
            len,
        } => {
            let ti = playlist_idx(&w.p, *track)?;
            pattern_idx(&w.p, *pattern)?;
            if *len == 0 {
                return Err(out_of_range("clip.len", 0.0));
            }
            if *start as u64 + *len as u64 > MAX_TICK as u64 {
                return Err(out_of_range("clip.end", *start as f64 + *len as f64));
            }
            let total: usize = w.p.playlist.iter().map(|t| t.clips.len()).sum();
            if total >= MAX_CLIPS {
                return Err(too_many("clips", MAX_CLIPS));
            }
            if let Some(o) = w.p.playlist[ti]
                .clips
                .iter()
                .find(|c| (c.start as u64) < *start as u64 + *len as u64 && *start < c.end())
            {
                return Err(overlap(o.id));
            }
            let id = ClipId(w.alloc()?);
            Arc::make_mut(&mut w.p.playlist[ti]).clips.push(Clip {
                id,
                pattern: *pattern,
                start: *start,
                len: *len,
            });
        }
        Edit::RemoveClips { clips } => {
            let found = locate_clips(&w.p, clips)?;
            let gone: HashSet<ClipId> = found.iter().map(|f| f.1).collect();
            for (row, _) in found {
                let t = Arc::make_mut(&mut w.p.playlist[row]);
                t.clips.retain(|c| !gone.contains(&c.id));
            }
        }
        Edit::MoveClips { clips, dt, dtrack } => move_clips(w, clips, *dt, *dtrack)?,
        Edit::ResizeClips { clips, dlen } => {
            let found = locate_clips(&w.p, clips)?;
            // Check every clip before touching any.
            for (row, id) in &found {
                let c = w.p.playlist[*row]
                    .clips
                    .iter()
                    .find(|c| c.id == *id)
                    .expect("located");
                let l = (c.len as i64)
                    .checked_add(*dlen)
                    .ok_or_else(|| out_of_range("clip.len", f64::INFINITY))?;
                if l < 1 {
                    return Err(out_of_range("clip.len", l as f64));
                }
                if c.start as i64 + l > MAX_TICK as i64 {
                    return Err(out_of_range("clip.end", (c.start as i64 + l) as f64));
                }
            }
            for (row, id) in found {
                let t = Arc::make_mut(&mut w.p.playlist[row]);
                if let Some(c) = t.clips.iter_mut().find(|c| c.id == id) {
                    c.len = (c.len as i64 + *dlen) as u32;
                }
            }
        }

        // Not ours; the dispatcher in `document.rs` never sends these here.
        _ => return Err(bad("edit is not a Milestone B edit")),
    }
    Ok(())
}

fn overlap(id: ClipId) -> EditError {
    EditError::Invalid {
        reason: ValidationError::Overlap {
            what: "clips".into(),
            id: id.0,
        },
    }
}

fn sampler_channel(p: &Project, id: ChannelId) -> Result<usize, EditError> {
    let ci = channel_idx(p, id)?;
    if matches!(p.channels[ci].instrument, Instrument::Sampler(_)) {
        Ok(ci)
    } else {
        Err(bad("channel is not a sampler"))
    }
}

fn bass_channel(p: &Project, id: ChannelId) -> Result<usize, EditError> {
    let ci = channel_idx(p, id)?;
    if matches!(p.channels[ci].instrument, Instrument::Bass808(_)) {
        Ok(ci)
    } else {
        Err(bad("channel is not an 808"))
    }
}

fn set_step_lanes(
    w: &mut Work,
    pattern: PatternId,
    channel: ChannelId,
    step: u8,
    vel: Option<u8>,
    off: Option<i8>,
    repeat: Option<u8>,
) -> Result<(), EditError> {
    let pi = pattern_idx(&w.p, pattern)?;
    let ci = channel_idx(&w.p, channel)?;
    let root = w.p.channels[ci].root_key;
    let (step_ticks, start) = {
        let pat = &w.p.patterns[pi];
        if step >= pat.length_steps {
            return Err(bad("step is outside the pattern"));
        }
        (pat.step_ticks, step as u32 * pat.step_ticks)
    };
    if let Some(v) = vel {
        check_vel(v)?;
    }
    if let Some(o) = off
        && o.unsigned_abs() > MAX_STEP_OFFSET as u8
    {
        return Err(out_of_range("note.off", o as f64));
    }
    if let Some(r) = repeat
        && (!RATCHETS.contains(&r) || !step_ticks.is_multiple_of(r as u32))
    {
        return Err(out_of_range("note.repeat", r as f64));
    }
    if let Some(o) = off {
        let k = root as i16 + o as i16;
        if !(0..=127).contains(&k) {
            return Err(out_of_range("note.key", k as f64));
        }
    }
    let pat = &w.p.patterns[pi];
    let targets: Vec<NoteId> = pat
        .notes_of(channel)
        .iter()
        .filter(|n| n.start == start && n.is_step_note(root, pat))
        .map(|n| n.id)
        .collect();
    if targets.is_empty() {
        return Err(bad("no step note at that step"));
    }
    let want: HashSet<NoteId> = targets.into_iter().collect();
    let pat = Arc::make_mut(&mut w.p.patterns[pi]);
    for cn in pat.notes.iter_mut().filter(|c| c.channel == channel) {
        for n in cn.notes.iter_mut().filter(|n| want.contains(&n.id)) {
            apply_lanes(n, root, vel, off, repeat);
        }
        cn.notes.sort_by_key(|n| (n.start, n.key, n.id));
    }
    Ok(())
}

fn apply_lanes(n: &mut Note, root: u8, vel: Option<u8>, off: Option<i8>, repeat: Option<u8>) {
    if let Some(v) = vel {
        n.vel = v;
    }
    if let Some(o) = off {
        n.off = o;
        n.key = (root as i16 + o as i16) as u8;
    }
    if let Some(r) = repeat {
        n.repeat = r;
    }
}

fn move_clips(w: &mut Work, clips: &[ClipId], dt: i64, dtrack: i32) -> Result<(), EditError> {
    let found = locate_clips(&w.p, clips)?;
    let rows = w.p.playlist.len() as i64;
    // (clip, destination row), all checked before anything changes.
    let mut moves: Vec<(Clip, usize)> = Vec::new();
    for (row, id) in &found {
        let c = *w.p.playlist[*row]
            .clips
            .iter()
            .find(|c| c.id == *id)
            .expect("located");
        let dest = *row as i64 + dtrack as i64;
        if !(0..rows).contains(&dest) {
            return Err(out_of_range("clip.track", dest as f64));
        }
        let s = (c.start as i64)
            .checked_add(dt)
            .ok_or_else(|| out_of_range("clip.start", f64::INFINITY))?;
        if s < 0 {
            return Err(out_of_range("clip.start", s as f64));
        }
        if s.saturating_add(c.len as i64) > MAX_TICK as i64 {
            return Err(out_of_range(
                "clip.end",
                s.saturating_add(c.len as i64) as f64,
            ));
        }
        moves.push((
            Clip {
                start: s as u32,
                ..c
            },
            dest as usize,
        ));
    }
    if moves.is_empty() || (dt == 0 && dtrack == 0) {
        return Ok(());
    }
    let gone: HashSet<ClipId> = moves.iter().map(|m| m.0.id).collect();
    for (row, _) in &found {
        let t = Arc::make_mut(&mut w.p.playlist[*row]);
        t.clips.retain(|c| !gone.contains(&c.id));
    }
    for (clip, dest) in moves {
        Arc::make_mut(&mut w.p.playlist[dest]).clips.push(clip);
    }
    // Overlap is reported by validation, after `sort_canonical`.
    Ok(())
}
