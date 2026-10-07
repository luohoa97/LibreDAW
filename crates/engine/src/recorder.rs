// SPDX-License-Identifier: GPL-3.0-or-later
//! Measurement storage. Everything is allocated up front; the audio thread
//! only does atomic stores, and the main thread reads after the stream has
//! stopped.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering::*};
use std::time::Instant;

pub struct Recorder {
    start: Instant,
    cb_len: AtomicUsize,
    cb_overflow: AtomicU64,
    cb_mono_ns: Vec<AtomicU64>,
    cb_stream_ns: Vec<AtomicU64>,
    cb_frames: Vec<AtomicU32>,
    onset_len: AtomicUsize,
    onset_overflow: AtomicU64,
    onset_sample: Vec<AtomicU64>,
    onset_accent: Vec<AtomicU8>,
    err_len: AtomicUsize,
    err_overflow: AtomicU64,
    err_mono_ns: Vec<AtomicU64>,
    err_kind: Vec<AtomicU8>,
}

/// One callback as seen by the audio thread.
#[derive(Clone, Copy, Debug)]
pub struct CallbackRecord {
    /// `Instant::now()` at callback entry, ns since the recorder was created.
    pub mono_ns: u64,
    /// The backend's own timestamp for the callback (0 if none).
    pub stream_ns: u64,
    pub frames: u32,
}

fn atomics<T, A: Fn() -> T>(n: usize, f: A) -> Vec<T> {
    (0..n).map(|_| f()).collect()
}

impl Recorder {
    pub fn new(callbacks: usize, onsets: usize, errors: usize) -> Arc<Recorder> {
        Arc::new(Recorder {
            start: Instant::now(),
            cb_len: AtomicUsize::new(0),
            cb_overflow: AtomicU64::new(0),
            cb_mono_ns: atomics(callbacks, || AtomicU64::new(0)),
            cb_stream_ns: atomics(callbacks, || AtomicU64::new(0)),
            cb_frames: atomics(callbacks, || AtomicU32::new(0)),
            onset_len: AtomicUsize::new(0),
            onset_overflow: AtomicU64::new(0),
            onset_sample: atomics(onsets, || AtomicU64::new(0)),
            onset_accent: atomics(onsets, || AtomicU8::new(0)),
            err_len: AtomicUsize::new(0),
            err_overflow: AtomicU64::new(0),
            err_mono_ns: atomics(errors, || AtomicU64::new(0)),
            err_kind: atomics(errors, || AtomicU8::new(0)),
        })
    }

    pub fn elapsed_ns(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }

    /// Audio thread only (single writer).
    pub fn push_callback(&self, mono_ns: u64, stream_ns: u64, frames: u32) {
        let i = self.cb_len.load(Relaxed);
        if i < self.cb_mono_ns.len() {
            self.cb_mono_ns[i].store(mono_ns, Relaxed);
            self.cb_stream_ns[i].store(stream_ns, Relaxed);
            self.cb_frames[i].store(frames, Relaxed);
            self.cb_len.store(i + 1, Release);
        } else {
            self.cb_overflow.fetch_add(1, Relaxed);
        }
    }

    /// Audio thread only (single writer).
    pub fn push_onset(&self, sample: u64, accent: bool) {
        let i = self.onset_len.load(Relaxed);
        if i < self.onset_sample.len() {
            self.onset_sample[i].store(sample, Relaxed);
            self.onset_accent[i].store(accent as u8, Relaxed);
            self.onset_len.store(i + 1, Release);
        } else {
            self.onset_overflow.fetch_add(1, Relaxed);
        }
    }

    /// Any thread: the backend may call its error callback from a thread of
    /// its own. `kind` is a small code chosen by the caller.
    pub fn push_error(&self, kind: u8) {
        let t = self.elapsed_ns();
        let i = self.err_len.fetch_add(1, Relaxed);
        if i < self.err_mono_ns.len() {
            self.err_mono_ns[i].store(t, Relaxed);
            self.err_kind[i].store(kind, Release);
        } else {
            self.err_overflow.fetch_add(1, Relaxed);
        }
    }

    // Readers: call after the stream has been stopped and dropped.

    pub fn callbacks(&self) -> Vec<CallbackRecord> {
        let n = self.cb_len.load(Acquire);
        (0..n)
            .map(|i| CallbackRecord {
                mono_ns: self.cb_mono_ns[i].load(Relaxed),
                stream_ns: self.cb_stream_ns[i].load(Relaxed),
                frames: self.cb_frames[i].load(Relaxed),
            })
            .collect()
    }

    pub fn callback_overflow(&self) -> u64 {
        self.cb_overflow.load(Relaxed)
    }

    pub fn onsets(&self) -> Vec<u64> {
        let n = self.onset_len.load(Acquire);
        (0..n).map(|i| self.onset_sample[i].load(Relaxed)).collect()
    }

    pub fn onset_accents(&self) -> Vec<bool> {
        let n = self.onset_len.load(Acquire);
        (0..n)
            .map(|i| self.onset_accent[i].load(Relaxed) != 0)
            .collect()
    }

    pub fn onset_overflow(&self) -> u64 {
        self.onset_overflow.load(Relaxed)
    }

    /// `(ns since start, kind code)` per error-callback event.
    pub fn errors(&self) -> Vec<(u64, u8)> {
        let n = self.err_len.load(Acquire).min(self.err_mono_ns.len());
        (0..n)
            .map(|i| {
                (
                    self.err_mono_ns[i].load(Relaxed),
                    self.err_kind[i].load(Acquire),
                )
            })
            .collect()
    }

    pub fn error_overflow(&self) -> u64 {
        self.err_overflow.load(Relaxed)
    }
}
