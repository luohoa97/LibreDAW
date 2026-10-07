// SPDX-License-Identifier: GPL-3.0-or-later
//! The Patterns lane (SPEC 20.7), without GTK: a pattern is a named group
//! of clips across rows, placed as one block. The blocks are computed from
//! the clips' group membership: one block per placed instance of a
//! pattern, as wide as its members together.

use protocol::edit::Edit;
use protocol::ids::{ChannelId, ClipId, GroupId};
use protocol::model::{Clip, Project};

use crate::timeline_logic as tl;

/// The tooltip of the lane and its blocks (SPEC 20.6).
pub const TOOLTIP: &str = "A beat or section made of several instruments, placed as one block";

/// One placed instance of a pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub group: GroupId,
    pub instance: u32,
    pub name: String,
    /// 0xRRGGBB.
    pub color: u32,
    pub start: u32,
    pub end: u32,
    /// The member clips, in row order.
    pub clips: Vec<ClipId>,
    /// The rows the members are on, in the same order.
    pub rows: Vec<ChannelId>,
}

/// Every placed instance, by start.
pub fn blocks(p: &Project) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    for c in &p.clips {
        let Some(g) = c.group else { continue };
        let at = out
            .iter()
            .position(|b| b.group == g.group && b.instance == g.instance);
        let i = match at {
            Some(i) => i,
            None => {
                let (name, color) = p
                    .groups
                    .iter()
                    .find(|x| x.id == g.group)
                    .map(|x| (x.name.clone(), x.color))
                    .unwrap_or_default();
                out.push(Block {
                    group: g.group,
                    instance: g.instance,
                    name,
                    color,
                    start: c.start,
                    end: c.end(),
                    clips: Vec::new(),
                    rows: Vec::new(),
                });
                out.len() - 1
            }
        };
        let b = &mut out[i];
        b.start = b.start.min(c.start);
        b.end = b.end.max(c.end());
        b.clips.push(c.id);
        b.rows.push(c.instrument);
    }
    out.sort_by_key(|b| (b.start, b.group, b.instance));
    out
}

/// The block at `tick`: of those covering it, the one that started last
/// (it is drawn on top).
pub fn block_at(blocks: &[Block], tick: u32) -> Option<&Block> {
    blocks
        .iter()
        .filter(|b| b.start <= tick && tick < b.end)
        .max_by_key(|b| b.start)
}

/// The block a clip belongs to.
pub fn block_of(blocks: &[Block], clip: ClipId) -> Option<&Block> {
    blocks.iter().find(|b| b.clips.contains(&clip))
}

/// Moving a block moves all its members.
pub fn move_edit(b: &Block, dt: i64) -> Edit {
    Edit::MoveClips {
        clips: b.clips.clone(),
        dt,
    }
}

/// Whether the block can move by `dt` without a member overlapping another
/// clip or going before the start.
pub fn fits_move(clips: &[Clip], b: &Block, dt: i64) -> bool {
    tl::fits_move(clips, &b.clips, dt)
}

/// Ctrl+D: a new instance right after the block, as linked copies of its
/// members. `None` when there is no room.
pub fn duplicate_edit(clips: &[Clip], b: &Block) -> Option<Edit> {
    let dt = tl::duplicate_offset(clips, &b.clips)?;
    Some(Edit::DuplicateClips {
        clips: b.clips.clone(),
        dt,
        linked: true,
    })
}

/// Delete: removes the members.
pub fn delete_edit(b: &Block) -> Edit {
    Edit::RemoveClips {
        clips: b.clips.clone(),
    }
}

/// Place at Playhead: another instance of the pattern at `tick`.
pub fn place_edit(group: GroupId, tick: u32) -> Edit {
    Edit::PlacePattern { group, start: tick }
}

/// Whether `ids` can become a pattern: at least one clip, none already in
/// a pattern, at most one per row.
pub fn can_make(p: &Project, ids: &[ClipId]) -> Result<(), &'static str> {
    let chosen: Vec<&Clip> = p.clips.iter().filter(|c| ids.contains(&c.id)).collect();
    if chosen.is_empty() {
        return Err("Select the clips to put in the pattern first");
    }
    if chosen.iter().any(|c| c.group.is_some()) {
        return Err("A selected clip is already in a pattern");
    }
    let mut rows: Vec<ChannelId> = chosen.iter().map(|c| c.instrument).collect();
    rows.sort();
    rows.dedup();
    if rows.len() != chosen.len() {
        return Err("A pattern holds one clip per row; select one clip on each row");
    }
    Ok(())
}

/// "Pattern 3": the first number not taken.
pub fn new_name(p: &Project) -> String {
    let mut n = p.groups.len() + 1;
    loop {
        let name = format!("Pattern {n}");
        if !p.groups.iter().any(|g| g.name == name) {
            return name;
        }
        n += 1;
    }
}

/// The patterns of the project for the lane's list: name, group, and how
/// many times it is placed.
pub fn list(p: &Project) -> Vec<(GroupId, String, usize)> {
    p.groups
        .iter()
        .map(|g| {
            let n = blocks(p).iter().filter(|b| b.group == g.id).count();
            (g.id, g.name.clone(), n)
        })
        .collect()
}

/// What opening a block does: the first member opens in the editor and
/// the rest are named (the editor shows one clip's content at a time).
pub fn open_note(p: &Project, b: &Block) -> String {
    if b.clips.len() < 2 {
        return format!("{} has one part", b.name);
    }
    let rows: Vec<String> = b
        .rows
        .iter()
        .skip(1)
        .filter_map(|r| p.channel(*r).map(|c| c.name.clone()))
        .collect();
    format!(
        "{} has {} parts; the first is open, the others are on {}",
        b.name,
        b.clips.len(),
        rows.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ids::PatternId;
    use protocol::model::{ClipGroup, PatternGroup};

    fn clip(id: u32, row: u32, start: u32, len: u32, g: Option<(u32, u32)>) -> Clip {
        Clip {
            id: ClipId(id),
            instrument: ChannelId(row),
            pattern: PatternId(100 + id),
            start,
            len,
            offset: 0,
            muted: false,
            audio: None,
            group: g.map(|(group, instance)| ClipGroup {
                group: GroupId(group),
                instance,
            }),
        }
    }

    fn project() -> Project {
        let mut p = Project::empty();
        p.groups.push(PatternGroup {
            id: GroupId(50),
            name: "Intro".into(),
            color: 0xE55B5B,
        });
        p.clips = vec![
            clip(1, 1, 0, 3840, Some((50, 1))),
            clip(2, 2, 0, 1920, Some((50, 1))),
            clip(3, 1, 7680, 3840, Some((50, 2))),
            clip(4, 3, 2 * 3840, 960, None),
        ];
        p
    }

    #[test]
    fn a_block_is_one_placed_instance() {
        let p = project();
        let b = blocks(&p);
        assert_eq!(b.len(), 2, "a lone clip is no block");
        assert_eq!((b[0].start, b[0].end), (0, 3840));
        assert_eq!(b[0].clips, vec![ClipId(1), ClipId(2)]);
        assert_eq!(b[0].rows, vec![ChannelId(1), ChannelId(2)]);
        assert_eq!((b[0].name.as_str(), b[0].color), ("Intro", 0xE55B5B));
        assert_eq!((b[1].start, b[1].instance), (7680, 2));
        assert_eq!(block_at(&b, 100).map(|x| x.instance), Some(1));
        assert_eq!(block_at(&b, 5000), None);
        assert_eq!(block_of(&b, ClipId(3)).map(|x| x.instance), Some(2));
        assert_eq!(block_of(&b, ClipId(4)), None);
    }

    #[test]
    fn moving_a_block_moves_every_member() {
        let p = project();
        let b = blocks(&p);
        assert_eq!(
            move_edit(&b[0], 960),
            Edit::MoveClips {
                clips: vec![ClipId(1), ClipId(2)],
                dt: 960
            }
        );
        assert!(fits_move(&p.clips, &b[0], 960));
        // Into the second instance: no.
        assert!(!fits_move(&p.clips, &b[0], 7000));
        assert!(!fits_move(&p.clips, &b[0], -1), "not before the start");
    }

    #[test]
    fn duplicating_goes_right_after_and_delete_removes_the_members() {
        let p = project();
        let b = blocks(&p);
        match duplicate_edit(&p.clips, &b[0]) {
            Some(Edit::DuplicateClips { clips, dt, linked }) => {
                assert_eq!(clips.len(), 2);
                assert_eq!(dt, 3840);
                assert!(linked);
            }
            e => panic!("{e:?}"),
        }
        // The second instance has room after it; the first one's copy would
        // touch the second at 3840..7680, which is free.
        assert_eq!(
            delete_edit(&b[1]),
            Edit::RemoveClips {
                clips: vec![ClipId(3)]
            }
        );
        assert_eq!(
            place_edit(GroupId(50), 960),
            Edit::PlacePattern {
                group: GroupId(50),
                start: 960
            }
        );
    }

    #[test]
    fn making_a_pattern_needs_one_free_clip_per_row() {
        let p = project();
        assert!(can_make(&p, &[]).is_err());
        assert!(can_make(&p, &[ClipId(1)]).is_err(), "already in a pattern");
        assert!(can_make(&p, &[ClipId(4)]).is_ok());
        let mut q = p.clone();
        q.clips.push(clip(5, 3, 20000, 960, None));
        assert!(can_make(&q, &[ClipId(4), ClipId(5)]).is_err(), "same row");
        assert_eq!(new_name(&p), "Pattern 2");
        assert_eq!(list(&p), vec![(GroupId(50), "Intro".to_string(), 2)]);
    }

    #[test]
    fn opening_a_block_names_the_other_parts() {
        let mut p = project();
        for (id, name) in [(1, "Kick"), (2, "Snare")] {
            p.channels
                .push(std::sync::Arc::new(protocol::model::Channel {
                    id: ChannelId(id),
                    name: name.into(),
                    root_key: 60,
                    track: protocol::ids::TrackId::MASTER,
                    mix: protocol::model::Mix::default(),
                    instrument: protocol::model::Instrument::Audio,
                    choke_group: 0,
                }));
        }
        let b = blocks(&p);
        assert!(open_note(&p, &b[1]).contains("one part"));
        let note = open_note(&p, &b[0]);
        assert!(note.contains("2 parts") && note.contains("Snare"), "{note}");
    }
}
