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

/// The key that fits `(midi key, length)` pairs best, for example
/// "A minor". `None` for fewer than three notes or a single pitch class.
pub fn key_name(notes: &[(u8, f64)]) -> Option<String> {
    let mut classes = [false; 12];
    for (k, _) in notes {
        classes[(*k % 12) as usize] = true;
    }
    if notes.len() < 3 || classes.iter().filter(|c| **c).count() < 2 {
        return None;
    }
    let events: Vec<transcribe::NoteEvent> = notes
        .iter()
        .map(|(k, len)| transcribe::NoteEvent {
            start_s: 0.0,
            end_s: len.max(0.0),
            midi_key: *k,
            velocity: 100,
            confidence: 1.0,
        })
        .collect();
    transcribe::detect_key(&events).map(|k| k.name())
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
        let a_minor = [
            (57, 2.0),
            (59, 1.0),
            (60, 1.0),
            (62, 1.0),
            (64, 1.5),
            (65, 0.5),
            (67, 0.5),
            (57, 2.0),
        ];
        assert_eq!(key_name(&a_minor).unwrap(), "A minor");
        let c_major = [
            (60, 2.0),
            (62, 1.0),
            (64, 1.0),
            (65, 1.0),
            (67, 1.5),
            (69, 1.0),
            (71, 0.5),
            (60, 2.0),
        ];
        assert_eq!(key_name(&c_major).unwrap(), "C major");
        assert_eq!(key_name(&[(60, 1.0)]), None);
    }
}
