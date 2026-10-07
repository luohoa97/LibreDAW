// SPDX-License-Identifier: GPL-3.0-or-later
//! The Hum flow without any widget (SPEC 21.3): the steps from the button to
//! the notes, and where the notes go. `hum.rs` shows it.

use protocol::consts::{DEFAULT_STEP_TICKS, MAX_STEPS, PPQ};
use protocol::edit::NewNote;

/// One note heard in the hum, in seconds from the start of the recording.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HumNote {
    pub start_s: f64,
    pub end_s: f64,
    pub key: u8,
    /// 0 to 1.
    pub volume: f32,
}

/// Longest melody put on the timeline, in bars.
pub const MAX_BARS: u32 = 16;

// ---- placing notes ---------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaceOpts {
    pub bpm: f64,
    pub bar_ticks: u32,
    /// Where the first clip starts, a bar line.
    pub start_tick: u32,
    /// "Keep My Timing": no snapping to the grid.
    pub keep_timing: bool,
}

/// A clip with its notes, as the timeline stores them. Note positions are
/// from the clip's start.
#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    pub start: u32,
    pub len: u32,
    pub steps: u8,
    pub notes: Vec<NewNote>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub chunks: Vec<Chunk>,
    /// Notes left out: past the longest melody, or on top of another.
    pub dropped: usize,
}

impl Placement {
    pub fn note_count(&self) -> usize {
        self.chunks.iter().map(|c| c.notes.len()).sum()
    }
}

fn to_ticks(seconds: f64, bpm: f64) -> i64 {
    (seconds * bpm / 60.0 * PPQ as f64).round() as i64
}

fn snap(v: i64, grid: i64) -> i64 {
    (v + grid / 2).div_euclid(grid) * grid
}

fn velocity(volume: f32) -> u8 {
    (40.0 + 87.0 * volume.clamp(0.0, 1.0)).round() as u8
}

/// Turns heard notes into clips of notes. Snapped to sixteenths unless the
/// user keeps their own timing. A clip holds at most `MAX_STEPS` grid cells,
/// so a long melody becomes several clips side by side.
pub fn place(heard: &[HumNote], o: &PlaceOpts) -> Placement {
    let grid = DEFAULT_STEP_TICKS as i64;
    let mut raw: Vec<(i64, i64, u8, u8)> = Vec::new();
    let mut dropped = 0;
    for n in heard {
        if n.start_s < 0.0 || n.end_s <= n.start_s {
            dropped += 1;
            continue;
        }
        let s = to_ticks(n.start_s, o.bpm);
        let l = to_ticks(n.end_s, o.bpm) - s;
        let (s, l) = if o.keep_timing {
            (s, l.max(60))
        } else {
            (snap(s, grid), snap(l, grid).max(grid))
        };
        raw.push((s, l, n.key.min(127), velocity(n.volume)));
    }
    raw.sort_by_key(|n| (n.0, std::cmp::Reverse(n.1)));
    // One voice: a note that lands where another starts is dropped; one that
    // would run into the next is cut short.
    let mut voice: Vec<(i64, i64, u8, u8)> = Vec::new();
    for n in raw {
        if let Some(last) = voice.last_mut() {
            if last.0 == n.0 {
                dropped += 1;
                continue;
            }
            if last.0 + last.1 > n.0 {
                last.1 = n.0 - last.0;
            }
        }
        voice.push(n);
    }
    let bar = o.bar_ticks.max(1) as i64;
    let cap = MAX_BARS as i64 * bar;
    let before = voice.len();
    voice.retain(|n| n.0 < cap);
    dropped += before - voice.len();
    let end = voice.iter().map(|n| n.0 + n.1).max().unwrap_or(0).min(cap);
    let bars = ((end + bar - 1) / bar).max(1);
    let steps_per_bar = (bar / grid).max(1);
    let per_clip = (MAX_STEPS as i64 / steps_per_bar).max(1);
    let mut chunks = Vec::new();
    let mut first_bar = 0;
    while first_bar < bars {
        let n_bars = per_clip.min(bars - first_bar);
        let (from, to) = (first_bar * bar, (first_bar + n_bars) * bar);
        let notes = voice
            .iter()
            .filter(|n| n.0 >= from && n.0 < to)
            .map(|n| NewNote {
                start: (n.0 - from) as u32,
                len: n.1.min(to - n.0).max(1) as u32,
                key: n.2,
                vel: n.3,
            })
            .collect();
        chunks.push(Chunk {
            start: o.start_tick + from as u32,
            len: (n_bars * bar) as u32,
            steps: (n_bars * steps_per_bar) as u8,
            notes,
        });
        first_bar += n_bars;
    }
    Placement { chunks, dropped }
}

/// The first bar line at or after `from` where `len` ticks fit between the
/// clips `taken` (start, length) of one row. `None` if there is none in a
/// reasonable distance.
pub fn free_start(taken: &[(u32, u32)], from: u32, len: u32, bar: u32) -> Option<u32> {
    let bar = bar.max(1);
    let mut start = from - from % bar;
    for _ in 0..512 {
        let clash = taken
            .iter()
            .find(|(s, l)| *s < start + len && start < s + l);
        match clash {
            None => return Some(start),
            Some((s, l)) => {
                let end = s + l;
                start = end.div_ceil(bar) * bar;
            }
        }
    }
    None
}

// ---- the flow --------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The sheet is open; the microphone is off.
    Ready,
    /// Clicks. `beat` clicks have sounded.
    CountIn { beat: u8 },
    /// The microphone is on.
    Recording,
    /// The hum is turning into notes.
    Working,
    /// Notes are on the timeline.
    Done,
    /// The user closed the sheet.
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    /// The user pressed Start.
    Start,
    /// One beat passed.
    Beat,
    /// The user pressed Stop, or Space.
    Stop,
    /// The user closed the sheet.
    Cancel,
    /// Transcription ended: notes found, or what went wrong.
    Heard(Result<usize, String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Click {
        accent: bool,
    },
    OpenMic,
    CloseMic,
    Transcribe,
    Place,
    /// Something to tell the user, with the sheet still open.
    Problem(String),
    /// The sheet is over; the user said no.
    Decline,
    Close,
}

#[derive(Clone, Copy, Debug)]
pub struct Flow {
    pub phase: Phase,
    /// Clicks in the count-in: one bar.
    beats: u8,
}

impl Flow {
    pub fn new(beats_per_bar: u8) -> Flow {
        Flow {
            phase: Phase::Ready,
            beats: beats_per_bar.max(1),
        }
    }

    pub fn step(&mut self, input: Input) -> Vec<Effect> {
        use Effect::*;
        use Phase::*;
        match (self.phase, input) {
            (Ready, Input::Start) => {
                self.phase = CountIn { beat: 1 };
                vec![Click { accent: true }]
            }
            (CountIn { beat }, Input::Beat) => {
                if beat < self.beats {
                    self.phase = CountIn { beat: beat + 1 };
                    vec![Click { accent: false }]
                } else {
                    self.phase = Recording;
                    vec![OpenMic]
                }
            }
            (CountIn { .. }, Input::Stop | Input::Cancel) | (Ready, Input::Cancel) => {
                self.phase = Closed;
                vec![Decline, Close]
            }
            (Recording, Input::Stop) => {
                self.phase = Working;
                vec![CloseMic, Transcribe]
            }
            (Recording, Input::Cancel) => {
                self.phase = Closed;
                vec![CloseMic, Decline, Close]
            }
            (Working, Input::Heard(Ok(n))) if n > 0 => {
                self.phase = Done;
                vec![Place, Close]
            }
            (Working, Input::Heard(Ok(_))) => {
                self.phase = Ready;
                vec![Problem(
                    "I could not hear a melody. Hum a little louder and try again.".into(),
                )]
            }
            (Working, Input::Heard(Err(e))) => {
                self.phase = Ready;
                vec![Problem(e)]
            }
            (Working, Input::Cancel) => {
                self.phase = Closed;
                vec![Decline, Close]
            }
            _ => Vec::new(),
        }
    }

    /// The microphone must be open only in this phase.
    pub fn mic_open(&self) -> bool {
        self.phase == Phase::Recording
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heard(start: f64, end: f64, key: u8) -> HumNote {
        HumNote {
            start_s: start,
            end_s: end,
            key,
            volume: 0.5,
        }
    }

    fn opts(keep: bool) -> PlaceOpts {
        PlaceOpts {
            bpm: 120.0,
            bar_ticks: 3840,
            start_tick: 7680,
            keep_timing: keep,
        }
    }

    #[test]
    fn snaps_to_sixteenths_by_default() {
        // At 120 bpm a second is two beats: 1920 ticks.
        let p = place(&[heard(0.03, 0.47, 60), heard(0.52, 1.0, 62)], &opts(false));
        assert_eq!(p.chunks.len(), 1);
        let c = &p.chunks[0];
        assert_eq!((c.start, c.len, c.steps), (7680, 3840, 16));
        let n = &c.notes;
        assert_eq!((n[0].start, n[0].len), (0, 960));
        assert_eq!((n[1].start, n[1].len), (960, 960));
        assert_eq!(n[0].key, 60);
    }

    #[test]
    fn keep_timing_keeps_the_ticks() {
        let p = place(&[heard(0.03, 0.47, 60)], &opts(true));
        let n = &p.chunks[0].notes[0];
        assert_eq!((n.start, n.len), (58, 844));
    }

    #[test]
    fn a_long_melody_becomes_clips_of_at_most_four_bars() {
        // 5.5 bars at 120 bpm 4/4: 11 seconds.
        let p = place(&[heard(0.0, 1.0, 60), heard(10.0, 11.0, 64)], &opts(false));
        assert_eq!(p.chunks.len(), 2);
        assert_eq!((p.chunks[0].len, p.chunks[0].steps), (15360, 64));
        assert_eq!(p.chunks[1].start, 7680 + 15360);
        assert_eq!(p.chunks[1].len, 7680);
        // 10 s is bar 5 (index 5 of 0): 19200 ticks, 3840 into the second clip.
        assert_eq!(p.chunks[1].notes[0].start, 19200 - 15360);
    }

    #[test]
    fn one_voice_and_nothing_negative() {
        let p = place(
            &[
                heard(0.0, 1.0, 60),
                heard(0.5, 1.5, 62),
                heard(0.51, 0.9, 64),
                heard(-1.0, 0.2, 65),
            ],
            &opts(false),
        );
        let n = &p.chunks[0].notes;
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].len, 960);
        assert_eq!(p.dropped, 2);
    }

    #[test]
    fn past_sixteen_bars_is_dropped() {
        let p = place(
            &[heard(0.0, 1.0, 60), heard(200.0, 201.0, 61)],
            &opts(false),
        );
        assert_eq!(p.note_count(), 1);
        assert_eq!(p.dropped, 1);
    }

    #[test]
    fn starts_at_the_first_free_bar() {
        let taken = [(0, 3840), (7680, 3840)];
        assert_eq!(free_start(&taken, 100, 3840, 3840), Some(3840));
        assert_eq!(free_start(&taken, 0, 7680, 3840), Some(11520));
        assert_eq!(free_start(&[], 5000, 3840, 3840), Some(3840));
    }

    #[test]
    fn flow_counts_in_then_records_then_places() {
        let mut f = Flow::new(4);
        assert!(!f.mic_open());
        assert_eq!(f.step(Input::Start), vec![Effect::Click { accent: true }]);
        for _ in 0..3 {
            assert_eq!(f.step(Input::Beat), vec![Effect::Click { accent: false }]);
            assert!(!f.mic_open());
        }
        // The fourth beat after the first click ends the bar: the mic opens.
        assert_eq!(f.step(Input::Beat), vec![Effect::OpenMic]);
        assert!(f.mic_open());
        assert_eq!(
            f.step(Input::Stop),
            vec![Effect::CloseMic, Effect::Transcribe]
        );
        assert!(!f.mic_open());
        assert_eq!(
            f.step(Input::Heard(Ok(6))),
            vec![Effect::Place, Effect::Close]
        );
        assert_eq!(f.phase, Phase::Done);
        assert!(f.step(Input::Start).is_empty());
    }

    #[test]
    fn the_mic_never_opens_if_the_user_cancels_the_count_in() {
        let mut f = Flow::new(4);
        f.step(Input::Start);
        f.step(Input::Beat);
        assert_eq!(f.step(Input::Cancel), vec![Effect::Decline, Effect::Close]);
        assert!(f.step(Input::Beat).is_empty());
        assert!(!f.mic_open());
    }

    #[test]
    fn closing_while_recording_shuts_the_mic_and_declines() {
        let mut f = Flow::new(1);
        f.step(Input::Start);
        f.step(Input::Beat);
        assert!(f.mic_open());
        assert_eq!(
            f.step(Input::Cancel),
            vec![Effect::CloseMic, Effect::Decline, Effect::Close]
        );
    }

    #[test]
    fn nothing_heard_lets_the_user_try_again() {
        let mut f = Flow::new(1);
        f.step(Input::Start);
        f.step(Input::Beat);
        f.step(Input::Stop);
        let e = f.step(Input::Heard(Ok(0)));
        assert!(matches!(e.as_slice(), [Effect::Problem(_)]));
        assert_eq!(f.phase, Phase::Ready);
        assert_eq!(f.step(Input::Start), vec![Effect::Click { accent: true }]);
    }
}
