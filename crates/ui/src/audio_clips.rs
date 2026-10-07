// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio clips on the timeline (SPEC 21.1, Amendments 28 and 31): dropping
//! a sound makes an Audio row with one full-length clip, the clip draws a
//! waveform from peak summaries, its edges trim it and its corners fade it.
//! Everything here is plain logic except the drop itself, which applies
//! the same protocol edits an agent sends, as one undo step.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use protocol::consts::{MAX_TICK, MAX_TRACKS, PPQ};
use protocol::edit::{Edit, NewInstrument};
use protocol::ids::{ChannelId, ClipId, TrackId};
use protocol::model::{Clip, SampleHash, SampleRef};

use crate::app::App;
use crate::timeline_logic as tl;
use doc::history::Author;

/// Frames summarised by one min/max pair (SPEC 23).
pub const FRAMES_PER_PEAK: usize = 256;

/// Where on the timeline a sound was dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropAt {
    /// The empty area below the rows: a new Audio row.
    NewRow { tick: u32 },
    /// An existing Audio row.
    Row { channel: ChannelId, tick: u32 },
}

// ---- time ----

/// Ticks in `secs` at `bpm`.
pub fn secs_to_ticks(secs: f64, bpm: f64) -> f64 {
    secs * bpm / 60.0 * PPQ as f64
}

/// Seconds in `ticks` at `bpm`.
pub fn ticks_to_secs(ticks: f64, bpm: f64) -> f64 {
    ticks * 60.0 / (bpm.max(1.0) * PPQ as f64)
}

/// The clip length for a whole sound: its full duration at the tempo, at
/// least one step.
pub fn full_len(secs: f64, bpm: f64) -> u32 {
    (secs_to_ticks(secs, bpm).round().max(0.0) as u64).clamp(tl::STEP_TICKS as u64, MAX_TICK as u64)
        as u32
}

/// The name of a new row: the file name without its extension.
pub fn row_name(orig_name: &str) -> String {
    let stem = Path::new(orig_name)
        .file_stem()
        .map(|s| s.to_string_lossy().trim().to_string())
        .unwrap_or_default();
    let name: String = if stem.is_empty() {
        "Audio".into()
    } else {
        stem
    };
    name.chars().take(40).collect()
}

// ---- trimming and fades ----

/// Whether trimming `clip` by `dlen` (the end, or the start when
/// `from_start`) stays inside the sound: the audible part never starts
/// before the sound or runs past its end. `sample_ticks` is the sound's
/// length in ticks, when known; unknown only allows shortening.
pub fn trim_ok(clip: &Clip, dlen: i64, from_start: bool, sample_ticks: Option<u32>) -> bool {
    let len = clip.len as i64 + dlen;
    if len < tl::STEP_TICKS as i64 {
        return false;
    }
    if from_start {
        // The start moves by -dlen and the offset with it.
        let offset = clip.offset as i64 - dlen;
        let start = clip.start as i64 - dlen;
        offset >= 0 && start >= 0
    } else {
        match sample_ticks {
            Some(total) => clip.offset as i64 + len <= total as i64,
            None => dlen <= 0,
        }
    }
}

/// The fade length a handle dragged to `tick_in_clip` (ticks from the
/// clip's start) means: a fade-in is the distance from the start, a
/// fade-out the distance from the end; never longer than the clip.
pub fn fade_from_drag(len: u32, fade_in: bool, tick_in_clip: i64) -> u32 {
    let d = if fade_in {
        tick_in_clip
    } else {
        len as i64 - tick_in_clip
    };
    d.clamp(0, len as i64) as u32
}

/// The two fades after setting one: together they never exceed the clip.
pub fn fades_after(len: u32, fade_in: u32, fade_out: u32, setting_in: bool) -> (u32, u32) {
    if fade_in as u64 + fade_out as u64 <= len as u64 {
        return (fade_in, fade_out);
    }
    if setting_in {
        (fade_in, len - fade_in.min(len))
    } else {
        (len - fade_out.min(len), fade_out)
    }
}

/// The gain edit of a clip in thousandths of a dB.
pub fn gain_mdb(db: f64) -> i32 {
    (db * 1000.0).round().clamp(-100_000.0, 24_000.0) as i32
}

// ---- peaks ----

/// Min and max of every `FRAMES_PER_PEAK` frames (all channels together).
#[derive(Debug, PartialEq)]
pub struct Peaks {
    /// The rate the frames were counted at.
    pub rate: u32,
    pub pairs: Vec<(f32, f32)>,
}

/// Summarises interleaved `data` of `channels` channels.
pub fn compute_peaks(channels: usize, rate: u32, data: &[f32]) -> Peaks {
    let ch = channels.max(1);
    let frames = data.len() / ch;
    let mut pairs = Vec::with_capacity(frames / FRAMES_PER_PEAK + 1);
    let mut f = 0;
    while f < frames {
        let end = (f + FRAMES_PER_PEAK).min(frames);
        let (mut lo, mut hi) = (0.0f32, 0.0f32);
        for v in &data[f * ch..end * ch] {
            lo = lo.min(*v);
            hi = hi.max(*v);
        }
        pairs.push((lo, hi));
        f = end;
    }
    Peaks { rate, pairs }
}

impl Peaks {
    /// Length of the sound in ticks at `bpm`.
    pub fn ticks(&self, bpm: f64) -> u32 {
        let secs = (self.pairs.len() * FRAMES_PER_PEAK) as f64 / self.rate.max(1) as f64;
        secs_to_ticks(secs, bpm).round().min(MAX_TICK as f64) as u32
    }

    /// The loudest swing between sound time `from` and `to` ticks (from
    /// the start of the sound).
    pub fn range(&self, from: f64, to: f64, bpm: f64) -> (f32, f32) {
        let per_tick = self.rate as f64 * 60.0 / (bpm.max(1.0) * PPQ as f64);
        let a = (from.max(0.0) * per_tick / FRAMES_PER_PEAK as f64).floor() as usize;
        let b = ((to.max(from) * per_tick / FRAMES_PER_PEAK as f64).ceil() as usize).max(a + 1);
        let (mut lo, mut hi) = (0.0f32, 0.0f32);
        for p in self.pairs.iter().take(b).skip(a) {
            lo = lo.min(p.0);
            hi = hi.max(p.1);
        }
        (lo, hi)
    }
}

enum Entry {
    Pending,
    Ready(Rc<Peaks>),
}

/// Peak summaries by sample hash, computed off the GTK thread.
#[derive(Default)]
pub struct PeakCache {
    map: Rc<RefCell<HashMap<SampleHash, Entry>>>,
    generation: Rc<Cell<u64>>,
}

impl PeakCache {
    /// Changes whenever a summary arrives (for the timeline's cache key).
    pub fn generation(&self) -> u64 {
        self.generation.get()
    }

    /// The summary of a sample, starting its computation when the decoded
    /// sound is available and nothing has been started yet. `None` until
    /// it is ready.
    pub fn get(&self, app: &Rc<App>, hash: &SampleHash) -> Option<Rc<Peaks>> {
        match self.map.borrow().get(hash) {
            Some(Entry::Ready(p)) => return Some(p.clone()),
            Some(Entry::Pending) => return None,
            None => {}
        }
        let data = app.session.borrow().store.get(&hash.to_hex())?;
        self.map.borrow_mut().insert(*hash, Entry::Pending);
        let (map, generation, h) = (self.map.clone(), self.generation.clone(), *hash);
        let a = app.clone();
        app.tasks.spawn(
            "wave-peaks",
            move || compute_peaks(data.channels as usize, data.rate, &data.data),
            move |p| {
                map.borrow_mut().insert(h, Entry::Ready(Rc::new(p)));
                generation.set(generation.get() + 1);
                a.notify();
            },
        );
        None
    }

    /// The summary of a sample if it is already known.
    pub fn peek(&self, hash: &SampleHash) -> Option<Rc<Peaks>> {
        match self.map.borrow().get(hash) {
            Some(Entry::Ready(p)) => Some(p.clone()),
            _ => None,
        }
    }

    /// Stores a summary directly (tests).
    pub fn put(&self, hash: SampleHash, peaks: Peaks) {
        self.map
            .borrow_mut()
            .insert(hash, Entry::Ready(Rc::new(peaks)));
        self.generation.set(self.generation.get() + 1);
    }
}

// ---- the drop ----

/// A sound read for dropping: where it is and how long it plays.
#[derive(Clone, Debug)]
pub struct Probed {
    pub sample: SampleRef,
    pub secs: f64,
}

/// Imports `items` and measures each, off the GTK thread; `then` runs on it
/// with one result per item. Any format audiofile reads works.
pub fn import_and_probe(
    app: &Rc<App>,
    items: Vec<crate::samples_ui::ImportItem>,
    then: impl FnOnce(Vec<Result<Probed, String>>) + 'static,
) {
    let Some(home) = app.session.borrow().sample_home().map(Path::to_path_buf) else {
        let n = items.len();
        then(vec![
            Err(
                "There is no project folder for samples yet".to_string()
            );
            n
        ]);
        return;
    };
    let any_local = items.iter().any(|i| i.local_only);
    let a = app.clone();
    app.tasks.spawn(
        "import-audio",
        move || {
            items
                .iter()
                .map(|i| {
                    let sample = crate::samples_ui::import_one(&home, i)?;
                    let secs = probe_secs(&i.path)?;
                    Ok(Probed { sample, secs })
                })
                .collect::<Vec<Result<Probed, String>>>()
        },
        move |results| {
            if any_local {
                a.session.borrow_mut().reload_local_samples();
            }
            then(results);
        },
    );
}

/// How long the file plays, in seconds.
pub fn probe_secs(path: &Path) -> Result<f64, String> {
    let a =
        audiofile::decode_file(path).map_err(|e| format!("Cannot use {}: {e}", path.display()))?;
    Ok(a.frames() as f64 / a.rate.max(1) as f64)
}

/// Whether the drop fits: the clip would not overlap another on the row.
pub fn room(clips: &[Clip], channel: ChannelId, start: u32, len: u32) -> bool {
    !clips
        .iter()
        .any(|c| c.instrument == channel && c.start < start + len && start < c.end())
}

/// What a drop made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dropped {
    pub clip: ClipId,
    pub channel: ChannelId,
    /// Every id the edits made, in order (track, row, clip).
    pub created: Vec<u32>,
}

/// Applies a drop as one undo step: registers the sample, makes the row
/// when needed (own mixer track, named after the file) and adds one clip
/// at the drop point as long as the whole sound. Returns the clip.
pub fn add_dropped(app: &Rc<App>, at: DropAt, sample: &SampleRef, secs: f64) -> Option<ClipId> {
    add_dropped_as(app, None, at, sample, secs).map(|d| d.clip)
}

/// As `add_dropped`, by an agent when `author` is given.
pub fn add_dropped_as(
    app: &Rc<App>,
    author: Option<Author>,
    at: DropAt,
    sample: &SampleRef,
    secs: f64,
) -> Option<Dropped> {
    let (bpm, taken, tracks, existing) = {
        let s = app.session.borrow();
        let p = &s.document().project;
        (
            p.tempo_bpm,
            p.channels
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>(),
            p.tracks.len(),
            p.clips.clone(),
        )
    };
    let len = full_len(secs, bpm);
    let hash = SampleHash::parse(&sample.hash)?;
    let tick = match at {
        DropAt::NewRow { tick } | DropAt::Row { tick, .. } => tick,
    };
    if let DropAt::Row { channel, .. } = at
        && !room(&existing, channel, tick, len)
    {
        app.toast("There is no room for the sound there; drop it on an empty spot");
        return None;
    }
    let grouped = match author {
        Some(a) => app.gesture_begin_as(a, "Add audio"),
        None => app.gesture_begin("Add audio"),
    };
    let in_gesture = grouped || app.session.borrow().editor.gesture_open();
    let mut created: Vec<u32> = Vec::new();
    let mut run = |e: Vec<Edit>| {
        let r = if in_gesture {
            app.gesture_edit(e)
        } else {
            app.edit(e)
        };
        if let Some(a) = &r {
            created.extend(a.created.iter().copied());
        }
        r
    };
    let finish = |ok: bool| {
        if grouped {
            app.gesture_end();
        }
        ok
    };
    if run(vec![Edit::AddSample {
        sample: sample.clone(),
    }])
    .is_none()
    {
        finish(false);
        return None;
    }
    let channel = match at {
        DropAt::Row { channel, .. } => channel,
        DropAt::NewRow { .. } => {
            let name = {
                let t: Vec<&str> = taken.iter().map(String::as_str).collect();
                doc::presets::unique_name(&row_name(&sample.orig_name), &t)
            };
            let mut track = app.ui.borrow().track;
            if tracks <= MAX_TRACKS
                && let Some(a) = run(vec![Edit::AddTrack { name: name.clone() }])
            {
                track = TrackId(a.created[0]);
            }
            let made = run(vec![Edit::AddChannel {
                name,
                instrument: NewInstrument::Audio,
                root_key: 60,
                track,
            }]);
            match made {
                Some(a) => ChannelId(a.created[0]),
                None => {
                    finish(false);
                    return None;
                }
            }
        }
    };
    let made = run(vec![Edit::AddAudioClip {
        instrument: channel,
        sample: hash,
        start: tick,
        len,
        offset: 0,
    }]);
    let clip = made.and_then(|a| a.created.last().copied()).map(ClipId);
    finish(clip.is_some());
    app.select_channel(channel);
    clip.map(|clip| Dropped {
        clip,
        channel,
        created,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_adapter::EngineLink;
    use crate::registry::Registry;
    use crate::session::Session;
    use doc::document::Document;
    use doc::persist::Dirs;
    use protocol::ids::PatternId;
    use protocol::model::Instrument;
    use std::cell::RefCell;

    fn app() -> Rc<App> {
        let dir = std::env::temp_dir().join(format!("ldaw-audioclips-{}", std::process::id()));
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

    fn sample() -> SampleRef {
        SampleRef {
            hash: "ab".repeat(32),
            orig_name: "Final Countdown.mp3".into(),
            size: 10,
            local_only: false,
        }
    }

    fn audio_clip(start: u32, len: u32, offset: u32) -> Clip {
        Clip {
            id: ClipId(1),
            instrument: ChannelId(1),
            pattern: PatternId::NONE,
            start,
            len,
            offset,
            muted: false,
            audio: None,
            group: None,
        }
    }

    #[test]
    fn a_drop_on_empty_space_is_one_row_one_full_length_clip_and_one_undo() {
        let a = app();
        let id = add_dropped(&a, DropAt::NewRow { tick: 7680 }, &sample(), 2.0).expect("dropped");
        {
            let s = a.session.borrow();
            let p = &s.document().project;
            assert_eq!(p.channels.len(), 1);
            let ch = &p.channels[0];
            assert!(matches!(ch.instrument, Instrument::Audio));
            assert_eq!(ch.name, "Final Countdown");
            assert_ne!(ch.track, TrackId::MASTER, "its own mixer track");
            assert_eq!(p.clips.len(), 1, "no pattern is asked for");
            let c = &p.clips[0];
            assert_eq!(c.id, id);
            // 2 s at 120 bpm is 4 beats.
            assert_eq!((c.start, c.len, c.offset), (7680, 4 * PPQ, 0));
            assert!(c.audio.is_some());
        }
        a.undo();
        let s = a.session.borrow();
        let p = &s.document().project;
        assert!(p.channels.is_empty() && p.clips.is_empty(), "one undo step");
        assert!(p.samples.is_empty());
    }

    #[test]
    fn a_drop_on_an_audio_row_adds_the_clip_there_and_never_overlaps() {
        let a = app();
        add_dropped(&a, DropAt::NewRow { tick: 0 }, &sample(), 1.0).unwrap();
        let row = a.session.borrow().document().project.channels[0].id;
        let second = add_dropped(
            &a,
            DropAt::Row {
                channel: row,
                tick: 2 * PPQ,
            },
            &sample(),
            1.0,
        );
        assert!(second.is_some());
        let n = a.session.borrow().document().project.clips.len();
        assert_eq!(n, 2);
        // The first clip covers 0..2*PPQ, the second 2*PPQ..4*PPQ: no room at PPQ.
        assert!(
            add_dropped(
                &a,
                DropAt::Row {
                    channel: row,
                    tick: PPQ
                },
                &sample(),
                1.0
            )
            .is_none()
        );
        assert_eq!(a.session.borrow().document().project.clips.len(), 2);
    }

    #[test]
    fn lengths_follow_the_tempo() {
        assert_eq!(full_len(2.0, 120.0), 4 * PPQ);
        assert_eq!(full_len(2.0, 60.0), 2 * PPQ);
        assert_eq!(full_len(0.0, 120.0), tl::STEP_TICKS);
        assert!((ticks_to_secs(4.0 * PPQ as f64, 120.0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn rows_are_named_after_the_file() {
        assert_eq!(row_name("Final Countdown.mp3"), "Final Countdown");
        assert_eq!(row_name("a.b.wav"), "a.b");
        assert_eq!(row_name(""), "Audio");
    }

    #[test]
    fn trimming_stays_inside_the_sound() {
        let c = audio_clip(1000, 2000, 500);
        let total = Some(4000);
        // The end can grow until the sound ends (offset 500 + len 3500 = 4000).
        assert!(trim_ok(&c, 1500, false, total));
        assert!(!trim_ok(&c, 1501, false, total));
        assert!(trim_ok(&c, -1000, false, total));
        // Unknown length: only shorter.
        assert!(!trim_ok(&c, 1, false, None));
        assert!(trim_ok(&c, -1, false, None));
        // The start can grow back to the start of the sound, not past it.
        assert!(trim_ok(&c, 500, true, total));
        assert!(!trim_ok(&c, 501, true, total));
        // And shrink, but not to nothing.
        assert!(trim_ok(&c, -1000, true, total));
        // The start never moves before the timeline's start.
        assert!(!trim_ok(&audio_clip(100, 2000, 500), 200, true, total));
    }

    #[test]
    fn fade_handles_measure_from_their_own_edge() {
        assert_eq!(fade_from_drag(1000, true, 300), 300);
        assert_eq!(fade_from_drag(1000, false, 700), 300);
        assert_eq!(fade_from_drag(1000, true, -50), 0);
        assert_eq!(fade_from_drag(1000, true, 5000), 1000);
        assert_eq!(fade_from_drag(1000, false, 1200), 0);
        // Fades share the clip: setting one shortens the other.
        assert_eq!(fades_after(1000, 700, 600, true), (700, 300));
        assert_eq!(fades_after(1000, 700, 600, false), (400, 600));
        assert_eq!(fades_after(1000, 100, 100, true), (100, 100));
        assert_eq!(gain_mdb(-6.0), -6000);
        assert_eq!(gain_mdb(500.0), 24_000);
    }

    #[test]
    fn peaks_hold_one_pair_per_256_frames() {
        // Stereo, 1000 frames: left rises, right is silent.
        let mut data = Vec::new();
        for f in 0..1000 {
            data.push(f as f32 / 1000.0);
            data.push(0.0);
        }
        let p = compute_peaks(2, 48000, &data);
        assert_eq!(p.pairs.len(), 4);
        assert_eq!(p.pairs[0].0, 0.0);
        assert!((p.pairs[0].1 - 255.0 / 1000.0).abs() < 1e-6);
        assert!((p.pairs[3].1 - 999.0 / 1000.0).abs() < 1e-6);
        // At 120 bpm 48 kHz a tick is 25 frames: ticks 0..11 are 275 frames: pairs 0 and 1.
        let (_, hi) = p.range(0.0, 11.0, 120.0);
        assert!(hi > p.pairs[0].1 && (hi - p.pairs[1].1).abs() < 1e-6);
        assert_eq!(
            p.ticks(120.0),
            (1024.0f64 / 48000.0 * 2.0 * 960.0).round() as u32
        );
    }

    /// A mono 16-bit WAV of `secs` seconds of a low tone at 8 kHz.
    fn wav(secs: u32) -> Vec<u8> {
        let frames = 8000 * secs;
        let mut b = Vec::new();
        b.extend(b"RIFF");
        b.extend((36 + frames * 2).to_le_bytes());
        b.extend(b"WAVEfmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(8000u32.to_le_bytes());
        b.extend(16000u32.to_le_bytes());
        b.extend(2u16.to_le_bytes());
        b.extend(16u16.to_le_bytes());
        b.extend(b"data");
        b.extend((frames * 2).to_le_bytes());
        for f in 0..frames {
            let v = ((f as f32 * 0.05).sin() * 8000.0) as i16;
            b.extend(v.to_le_bytes());
        }
        b
    }

    #[test]
    fn a_dropped_file_is_imported_measured_and_placed_whole() {
        let dir = std::env::temp_dir().join(format!("ldaw-drop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("Final Countdown.wav");
        std::fs::write(&file, wav(3)).unwrap();
        let a = app();
        a.session
            .borrow_mut()
            .set_sample_home(Some(dir.join("bundle")));
        type Got = Rc<RefCell<Option<Vec<Result<Probed, String>>>>>;
        let got: Got = Rc::default();
        let g = got.clone();
        import_and_probe(
            &a,
            vec![crate::samples_ui::ImportItem::file(file)],
            move |r| *g.borrow_mut() = Some(r),
        );
        for _ in 0..500 {
            a.tasks.poll();
            if got.borrow().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let results = got.borrow_mut().take().expect("imported");
        let probed = results.into_iter().next().unwrap().expect("a good file");
        assert!((probed.secs - 3.0).abs() < 1e-6, "{}", probed.secs);
        add_dropped(&a, DropAt::NewRow { tick: 0 }, &probed.sample, probed.secs).unwrap();
        let s = a.session.borrow();
        let p = &s.document().project;
        // Three seconds at 120 beats a minute: six beats.
        assert_eq!(p.clips.len(), 1);
        assert_eq!(p.clips[0].len, 6 * PPQ);
        assert_eq!(p.channels[0].name, "Final Countdown");
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_drop_with_a_bad_hash_does_nothing() {
        let a = app();
        let mut s = sample();
        s.hash = "zz".into();
        assert!(add_dropped(&a, DropAt::NewRow { tick: 0 }, &s, 1.0).is_none());
        assert!(a.session.borrow().document().project.channels.is_empty());
    }
}
