// SPDX-License-Identifier: GPL-3.0-or-later
//! The Hum flow's shared pieces (SPEC 21.3, 21.5).
//!
//! `hum_prepare` has no request of its own in the protocol yet, so it rides
//! on two existing ones, both plain data:
//! - `SetActivity` whose text starts with a private marker (`encode`) opens
//!   the Hum sheet with the agent's message. The `activity_set` tool strips
//!   control characters, so an agent cannot forge the marker through it.
//! - `JobStatus` and `JobResult` of the reserved job `JOB` report the sheet:
//!   running while it waits, done with the new clip (`Applied::created`
//!   ends with the clip id), cancelled if the user declined.
//!
//! Proposed protocol diff, to replace this once the owner allows it:
//! `RequestBody::HumPrepare { instrument: Option<ChannelId>, bars: Option<u32>,
//! message: Option<String> }` and `ReplyBody::Hum { clip: ClipId }`.

/// The job id the Hum sheet answers to.
pub const JOB: u64 = u64::MAX - 7;

const MARK: &str = "\u{1}hum\u{1}";

/// What an agent asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepare {
    /// Instrument to put the notes on; `None`: the selected one, or a new
    /// "Hum Melody" instrument.
    pub instrument: Option<u32>,
    /// Bars the user is asked to fill (the sheet shows it as a hint).
    pub bars: Option<u32>,
    /// Shown in the sheet, for example "Hum the chorus melody".
    pub message: String,
}

pub fn encode(p: &Prepare) -> String {
    let message: String = p.message.chars().filter(|c| !c.is_control()).collect();
    format!(
        "{MARK}{};{};{message}",
        p.instrument.unwrap_or(0),
        p.bars.unwrap_or(0)
    )
}

pub fn decode(text: &str) -> Option<Prepare> {
    let rest = text.strip_prefix(MARK)?;
    let mut it = rest.splitn(3, ';');
    let instrument: u32 = it.next()?.parse().ok()?;
    let bars: u32 = it.next()?.parse().ok()?;
    let message = it.next()?.to_string();
    Some(Prepare {
        instrument: (instrument != 0).then_some(instrument),
        bars: (bars != 0).then_some(bars),
        message,
    })
}

/// A key: tonic as a pitch class (0 is C) and major or minor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub tonic: u8,
    pub minor: bool,
}

impl Key {
    /// "A minor", "F# major".
    pub fn name(&self) -> String {
        const NAMES: [&str; 12] = [
            "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
        ];
        format!(
            "{} {}",
            NAMES[(self.tonic % 12) as usize],
            if self.minor { "minor" } else { "major" }
        )
    }
}

const MAJOR: [f32; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR: [f32; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

fn correlation(a: &[f32; 12], b: &[f32; 12], shift: usize) -> f32 {
    let ma = a.iter().sum::<f32>() / 12.0;
    let mb = b.iter().sum::<f32>() / 12.0;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let x = a[(i + shift) % 12] - ma;
        let y = b[i] - mb;
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da == 0.0 || db == 0.0 {
        0.0
    } else {
        num / (da.sqrt() * db.sqrt())
    }
}

/// The best fitting key for `(midi key, weight)` pairs (Krumhansl-Schmuckler;
/// the weight is usually the note's length). `None` for fewer than three
/// notes or one pitch class only.
pub fn detect_key(notes: &[(u8, f32)]) -> Option<Key> {
    if notes.len() < 3 {
        return None;
    }
    let mut hist = [0.0f32; 12];
    for (k, w) in notes {
        hist[(*k % 12) as usize] += w.max(0.0);
    }
    if hist.iter().filter(|v| **v > 0.0).count() < 2 {
        return None;
    }
    let mut best: Option<(f32, Key)> = None;
    for tonic in 0..12u8 {
        for (minor, profile) in [(false, &MAJOR), (true, &MINOR)] {
            let c = correlation(&hist, profile, tonic as usize);
            if best.is_none_or(|(b, _)| c > b) {
                best = Some((c, Key { tonic, minor }));
            }
        }
    }
    best.map(|(_, k)| k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_round_trips() {
        let p = Prepare {
            instrument: Some(12),
            bars: Some(4),
            message: "Hum the chorus; slowly".into(),
        };
        assert_eq!(decode(&encode(&p)), Some(p));
        let q = Prepare {
            instrument: None,
            bars: None,
            message: String::new(),
        };
        assert_eq!(decode(&encode(&q)), Some(q));
        assert_eq!(decode("Hum the chorus"), None);
    }

    #[test]
    fn controls_cannot_end_the_message() {
        let p = Prepare {
            instrument: None,
            bars: None,
            message: "a\u{1}b\nc".into(),
        };
        assert_eq!(decode(&encode(&p)).unwrap().message, "abc");
    }

    #[test]
    fn finds_a_minor_and_c_major() {
        // A minor: A B C D E F G A, tonic and fifth held longer.
        let a_minor: Vec<(u8, f32)> = [
            (57, 2.0),
            (59, 1.0),
            (60, 1.0),
            (62, 1.0),
            (64, 1.5),
            (65, 0.5),
            (67, 0.5),
            (57, 2.0),
        ]
        .into();
        assert_eq!(detect_key(&a_minor).unwrap().name(), "A minor");
        let c_major: Vec<(u8, f32)> = [
            (60, 2.0),
            (62, 1.0),
            (64, 1.0),
            (65, 1.0),
            (67, 1.5),
            (69, 1.0),
            (71, 0.5),
            (60, 2.0),
        ]
        .into();
        assert_eq!(detect_key(&c_major).unwrap().name(), "C major");
        assert_eq!(detect_key(&[(60, 1.0)]), None);
    }
}
