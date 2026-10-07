// SPDX-License-Identifier: GPL-3.0-or-later
//! What the timeline's clip actions do (SPEC 20.3), in one place so the
//! menu, the keys and the pointer cannot drift apart. Each is one undo
//! step made of protocol `Edit`s, the same ones an agent sends, so every
//! action here is also an agent operation.

use std::rc::Rc;

use protocol::edit::Edit;
use protocol::ids::{ChannelId, ClipId};
use protocol::model::{Clip, ticks_per_bar};

use crate::app::App;
use crate::timeline_logic as tl;

fn clips(app: &App) -> Vec<Clip> {
    app.session.borrow().document().project.clips.clone()
}

/// Whether the clip plays audio instead of notes.
pub fn is_audio(app: &App, id: ClipId) -> bool {
    clips(app).iter().any(|c| c.id == id && c.audio.is_some())
}

/// Click on an empty spot of `instrument`'s row: a one-bar clip at the
/// bar under the pointer (shortened before the next clip), selected.
/// Returns the new clip.
pub fn add_at(app: &Rc<App>, instrument: ChannelId, tick: u32) -> Option<ClipId> {
    let (all, bar) = {
        let s = app.session.borrow();
        let p = &s.document().project;
        (p.clips.clone(), ticks_per_bar(p.time_sig_num))
    };
    let Some((start, len)) = tl::place_new_clip(&all, instrument, tick, bar) else {
        app.toast("No room for a clip there");
        return None;
    };
    let made = app.edit(vec![Edit::AddClip {
        instrument,
        pattern: None,
        start,
        len,
    }])?;
    let id = ClipId(*made.created.last()?);
    app.select_clip(id);
    Some(id)
}

/// Ctrl+D: linked copies right after the selection.
pub fn duplicate(app: &Rc<App>, ids: &[ClipId]) -> Vec<ClipId> {
    let Some(dt) = tl::duplicate_offset(&clips(app), ids) else {
        if !ids.is_empty() {
            app.toast("No room after these clips for a copy");
        }
        return Vec::new();
    };
    app.edit(vec![Edit::DuplicateClips {
        clips: ids.to_vec(),
        dt,
        linked: true,
    }])
    .map(|a| a.created.iter().map(|c| ClipId(*c)).collect())
    .unwrap_or_default()
}

/// Gives each clip its own copy of its content.
pub fn make_unique(app: &Rc<App>, ids: &[ClipId]) {
    let edits: Vec<Edit> = ids.iter().map(|c| Edit::MakeUnique { clip: *c }).collect();
    if !edits.is_empty() {
        app.edit(edits);
    }
}

/// S: splits the clips under `tick` (the playhead).
pub fn split_at(app: &Rc<App>, ids: &[ClipId], tick: u32) {
    let edits: Vec<Edit> = clips(app)
        .iter()
        .filter(|c| ids.contains(&c.id) && c.start < tick && tick < c.end())
        .map(|c| Edit::SplitClip {
            clip: c.id,
            at: tick,
        })
        .collect();
    if edits.is_empty() {
        app.toast("The playhead is not inside the selected clips");
    } else {
        app.edit(edits);
    }
}

/// 0: mutes the clips, or unmutes them when all are muted.
pub fn toggle_mute(app: &Rc<App>, ids: &[ClipId]) {
    let all = clips(app);
    let chosen: Vec<&Clip> = all.iter().filter(|c| ids.contains(&c.id)).collect();
    if chosen.is_empty() {
        return;
    }
    let muted = !chosen.iter().all(|c| c.muted);
    app.edit(vec![Edit::SetClipMuted {
        clips: ids.to_vec(),
        muted,
    }]);
}

/// Delete: removes the clips, with Undo in the toast.
pub fn delete(app: &Rc<App>, ids: &[ClipId]) {
    if ids.is_empty() {
        return;
    }
    if app
        .edit(vec![Edit::RemoveClips {
            clips: ids.to_vec(),
        }])
        .is_some()
    {
        let a = app.clone();
        let what = if ids.len() == 1 {
            "Clip removed".to_string()
        } else {
            format!("{} clips removed", ids.len())
        };
        app.toast_action(&what, "Undo", move || a.undo());
    }
}

/// Ctrl+G or Make Pattern: the selected clips, one per row, become a
/// pattern that moves and copies as one block (SPEC 20.7). Returns whether
/// it was made.
pub fn make_pattern(app: &Rc<App>, ids: &[ClipId]) -> bool {
    let (ok, name) = {
        let s = app.session.borrow();
        let p = &s.document().project;
        (
            crate::pattern_logic::can_make(p, ids),
            crate::pattern_logic::new_name(p),
        )
    };
    if let Err(m) = ok {
        app.toast(m);
        return false;
    }
    app.edit(vec![Edit::MakePattern {
        clips: ids.to_vec(),
        name,
    }])
    .is_some()
}

/// What a clip menu item does, by its action name (`menus::CLIP_ACTIONS`).
pub fn perform(app: &Rc<App>, ids: &[ClipId], action: &str) {
    match action {
        // An audio clip has no notes to edit.
        "edit" if ids.first().is_some_and(|c| is_audio(app, *c)) => {}
        "edit" => {
            if let Some(c) = ids.first() {
                app.select_clip(*c);
                app.command(crate::app::UiCommand::EditNotes);
            }
        }
        "duplicate" => {
            duplicate(app, ids);
        }
        "unique" => make_unique(app, ids),
        "split" => split_at(app, ids, app.playhead_tick().min(u32::MAX as u64) as u32),
        "mute" => toggle_mute(app, ids),
        "delete" => delete(app, ids),
        "make-pattern" => {
            make_pattern(app, ids);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_adapter::EngineLink;
    use crate::registry::Registry;
    use crate::session::Session;
    use doc::document::Document;
    use doc::persist::Dirs;
    use protocol::edit::NewInstrument;
    use protocol::ids::TrackId;
    use protocol::model::SynthParams;

    fn app() -> Rc<App> {
        let dir = std::env::temp_dir().join(format!("ldaw-clipops-{}", std::process::id()));
        let s = Session::new(
            Document::new(),
            true,
            EngineLink::stub(48000.0),
            Registry::new(Vec::new(), 48000.0),
        );
        App::with_dirs(
            s,
            Dirs {
                music: dir.join("m"),
                data: dir.join("d"),
                config: dir.join("c"),
            },
        )
    }

    fn instrument(a: &Rc<App>) -> ChannelId {
        let r = a
            .edit(vec![Edit::AddChannel {
                name: "Kick".into(),
                instrument: NewInstrument::Synth {
                    params: SynthParams::default(),
                },
                root_key: 36,
                track: TrackId::MASTER,
            }])
            .unwrap();
        ChannelId(r.created[0])
    }

    const BAR: u32 = 3840;

    #[test]
    fn every_clip_action_does_what_it_says() {
        let a = app();
        let k = instrument(&a);
        // Click in bar 2: a one-bar clip at bar 2, selected.
        let c = add_at(&a, k, BAR + 100).expect("added");
        assert_eq!(a.current_clip(), Some(c));
        let clip = clips(&a)[0];
        assert_eq!((clip.start, clip.len), (BAR, BAR));
        // Duplicate: a linked copy right after it.
        let copies = duplicate(&a, &[c]);
        assert_eq!(copies.len(), 1);
        let all = clips(&a);
        let copy = all.iter().find(|x| x.id == copies[0]).unwrap();
        assert_eq!(copy.start, 2 * BAR);
        assert_eq!(copy.pattern, clip.pattern, "linked");
        // Make Unique: its own content.
        make_unique(&a, &[copies[0]]);
        let copy = clips(&a).into_iter().find(|x| x.id == copies[0]).unwrap();
        assert_ne!(copy.pattern, clip.pattern);
        // Split at a tick inside the first clip.
        split_at(&a, &[c], BAR + BAR / 2);
        assert_eq!(clips(&a).len(), 3);
        // Mute and unmute.
        toggle_mute(&a, &[c]);
        assert!(clips(&a).iter().find(|x| x.id == c).unwrap().muted);
        toggle_mute(&a, &[c]);
        assert!(!clips(&a).iter().find(|x| x.id == c).unwrap().muted);
        // Delete, then undo.
        let n = clips(&a).len();
        delete(&a, &[c]);
        assert_eq!(clips(&a).len(), n - 1);
        assert_eq!(a.current_clip(), None, "the deleted clip is deselected");
        a.undo();
        assert_eq!(clips(&a).len(), n);
        // Unknown actions do nothing.
        perform(&a, &[c], "no-such-action");
        assert_eq!(clips(&a).len(), n);
    }

    #[test]
    fn a_click_inside_a_bar_with_a_clip_goes_after_it() {
        let a = app();
        let k = instrument(&a);
        add_at(&a, k, 0).unwrap();
        let second = add_at(&a, k, 10).unwrap();
        let c = clips(&a).into_iter().find(|x| x.id == second).unwrap();
        assert_eq!(c.start, BAR);
    }
}
