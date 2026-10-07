// SPDX-License-Identifier: GPL-3.0-or-later
//! Presence (SPEC 18, 18.1, 18.2, Amendment 18): when an agent is working,
//! the window glows, the objects it changes glow, a pill says what it is
//! doing, and Escape (or the pill's Stop) cancels it.
//!
//! This file is the logic: what is active, what glows and how strongly,
//! what the pill says, what a stop does. It has no widgets and takes the
//! time as an argument, so the tests do not wait. `presence_ui.rs` draws it.
//!
//! The state lives in one thread-local (the GTK thread), so the timeline
//! can ask "does this clip glow?" while it draws, without the bridge
//! borrowed.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use doc::history::{Author, describe_edit};
use gtk::gdk;
use protocol::control::Focus;
use protocol::edit::Edit;
use protocol::ids::{ChannelId, ClipId, InstanceId, PatternId, TrackId};
use protocol::model::{Clip, Project};

use crate::app::App;
use crate::control_bridge;

/// An agent counts as active until this long after its last request.
pub const ACTIVE_FOR: Duration = Duration::from_secs(2);
/// An object stops glowing this long after the agent moves on.
pub const FADE: Duration = Duration::from_millis(1500);
/// The answer to a request from an agent that was stopped.
pub const STOPPED: &str = "stopped by the user";

#[derive(Clone, Debug)]
struct Lit {
    focus: Focus,
    /// When the agent moved on; `None` while it works on it.
    released: Option<Instant>,
}

#[derive(Debug, Default)]
pub struct Presence {
    author: Option<Author>,
    /// The client's name, shown in the pill and the project toast.
    who: String,
    last: Option<Instant>,
    /// The agent's own words (`SetActivity`), shown in the pill.
    text: Option<String>,
    /// What the latest commit did, used when there are no words.
    latest_edit: Option<String>,
    /// A `SetActivity` focus wins over the one derived from edits.
    explicit_focus: bool,
    stopped: HashSet<Author>,
    /// The agent's commits since this activity began.
    commits: u32,
    lit: Vec<Lit>,
    hover: Vec<Focus>,
}

/// What a stop leaves to undo.
#[derive(Clone, Debug, PartialEq)]
pub struct Stopped {
    pub author: Author,
    pub commits: u32,
}

impl Presence {
    /// Whether an agent is working at `now`.
    pub fn active(&self, now: Instant) -> bool {
        self.author
            .as_ref()
            .is_some_and(|a| !self.stopped.contains(a))
            && self
                .last
                .is_some_and(|t| now.saturating_duration_since(t) < ACTIVE_FOR)
    }

    /// A request arrived. `false` means the agent was stopped: refuse it.
    pub fn request(&mut self, now: Instant, author: &Author, who: &str) -> bool {
        if self.stopped.contains(author) {
            return false;
        }
        if !self.active(now) || self.author.as_ref() != Some(author) {
            self.commits = 0;
            self.text = None;
            self.latest_edit = None;
            self.explicit_focus = false;
        }
        self.author = Some(author.clone());
        self.who = who.to_string();
        self.last = Some(now);
        true
    }

    /// `SetActivity`: `text` `None` ends the activity and its group.
    pub fn activity(&mut self, now: Instant, text: Option<String>, focus: Option<Focus>) {
        match text {
            None => {
                self.text = None;
                self.last = None;
                self.commits = 0;
                self.explicit_focus = false;
                self.set_current(now, Vec::new());
            }
            Some(t) => {
                self.text = Some(t);
                self.last = Some(now);
                self.explicit_focus = focus.is_some();
                if let Some(f) = focus {
                    self.set_current(now, vec![f]);
                }
            }
        }
    }

    /// An agent batch was applied: it touched `foci`, in short `what`.
    pub fn edit_applied(&mut self, now: Instant, foci: Vec<Focus>, what: String) {
        self.commits += 1;
        self.latest_edit = Some(what);
        self.last = Some(now);
        if !self.explicit_focus {
            self.set_current(now, foci);
        }
    }

    /// The objects in `foci` glow; the ones that glowed before fade out.
    fn set_current(&mut self, now: Instant, foci: Vec<Focus>) {
        let foci: Vec<Focus> = foci.into_iter().filter(|f| *f != Focus::Window).collect();
        for l in &mut self.lit {
            if foci.contains(&l.focus) {
                l.released = None;
            } else if l.released.is_none() {
                l.released = Some(now);
            }
        }
        for f in foci {
            if !self.lit.iter().any(|l| l.focus == f) {
                self.lit.push(Lit {
                    focus: f,
                    released: None,
                });
            }
        }
    }

    /// Housekeeping, called a few times a second: an idle agent lets go
    /// of its objects, and faded ones are forgotten.
    pub fn tick(&mut self, now: Instant) {
        if !self.active(now) {
            for l in &mut self.lit {
                l.released.get_or_insert(now);
            }
            self.explicit_focus = false;
        }
        self.lit.retain(|l| {
            l.released
                .is_none_or(|t| now.saturating_duration_since(t) < FADE)
        });
    }

    /// How strongly `focus` glows now, 0 to 1.
    pub fn intensity(&self, focus: Focus, now: Instant) -> f32 {
        let mut best: f32 = if self.hover.contains(&focus) {
            1.0
        } else {
            0.0
        };
        for l in self.lit.iter().filter(|l| l.focus == focus) {
            let v = match l.released {
                None => 1.0,
                Some(t) => {
                    1.0 - now.saturating_duration_since(t).as_secs_f32() / FADE.as_secs_f32()
                }
            };
            best = best.max(v.clamp(0.0, 1.0));
        }
        best
    }

    /// Whether anything needs redrawing over time.
    pub fn animating(&self, now: Instant) -> bool {
        self.active(now) || !self.lit.is_empty() || !self.hover.is_empty()
    }

    /// What the pill says: the agent's words, else the latest commit.
    pub fn label(&self) -> String {
        self.text
            .clone()
            .or_else(|| self.latest_edit.clone())
            .unwrap_or_else(|| "Working on your project".to_string())
    }

    pub fn who(&self) -> &str {
        &self.who
    }

    pub fn set_hover(&mut self, foci: Vec<Focus>) {
        self.hover = foci;
    }

    /// The hard stop: the agent is refused from now on and the glow goes
    /// at once. `None` when no agent was active.
    pub fn stop(&mut self, now: Instant) -> Option<Stopped> {
        if !self.active(now) {
            return None;
        }
        let author = self.author.clone()?;
        let out = Stopped {
            author: author.clone(),
            commits: self.commits,
        };
        self.stopped.insert(author);
        self.last = None;
        self.commits = 0;
        self.text = None;
        self.latest_edit = None;
        self.explicit_focus = false;
        self.lit.clear();
        Some(out)
    }

    /// The user turned agent control back on.
    pub fn clear_stopped(&mut self) {
        self.stopped.clear();
    }

    pub fn is_stopped(&self, author: &Author) -> bool {
        self.stopped.contains(author)
    }
}

thread_local! {
    static STATE: RefCell<Presence> = RefCell::new(Presence::default());
    static COLOR: RefCell<Option<gdk::RGBA>> = const { RefCell::new(None) };
}

/// Runs `f` on the one presence of this window.
pub fn with<R>(f: impl FnOnce(&mut Presence) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

pub fn active() -> bool {
    with(|p| p.active(Instant::now()))
}

/// The warm orange, set by the glow widget from the stylesheet.
pub fn set_glow_color(c: gdk::RGBA) {
    COLOR.with(|x| *x.borrow_mut() = Some(c));
}

pub fn glow_color() -> gdk::RGBA {
    COLOR
        .with(|x| *x.borrow())
        .unwrap_or_else(|| gdk::RGBA::new(1.0, 0.47, 0.0, 1.0))
}

/// How strongly the timeline draws `clip`'s outline: its own focus, or
/// the content it plays.
pub fn clip_glow(clip: &Clip) -> f32 {
    let now = Instant::now();
    with(|p| {
        p.intensity(Focus::Clip(clip.id), now)
            .max(p.intensity(Focus::Pattern(clip.pattern), now))
    })
}

/// How strongly an instrument row (and its lane) glows.
pub fn channel_glow(id: ChannelId) -> f32 {
    let now = Instant::now();
    with(|p| p.intensity(Focus::Channel(id), now))
}

/// The objects an edit batch touched (18.1, BRIDGE.md "Glow"). Ids the
/// batch created are looked up in the project, since the document hands
/// out one counter for every kind of id.
pub fn foci_of_edits(edits: &[Edit], created: &[u32], project: &Project) -> Vec<Focus> {
    let mut out: Vec<Focus> = Vec::new();
    let mut add = |f: Focus| {
        if !out.contains(&f) {
            out.push(f);
        }
    };
    let insert = |add: &mut dyn FnMut(Focus), track: TrackId, instance: InstanceId| {
        add(Focus::Insert(instance));
        add(Focus::Track(track));
    };
    for e in edits {
        use Edit::*;
        match e {
            RenameChannel { channel, .. }
            | SetChannelMix { channel, .. }
            | SetChannelTrack { channel, .. }
            | SetRootKey { channel, .. }
            | SetSynthParam { channel, .. }
            | SetSynthWave { channel, .. }
            | SetSamplerSample { channel, .. }
            | SetSamplerMode { channel, .. }
            | SetSamplerParam { channel, .. }
            | SetBass808Mono { channel, .. }
            | SetBass808Param { channel, .. }
            | SetChokeGroup { channel, .. } => add(Focus::Channel(*channel)),
            AddPattern { instrument, .. } | AddClip { instrument, .. } => {
                add(Focus::Channel(*instrument))
            }
            MoveClipToInstrument { clip, instrument } => {
                add(Focus::Clip(*clip));
                add(Focus::Channel(*instrument));
            }
            SplitClip { clip, .. } | MakeUnique { clip } => add(Focus::Clip(*clip)),
            DuplicateClips { clips, .. }
            | MoveClips { clips, .. }
            | ResizeClips { clips, .. }
            | SetClipMuted { clips, .. } => clips.iter().for_each(|c| add(Focus::Clip(*c))),
            RenamePattern { pattern, .. }
            | SetPatternLength { pattern, .. }
            | SetStepTicks { pattern, .. }
            | SetStep { pattern, .. }
            | AddNotes { pattern, .. }
            | RemoveNotes { pattern, .. }
            | MoveNotes { pattern, .. }
            | ResizeNotes { pattern, .. }
            | SetNoteVelocity { pattern, .. }
            | SetStepLanes { pattern, .. }
            | SetNoteRepeat { pattern, .. }
            | SetSwing { pattern, .. } => add(Focus::Pattern(*pattern)),
            RenameTrack { track, .. }
            | SetTrackMix { track, .. }
            | SetSend { track, .. }
            | RemoveSend { track, .. } => add(Focus::Track(*track)),
            AddInsert { track, .. } | AddBuiltinInsert { track, .. } => add(Focus::Track(*track)),
            SetFxParam {
                track, instance, ..
            }
            | SetSaturatorCurve {
                track, instance, ..
            }
            | SetDelayPingPong {
                track, instance, ..
            }
            | SetSidechain {
                track, instance, ..
            }
            | MoveInsert {
                track, instance, ..
            } => insert(&mut add, *track, *instance),
            SetPluginParam { instance, .. } => add(Focus::Insert(*instance)),
            // The transport bar is part of the window glow; removals and
            // library edits leave nothing to point at.
            _ => {}
        }
    }
    for id in created {
        if project.clips.iter().any(|c| c.id == ClipId(*id)) {
            add(Focus::Clip(ClipId(*id)));
        } else if project.channels.iter().any(|c| c.id == ChannelId(*id)) {
            add(Focus::Channel(ChannelId(*id)));
        } else if project.tracks.iter().any(|t| t.id == TrackId(*id)) {
            add(Focus::Track(TrackId(*id)));
        } else if project.patterns.iter().any(|p| p.id == PatternId(*id)) {
            add(Focus::Pattern(PatternId(*id)));
        }
    }
    out
}

/// The pill text for a batch: "Adding a clip to Bass".
pub fn describe_batch(edits: &[Edit]) -> String {
    match edits {
        [] => String::new(),
        [one] => describe_edit(one),
        [first, rest @ ..] => format!("{} and {} more", describe_edit(first), rest.len()),
    }
}

// ---- hooks called from the bridge ----

/// First look at a request. `true`: the agent was stopped, refuse it.
pub fn refuse(client: &control::ClientInfo) -> bool {
    if client.transport != protocol::control::Transport::Agent {
        return false;
    }
    let author = control_bridge::author_of(client);
    let who = protocol::control::agent_string(&client.name);
    !with(|p| p.request(Instant::now(), &author, &who))
}

/// An agent batch was applied.
pub fn on_edit(app: &App, author: &Author, edits: &[Edit], created: &[u32]) {
    if !matches!(author, Author::Agent(_)) {
        return;
    }
    let foci = {
        let s = app.session.borrow();
        foci_of_edits(edits, created, &s.document().project)
    };
    let what = describe_batch(edits);
    with(|p| p.edit_applied(Instant::now(), foci, what));
}

/// A commit whose edits are not at hand (a queued batch, a kit): it counts
/// for Undo Its Changes and glows what it created.
pub fn on_applied(app: &App, author: &Author, created: &[u32], what: &str) {
    if !matches!(author, Author::Agent(_)) {
        return;
    }
    let foci = foci_of_edits(&[], created, &app.session.borrow().document().project);
    with(|p| p.edit_applied(Instant::now(), foci, what.to_string()));
}

/// An agent declared or ended its activity.
pub fn on_activity(author: &Author, text: Option<String>, focus: Option<Focus>) {
    if matches!(author, Author::Agent(_)) {
        with(|p| p.activity(Instant::now(), text, focus));
    }
}

/// An agent turned control back on or off from the Agent page.
pub fn on_enabled(on: bool) {
    if on {
        with(Presence::clear_stopped);
    }
}

/// After an agent's `ProjectNew` or `ProjectOpen` (18.6): a toast names
/// the project and offers the way back. `previous` is the project that was
/// open before. `ProjectClose` calls `on_project_closed` once Home wires it.
pub fn on_project_changed(app: &Rc<App>, author: &Author, name: &str, previous: Option<PathBuf>) {
    if !matches!(author, Author::Agent(_)) {
        return;
    }
    let who = with(|p| p.who().to_string());
    let who = if who.is_empty() {
        "An agent".into()
    } else {
        who
    };
    let msg = format!("{who} opened {name}");
    match previous {
        Some(prev) => {
            let a = app.clone();
            app.toast_action(&msg, "Go Back", move || go_back(&a, prev.clone()));
        }
        None => app.toast(&msg),
    }
}

/// Hook for Home: an agent closed the project and Home came up.
pub fn on_project_closed(app: &Rc<App>, author: &Author, previous: Option<PathBuf>) {
    on_project_changed(app, author, "Home", previous);
}

/// Saves what is open now, then reopens `prev`.
fn go_back(app: &Rc<App>, prev: PathBuf) {
    let a = app.clone();
    crate::files::save_current(app, move |r| match r {
        Ok(_) => crate::files::open_path(&a, prev),
        Err(e) => a.toast(&e),
    });
}

/// The hard stop (Escape, the pill's Stop). Returns whether an agent was
/// active.
pub fn stop(app: &Rc<App>) -> bool {
    let Some(s) = with(|p| p.stop(Instant::now())) else {
        return false;
    };
    control_bridge::stop_agents(app);
    crate::presence_ui::snap_off();
    if s.commits == 0 {
        app.toast("Agent stopped");
    } else {
        let a = app.clone();
        app.toast_action("Agent stopped", "Undo Its Changes", move || {
            undo_its_changes(&a, &s)
        });
    }
    true
}

/// Undoes the stopped agent's commits since its activity began, one by one,
/// each through the author-scoped undo.
fn undo_its_changes(app: &Rc<App>, s: &Stopped) {
    for _ in 0..s.commits {
        if matches!(
            control_bridge::undo_redo(app, &s.author, true),
            protocol::control::Outcome::Err { .. }
        ) {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ids::{ChannelId, ClipId};

    fn agent(n: &str) -> Author {
        Author::Agent(n.into())
    }

    fn t(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn active_until_two_seconds_after_the_last_request() {
        let (mut p, t0) = (Presence::default(), Instant::now());
        assert!(!p.active(t0));
        assert!(p.request(t0, &agent("a-1"), "Claude"));
        assert!(p.active(t(t0, 1999)));
        assert!(!p.active(t(t0, 2000)));
        // Another request keeps it going.
        assert!(p.request(t(t0, 1500), &agent("a-1"), "Claude"));
        assert!(p.active(t(t0, 3000)));
        assert!(!p.active(t(t0, 3500)));
    }

    #[test]
    fn a_stopped_agent_is_refused_until_it_is_re_enabled() {
        let (mut p, t0) = (Presence::default(), Instant::now());
        let a = agent("a-1");
        p.request(t0, &a, "Claude");
        p.edit_applied(t0, vec![Focus::Channel(ChannelId(2))], "x".into());
        let s = p.stop(t(t0, 100)).expect("it was active");
        assert_eq!(s.author, a);
        assert_eq!(s.commits, 1);
        assert!(!p.active(t(t0, 100)), "the glow goes at once");
        assert_eq!(p.intensity(Focus::Channel(ChannelId(2)), t(t0, 100)), 0.0);
        assert!(!p.request(t(t0, 200), &a, "Claude"), "refused");
        assert!(!p.request(t(t0, 900), &a, "Claude"), "still refused");
        assert!(
            p.request(t(t0, 900), &agent("a-2"), "Claude"),
            "a new session"
        );
        p.clear_stopped();
        assert!(p.request(t(t0, 1000), &a, "Claude"));
        assert!(p.stop(t(t0, 5000)).is_none(), "nothing active to stop");
    }

    #[test]
    fn undo_covers_only_commits_since_the_activity_began() {
        let (mut p, t0) = (Presence::default(), Instant::now());
        let a = agent("a-1");
        p.request(t0, &a, "Claude");
        p.edit_applied(t0, vec![], "one".into());
        // Idle for longer than 2 s: the next request opens a new group.
        p.request(t(t0, 5000), &a, "Claude");
        p.edit_applied(t(t0, 5000), vec![], "two".into());
        p.edit_applied(t(t0, 5100), vec![], "three".into());
        assert_eq!(p.stop(t(t0, 5200)).map(|s| s.commits), Some(2));
    }

    #[test]
    fn ending_an_activity_closes_the_group() {
        let (mut p, t0) = (Presence::default(), Instant::now());
        let a = agent("a-1");
        p.request(t0, &a, "Claude");
        p.activity(t0, Some("Building a beat".into()), None);
        assert_eq!(p.label(), "Building a beat");
        p.edit_applied(t0, vec![], "Adding a clip to Bass".into());
        p.activity(t(t0, 10), None, None);
        assert!(!p.active(t(t0, 10)));
        assert_eq!(p.label(), "Adding a clip to Bass");
    }

    #[test]
    fn the_pill_falls_back_to_the_latest_edit() {
        let (mut p, t0) = (Presence::default(), Instant::now());
        p.request(t0, &agent("a-1"), "Claude");
        assert_eq!(p.label(), "Working on your project");
        p.edit_applied(t0, vec![], "Adding a clip to Bass".into());
        assert_eq!(p.label(), "Adding a clip to Bass");
        p.activity(t0, Some("Making the drums".into()), None);
        assert_eq!(p.label(), "Making the drums");
    }

    #[test]
    fn objects_glow_then_fade_for_one_and_a_half_seconds() {
        let (mut p, t0) = (Presence::default(), Instant::now());
        let (bass, keys) = (Focus::Channel(ChannelId(1)), Focus::Channel(ChannelId(2)));
        p.request(t0, &agent("a-1"), "Claude");
        p.edit_applied(t0, vec![bass], "a".into());
        assert_eq!(p.intensity(bass, t(t0, 500)), 1.0);
        p.edit_applied(t(t0, 600), vec![keys], "b".into());
        assert_eq!(p.intensity(keys, t(t0, 600)), 1.0);
        let half = p.intensity(bass, t(t0, 600 + 750));
        assert!((half - 0.5).abs() < 0.01, "{half}");
        assert_eq!(p.intensity(bass, t(t0, 600 + 1500)), 0.0);
        p.tick(t(t0, 600 + 1600));
        assert!(!p.lit.iter().any(|l| l.focus == bass), "forgotten");
        // When the agent goes idle, the rest let go too.
        p.tick(t(t0, 4000));
        assert!(p.intensity(keys, t(t0, 5600)) == 0.0);
        p.tick(t(t0, 5600));
        assert!(p.lit.is_empty() && !p.animating(t(t0, 5600)));
    }

    #[test]
    fn a_declared_focus_wins_over_the_edits() {
        let (mut p, t0) = (Presence::default(), Instant::now());
        let (a, b) = (Focus::Clip(ClipId(5)), Focus::Channel(ChannelId(1)));
        p.request(t0, &agent("a-1"), "Claude");
        p.activity(t0, Some("Fixing the hats".into()), Some(a));
        p.edit_applied(t0, vec![b], "x".into());
        assert_eq!(p.intensity(a, t0), 1.0);
        assert_eq!(p.intensity(b, t0), 0.0);
        // Words without a focus hand the glow back to the edits.
        p.activity(t(t0, 10), Some("Next".into()), None);
        p.edit_applied(t(t0, 20), vec![b], "y".into());
        assert_eq!(p.intensity(b, t(t0, 20)), 1.0);
    }

    #[test]
    fn hovering_an_entry_glows_its_objects() {
        let (mut p, t0) = (Presence::default(), Instant::now());
        let f = Focus::Track(TrackId(3));
        p.set_hover(vec![f]);
        assert_eq!(p.intensity(f, t0), 1.0);
        p.set_hover(vec![]);
        assert_eq!(p.intensity(f, t0), 0.0);
    }

    #[test]
    fn edits_map_to_the_objects_they_touch() {
        let mut project = Project::empty();
        let c = Clip {
            id: ClipId(9),
            instrument: ChannelId(1),
            pattern: PatternId(4),
            start: 0,
            len: 96,
            offset: 0,
            muted: false,
            audio: None,
            group: None,
        };
        project.clips.push(c);
        let edits = [
            Edit::AddClip {
                instrument: ChannelId(1),
                pattern: None,
                start: 0,
                len: 96,
            },
            Edit::SetStep {
                pattern: PatternId(4),
                step: 0,
                on: true,
                vel: None,
            },
            Edit::MoveClips {
                clips: vec![ClipId(9)],
                dt: 4,
            },
            Edit::SetFxParam {
                track: TrackId(2),
                instance: InstanceId(7),
                param: 0,
                value: 0.5,
            },
            Edit::SetTempo { bpm: 90.0 },
        ];
        let got = foci_of_edits(&edits, &[9], &project);
        assert_eq!(
            got,
            vec![
                Focus::Channel(ChannelId(1)),
                Focus::Pattern(PatternId(4)),
                Focus::Clip(ClipId(9)),
                Focus::Insert(InstanceId(7)),
                Focus::Track(TrackId(2)),
            ]
        );
        assert!(foci_of_edits(&edits[4..], &[], &project).is_empty());
    }

    #[test]
    fn a_clip_glows_for_its_own_focus_or_the_content_it_plays() {
        let (mut p, t0) = (Presence::default(), Instant::now());
        p.request(t0, &agent("a-1"), "Claude");
        p.edit_applied(t0, vec![Focus::Pattern(PatternId(4))], "x".into());
        let on_pattern = p.intensity(Focus::Pattern(PatternId(4)), t0);
        let other = p.intensity(Focus::Pattern(PatternId(5)), t0);
        assert_eq!((on_pattern, other), (1.0, 0.0));
    }
}
