// SPDX-License-Identifier: GPL-3.0-or-later
//! Position text and tap tempo (docs/ui-design.md 5), without GTK.

use protocol::consts::PPQ;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PositionFormat {
    /// `012:3:2`: bar, beat, sixteenth, all 1-based.
    #[default]
    Bars,
    /// `01:23.4`: minutes, seconds, tenths.
    Time,
}

impl PositionFormat {
    pub fn toggle(self) -> PositionFormat {
        match self {
            PositionFormat::Bars => PositionFormat::Time,
            PositionFormat::Time => PositionFormat::Bars,
        }
    }
}

/// The text of the position readout.
pub fn format_position(tick: u64, beats_per_bar: u8, bpm: f64, fmt: PositionFormat) -> String {
    match fmt {
        PositionFormat::Bars => {
            let ppq = PPQ as u64;
            let beats = tick / ppq;
            let bar = beats / beats_per_bar.max(1) as u64 + 1;
            let beat = beats % beats_per_bar.max(1) as u64 + 1;
            let step = (tick % ppq) / (ppq / 4) + 1;
            format!("{bar:03}:{beat}:{step}")
        }
        PositionFormat::Time => {
            let secs = tick as f64 / PPQ as f64 * 60.0 / bpm.max(1.0);
            let tenths = (secs * 10.0 + 1e-6).floor() as u64;
            let (m, s, t) = (tenths / 600, (tenths / 10) % 60, tenths % 10);
            format!("{m:02}:{s:02}.{t}")
        }
    }
}

/// The accessible value text: "Bar 12, beat 3, sixteenth 2".
pub fn spoken_position(tick: u64, beats_per_bar: u8) -> String {
    let t = format_position(tick, beats_per_bar, 120.0, PositionFormat::Bars);
    let mut it = t.split(':').map(|p| p.trim_start_matches('0'));
    let bar = it.next().unwrap_or("1");
    let beat = it.next().unwrap_or("1");
    let step = it.next().unwrap_or("1");
    format!(
        "Bar {}, beat {beat}, sixteenth {step}",
        if bar.is_empty() { "0" } else { bar }
    )
}

/// Tempo from the last taps (milliseconds, any origin). Taps more than two
/// seconds apart start a new count.
#[derive(Default, Debug)]
pub struct TapTempo {
    taps: Vec<f64>,
}

impl TapTempo {
    pub const KEEP: usize = 4;
    pub const RESET_MS: f64 = 2000.0;

    /// Records a tap. Returns the tempo once there are two taps.
    pub fn tap(&mut self, now_ms: f64) -> Option<f64> {
        if let Some(&last) = self.taps.last()
            && now_ms - last > Self::RESET_MS
        {
            self.taps.clear();
        }
        self.taps.push(now_ms);
        if self.taps.len() > Self::KEEP {
            self.taps.remove(0);
        }
        if self.taps.len() < 2 {
            return None;
        }
        let span = self.taps[self.taps.len() - 1] - self.taps[0];
        let avg = span / (self.taps.len() - 1) as f64;
        if avg <= 0.0 {
            return None;
        }
        Some(60_000.0 / avg)
    }
}

/// Parses what the user typed in the tempo box. `None` if it is not a
/// number in the allowed range (the box then reverts and shows a toast).
pub fn parse_tempo(text: &str, min: f64, max: f64) -> Option<f64> {
    let t = text.trim();
    let t = t.strip_suffix("BPM").unwrap_or(t).trim();
    let v: f64 = t.parse().ok()?;
    (v.is_finite() && (min..=max).contains(&v)).then_some(v)
}

/// Keeps the Play button's state in step with the engine. The engine's flag
/// lags a command by up to one audio block, so a press must not be undone by
/// the stale flag: the engine wins when it changes by itself (end of song, a
/// stop from an agent), or when it still disagrees after a short wait.
#[derive(Debug, Default)]
pub struct PlayingSync {
    last: bool,
    since: Option<std::time::Instant>,
}

impl PlayingSync {
    /// How long a disagreement may last before the engine's state is taken.
    pub const SETTLE: std::time::Duration = std::time::Duration::from_millis(400);

    /// Returns the state the button should take, if it must change.
    pub fn step(&mut self, shown: bool, engine: bool, now: std::time::Instant) -> Option<bool> {
        if engine != self.last {
            self.last = engine;
            self.since = None;
            return (shown != engine).then_some(engine);
        }
        if shown == engine {
            self.since = None;
            return None;
        }
        let since = *self.since.get_or_insert(now);
        if now.duration_since(since) >= Self::SETTLE {
            self.since = None;
            return Some(engine);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_are_one_based() {
        let f = |t| format_position(t, 4, 120.0, PositionFormat::Bars);
        assert_eq!(f(0), "001:1:1");
        assert_eq!(f(240), "001:1:2");
        assert_eq!(f(960), "001:2:1");
        assert_eq!(f(960 * 4), "002:1:1");
        // Bar 12, beat 3, step 2.
        assert_eq!(f(11 * 3840 + 2 * 960 + 240), "012:3:2");
    }

    #[test]
    fn other_time_signatures() {
        assert_eq!(
            format_position(960 * 3, 3, 120.0, PositionFormat::Bars),
            "002:1:1"
        );
    }

    #[test]
    fn time_format() {
        let f = |t, bpm| format_position(t, 4, bpm, PositionFormat::Time);
        assert_eq!(f(0, 120.0), "00:00.0");
        // One beat at 120 BPM is half a second.
        assert_eq!(f(960, 120.0), "00:00.5");
        // 123.4 s at 60 BPM is 123.4 beats.
        assert_eq!(f(960 * 123 + 384, 60.0), "02:03.4");
    }

    #[test]
    fn spoken() {
        assert_eq!(
            spoken_position(11 * 3840 + 2 * 960 + 240, 4),
            "Bar 12, beat 3, sixteenth 2"
        );
        assert_eq!(spoken_position(0, 4), "Bar 1, beat 1, sixteenth 1");
    }

    #[test]
    fn toggling_format() {
        assert_eq!(PositionFormat::Bars.toggle(), PositionFormat::Time);
        assert_eq!(PositionFormat::Time.toggle(), PositionFormat::Bars);
    }

    #[test]
    fn tap_tempo_averages_the_last_taps() {
        let mut t = TapTempo::default();
        assert_eq!(t.tap(0.0), None);
        assert!((t.tap(500.0).unwrap() - 120.0).abs() < 1e-9);
        assert!((t.tap(1000.0).unwrap() - 120.0).abs() < 1e-9);
        // Slower taps pull the average down; only the last 4 count.
        let v = t.tap(1600.0).unwrap();
        assert!((v - 60_000.0 / (1600.0 / 3.0)).abs() < 1e-9);
        let v = t.tap(2200.0).unwrap();
        assert!((v - 60_000.0 / ((2200.0 - 500.0) / 3.0)).abs() < 1e-9);
    }

    #[test]
    fn a_long_pause_starts_over() {
        let mut t = TapTempo::default();
        t.tap(0.0);
        t.tap(500.0);
        assert_eq!(t.tap(10_000.0), None);
        assert!((t.tap(10_400.0).unwrap() - 150.0).abs() < 1e-9);
    }

    #[test]
    fn tempo_text_is_validated() {
        assert_eq!(parse_tempo(" 140 ", 20.0, 300.0), Some(140.0));
        assert_eq!(parse_tempo("140.5", 20.0, 300.0), Some(140.5));
        assert_eq!(parse_tempo("19", 20.0, 300.0), None);
        assert_eq!(parse_tempo("301", 20.0, 300.0), None);
        assert_eq!(parse_tempo("fast", 20.0, 300.0), None);
        assert_eq!(parse_tempo("NaN", 20.0, 300.0), None);
    }
}

#[cfg(test)]
mod playing_sync_tests {
    use super::PlayingSync;
    use std::time::{Duration, Instant};

    #[test]
    fn a_stale_engine_flag_does_not_undo_a_stop() {
        let t = Instant::now();
        let mut s = PlayingSync::default();
        // Playing, confirmed by the engine.
        assert_eq!(s.step(true, true, t), None);
        // The user presses Stop; the engine flag is still true for a block.
        assert_eq!(s.step(false, true, t + Duration::from_millis(10)), None);
        assert_eq!(s.step(false, true, t + Duration::from_millis(20)), None);
        // The engine catches up.
        assert_eq!(s.step(false, false, t + Duration::from_millis(30)), None);
    }

    #[test]
    fn the_engine_ending_the_song_by_itself_updates_the_button() {
        let t = Instant::now();
        let mut s = PlayingSync::default();
        assert_eq!(s.step(true, true, t), None);
        assert_eq!(
            s.step(true, false, t + Duration::from_millis(10)),
            Some(false)
        );
    }

    #[test]
    fn a_lasting_disagreement_follows_the_engine() {
        let t = Instant::now();
        let mut s = PlayingSync::default();
        // Play was pressed but the engine never started.
        assert_eq!(s.step(true, false, t), None);
        assert_eq!(
            s.step(
                true,
                false,
                t + PlayingSync::SETTLE - Duration::from_millis(1)
            ),
            None
        );
        assert_eq!(
            s.step(
                true,
                false,
                t + PlayingSync::SETTLE + Duration::from_millis(1)
            ),
            Some(false)
        );
    }
}
