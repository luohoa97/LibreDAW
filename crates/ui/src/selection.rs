// SPDX-License-Identifier: GPL-3.0-or-later
//! What is selected, as plain values (no GTK). One selection is the single
//! source of truth for every view (SPEC 20): the selected clip (the clip
//! editor shows it), the instrument (row) and the mixer track. The clip's
//! content is derived from the clip, never chosen on its own. `App` keeps
//! the selection valid with `fix_with` after every change.

use protocol::ids::{ChannelId, ClipId, PatternId, TrackId};
use protocol::model::{Clip, Project};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    /// The clip the editor shows.
    pub clip: Option<ClipId>,
    /// The selected clip's content (derived: `None` without a clip).
    pub pattern: Option<PatternId>,
    /// The selected instrument (its row).
    pub channel: Option<ChannelId>,
    pub track: TrackId,
}

impl Selection {
    pub const NONE: Selection = Selection {
        clip: None,
        pattern: None,
        channel: None,
        track: TrackId::MASTER,
    };
}

pub fn clip_of(p: &Project, id: ClipId) -> Option<&Clip> {
    p.clips.iter().find(|c| c.id == id)
}

/// Points the selection at things that exist. A clip that is gone is
/// deselected; the instrument is the clip's, or the chosen one, or (unless
/// the user cleared it) the first one.
pub fn fix_with(sel: Selection, p: &Project, user_cleared: bool) -> Selection {
    let clip = sel.clip.and_then(|id| clip_of(p, id));
    let channel = match clip {
        Some(c) => Some(c.instrument),
        None => {
            let kept = sel.channel.filter(|id| p.channel(*id).is_some());
            if user_cleared {
                kept
            } else {
                kept.or_else(|| p.channels.first().map(|x| x.id))
            }
        }
    };
    let track = if p.track(sel.track).is_some() {
        sel.track
    } else {
        TrackId::MASTER
    };
    Selection {
        clip: clip.map(|c| c.id),
        pattern: clip.map(|c| c.pattern),
        channel,
        track,
    }
}

pub fn fix(sel: Selection, p: &Project) -> Selection {
    fix_with(sel, p, false)
}

/// Selects an instrument (row) and the mixer track it feeds. The selected
/// clip stays only if it is on that row. An unknown id changes nothing.
pub fn select_channel(sel: Selection, p: &Project, id: ChannelId) -> Selection {
    let Some(c) = p.channel(id) else {
        return sel;
    };
    let clip = sel
        .clip
        .and_then(|cid| clip_of(p, cid))
        .filter(|cl| cl.instrument == id);
    Selection {
        clip: clip.map(|c| c.id),
        pattern: clip.map(|c| c.pattern),
        channel: Some(id),
        track: c.track,
    }
}

/// Selects a clip: its instrument and track come with it.
pub fn select_clip(sel: Selection, p: &Project, id: ClipId) -> Selection {
    let Some(clip) = clip_of(p, id) else {
        return sel;
    };
    let track = p
        .channel(clip.instrument)
        .map(|c| c.track)
        .unwrap_or(sel.track);
    Selection {
        clip: Some(id),
        pattern: Some(clip.pattern),
        channel: Some(clip.instrument),
        track,
    }
}

/// The clip "open the editor" means: on `channel`'s row (or the selected
/// row when `None`), the selected clip if it is there, else the row's
/// first clip. `None` when the row has no clip.
pub fn clip_to_edit(p: &Project, sel: &Selection, channel: Option<ChannelId>) -> Option<ClipId> {
    let row = channel.or(sel.channel);
    if let Some(c) = sel.clip.and_then(|id| clip_of(p, id))
        && (row.is_none() || Some(c.instrument) == row)
    {
        return Some(c.id);
    }
    let row = row?;
    p.clips
        .iter()
        .filter(|c| c.instrument == row)
        .min_by_key(|c| c.start)
        .map(|c| c.id)
}

/// How many clips share this content (linked copies, SPEC 20.2).
pub fn linked_count(p: &Project, pattern: PatternId) -> usize {
    p.clips.iter().filter(|c| c.pattern == pattern).count()
}

/// Whether the editor shows its "add notes" hint: the content is empty.
pub fn show_notes_hint(p: &Project, sel: &Selection) -> bool {
    sel.pattern
        .and_then(|id| p.pattern(id))
        .is_some_and(|pat| pat.notes.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use doc::document::{Document, apply_batch};
    use protocol::edit::{Edit, NewInstrument};
    use protocol::model::SynthParams;

    /// `channels` instruments; the first gets two clips (bars 1 and 3).
    fn build(channels: usize) -> (Project, Vec<ChannelId>, Vec<ClipId>) {
        let mut edits = Vec::new();
        for i in 0..channels {
            edits.push(Edit::AddChannel {
                name: format!("c{i}"),
                instrument: NewInstrument::Synth {
                    params: SynthParams::default(),
                },
                root_key: 60,
                track: TrackId::MASTER,
            });
        }
        let (d, ids) = apply_batch(&Document::new(), &edits).unwrap();
        let ch: Vec<ChannelId> = ids.iter().map(|i| ChannelId(*i)).collect();
        let mut clips = Vec::new();
        let mut d = d;
        if let Some(first) = ch.first() {
            for start in [0, 2 * 3840] {
                let (d2, made) = apply_batch(
                    &d,
                    &[Edit::AddClip {
                        instrument: *first,
                        pattern: None,
                        start,
                        len: 3840,
                    }],
                )
                .unwrap();
                d = d2;
                clips.push(ClipId(*made.last().unwrap()));
            }
        }
        ((*d.project).clone(), ch, clips)
    }

    #[test]
    fn fixing_keeps_valid_choices_and_derives_the_content() {
        let (p, ch, clips) = build(2);
        let s = fix(Selection::NONE, &p);
        assert_eq!(s.channel, Some(ch[0]), "first instrument");
        assert_eq!(s.clip, None, "no clip is picked for the user");
        let s = select_clip(s, &p, clips[1]);
        assert_eq!(fix(s, &p), s);
        assert_eq!(s.pattern, clip_of(&p, clips[1]).map(|c| c.pattern));
        // A removed clip deselects; its row stays.
        let gone = Selection {
            clip: Some(ClipId(9999)),
            ..s
        };
        let f = fix(gone, &p);
        assert_eq!((f.clip, f.pattern, f.channel), (None, None, Some(ch[0])));
    }

    #[test]
    fn a_cleared_selection_stays_cleared() {
        let (p, ch, _) = build(2);
        let none = Selection::NONE;
        assert_eq!(fix_with(none, &p, true).channel, None);
        assert_eq!(fix_with(none, &p, false).channel, Some(ch[0]));
        let chosen = Selection {
            channel: Some(ch[1]),
            ..none
        };
        assert_eq!(fix_with(chosen, &p, true).channel, Some(ch[1]));
    }

    #[test]
    fn selecting_a_row_keeps_only_its_own_clip() {
        let (p, ch, clips) = build(2);
        let s = select_clip(Selection::NONE, &p, clips[0]);
        assert_eq!(select_channel(s, &p, ch[0]).clip, Some(clips[0]));
        let other = select_channel(s, &p, ch[1]);
        assert_eq!((other.clip, other.channel), (None, Some(ch[1])));
        assert_eq!(select_channel(s, &p, ChannelId(999)), s);
    }

    #[test]
    fn the_clip_to_edit() {
        let (p, ch, clips) = build(2);
        let none = fix(Selection::NONE, &p);
        // The row's first clip.
        assert_eq!(clip_to_edit(&p, &none, Some(ch[0])), Some(clips[0]));
        // The selected clip when it is on that row.
        let s = select_clip(none, &p, clips[1]);
        assert_eq!(clip_to_edit(&p, &s, Some(ch[0])), Some(clips[1]));
        assert_eq!(clip_to_edit(&p, &s, None), Some(clips[1]));
        // A row without clips.
        assert_eq!(clip_to_edit(&p, &s, Some(ch[1])), None);
        assert_eq!(linked_count(&p, s.pattern.unwrap()), 1);
    }
}
