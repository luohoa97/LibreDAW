// SPDX-License-Identifier: GPL-3.0-or-later
//! Adding and removing channels. A new channel gets a mixer track of its own
//! (so the Mixer lists tracks, docs/ui-design.md 3.5, and a beginner never
//! has to route anything); both edits are one undo step. A kit adds all of
//! its pieces on one shared track.

use std::rc::Rc;

use protocol::beats::{SampleMode, SamplerParam};
use protocol::consts::MAX_TRACKS;
use protocol::edit::{Edit, MixValue, NewInstrument};
use protocol::ids::{ChannelId, TrackId};
use protocol::model::{SampleRef, SynthParams};

use crate::app::App;
use doc::history::Author;
use doc::presets::{self, unique_name};

/// A sample for a new sampler channel and the settings that come with it.
#[derive(Clone, Debug)]
pub struct SamplerSetup {
    /// Registered with `AddSample` first; `None` makes an empty sampler.
    pub sample: Option<SampleRef>,
    pub name: String,
    /// The key the sample sounds at; a step plays this key (5.2).
    pub root_key: u8,
    pub mode: SampleMode,
    pub choke_group: u8,
    pub gain_db: f64,
    pub pan: f64,
}

impl SamplerSetup {
    /// A one-shot at middle C, named after the sample file.
    pub fn one_shot(sample: SampleRef) -> SamplerSetup {
        let name = sample
            .orig_name
            .strip_suffix(".wav")
            .or_else(|| sample.orig_name.strip_suffix(".WAV"))
            .unwrap_or(&sample.orig_name)
            .to_string();
        SamplerSetup {
            sample: Some(sample),
            name,
            root_key: 60,
            mode: SampleMode::OneShot,
            choke_group: 0,
            gain_db: 0.0,
            pan: 0.0,
        }
    }
}

/// What a new channel plays.
pub enum NewChannel {
    /// A built-in sound by name (see `presets::presets`).
    Preset(String),
    /// A CLAP instrument.
    Plugin { id: String, name: String },
    /// The built-in 808 (15.2).
    Bass808,
    /// A sampler (15.1).
    Sampler(SamplerSetup),
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
            NewChannel::Bass808 => (
                unique_name("808 Bass", taken),
                NewInstrument::Bass808 { mono: true },
                36,
            ),
            NewChannel::Sampler(s) => (
                unique_name(&s.name, taken),
                NewInstrument::Sampler {
                    sample: s.sample.as_ref().map(|r| r.hash.clone()),
                    mode: s.mode,
                },
                s.root_key,
            ),
        }
    }

    /// Edits that follow the channel: choke group, level, pan.
    fn finish(&self, channel: ChannelId) -> Vec<Edit> {
        let NewChannel::Sampler(s) = self else {
            return Vec::new();
        };
        let mut e = Vec::new();
        if s.choke_group > 0 {
            e.push(Edit::SetChokeGroup {
                channel,
                group: s.choke_group,
            });
        }
        if s.gain_db != 0.0 {
            e.push(Edit::SetSamplerParam {
                channel,
                param: SamplerParam::GainDb,
                value: s.gain_db,
            });
        }
        if s.pan != 0.0 {
            e.push(Edit::SetChannelMix {
                channel,
                value: MixValue::Pan(s.pan),
            });
        }
        e
    }

    fn sample(&self) -> Option<&SampleRef> {
        match self {
            NewChannel::Sampler(s) => s.sample.as_ref(),
            _ => None,
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
    // A project always has a pattern: the first channel creates one in the
    // same undo step, so its steps are there to click.
    app.ensure_pattern();
    let run = |e: Vec<Edit>| {
        if grouped {
            app.gesture_edit(e)
        } else {
            app.edit(e)
        }
    };
    if let Some(s) = what.sample() {
        run(vec![Edit::AddSample { sample: s.clone() }]);
    }
    let mut track = fallback;
    if own_track && let Some(a) = run(vec![Edit::AddTrack { name: name.clone() }]) {
        track = TrackId(a.created[0]);
    }
    let made = run(vec![Edit::AddChannel {
        name,
        instrument,
        root_key,
        track,
    }]);
    let id = made.map(|a| ChannelId(a.created[0]));
    if let Some(id) = id {
        let more = what.finish(id);
        if !more.is_empty() {
            run(more);
        }
    }
    if grouped {
        app.gesture_end();
    }
    let id = id?;
    app.select_channel(id);
    Some(id)
}

/// The starter beat of a new project: "Pattern 1" (one bar) with Kick,
/// Snare, Hat and Bass channels, each on its own track, and a plain steady
/// pattern so Play already sounds like something. One undo step. Nothing
/// plays until the user presses Play.
pub fn add_starter_beat(app: &Rc<App>) {
    let grouped = app.gesture_begin("New project");
    let run = |e: Vec<Edit>| {
        if grouped {
            app.gesture_edit(e)
        } else {
            app.edit(e)
        }
    };
    let pattern = app.ensure_pattern();
    let preset = |name: &str| {
        presets::presets()
            .into_iter()
            .find(|p| p.name == name)
            .map(|p| (NewInstrument::Synth { params: p.params }, p.root_key))
    };
    // (channel name, instrument and root key, steps, velocity)
    type Part = (&'static str, (NewInstrument, u8), Vec<u8>, u8);
    let parts: Vec<Part> = vec![
        (
            "Kick",
            preset("Kick").unwrap_or((
                NewInstrument::Synth {
                    params: SynthParams::default(),
                },
                36,
            )),
            vec![0, 4, 8, 12],
            110,
        ),
        (
            "Snare",
            preset("Snare").unwrap_or((
                NewInstrument::Synth {
                    params: SynthParams::default(),
                },
                38,
            )),
            vec![4, 12],
            100,
        ),
        (
            "Hat",
            preset("Closed hat").unwrap_or((
                NewInstrument::Synth {
                    params: SynthParams::default(),
                },
                42,
            )),
            vec![2, 6, 10, 14],
            80,
        ),
        (
            "Bass",
            (NewInstrument::Bass808 { mono: true }, 36),
            vec![0, 10],
            100,
        ),
    ];
    let mut first = None;
    for (name, (instrument, root_key), steps, vel) in parts {
        let Some(t) = run(vec![Edit::AddTrack { name: name.into() }]) else {
            continue;
        };
        let track = TrackId(t.created[0]);
        let made = run(vec![Edit::AddChannel {
            name: name.into(),
            instrument,
            root_key,
            track,
        }]);
        let Some(ch) = made.map(|a| ChannelId(a.created[0])) else {
            continue;
        };
        first.get_or_insert(ch);
        if let Some(pattern) = pattern {
            run(steps
                .into_iter()
                .map(|step| Edit::SetStep {
                    pattern,
                    channel: ch,
                    step,
                    on: true,
                    vel: Some(vel),
                })
                .collect());
        }
    }
    if grouped {
        app.gesture_end();
    }
    if let Some(c) = first {
        app.select_channel(c);
    }
}

/// Where the channels of a kit go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KitTrack {
    /// One new mixer track with this name.
    New(String),
    /// An existing track.
    Existing(TrackId),
}

/// Adds every piece of a kit as a sampler channel, all on one new track
/// named `track_name`, as one undo step. Returns the channels.
pub fn add_kit(app: &Rc<App>, track_name: &str, pieces: Vec<SamplerSetup>) -> Vec<ChannelId> {
    add_kit_as(
        app,
        Author::User,
        KitTrack::New(track_name.to_string()),
        pieces,
    )
    .unwrap_or_default()
}

/// As `add_kit`, by `author` and onto `track`. A script's or agent's kit is
/// always one undo group: `None` when another gesture is open (the
/// caller answers Busy). The user's falls back to separate steps.
pub fn add_kit_as(
    app: &Rc<App>,
    author: Author,
    track: KitTrack,
    pieces: Vec<SamplerSetup>,
) -> Option<Vec<ChannelId>> {
    if pieces.is_empty() {
        return Some(Vec::new());
    }
    let (taken, room): (Vec<String>, bool) = {
        let s = app.session.borrow();
        let p = &s.document().project;
        (
            p.channels.iter().map(|c| c.name.clone()).collect(),
            p.tracks.len() <= MAX_TRACKS,
        )
    };
    let fallback = app.ui.borrow().track;
    let user = author == Author::User;
    let grouped = app.gesture_begin_as(author, "Add kit");
    if !grouped && !user {
        return None;
    }
    app.ensure_pattern();
    let run = |e: Vec<Edit>| {
        if grouped {
            app.gesture_edit(e)
        } else {
            app.edit(e)
        }
    };
    let samples: Vec<Edit> = pieces
        .iter()
        .filter_map(|p| p.sample.clone())
        .map(|sample| Edit::AddSample { sample })
        .collect();
    if !samples.is_empty() {
        run(samples);
    }
    let track = match track {
        KitTrack::Existing(t) => t,
        KitTrack::New(name) => {
            let made = if room {
                run(vec![Edit::AddTrack { name }])
            } else {
                None
            };
            made.map(|a| TrackId(a.created[0])).unwrap_or(fallback)
        }
    };
    let mut names: Vec<String> = taken;
    let mut adds = Vec::new();
    let mut whats = Vec::new();
    for p in pieces {
        let what = NewChannel::Sampler(p);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let (name, instrument, root_key) = what.parts(&refs);
        names.push(name.clone());
        adds.push(Edit::AddChannel {
            name,
            instrument,
            root_key,
            track,
        });
        whats.push(what);
    }
    let ids: Vec<ChannelId> = run(adds)
        .map(|a| a.created.iter().map(|i| ChannelId(*i)).collect())
        .unwrap_or_default();
    let mut more = Vec::new();
    for (id, what) in ids.iter().zip(&whats) {
        more.extend(what.finish(*id));
    }
    if !more.is_empty() {
        run(more);
    }
    if grouped {
        app.gesture_end();
    }
    // An agent's kit never moves the user's selection.
    if user && let Some(first) = ids.first() {
        app.select_channel(*first);
    }
    Some(ids)
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
    use crate::engine_adapter::EngineLink;
    use crate::registry::Registry;
    use crate::session::Session;
    use doc::document::Document;
    use doc::persist::Dirs;

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

    fn sample_ref(n: &str) -> SampleRef {
        SampleRef {
            hash: format!("{:0>64}", n.len()),
            orig_name: format!("{n}.wav"),
            size: 10,
            local_only: false,
        }
    }

    #[test]
    fn an_808_channel_is_mono_and_named() {
        use protocol::model::Instrument;
        let a = app();
        let id = add(&a, NewChannel::Bass808).unwrap();
        let s = a.session.borrow();
        let c = s.document().project.channel(id).unwrap();
        assert_eq!(c.name, "808 Bass");
        let Instrument::Bass808(b) = &c.instrument else {
            panic!("808");
        };
        assert!(b.mono);
    }

    #[test]
    fn a_sampler_channel_registers_its_sample_and_applies_the_kit_settings() {
        use protocol::model::Instrument;
        let a = app();
        let mut setup = SamplerSetup::one_shot(sample_ref("kick"));
        setup.choke_group = 3;
        setup.gain_db = -2.0;
        let id = add(&a, NewChannel::Sampler(setup)).unwrap();
        {
            let s = a.session.borrow();
            let p = &s.document().project;
            assert_eq!(p.samples.len(), 1);
            let c = p.channel(id).unwrap();
            assert_eq!(c.name, "kick");
            assert_eq!(c.choke_group, 3);
            let Instrument::Sampler(sm) = &c.instrument else {
                panic!("sampler");
            };
            assert_eq!(sm.sample.as_deref(), Some(p.samples[0].hash.as_str()));
            assert_eq!(sm.params.gain_db, -2.0);
        }
        a.undo();
        let s = a.session.borrow();
        assert!(s.document().project.channels.is_empty());
        assert!(s.document().project.samples.is_empty(), "one undo step");
    }

    #[test]
    fn a_kit_is_one_track_and_one_undo_step() {
        let a = app();
        let pieces = ["kick", "snare", "hat"]
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let mut s = SamplerSetup::one_shot(sample_ref(&format!("{n}{i}")));
                s.choke_group = if *n == "hat" { 1 } else { 0 };
                s
            })
            .collect();
        let ids = add_kit(&a, "Phonk", pieces);
        assert_eq!(ids.len(), 3);
        assert_eq!(counts(&a), (3, 2), "three channels share one new track");
        {
            let s = a.session.borrow();
            let p = &s.document().project;
            let t = p.channel(ids[0]).unwrap().track;
            assert!(ids.iter().all(|i| p.channel(*i).unwrap().track == t));
            assert_eq!(p.track(t).unwrap().name, "Phonk");
            assert_eq!(p.channel(ids[2]).unwrap().choke_group, 1);
        }
        a.undo();
        assert_eq!(counts(&a), (0, 1));
    }

    fn pattern_count(a: &App) -> usize {
        a.session.borrow().document().project.patterns.len()
    }

    #[test]
    fn the_first_channel_creates_pattern_1_in_the_same_undo_step() {
        let a = app();
        assert_eq!(pattern_count(&a), 0);
        let id = add(&a, NewChannel::Preset("Kick".into())).unwrap();
        assert_eq!(pattern_count(&a), 1);
        assert!(a.current_pattern().is_some(), "and it is selected");
        assert_eq!(a.current_channel(), Some(id));
        a.undo();
        assert_eq!(pattern_count(&a), 0, "one step undoes the pattern too");
        assert_eq!(counts(&a), (0, 1));
        a.redo();
        assert_eq!((pattern_count(&a), counts(&a)), (1, (1, 2)));
        // A second channel does not add another pattern.
        add(&a, NewChannel::Bass808).unwrap();
        assert_eq!(pattern_count(&a), 1);
    }

    #[test]
    fn the_starter_beat_is_one_pattern_four_channels_and_one_undo_step() {
        let a = app();
        add_starter_beat(&a);
        let names: Vec<String> = {
            let s = a.session.borrow();
            s.document()
                .project
                .channels
                .iter()
                .map(|c| c.name.clone())
                .collect()
        };
        assert_eq!(names, ["Kick", "Snare", "Hat", "Bass"]);
        assert_eq!(counts(&a), (4, 5), "each channel has a track of its own");
        assert_eq!(pattern_count(&a), 1);
        {
            let s = a.session.borrow();
            let p = &s.document().project;
            let pat = &p.patterns[0];
            assert_eq!(pat.length_steps, 16);
            for c in &p.channels {
                assert!(
                    !pat.notes_of(c.id).is_empty(),
                    "{} has steps to see",
                    c.name
                );
            }
        }
        assert!(a.current_pattern().is_some() && a.current_channel().is_some());
        assert!(!a.ui.borrow().playing, "nothing plays until Play");
        a.undo();
        assert_eq!((counts(&a), pattern_count(&a)), ((0, 1), 0));
    }

    #[test]
    fn edit_notes_selects_makes_a_pattern_and_asks_the_page_to_show_it() {
        use crate::app::UiCommand;
        use std::cell::Cell;
        let a = app();
        let id = add(&a, NewChannel::Preset("Kick".into())).unwrap();
        let other = add(&a, NewChannel::Preset("Snare".into())).unwrap();
        assert_eq!(a.current_channel(), Some(other));
        let got = Rc::new(Cell::new(0));
        let g = got.clone();
        a.on_command(move |c| {
            if c == UiCommand::EditNotes {
                g.set(g.get() + 1);
            }
        });
        a.edit_notes(Some(id));
        assert_eq!(a.current_channel(), Some(id));
        assert_eq!(got.get(), 1);
        // No channel named: the selected one.
        a.edit_notes(None);
        assert_eq!(a.current_channel(), Some(id));
        assert_eq!(got.get(), 2);
        // With the pattern gone it is made again, as part of the call.
        let p = a.current_pattern().unwrap();
        a.edit(vec![Edit::RemovePattern { pattern: p }]);
        assert_eq!(pattern_count(&a), 0);
        a.edit_notes(Some(other));
        assert_eq!(pattern_count(&a), 1);
        assert_eq!(a.current_channel(), Some(other));
        assert!(a.current_pattern().is_some());
        // Nothing to edit: a toast, no command.
        let empty = {
            let s = Session::new(
                Document::new(),
                true,
                EngineLink::stub(48000.0),
                Registry::new(Vec::new(), 48000.0),
            );
            App::with_dirs(
                s,
                Dirs {
                    music: std::env::temp_dir(),
                    data: std::env::temp_dir(),
                    config: std::env::temp_dir(),
                },
            )
        };
        let n = Rc::new(Cell::new(0));
        let n2 = n.clone();
        empty.on_command(move |_| n2.set(n2.get() + 1));
        empty.edit_notes(None);
        assert_eq!(n.get(), 0);
    }

    #[test]
    fn the_selection_stays_one_thing_through_edits() {
        let a = app();
        let k = add(&a, NewChannel::Preset("Kick".into())).unwrap();
        let s = add(&a, NewChannel::Preset("Snare".into())).unwrap();
        a.select_channel(k);
        let sel = a.selection();
        assert_eq!(sel.channel, Some(k));
        assert_eq!(a.current_channel(), sel.channel);
        let track = a
            .session
            .borrow()
            .document()
            .project
            .channel(k)
            .unwrap()
            .track;
        assert_eq!(sel.track, track, "the mixer track follows the channel");
        // Removing the selected channel moves the selection to a channel
        // that exists, never leaves it dangling.
        remove(&a, k);
        assert_eq!(a.current_channel(), Some(s));
        // Undo brings it back; selecting an unknown id changes nothing.
        a.undo();
        a.select_channel(protocol::ids::ChannelId(9999));
        assert_eq!(a.current_channel(), Some(s));
    }

    #[test]
    fn an_unknown_preset_falls_back_to_a_synth() {
        let a = app();
        let id = add(&a, NewChannel::Preset("No such sound".into())).unwrap();
        assert!(a.session.borrow().document().project.channel(id).is_some());
    }
}
