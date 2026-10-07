// SPDX-License-Identifier: GPL-3.0-or-later
//! Hum a melody (SPEC 21.2, 21.3): the Hum button's sheet, the microphone
//! while it listens, and the notes it leaves on the timeline. The steps are
//! `hum_logic::Flow`; this file shows them and does what they ask.
//!
//! Privacy: the microphone opens only when the user presses Start and the
//! count-in ends, and closes on Stop or when the sheet closes. An agent can
//! ask for the sheet (`hum_prepare`) but only the user presses Start.
//! Recordings stay in memory and are never written to disk.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Mutex;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use control::hum::{self as shared, Prepare};
use doc::history::Author;
use protocol::control::{Focus, JobState};
use protocol::edit::Edit;
use protocol::engine::AuditionSource;
use protocol::ids::{ChannelId, ClipId, PatternId};
use protocol::model::{SynthParams, ticks_per_bar};

use crate::app::{App, UiCommand};
use crate::channels::{self, NewChannel};
use crate::hum_logic::{Effect, Flow, HumNote, Input, Phase, PlaceOpts, free_start, place};

/// Turns a recording into the notes heard in it.
pub type Transcriber = fn(&engine::Captured) -> Result<Vec<HumNote>, String>;

/// The real model: the hum downmixed to mono, one voice.
fn real(c: &engine::Captured) -> Result<Vec<HumNote>, String> {
    let opts = transcribe::Options {
        monophonic: true,
        onset_threshold: 0.4,
        ..Default::default()
    };
    let notes = transcribe::transcribe(&c.mono(), c.rate, &opts).map_err(|e| e.to_string())?;
    Ok(notes
        .iter()
        .map(|n| HumNote {
            start_s: n.start_s,
            end_s: n.end_s,
            key: n.midi_key,
            volume: n.velocity as f32 / 127.0,
        })
        .collect())
}

static TRANSCRIBER: Mutex<Transcriber> = Mutex::new(real);

/// Replaces the transcription (the real model, or a fake in tests).
pub fn set_transcriber(f: Transcriber) {
    *TRANSCRIBER.lock().unwrap_or_else(|e| e.into_inner()) = f;
}

fn transcriber() -> Transcriber {
    *TRANSCRIBER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Longest the sheet listens, in seconds.
const MAX_LISTEN_S: f64 = 60.0;

// ---- what an agent's request leaves behind ----------------------------------

#[derive(Default)]
struct Host {
    ask: Option<(Prepare, Author)>,
    job: Option<JobState>,
    clips: Vec<u32>,
    revision: u64,
    open: bool,
    close: Option<Rc<dyn Fn()>>,
}

thread_local! {
    static HOST: RefCell<Host> = RefCell::new(Host::default());
}

fn host<R>(f: impl FnOnce(&mut Host) -> R) -> R {
    HOST.with(|h| f(&mut h.borrow_mut()))
}

/// An agent asks for the sheet. `false` if one is already open.
pub fn ask(app: &Rc<App>, author: &Author, p: Prepare, focus: Option<Focus>) -> bool {
    if host(|h| h.open) {
        return false;
    }
    let pill = if p.message.is_empty() {
        "Waiting for you to hum".to_string()
    } else {
        p.message.clone()
    };
    crate::control_bridge::set_activity(app, author, Some(&pill), focus);
    host(|h| {
        h.ask = Some((p, author.clone()));
        h.job = Some(JobState::Running);
        h.clips.clear();
    });
    app.command(UiCommand::Hum);
    true
}

/// State and result of the agent's request, for `JobStatus` and `JobResult`.
pub fn job() -> Option<(JobState, Vec<u32>, u64)> {
    host(|h| h.job.map(|s| (s, h.clips.clone(), h.revision)))
}

/// An agent gives up waiting: the sheet closes as if the user declined.
pub fn cancel_job() {
    if let Some(close) = host(|h| h.close.clone()) {
        close();
    }
}

fn finish_ask(app: &Rc<App>, state: JobState, clips: Vec<ClipId>) {
    let who = host(|h| {
        let who = h.ask.take().map(|(_, a)| a);
        if who.is_some() {
            h.job = Some(state);
            h.clips = clips.iter().map(|c| c.0).collect();
        }
        who
    });
    if let Some(a) = who {
        crate::control_bridge::set_activity(app, &a, None, None);
    }
    host(|h| h.revision = 0);
}

// ---- the sheet ---------------------------------------------------------------

struct Sheet {
    app: Rc<App>,
    dialog: adw::Dialog,
    flow: Cell<Flow>,
    status: gtk::Label,
    level: gtk::LevelBar,
    button: gtk::Button,
    keep: adw::SwitchRow,
    bpm: f64,
    beat_timer: RefCell<Option<glib::SourceId>>,
    meter_timer: RefCell<Option<glib::SourceId>>,
    heard: RefCell<Vec<HumNote>>,
    listened_s: Cell<f64>,
}

/// Puts the Hum button and the sheet in the window.
pub fn install(app: &Rc<App>, window: &impl IsA<gtk::Widget>) {
    let (a, w) = (app.clone(), window.clone().upcast::<gtk::Widget>());
    app.on_command(move |c| {
        if c == UiCommand::Hum {
            show(&a, &w);
        }
    });
}

fn show(app: &Rc<App>, parent: &gtk::Widget) {
    if host(|h| std::mem::replace(&mut h.open, true)) {
        return;
    }
    let (bpm, beats) = {
        let s = app.session.borrow();
        let p = &s.document().project;
        (p.tempo_bpm, p.time_sig_num)
    };
    let ask = host(|h| h.ask.clone().map(|(p, _)| p));

    let intro = gtk::Label::new(Some(&match &ask {
        Some(p) if !p.message.is_empty() => p.message.clone(),
        _ => "Hum a melody and Oto turns it into notes.".to_string(),
    }));
    intro.set_wrap(true);
    intro.set_xalign(0.0);
    intro.add_css_class("title-4");
    let privacy = gtk::Label::new(Some(
        "Oto listens only while this sheet is counting or listening. Nothing is saved as sound.",
    ));
    privacy.set_wrap(true);
    privacy.set_xalign(0.0);
    privacy.add_css_class("dim-label");
    let status = gtk::Label::new(None);
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.set_accessible_role(gtk::AccessibleRole::Status);
    let level = gtk::LevelBar::for_interval(0.0, 1.0);
    level.set_tooltip_text(Some("How loud Oto hears you"));
    level.update_property(&[gtk::accessible::Property::Label("Microphone level")]);
    let keep = adw::SwitchRow::new();
    keep.set_title("Keep My Timing");
    keep.set_subtitle("Off: notes snap to the grid");
    let button = gtk::Button::with_label("Start");
    button.add_css_class("suggested-action");
    button.add_css_class("pill");
    button.set_halign(gtk::Align::Center);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(18);
    body.set_margin_end(18);
    body.append(&intro);
    body.append(&privacy);
    body.append(&status);
    body.append(&level);
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.append(&keep);
    body.append(&list);
    body.append(&button);

    let header = adw::HeaderBar::new();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&body));
    let dialog = adw::Dialog::builder()
        .title("Hum your melody")
        .content_width(420)
        .child(&view)
        .follows_content_size(true)
        .build();

    let sheet = Rc::new(Sheet {
        app: app.clone(),
        dialog: dialog.clone(),
        flow: Cell::new(Flow::new(beats)),
        status,
        level,
        button: button.clone(),
        keep,
        bpm,
        beat_timer: RefCell::new(None),
        meter_timer: RefCell::new(None),
        heard: RefCell::new(Vec::new()),
        listened_s: Cell::new(0.0),
    });
    sheet.refresh();

    {
        let s = sheet.clone();
        button.connect_clicked(move |_| {
            let input = match s.flow.get().phase {
                Phase::Ready => Input::Start,
                _ => Input::Stop,
            };
            s.run(input);
        });
    }
    {
        // Space stops, wherever the focus is.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let s = sheet.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            let live = matches!(s.flow.get().phase, Phase::CountIn { .. } | Phase::Recording);
            if key == gtk::gdk::Key::space && live {
                s.run(Input::Stop);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        dialog.add_controller(keys);
    }
    {
        let s = sheet.clone();
        dialog.connect_closed(move |_| s.closed());
    }
    {
        let weak = Rc::downgrade(&sheet);
        host(|h| {
            h.close = Some(Rc::new(move || {
                if let Some(s) = weak.upgrade() {
                    s.dialog.close();
                }
            }))
        });
    }
    dialog.present(Some(parent));
}

impl Sheet {
    fn run(self: &Rc<Sheet>, input: Input) {
        let mut flow = self.flow.get();
        let effects = flow.step(input);
        self.flow.set(flow);
        self.refresh();
        for e in effects {
            self.apply(e);
        }
    }

    fn refresh(&self) {
        let phase = self.flow.get().phase;
        let (text, label) = match phase {
            Phase::Ready => (
                "Press Start. You get one bar of clicks, then hum.".to_string(),
                "Start",
            ),
            Phase::CountIn { beat } => (format!("Get ready... {beat}"), "Stop"),
            Phase::Recording => (
                "Listening. Hum your melody, then press Stop or Space.".to_string(),
                "Stop",
            ),
            Phase::Working => ("Turning your hum into notes...".to_string(), "Stop"),
            Phase::Done | Phase::Closed => (String::new(), "Start"),
        };
        if phase != Phase::Ready || self.status.text().is_empty() {
            self.status.set_text(&text);
        }
        self.button.set_label(label);
        self.button.set_sensitive(phase != Phase::Working);
        self.keep.set_sensitive(phase == Phase::Ready);
    }

    fn apply(self: &Rc<Sheet>, e: Effect) {
        match e {
            Effect::Click { accent } => {
                self.click(accent);
                if self.beat_timer.borrow().is_none() {
                    let ms = (60_000.0 / self.bpm.max(20.0)) as u64;
                    let me = Rc::downgrade(self);
                    let id = glib::timeout_add_local(Duration::from_millis(ms), move || {
                        let Some(s) = me.upgrade() else {
                            return glib::ControlFlow::Break;
                        };
                        if !matches!(s.flow.get().phase, Phase::CountIn { .. }) {
                            s.beat_timer.borrow_mut().take();
                            return glib::ControlFlow::Break;
                        }
                        s.run(Input::Beat);
                        if matches!(s.flow.get().phase, Phase::CountIn { .. }) {
                            glib::ControlFlow::Continue
                        } else {
                            s.beat_timer.borrow_mut().take();
                            glib::ControlFlow::Break
                        }
                    });
                    *self.beat_timer.borrow_mut() = Some(id);
                }
            }
            Effect::OpenMic => {
                let r = self.app.session.borrow_mut().link.start_capture();
                match r {
                    Ok(()) => self.start_meter(),
                    Err(msg) => {
                        let mut f = self.flow.get();
                        f.phase = Phase::Ready;
                        self.flow.set(f);
                        self.status.set_text(&format!("Oto cannot listen: {msg}"));
                        self.refresh();
                    }
                }
            }
            Effect::CloseMic => {
                self.stop_meter();
                let rec = self.app.session.borrow_mut().link.stop_capture();
                self.listened_s.set(rec.seconds());
                if self.flow.get().phase == Phase::Working {
                    let me = self.clone();
                    let f = transcriber();
                    self.app.tasks.spawn(
                        "hum-transcribe",
                        move || f(&rec),
                        move |r| match r {
                            Ok(notes) => {
                                let n = notes.len();
                                *me.heard.borrow_mut() = notes;
                                me.run(Input::Heard(Ok(n)));
                            }
                            Err(e) => me.run(Input::Heard(Err(e))),
                        },
                    );
                }
            }
            // The recording is handed to the worker by `CloseMic`.
            Effect::Transcribe => {}
            Effect::Place => {
                let keep = self.keep.is_active();
                let heard = self.heard.borrow().clone();
                let ask = host(|h| h.ask.clone().map(|(p, _)| p));
                let clips = put_on_timeline(&self.app, &heard, ask.as_ref(), keep);
                finish_ask(&self.app, JobState::Done, clips);
            }
            Effect::Problem(msg) => {
                self.status.set_text(&msg);
            }
            Effect::Decline => {
                finish_ask(&self.app, JobState::Cancelled, Vec::new());
            }
            Effect::Close => {
                self.cleanup();
                self.dialog.close();
            }
        }
    }

    fn click(&self, accent: bool) {
        let key = if accent { 96 } else { 84 };
        let mut s = self.app.session.borrow_mut();
        let p = SynthParams::default();
        let _ = s
            .link
            .audition(None, AuditionSource::Synth(p), key, 100, true);
        drop(s);
        let app = self.app.clone();
        glib::timeout_add_local_once(Duration::from_millis(60), move || {
            let _ = app.session.borrow_mut().link.audition(
                None,
                AuditionSource::Synth(SynthParams::default()),
                key,
                0,
                false,
            );
        });
    }

    fn start_meter(self: &Rc<Sheet>) {
        let me = Rc::downgrade(self);
        let id = glib::timeout_add_local(Duration::from_millis(50), move || {
            let Some(s) = me.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let level = s.app.session.borrow().link.capture_level();
            s.level.set_value((level.sqrt() as f64).clamp(0.0, 1.0));
            s.listened_s.set(s.listened_s.get() + 0.05);
            if s.listened_s.get() >= MAX_LISTEN_S && s.flow.get().phase == Phase::Recording {
                s.run(Input::Stop);
            }
            if s.flow.get().phase == Phase::Recording {
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
        self.listened_s.set(0.0);
        *self.meter_timer.borrow_mut() = Some(id);
    }

    fn stop_meter(&self) {
        // The timer ends itself when the phase changes; forget its id.
        self.meter_timer.borrow_mut().take();
        self.level.set_value(0.0);
    }

    fn cleanup(&self) {
        // Never leave the microphone on.
        if self.app.session.borrow().link.is_capturing() {
            let _ = self.app.session.borrow_mut().link.stop_capture();
        }
        if let Some(id) = self.beat_timer.borrow_mut().take() {
            id.remove();
        }
        self.meter_timer.borrow_mut().take();
    }

    /// The dialog closed, by us or by the user.
    fn closed(self: &Rc<Sheet>) {
        let phase = self.flow.get().phase;
        if !matches!(phase, Phase::Done | Phase::Closed) {
            self.run(Input::Cancel);
        }
        self.cleanup();
        host(|h| {
            h.open = false;
            h.close = None;
        });
    }
}

// ---- the notes on the timeline ------------------------------------------------

/// Creates clips of notes on the chosen instrument, in one undo step, and
/// tells the user the key. Returns the clips.
pub fn put_on_timeline(
    app: &Rc<App>,
    heard: &[HumNote],
    ask: Option<&Prepare>,
    keep_timing: bool,
) -> Vec<ClipId> {
    let (bpm, bar, playhead) = {
        let s = app.session.borrow();
        let p = &s.document().project;
        (
            p.tempo_bpm,
            ticks_per_bar(p.time_sig_num),
            app.playhead_tick() as u32,
        )
    };
    let has = |c: &ChannelId| {
        app.session
            .borrow()
            .document()
            .project
            .channels
            .iter()
            .any(|x| x.id == *c)
    };
    let target = ask
        .and_then(|p| p.instrument)
        .map(ChannelId)
        .filter(has)
        .or_else(|| app.current_channel().filter(has));
    let opts = PlaceOpts {
        bpm,
        bar_ticks: bar,
        start_tick: 0,
        keep_timing,
    };
    let placement = place(heard, &opts);
    let total: u32 = placement.chunks.iter().map(|c| c.len).sum();

    let grouped = app.gesture_begin("Hum a melody");
    let in_gesture = grouped || app.session.borrow().editor.gesture_open();
    let run = |e: Vec<Edit>| {
        if in_gesture {
            app.gesture_edit(e)
        } else {
            app.edit(e)
        }
    };
    let instrument = match target {
        Some(c) => c,
        None => match add_hum_instrument(app) {
            Some(c) => c,
            None => {
                if grouped {
                    app.gesture_end();
                }
                return Vec::new();
            }
        },
    };
    let taken: Vec<(u32, u32)> = app
        .session
        .borrow()
        .document()
        .project
        .clips
        .iter()
        .filter(|c| c.instrument == instrument)
        .map(|c| (c.start, c.len))
        .collect();
    let mut clips = Vec::new();
    if let Some(origin) = free_start(&taken, playhead, total, bar) {
        for chunk in &placement.chunks {
            let Some(made) = run(vec![Edit::AddClip {
                instrument,
                pattern: None,
                start: origin + chunk.start,
                len: chunk.len,
            }]) else {
                break;
            };
            let pattern = PatternId(made.created[0]);
            let clip = ClipId(*made.created.last().unwrap_or(&0));
            run(vec![
                Edit::SetPatternLength {
                    pattern,
                    length_steps: chunk.steps,
                },
                Edit::AddNotes {
                    pattern,
                    notes: chunk.notes.clone(),
                },
            ]);
            clips.push(clip);
        }
    } else {
        app.toast("No room for the melody on that row");
    }
    if grouped {
        app.gesture_end();
    }
    if let Some(first) = clips.first() {
        app.select_clip(*first);
    }
    announce(app, ask, heard, &placement, keep_timing, &clips);
    clips
}

/// A "Hum Melody" instrument: a Lead sound if one is installed, else the
/// built-in Lead.
fn add_hum_instrument(app: &Rc<App>) -> Option<ChannelId> {
    let lead = plugin_host::sounds::sounds()
        .iter()
        .find(|s| s.role == "Lead" && crate::sound_picker::available(app, s))
        .and_then(crate::sound_picker::new_channel);
    let what = lead.unwrap_or_else(|| NewChannel::Preset("Lead".into()));
    let id = channels::add(app, what)?;
    let in_gesture = app.session.borrow().editor.gesture_open();
    let rename = vec![Edit::RenameChannel {
        channel: id,
        name: "Hum Melody".into(),
    }];
    if in_gesture {
        app.gesture_edit(rename);
    } else {
        app.edit(rename);
    }
    Some(id)
}

fn announce(
    app: &Rc<App>,
    ask: Option<&Prepare>,
    heard: &[HumNote],
    placement: &crate::hum_logic::Placement,
    keep_timing: bool,
    clips: &[ClipId],
) {
    if clips.is_empty() {
        return;
    }
    let weights: Vec<(u8, f32)> = heard
        .iter()
        .map(|n| (n.key, (n.end_s - n.start_s) as f32))
        .collect();
    let key = shared::detect_key(&weights);
    let msg = match key {
        Some(k) => format!("Your hum is in {}", k.name()),
        None => format!("Added {} notes from your hum", placement.note_count()),
    };
    let after = app.session.borrow().document().revision;
    let (label, again) = if keep_timing {
        ("Snap to Grid", false)
    } else {
        ("Keep My Timing", true)
    };
    let (a, heard, ask) = (app.clone(), heard.to_vec(), ask.cloned());
    app.toast_action(&msg, label, move || {
        // Only while nothing else has changed: the same undo step.
        if a.session.borrow().document().revision != after {
            a.toast("The song changed since; hum again to place it differently");
            return;
        }
        a.undo();
        put_on_timeline(&a, &heard, ask.as_ref(), again);
    });
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
    use std::sync::atomic::Ordering;

    fn app() -> Rc<App> {
        let dir = std::env::temp_dir().join(format!("ldaw-hum-{}", std::process::id()));
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

    fn add_synth(a: &Rc<App>) -> ChannelId {
        let r = a
            .edit(vec![Edit::AddChannel {
                name: "Lead".into(),
                instrument: NewInstrument::Synth {
                    params: SynthParams::default(),
                },
                root_key: 60,
                track: TrackId::MASTER,
            }])
            .expect("edit");
        ChannelId(r.created[0])
    }

    fn hummed() -> Vec<HumNote> {
        [
            (0.0, 0.5, 57),
            (0.5, 1.0, 59),
            (1.0, 1.5, 60),
            (1.5, 2.0, 62),
        ]
        .iter()
        .map(|&(a, b, k)| HumNote {
            start_s: a,
            end_s: b,
            key: k,
            volume: 0.6,
        })
        .collect()
    }

    fn clips(a: &App) -> Vec<protocol::model::Clip> {
        a.session.borrow().document().project.clips.clone()
    }

    #[test]
    fn the_notes_go_on_the_selected_instrument_in_one_undo_step() {
        let a = app();
        let c = add_synth(&a);
        a.select_channel(c);
        let made = put_on_timeline(&a, &hummed(), None, false);
        assert_eq!(made.len(), 1);
        let cl = clips(&a);
        assert_eq!((cl[0].instrument, cl[0].start), (c, 0));
        let p = a.session.borrow().document().project.clone();
        let pattern = p.pattern(cl[0].pattern).unwrap();
        assert_eq!(pattern.notes.len(), 4);
        a.undo();
        assert!(clips(&a).is_empty());
        assert_eq!(a.session.borrow().document().project.channels.len(), 1);
    }

    #[test]
    fn a_new_hum_melody_instrument_is_one_undo_step_with_its_clip() {
        let a = app();
        let made = put_on_timeline(&a, &hummed(), None, false);
        assert_eq!(made.len(), 1);
        let p = a.session.borrow().document().project.clone();
        assert_eq!(p.channels.len(), 1);
        assert_eq!(p.channels[0].name, "Hum Melody");
        a.undo();
        let p = a.session.borrow().document().project.clone();
        assert!(p.channels.is_empty() && p.clips.is_empty());
    }

    #[test]
    fn it_starts_at_the_playhead_bar_and_skips_clips_in_the_way() {
        let a = app();
        let c = add_synth(&a);
        a.select_channel(c);
        // Playhead in bar 2 (ticks 3840..7680): the melody starts at bar 2.
        a.session
            .borrow()
            .link
            .status
            .playhead_tick
            .store(4000, Ordering::Relaxed);
        put_on_timeline(&a, &hummed(), None, false);
        assert_eq!(clips(&a)[0].start, 3840);
        // Same spot again: the first free bar after the first melody.
        put_on_timeline(&a, &hummed(), None, false);
        let starts: Vec<u32> = clips(&a).iter().map(|c| c.start).collect();
        assert_eq!(starts, vec![3840, 7680]);
    }

    #[test]
    fn keep_my_timing_leaves_notes_off_the_grid() {
        let a = app();
        let c = add_synth(&a);
        a.select_channel(c);
        let mut heard = hummed();
        heard[1].start_s = 0.53;
        put_on_timeline(&a, &heard, None, true);
        let p = a.session.borrow().document().project.clone();
        let cl = clips(&a);
        let notes = &p.pattern(cl[0].pattern).unwrap().notes;
        assert!(notes.iter().any(|n| n.start % 240 != 0));
        a.undo();
        put_on_timeline(&a, &heard, None, false);
        let p = a.session.borrow().document().project.clone();
        let cl = clips(&a);
        let notes = &p.pattern(cl[0].pattern).unwrap().notes;
        assert!(notes.iter().all(|n| n.start % 240 == 0));
    }

    #[test]
    fn the_key_is_told_in_a_toast_with_a_way_to_keep_the_timing() {
        let a = app();
        let seen: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
        let s = seen.clone();
        a.set_action_toaster(move |m, l, _| s.borrow_mut().push((m.into(), l.into())));
        let c = add_synth(&a);
        a.select_channel(c);
        let heard: Vec<HumNote> = [57, 59, 60, 62, 64, 57, 60, 57]
            .iter()
            .enumerate()
            .map(|(i, k)| HumNote {
                start_s: i as f64 * 0.5,
                end_s: i as f64 * 0.5 + if *k == 57 { 0.5 } else { 0.25 },
                key: *k,
                volume: 0.5,
            })
            .collect();
        put_on_timeline(&a, &heard, None, false);
        assert_eq!(
            seen.borrow().last().unwrap(),
            &(
                "Your hum is in A minor".to_string(),
                "Keep My Timing".to_string()
            )
        );
    }

    #[test]
    fn the_microphone_is_not_there_in_tests_unless_a_fake_is_given() {
        let a = app();
        assert!(a.session.borrow_mut().link.start_capture().is_err());
        assert!(!a.session.borrow().link.is_capturing());
        a.session.borrow_mut().link.stub_capture = Some(engine::Captured {
            rate: 8000,
            channels: 1,
            samples: vec![0.1; 800],
        });
        a.session.borrow_mut().link.start_capture().unwrap();
        assert!(a.session.borrow().link.is_capturing());
        let rec = a.session.borrow_mut().link.stop_capture();
        assert_eq!(rec.samples.len(), 800);
        assert!(!a.session.borrow().link.is_capturing());
    }

    #[test]
    fn an_agents_request_waits_then_reports_declined_or_the_clips() {
        let a = app();
        let ask_ = |a: &Rc<App>| {
            ask(
                a,
                &Author::Agent("claude-1".into()),
                Prepare {
                    instrument: None,
                    bars: Some(4),
                    message: "Hum the chorus melody".into(),
                },
                None,
            )
        };
        assert!(job().is_none());
        assert!(ask_(&a));
        assert_eq!(job().unwrap().0, JobState::Running);
        finish_ask(&a, JobState::Cancelled, Vec::new());
        assert_eq!(job().unwrap().0, JobState::Cancelled);
        assert!(ask_(&a));
        let c = add_synth(&a);
        a.select_channel(c);
        let made = put_on_timeline(&a, &hummed(), None, false);
        finish_ask(&a, JobState::Done, made.clone());
        let (state, ids, _) = job().unwrap();
        assert_eq!(state, JobState::Done);
        assert_eq!(ids, vec![made[0].0]);
    }
}
