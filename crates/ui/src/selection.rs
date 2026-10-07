// SPDX-License-Identifier: GPL-3.0-or-later
//! What is selected, and what the Pattern page shows for it, as plain
//! values (no GTK). One selection (pattern, channel, track) is the single
//! source of truth: the step rows, the notes toolbar, the piano roll and the
//! inspector all read it from `App`, which keeps it valid with `fix`.
//! The same module decides the empty states, so a page never says "Pick a
//! Channel" while a channel is selected, and "Edit Notes" always lands in a
//! state where the piano roll can be used.

use protocol::ids::{ChannelId, PatternId, TrackId};
use protocol::model::Project;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub pattern: Option<PatternId>,
    pub channel: Option<ChannelId>,
    pub track: TrackId,
}

impl Selection {
    pub const NONE: Selection = Selection {
        pattern: None,
        channel: None,
        track: TrackId::MASTER,
    };
}

/// Points the selection at things that exist: the first pattern and the
/// first channel when the old ones are gone or were never chosen.
pub fn fix(sel: Selection, p: &Project) -> Selection {
    Selection {
        pattern: sel
            .pattern
            .filter(|id| p.pattern(*id).is_some())
            .or_else(|| p.patterns.first().map(|x| x.id)),
        channel: sel
            .channel
            .filter(|id| p.channel(*id).is_some())
            .or_else(|| p.channels.first().map(|x| x.id)),
        track: if p.track(sel.track).is_some() {
            sel.track
        } else {
            TrackId::MASTER
        },
    }
}

/// Selects a channel, and the mixer track it feeds. An id that is not in
/// the project changes nothing.
pub fn select_channel(sel: Selection, p: &Project, id: ChannelId) -> Selection {
    match p.channel(id) {
        Some(c) => Selection {
            channel: Some(id),
            track: c.track,
            ..sel
        },
        None => sel,
    }
}

/// What the steps section shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepsState {
    /// No channels: "No Channels Yet" and the Add Channel button.
    NoChannels,
    /// Channels but no pattern: "No Pattern" and a New Pattern button.
    NoPattern,
    /// One row of step cells per channel.
    Rows,
}

pub fn steps_state(p: &Project, sel: &Selection) -> StepsState {
    if p.channels.is_empty() {
        StepsState::NoChannels
    } else if sel.pattern.is_none() {
        StepsState::NoPattern
    } else {
        StepsState::Rows
    }
}

/// What the notes section shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotesState {
    /// No pattern: "No Pattern" and a New Pattern button.
    NoPattern,
    /// A pattern but no channel selected: "Pick a Channel".
    PickChannel,
    /// The piano roll (with a hint inside it when the channel has no notes).
    Roll,
}

pub fn notes_state(sel: &Selection) -> NotesState {
    match (sel.pattern, sel.channel) {
        (None, _) => NotesState::NoPattern,
        (Some(_), None) => NotesState::PickChannel,
        (Some(_), Some(_)) => NotesState::Roll,
    }
}

/// Whether the roll shows its "Click to add notes" hint: the channel is
/// selected and the pattern has no notes for it.
pub fn show_notes_hint(p: &Project, sel: &Selection) -> bool {
    match (sel.pattern.and_then(|id| p.pattern(id)), sel.channel) {
        (Some(pat), Some(ch)) => pat.notes_of(ch).is_empty(),
        _ => false,
    }
}

/// Everything "Edit Notes" has to do, in one place, so every way of asking
/// for it (row menu, row button, double-click, Return, shortcut) behaves
/// the same: pick the channel, make sure a pattern exists, then show the
/// notes page for that channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditNotesPlan {
    pub select: ChannelId,
    /// The project has no pattern: make "Pattern 1" first (one undo step).
    pub create_pattern: bool,
}

/// `channel` is the one asked for; `None` means the selected channel.
/// Returns `None` when there is no channel to edit.
pub fn plan_edit_notes(
    p: &Project,
    sel: &Selection,
    channel: Option<ChannelId>,
) -> Option<EditNotesPlan> {
    let select = channel
        .filter(|id| p.channel(*id).is_some())
        .or(sel.channel)
        .or_else(|| p.channels.first().map(|c| c.id))?;
    Some(EditNotesPlan {
        select,
        create_pattern: p.patterns.is_empty(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use doc::document::{Document, apply_batch};
    use protocol::edit::{Edit, NewInstrument};
    use protocol::model::SynthParams;

    fn build(channels: usize, patterns: usize) -> (Document, Vec<ChannelId>, Vec<PatternId>) {
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
        for i in 0..patterns {
            edits.push(Edit::AddPattern {
                name: format!("p{i}"),
                length_steps: 16,
            });
        }
        let (d, ids) = apply_batch(&Document::new(), &edits).unwrap();
        let ch = ids[..channels].iter().map(|i| ChannelId(*i)).collect();
        let pa = ids[channels..].iter().map(|i| PatternId(*i)).collect();
        (d, ch, pa)
    }

    fn project(channels: usize, patterns: usize) -> (Project, Vec<ChannelId>, Vec<PatternId>) {
        let (d, ch, pa) = build(channels, patterns);
        ((*d.project).clone(), ch, pa)
    }

    #[test]
    fn fix_picks_the_first_of_each_and_keeps_valid_choices() {
        let (p, ch, pa) = project(2, 2);
        let s = fix(Selection::NONE, &p);
        assert_eq!((s.channel, s.pattern), (Some(ch[0]), Some(pa[0])));
        let chosen = Selection {
            channel: Some(ch[1]),
            pattern: Some(pa[1]),
            track: TrackId::MASTER,
        };
        assert_eq!(fix(chosen, &p), chosen);
        // A removed channel falls back to the first one.
        let gone = Selection {
            channel: Some(ChannelId(999)),
            ..chosen
        };
        assert_eq!(fix(gone, &p).channel, Some(ch[0]));
        // An empty project selects nothing (and does not invent anything).
        let (empty, _, _) = project(0, 0);
        assert_eq!(fix(chosen, &empty), Selection::NONE);
    }

    #[test]
    fn selecting_a_channel_selects_its_track_and_ignores_strangers() {
        let (p, ch, _) = project(2, 1);
        let s = fix(Selection::NONE, &p);
        let t = select_channel(s, &p, ch[1]);
        assert_eq!(t.channel, Some(ch[1]));
        assert_eq!(t.track, p.channel(ch[1]).unwrap().track);
        assert_eq!(select_channel(t, &p, ChannelId(999)), t);
    }

    #[test]
    fn empty_states_match_the_real_state() {
        // No pattern: both halves say so, whatever is selected.
        let (p, ch, pa) = project(1, 0);
        let s = fix(Selection::NONE, &p);
        assert_eq!(s.channel, Some(ch[0]));
        assert_eq!(steps_state(&p, &s), StepsState::NoPattern);
        assert_eq!(notes_state(&s), NotesState::NoPattern);
        // No channels at all.
        let (p0, _, _) = project(0, 1);
        let s0 = fix(Selection::NONE, &p0);
        assert_eq!(steps_state(&p0, &s0), StepsState::NoChannels);
        assert_eq!(notes_state(&s0), NotesState::PickChannel);
        // Pattern and channel: rows and the roll, with the hint while the
        // channel has no notes.
        let (p1, _, _) = project(2, 1);
        let s1 = fix(Selection::NONE, &p1);
        assert_eq!(steps_state(&p1, &s1), StepsState::Rows);
        assert_eq!(notes_state(&s1), NotesState::Roll);
        assert!(show_notes_hint(&p1, &s1));
        let _ = pa;
    }

    #[test]
    fn the_hint_goes_away_when_the_channel_has_notes() {
        let (d, ch, pa) = build(1, 1);
        let s = fix(Selection::NONE, &d.project);
        assert!(show_notes_hint(&d.project, &s));
        let (d2, _) = apply_batch(
            &d,
            &[Edit::SetStep {
                pattern: pa[0],
                channel: ch[0],
                step: 0,
                on: true,
                vel: None,
            }],
        )
        .unwrap();
        assert!(!show_notes_hint(&d2.project, &s));
    }

    #[test]
    fn edit_notes_selects_the_asked_channel_and_creates_a_missing_pattern() {
        // No pattern yet: it must be made, and the channel selected.
        let (p, ch, _) = project(2, 0);
        let s = fix(Selection::NONE, &p);
        let plan = plan_edit_notes(&p, &s, Some(ch[1])).unwrap();
        assert_eq!(plan.select, ch[1]);
        assert!(plan.create_pattern);
        // With a pattern nothing is created, and without a named channel
        // the selected one is used.
        let (p, ch, _) = project(1, 1);
        let s = fix(Selection::NONE, &p);
        let plan = plan_edit_notes(&p, &s, None).unwrap();
        assert_eq!(plan.select, ch[0]);
        assert!(!plan.create_pattern);
        // A channel that does not exist is ignored; no channels, no plan.
        let plan = plan_edit_notes(&p, &s, Some(ChannelId(999))).unwrap();
        assert_eq!(plan.select, ch[0]);
        let (empty, _, _) = project(0, 1);
        assert!(plan_edit_notes(&empty, &Selection::NONE, None).is_none());
    }
}
