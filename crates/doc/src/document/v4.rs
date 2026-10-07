// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio clips, patterns on the timeline, and shapes (SPEC 20.7, 21.1,
//! 24.2-1).
//!
//! Choices where the spec leaves room:
//! - `MakePattern` takes at most one clip per row, none already in a
//!   pattern. Each clip moves to the earliest start of the set and keeps
//!   its own length. The pattern gets a colour from a fixed palette, by
//!   how many patterns there are already. The new instance is number 1.
//! - `PlacePattern` copies the members of the lowest-numbered instance,
//!   linked (same content), at `start` plus their offset from that
//!   instance's first clip, as the next free instance number.
//! - A pattern with no clips left is removed (`collect_groups`), so a
//!   pattern list never shows one that cannot be placed.
//! - Removing a channel, track or insert removes the shapes that
//!   targeted it.
//! - Points given to `AddShape` and `SetShapePoints` are sorted by tick;
//!   two points on one tick are refused by validation.

use std::collections::HashSet;

use protocol::consts::*;
use protocol::edit::{Edit, EditError};
use protocol::ids::{ChannelId, ClipId, GroupId, InstanceId, TrackId};
use protocol::model::{
    AudioSource, Clip, ClipGroup, Instrument, PatternGroup, Project, Shape, ShapeTarget,
};
use protocol::ids::ShapeId;
use protocol::validate::check_name;

use super::clips::{check_clip_room, check_placements, check_span, locate_clips, Placement};
use super::beats::require_sample;
use super::{Work, bad, channel_idx, not_found, out_of_range, too_many};

/// Colours handed to new patterns, in turn.
const PALETTE: [u32; 8] = [
    0xE5_5B_5B, 0xE5_9A_3C, 0xD9_C8_3A, 0x5F_C2_6B, 0x3F_B8_C9, 0x5B_8C_E5, 0x9A_6B_E5, 0xD9_5B_B5,
];

pub(super) fn apply(w: &mut Work, e: &Edit) -> Result<(), EditError> {
    match e {
        Edit::AddAudioClip {
            instrument,
            sample,
            start,
            len,
            offset,
        } => {
            let ci = channel_idx(&w.p, *instrument)?;
            if !matches!(w.p.channels[ci].instrument, Instrument::Audio) {
                return Err(bad("audio clips go on an audio row"));
            }
            require_sample(&w.p, &sample.to_hex())?;
            let (start, len) = check_span(*start as i64, *len as i64)?;
            if *offset > MAX_TICK {
                return Err(out_of_range("clip.offset", *offset as f64));
            }
            check_clip_room(&w.p, 1)?;
            check_placements(
                &w.p,
                &[(*instrument, start, start + len, None)],
                &HashSet::new(),
            )?;
            let id = ClipId(w.alloc()?);
            w.p.clips.push(Clip {
                id,
                instrument: *instrument,
                pattern: protocol::ids::PatternId::NONE,
                start,
                len,
                offset: *offset,
                muted: false,
                audio: Some(AudioSource {
                    sample: *sample,
                    gain_mdb: 0,
                    fade_in: 0,
                    fade_out: 0,
                }),
                group: None,
            });
            Ok(())
        }
        Edit::SetClipAudio {
            clip,
            gain_mdb,
            fade_in,
            fade_out,
        } => {
            let c = locate_clips(&w.p, &[*clip])?[0];
            if c.audio.is_none() {
                return Err(bad("clip is not an audio clip"));
            }
            if !(-100_000..=24_000).contains(gain_mdb) {
                return Err(out_of_range("clip.audio.gain_mdb", *gain_mdb as f64));
            }
            if *fade_in > c.len {
                return Err(out_of_range("clip.audio.fade_in", *fade_in as f64));
            }
            if *fade_out > c.len {
                return Err(out_of_range("clip.audio.fade_out", *fade_out as f64));
            }
            let m = w.p.clips.iter_mut().find(|x| x.id == *clip).expect("located");
            if let Some(a) = &mut m.audio {
                a.gain_mdb = *gain_mdb;
                a.fade_in = *fade_in;
                a.fade_out = *fade_out;
            }
            Ok(())
        }
        Edit::MakePattern { clips, name } => make_pattern(w, clips, name),
        Edit::PlacePattern { group, start } => place_pattern(w, *group, *start),
        Edit::Ungroup { clips } => {
            let found = locate_clips(&w.p, clips)?;
            for c in found {
                w.p.clips
                    .iter_mut()
                    .find(|x| x.id == c.id)
                    .expect("located")
                    .group = None;
            }
            Ok(())
        }
        Edit::RenameGroup { group, name } => {
            check_name("group.name", name)?;
            let g = w
                .p
                .groups
                .iter_mut()
                .find(|g| g.id == *group)
                .ok_or_else(|| not_found("pattern", group.0))?;
            g.name = name.clone();
            Ok(())
        }
        Edit::AddShape { target, points } => {
            if w.p.shapes.len() >= MAX_SHAPES {
                return Err(too_many("shapes", MAX_SHAPES));
            }
            let mut points = points.clone();
            points.sort_by_key(|p| p.tick);
            let id = ShapeId(w.alloc()?);
            w.p.shapes.push(Shape {
                id,
                target: *target,
                points,
            });
            Ok(())
        }
        Edit::SetShapePoints { shape, points } => {
            let s = w
                .p
                .shapes
                .iter_mut()
                .find(|s| s.id == *shape)
                .ok_or_else(|| not_found("shape", shape.0))?;
            let mut points = points.clone();
            points.sort_by_key(|p| p.tick);
            s.points = points;
            Ok(())
        }
        Edit::RemoveShape { shape } => {
            let i = w
                .p
                .shapes
                .iter()
                .position(|s| s.id == *shape)
                .ok_or_else(|| not_found("shape", shape.0))?;
            w.p.shapes.remove(i);
            Ok(())
        }
        _ => Err(bad("edit is not an audio, pattern or shape edit")),
    }
}

fn make_pattern(w: &mut Work, clips: &[ClipId], name: &str) -> Result<(), EditError> {
    check_name("group.name", name)?;
    let found = locate_clips(&w.p, clips)?;
    if found.is_empty() {
        return Err(bad("a pattern needs at least one clip"));
    }
    if w.p.groups.len() >= MAX_GROUPS {
        return Err(too_many("patterns", MAX_GROUPS));
    }
    if found.iter().any(|c| c.group.is_some()) {
        return Err(bad("a clip is already in a pattern; ungroup it first"));
    }
    let rows: HashSet<ChannelId> = found.iter().map(|c| c.instrument).collect();
    if rows.len() != found.len() {
        return Err(bad("a pattern holds one clip per row"));
    }
    let at = found.iter().map(|c| c.start).min().expect("not empty");
    let planned: Vec<Placement> = found
        .iter()
        .map(|c| (c.instrument, at, at + c.len, Some(c.id)))
        .collect();
    let moving: HashSet<ClipId> = found.iter().map(|c| c.id).collect();
    check_placements(&w.p, &planned, &moving)?;
    let gid = GroupId(w.alloc()?);
    w.p.groups.push(PatternGroup {
        id: gid,
        name: name.to_string(),
        color: PALETTE[(w.p.groups.len()) % PALETTE.len()],
    });
    for c in &found {
        let m = w.p.clips.iter_mut().find(|x| x.id == c.id).expect("located");
        m.start = at;
        m.group = Some(ClipGroup {
            group: gid,
            instance: 1,
        });
    }
    Ok(())
}

fn place_pattern(w: &mut Work, group: GroupId, start: u32) -> Result<(), EditError> {
    if !w.p.groups.iter().any(|g| g.id == group) {
        return Err(not_found("pattern", group.0));
    }
    let first = w
        .p
        .clips
        .iter()
        .filter_map(|c| c.group.filter(|g| g.group == group))
        .map(|g| g.instance)
        .min()
        .ok_or_else(|| bad("pattern has no clips to place"))?;
    let next = w
        .p
        .clips
        .iter()
        .filter_map(|c| c.group.filter(|g| g.group == group))
        .map(|g| g.instance)
        .max()
        .expect("has a first")
        + 1;
    let members: Vec<Clip> = w
        .p
        .clips
        .iter()
        .filter(|c| c.group == Some(ClipGroup { group, instance: first }))
        .copied()
        .collect();
    let base = members.iter().map(|c| c.start).min().expect("has members");
    check_clip_room(&w.p, members.len())?;
    let mut planned: Vec<Placement> = Vec::with_capacity(members.len());
    for c in &members {
        let s = start as i64 + (c.start - base) as i64;
        let (s, l) = check_span(s, c.len as i64)?;
        planned.push((c.instrument, s, s + l, None));
    }
    check_placements(&w.p, &planned, &HashSet::new())?;
    for (c, pl) in members.iter().zip(&planned) {
        let id = ClipId(w.alloc()?);
        w.p.clips.push(Clip {
            id,
            start: pl.1,
            group: Some(ClipGroup {
                group,
                instance: next,
            }),
            ..*c
        });
    }
    Ok(())
}

/// Removes patterns that no clip belongs to any more.
pub(super) fn collect_groups(p: &mut Project) {
    if p.groups.is_empty() {
        return;
    }
    let used: HashSet<GroupId> = p.clips.iter().filter_map(|c| c.group.map(|g| g.group)).collect();
    p.groups.retain(|g| used.contains(&g.id));
}

/// Removes the shapes that target something that is gone.
fn forget(p: &mut Project, gone: impl Fn(&ShapeTarget) -> bool) {
    p.shapes.retain(|s| !gone(&s.target));
}

pub(super) fn forget_channel(p: &mut Project, ch: ChannelId) {
    forget(p, |t| {
        matches!(t, ShapeTarget::Pitch { instrument } | ShapeTarget::Filter { instrument } if *instrument == ch)
    });
}

pub(super) fn forget_track(p: &mut Project, tr: TrackId) {
    forget(p, |t| {
        matches!(
            t,
            ShapeTarget::Volume { track }
                | ShapeTarget::Pan { track }
                | ShapeTarget::FxParam { track, .. } if *track == tr
        )
    });
}

pub(super) fn forget_insert(p: &mut Project, tr: TrackId, inst: InstanceId) {
    forget(p, |t| {
        matches!(t, ShapeTarget::FxParam { track, instance, .. } if *track == tr && *instance == inst)
    });
}
