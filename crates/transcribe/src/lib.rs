// SPDX-License-Identifier: GPL-3.0-or-later
//! Hum to notes (SPEC 21.3): audio in, MIDI-like note events out.
//!
//! The model is Spotify's Basic Pitch (Apache-2.0), run on the CPU with
//! `tract-onnx` on the caller's thread. Windowing and note extraction are
//! ported from the upstream Python (`inference.py`, `note_creation.py`).
//! Pitch bends are not extracted.

mod model;
mod notes;
mod post;
mod resample;

pub use post::{Key, Scale, detect_key, quantize};

use std::fmt;

/// One transcribed note. Times are seconds from the start of the audio.
#[derive(Debug, Clone, PartialEq)]
pub struct NoteEvent {
    pub start_s: f64,
    pub end_s: f64,
    /// MIDI key number, 21 (A0) to 108 (C8).
    pub midi_key: u8,
    /// MIDI velocity, 1 to 127.
    pub velocity: u8,
    /// Mean frame activation of the note, 0 to 1.
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    /// Minimum onset activation (Basic Pitch default 0.5).
    pub onset_threshold: f32,
    /// Minimum frame activation for a note to stay on (default 0.3).
    pub frame_threshold: f32,
    /// Notes shorter than this are dropped (default 127.7 ms).
    pub min_note_ms: f32,
    /// Keep one note at a time, the strongest. For humming.
    pub monophonic: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            onset_threshold: 0.5,
            frame_threshold: 0.3,
            min_note_ms: 127.7,
            monophonic: false,
        }
    }
}

#[derive(Debug)]
pub enum Error {
    /// The sample rate is zero.
    BadSampleRate,
    /// The bundled model failed to load or run.
    Model(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::BadSampleRate => write!(f, "sample rate must be above zero"),
            Error::Model(e) => write!(f, "transcription model error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Transcribes mono `audio` (any sample rate) into notes, sorted by start.
/// Blocking and CPU heavy: call it from a worker thread. The first call
/// loads the model, later calls reuse it.
pub fn transcribe(audio: &[f32], sample_rate: u32, opts: &Options) -> Result<Vec<NoteEvent>> {
    if sample_rate == 0 {
        return Err(Error::BadSampleRate);
    }
    let audio = resample::to_model_rate(audio, sample_rate);
    if audio.is_empty() {
        return Ok(Vec::new());
    }
    let out = model::run(&audio)?;
    let mut notes = notes::extract(&out, opts);
    if opts.monophonic {
        notes = post::make_monophonic(notes);
    }
    Ok(notes)
}

#[cfg(test)]
mod tests;
