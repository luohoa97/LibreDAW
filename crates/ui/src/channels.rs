// SPDX-License-Identifier: GPL-3.0-or-later
//! Adding and removing channels. A new channel gets a mixer track of its own
//! (so the Mixer lists tracks, docs/ui-design.md 3.5, and a beginner never
//! has to route anything); both edits are one undo step.

use std::rc::Rc;

use protocol::consts::MAX_TRACKS;
use protocol::edit::{Edit, NewInstrument};
use protocol::ids::{ChannelId, TrackId};
use protocol::model::SynthParams;

use crate::app::App;
use crate::presets::{self, unique_name};

/// What a new channel plays.
pub enum NewChannel {
    /// A built-in sound by name (see `presets::presets`).
    Preset(String),
    /// A CLAP instrument.
    Plugin { id: String, name: String },
}

impl NewChannel {
    fn parts(&self, taken: &[&str]) -> (String, NewInstrument, u8) {
        match self {
            NewChannel::Preset(n) => {
                let p = presets::presets()
                    .into_iter()
                    .find(|p| p.name == n)
                    .or_else(|| presets::presets().into_iter().last());
                match p {
                    Some(p) => (
                        unique_name(p.name, taken),
                        NewInstrument::Synth { params: p.params },
                        p.root_key,
                    ),
                    None => (
                        unique_name("Synth", taken),
                        NewInstrument::Synth {
                            params: SynthParams::default(),
                        },
                        60,
                    ),
                }
            }
            NewChannel::Plugin { id, name } => (
                unique_name(name, taken),
                NewInstrument::Clap {
                    plugin_id: id.clone(),
                },
                60,
            ),
        }
    }
}

/// Adds a channel on a new track of its own and selects it. Returns the
/// channel.
pub fn add(app: &Rc<App>, what: NewChannel) -> Option<ChannelId> {
    let (name, instrument, root_key, own_track) = {
        let s = app.session.borrow();
        let p = &s.document().project;
        let taken: Vec<&str> = p.channels.iter().map(|c| c.name.as_str()).collect();
        let (name, instrument, root_key) = what.parts(&taken);
        // The master is not counted against the track limit.
        let room = p.tracks.len() <= MAX_TRACKS;
        (name, instrument, root_key, room)
    };
    let fallback = app.ui.borrow().track;
    let grouped = app.gesture_begin("Add channel");
    let mut track = fallback;
    if own_track {
        let edit = vec![Edit::AddTrack { name: name.clone() }];
        let made = if grouped {
            app.gesture_edit(edit)
        } else {
            app.edit(edit)
        };
        if let Some(a) = made {
            track = TrackId(a.created[0]);
        }
    }
    let add = vec![Edit::AddChannel {
        name,
        instrument,
        root_key,
        track,
    }];
    let made = if grouped {
        app.gesture_edit(add)
    } else {
        app.edit(add)
    };
    if grouped {
        app.gesture_end();
    }
    let id = made.map(|a| ChannelId(a.created[0]))?;
    app.select_channel(id);
    Some(id)
}

/// Removes a channel (and its track when nothing else uses it) and offers
/// Undo in a toast instead of asking first.
pub fn remove(app: &Rc<App>, id: ChannelId) {
    let track = {
        let s = app.session.borrow();
        let p = &s.document().project;
        let Some(c) = p.channel(id) else { return };
        let only_user = p.channels.iter().filter(|x| x.track == c.track).count() == 1;
        let empty = p
            .track(c.track)
            .map(|t| t.inserts.is_empty())
            .unwrap_or(false);
        (c.track != TrackId::MASTER && only_user && empty).then_some(c.track)
    };
    let grouped = app.gesture_begin("Remove channel");
    let run = |e: Vec<Edit>| {
        if grouped {
            app.gesture_edit(e)
        } else {
            app.edit(e)
        }
    };
    run(vec![Edit::RemoveChannel { channel: id }]);
    if let Some(t) = track {
        run(vec![Edit::RemoveTrack { track: t }]);
    }
    if grouped {
        app.gesture_end();
    }
    let a = app.clone();
    app.toast_action("Channel removed", "Undo", move || a.undo());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::engine_adapter::EngineLink;
    use crate::persist::Dirs;
    use crate::registry::Registry;
    use crate::session::Session;

    fn app() -> Rc<App> {
        let dir = std::env::temp_dir().join(format!("ldaw-chan-{}", std::process::id()));
        let dirs = Dirs {
            music: dir.join("m"),
            data: dir.join("d"),
            config: dir.join("c"),
        };
        let s = Session::new(
            Document::new(),
            true,
            EngineLink::stub(48000.0),
            Registry::new(Vec::new(), 48000.0),
        );
        App::with_dirs(s, dirs)
    }

    fn counts(a: &App) -> (usize, usize) {
        let s = a.session.borrow();
        let p = &s.document().project;
        (p.channels.len(), p.tracks.len())
    }

    #[test]
    fn a_new_channel_gets_its_own_track_in_one_undo_step() {
        let a = app();
        let id = add(&a, NewChannel::Preset("Kick".into())).expect("added");
        assert_eq!(counts(&a), (1, 2));
        {
            let s = a.session.borrow();
            let p = &s.document().project;
            let c = p.channel(id).unwrap();
            assert_eq!(c.name, "Kick");
            assert_ne!(c.track, TrackId::MASTER);
            assert_eq!(p.track(c.track).unwrap().name, "Kick");
        }
        assert_eq!(a.current_channel(), Some(id));
        a.undo();
        assert_eq!(counts(&a), (0, 1), "channel and track go together");
        a.redo();
        assert_eq!(counts(&a), (1, 2));
    }

    #[test]
    fn names_stay_unique() {
        let a = app();
        add(&a, NewChannel::Preset("Kick".into()));
        let b = add(&a, NewChannel::Preset("Kick".into())).unwrap();
        let s = a.session.borrow();
        assert_eq!(s.document().project.channel(b).unwrap().name, "Kick 2");
    }

    #[test]
    fn removing_takes_the_empty_track_along_and_undo_restores_both() {
        let a = app();
        let id = add(&a, NewChannel::Preset("Snare".into())).unwrap();
        remove(&a, id);
        assert_eq!(counts(&a), (0, 1));
        a.undo();
        assert_eq!(counts(&a), (1, 2));
    }

    #[test]
    fn an_unknown_preset_falls_back_to_a_synth() {
        let a = app();
        let id = add(&a, NewChannel::Preset("No such sound".into())).unwrap();
        assert!(a.session.borrow().document().project.channel(id).is_some());
    }
}
