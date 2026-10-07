// SPDX-License-Identifier: GPL-3.0-or-later
//! Sample memory (SPEC 15.1, 17.2): the `SampleStore` the GTK side owns.
//!
//! A dedicated loader thread (not the compiler) reads and decodes WAV files,
//! resamples them once to the stream rate, and stores them as immutable
//! `Arc<[f32]>`. The compiler asks the store for a sample by hash; one that
//! is not decoded yet compiles as silence, and `poll` reports when the load
//! finishes so the caller can recompile. The audio thread never sees the
//! store, only the `SampleData` handles inside `Compiled`, which the retire
//! ring frees off the audio thread.

use crate::wavread::{WavError, decode_wav};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Default memory budget for decoded samples (17.2).
pub const DEFAULT_BUDGET_BYTES: usize = 1 << 30;
/// Largest file the loader reads.
pub const MAX_FILE_BYTES: u64 = 512 << 20;

/// Decoded, resampled audio. Interleaved when stereo.
#[derive(Clone, Debug)]
pub struct SampleData {
    pub channels: u8,
    /// Rate of `data`: the stream rate it was resampled to.
    pub rate: u32,
    pub data: Arc<[f32]>,
}

impl SampleData {
    pub fn frames(&self) -> usize {
        self.data.len() / self.channels.max(1) as usize
    }

    pub fn bytes(&self) -> usize {
        self.data.len() * 4
    }

    pub fn from_vec(channels: u8, rate: u32, data: Vec<f32>) -> SampleData {
        SampleData {
            channels,
            rate,
            data: data.into(),
        }
    }
}

impl PartialEq for SampleData {
    fn eq(&self, o: &SampleData) -> bool {
        self.channels == o.channels && self.rate == o.rate && Arc::ptr_eq(&self.data, &o.data)
    }
}

#[derive(Debug)]
pub enum SampleError {
    Io(std::io::Error),
    Wav(WavError),
    /// A compressed format (Vorbis, FLAC, WavPack, MP3 ...) failed to decode.
    Audio(audiofile::Error),
    TooLarge,
    OverBudget,
}

impl std::fmt::Display for SampleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SampleError::Io(e) => write!(f, "cannot read sample: {e}"),
            SampleError::Wav(e) => write!(f, "{e}"),
            SampleError::Audio(e) => write!(f, "{e}"),
            SampleError::TooLarge => f.write_str("sample file is too large"),
            SampleError::OverBudget => f.write_str("sample memory budget is used up"),
        }
    }
}

impl std::error::Error for SampleError {}

/// Windowed-sinc resampling of interleaved audio. Blackman window, 16 input
/// taps on each side (more when downsampling, to stay below the new Nyquist).
pub fn resample(data: &[f32], channels: usize, from: u32, to: u32) -> Vec<f32> {
    if from == to || data.is_empty() || channels == 0 {
        return data.to_vec();
    }
    let frames = data.len() / channels;
    let ratio = to as f64 / from as f64;
    let out_frames = ((frames as u64 * to as u64).div_ceil(from as u64) as usize).max(1);
    let cut = ratio.min(1.0);
    let half = 16.0 / cut;
    let mut out = vec![0.0f32; out_frames * channels];
    for i in 0..out_frames {
        let center = i as f64 / ratio;
        let lo = ((center - half).ceil().max(0.0)) as usize;
        let hi = ((center + half).floor() as i64).min(frames as i64 - 1);
        if hi < lo as i64 {
            continue;
        }
        for j in lo..=hi as usize {
            let x = j as f64 - center;
            let w = {
                let t = x / half;
                0.42 + 0.5 * (std::f64::consts::PI * t).cos()
                    + 0.08 * (2.0 * std::f64::consts::PI * t).cos()
            };
            let s = if x == 0.0 {
                1.0
            } else {
                let a = std::f64::consts::PI * x * cut;
                a.sin() / a
            };
            let k = (cut * s * w) as f32;
            for c in 0..channels {
                out[i * channels + c] += data[j * channels + c] * k;
            }
        }
    }
    out
}

/// Reads and decodes `path`, resampled to `rate`.
pub fn load_sample_file(path: &Path, rate: u32) -> Result<SampleData, SampleError> {
    let meta = std::fs::metadata(path).map_err(SampleError::Io)?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(SampleError::TooLarge);
    }
    let bytes = std::fs::read(path).map_err(SampleError::Io)?;
    decode_sample(&bytes, rate)
}

/// Decodes WAV bytes, resampled to `rate`.
pub fn decode_sample(bytes: &[u8], rate: u32) -> Result<SampleData, SampleError> {
    let d = match decode_wav(bytes) {
        Ok(d) => d,
        // Not plain PCM WAV: FLAC, Ogg, MP3, WavPack, Vorbis-in-WAV, or WAV
        // with more than two channels. Runs on the loader thread, like all
        // decoding.
        Err(WavError::NotWav | WavError::Unsupported(_)) => return decode_other(bytes, rate),
        Err(e) => return Err(SampleError::Wav(e)),
    };
    let data = resample(&d.data, d.channels as usize, d.rate, rate);
    Ok(SampleData::from_vec(d.channels, rate, data))
}

fn decode_other(bytes: &[u8], rate: u32) -> Result<SampleData, SampleError> {
    let a = audiofile::decode_bytes(bytes, None).map_err(SampleError::Audio)?;
    let ch = usize::from(a.channels.max(1));
    let (channels, data) = if ch <= 2 {
        (ch as u8, a.data)
    } else {
        // More than two channels: keep the front pair.
        (
            2u8,
            a.data.chunks_exact(ch).flat_map(|f| [f[0], f[1]]).collect(),
        )
    };
    let data = resample(&data, usize::from(channels), a.rate, rate);
    Ok(SampleData::from_vec(channels, rate, data))
}

/// The store key of a raw SHA-256: 64 lowercase hex digits.
pub fn hash_hex(hash: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in hash {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
        s.push(char::from_digit((b & 15) as u32, 16).unwrap_or('0'));
    }
    s
}

/// The state of one sample in the store.
#[derive(Clone, Debug)]
pub enum SampleState {
    Loading,
    Ready(SampleData),
    Failed(String),
}

/// A finished load, reported by `poll`. Recompile when you see one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadEvent {
    pub hash: String,
    /// `Err` carries a message for a toast; the sample stays silent.
    pub result: Result<(), String>,
}

struct Entry {
    path: Option<PathBuf>,
    state: SampleState,
}

struct Inner {
    rate: u32,
    /// Bumped when the rate changes, so stale loads are dropped.
    epoch: u64,
    budget: usize,
    used: usize,
    map: HashMap<String, Entry>,
}

enum Job {
    Load {
        hash: String,
        path: PathBuf,
        rate: u32,
        epoch: u64,
    },
    Quit,
}

pub struct SampleStore {
    inner: Arc<Mutex<Inner>>,
    jobs: Sender<Job>,
    done: Mutex<Receiver<LoadEvent>>,
    thread: Option<JoinHandle<()>>,
}

fn lock(m: &Mutex<Inner>) -> std::sync::MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl SampleStore {
    /// A store for a stream at `rate`, holding at most `budget` bytes of
    /// decoded samples. Starts the loader thread.
    pub fn new(rate: u32, budget: usize) -> SampleStore {
        let inner = Arc::new(Mutex::new(Inner {
            rate,
            epoch: 0,
            budget,
            used: 0,
            map: HashMap::new(),
        }));
        let (jobs, rx) = channel::<Job>();
        let (done_tx, done_rx) = channel::<LoadEvent>();
        let shared = inner.clone();
        let thread = std::thread::Builder::new()
            .name("libredaw-sample-loader".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let Job::Load {
                        hash,
                        path,
                        rate,
                        epoch,
                    } = job
                    else {
                        return;
                    };
                    let res = load_sample_file(&path, rate);
                    let mut g = lock(&shared);
                    if g.epoch != epoch {
                        continue; // the rate changed; a newer job follows
                    }
                    let result = match res {
                        Ok(s) if g.used + s.bytes() <= g.budget => {
                            g.used += s.bytes();
                            if let Some(e) = g.map.get_mut(&hash) {
                                e.state = SampleState::Ready(s);
                            }
                            Ok(())
                        }
                        Ok(_) => Err(SampleError::OverBudget.to_string()),
                        Err(e) => Err(e.to_string()),
                    };
                    if let Err(msg) = &result
                        && let Some(e) = g.map.get_mut(&hash)
                    {
                        e.state = SampleState::Failed(msg.clone());
                    }
                    drop(g);
                    let _ = done_tx.send(LoadEvent { hash, result });
                }
            })
            .expect("spawn sample loader");
        SampleStore {
            inner,
            jobs,
            done: Mutex::new(done_rx),
            thread: Some(thread),
        }
    }

    /// Starts loading `hash` from `path` unless it is known already.
    pub fn request(&self, hash: &str, path: PathBuf) {
        let mut g = lock(&self.inner);
        if g.map.contains_key(hash) {
            return;
        }
        g.map.insert(
            hash.to_string(),
            Entry {
                path: Some(path.clone()),
                state: SampleState::Loading,
            },
        );
        let job = Job::Load {
            hash: hash.to_string(),
            path,
            rate: g.rate,
            epoch: g.epoch,
        };
        drop(g);
        let _ = self.jobs.send(job);
    }

    /// Forgets a failed sample so `request` tries again (a file that
    /// appeared since, for example).
    pub fn retry(&self, hash: &str) {
        let mut g = lock(&self.inner);
        if matches!(
            g.map.get(hash).map(|e| &e.state),
            Some(SampleState::Failed(_))
        ) {
            let path = g.map.remove(hash).and_then(|e| e.path);
            drop(g);
            if let Some(p) = path {
                self.request(hash, p);
            }
        }
    }

    /// Puts already decoded audio in the store (tests, in-memory renders).
    /// It is not reloaded on a rate change.
    pub fn insert(&self, hash: &str, data: SampleData) {
        let mut g = lock(&self.inner);
        g.used += data.bytes();
        g.map.insert(
            hash.to_string(),
            Entry {
                path: None,
                state: SampleState::Ready(data),
            },
        );
    }

    pub fn state(&self, hash: &str) -> Option<SampleState> {
        lock(&self.inner).map.get(hash).map(|e| e.state.clone())
    }

    /// The decoded sample, if it is ready.
    pub fn get(&self, hash: &str) -> Option<SampleData> {
        match lock(&self.inner).map.get(hash).map(|e| &e.state) {
            Some(SampleState::Ready(s)) => Some(s.clone()),
            _ => None,
        }
    }

    /// Loads that finished since the last call. Call from the 10 ms source;
    /// recompile when it is not empty.
    pub fn poll(&self) -> Vec<LoadEvent> {
        let rx = self.done.lock().unwrap_or_else(|e| e.into_inner());
        rx.try_iter().collect()
    }

    pub fn used_bytes(&self) -> usize {
        lock(&self.inner).used
    }

    pub fn budget_bytes(&self) -> usize {
        lock(&self.inner).budget
    }

    pub fn set_budget_bytes(&self, budget: usize) {
        lock(&self.inner).budget = budget;
    }

    pub fn sample_rate(&self) -> u32 {
        lock(&self.inner).rate
    }

    /// A 4.7 rate change: every file-backed sample is decoded again at the
    /// new rate, and reports through `poll` when done. Until then it
    /// compiles as silence. Samples added with `insert` are dropped.
    pub fn set_sample_rate(&self, rate: u32) {
        let mut g = lock(&self.inner);
        if g.rate == rate {
            return;
        }
        g.rate = rate;
        g.epoch += 1;
        g.used = 0;
        let epoch = g.epoch;
        g.map.retain(|_, e| e.path.is_some());
        let mut jobs = Vec::new();
        for (hash, e) in g.map.iter_mut() {
            e.state = SampleState::Loading;
            if let Some(p) = &e.path {
                jobs.push(Job::Load {
                    hash: hash.clone(),
                    path: p.clone(),
                    rate,
                    epoch,
                });
            }
        }
        drop(g);
        for j in jobs {
            let _ = self.jobs.send(j);
        }
    }
}

impl Drop for SampleStore {
    fn drop(&mut self) {
        let _ = self.jobs.send(Job::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    // Compressed formats go through the audiofile crate (15.3); the fixtures
    // are 0.2 s sines made for its tests.
    #[test]
    fn compressed_formats_load_resampled() {
        for (name, bytes, ch) in [
            (
                "flac",
                &include_bytes!("../../audiofile/tests/fixtures/sine440.flac")[..],
                1u8,
            ),
            (
                "ogg",
                &include_bytes!("../../audiofile/tests/fixtures/sine440.ogg")[..],
                2,
            ),
            (
                "wavpack",
                &include_bytes!("../../audiofile/tests/fixtures/sine440.wv")[..],
                1,
            ),
            (
                "mp3",
                &include_bytes!("../../audiofile/tests/fixtures/sine440.mp3")[..],
                1,
            ),
        ] {
            let s = decode_sample(bytes, 48000).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!((s.channels, s.rate), (ch, 48000), "{name}");
            // 0.2 s at 48 kHz.
            assert!(
                (9000..=11000).contains(&s.frames()),
                "{name}: {}",
                s.frames()
            );
        }
        assert!(matches!(
            decode_sample(b"junk junk junk junk", 48000),
            Err(SampleError::Audio(_))
        ));
    }

    use super::*;
    use crate::testutil::tone_level;
    use std::time::{Duration, Instant};

    fn sine(freq: f64, rate: u32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (std::f64::consts::TAU * freq * i as f64 / rate as f64).sin() as f32)
            .collect()
    }

    fn wav16(rate: u32, x: &[f32]) -> Vec<u8> {
        let mut data = Vec::new();
        for v in x {
            data.extend_from_slice(&((v * 32767.0) as i16).to_le_bytes());
        }
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * 2).to_le_bytes());
        v.extend_from_slice(&2u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(&data);
        v
    }

    #[test]
    fn resampling_keeps_pitch_and_level() {
        for (from, to) in [
            (44100u32, 48000u32),
            (48000, 44100),
            (22050, 48000),
            (96000, 48000),
        ] {
            let x = sine(1000.0, from, from as usize / 2);
            let y = resample(&x, 1, from, to);
            let want = (x.len() as u64 * to as u64).div_ceil(from as u64) as usize;
            assert_eq!(y.len(), want);
            // Compare a middle stretch, away from the edges.
            let mid = &y[y.len() / 4..y.len() * 3 / 4];
            let lvl = tone_level(mid, to as f64, 1000.0);
            assert!((lvl - 1.0).abs() < 0.01, "{from}->{to}: level {lvl}");
            let off = tone_level(mid, to as f64, 1300.0);
            assert!(off < 0.01);
        }
        assert_eq!(resample(&[0.5, 0.25], 1, 48000, 48000), vec![0.5, 0.25]);
    }

    #[test]
    fn stereo_channels_stay_separate() {
        let l = sine(500.0, 44100, 4410);
        let mut inter = Vec::new();
        for v in &l {
            inter.push(*v);
            inter.push(0.0);
        }
        let y = resample(&inter, 2, 44100, 48000);
        assert!(y.chunks(2).all(|f| f[1] == 0.0));
        assert!(y.chunks(2).any(|f| f[0].abs() > 0.9));
    }

    fn wait_for(store: &SampleStore, hash: &str) -> LoadEvent {
        let t0 = Instant::now();
        loop {
            if let Some(e) = store.poll().into_iter().find(|e| e.hash == hash) {
                return e;
            }
            assert!(t0.elapsed() < Duration::from_secs(10), "loader timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("libredaw-samples-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn loader_thread_decodes_resamples_and_reports() {
        let dir = temp_dir("load");
        let p = dir.join("a.wav");
        std::fs::write(&p, wav16(44100, &sine(440.0, 44100, 44100))).unwrap();
        let store = SampleStore::new(48000, DEFAULT_BUDGET_BYTES);
        store.request("aa", p.clone());
        assert!(matches!(
            store.state("aa"),
            Some(SampleState::Loading) | Some(SampleState::Ready(_))
        ));
        let ev = wait_for(&store, "aa");
        assert_eq!(ev.result, Ok(()));
        let s = store.get("aa").unwrap();
        assert_eq!((s.channels, s.rate), (1, 48000));
        assert_eq!(s.frames(), 48000);
        assert_eq!(store.used_bytes(), 48000 * 4);

        // A rate change reloads at the new rate and reports again.
        store.set_sample_rate(96000);
        assert!(store.get("aa").is_none(), "silent until the reload");
        wait_for(&store, "aa");
        assert_eq!(store.get("aa").unwrap().frames(), 96000);
        assert_eq!(store.used_bytes(), 96000 * 4);

        // Missing and broken files fail with a message and stay silent.
        store.request("missing", dir.join("nope.wav"));
        assert!(wait_for(&store, "missing").result.is_err());
        assert!(store.get("missing").is_none());
        let bad = dir.join("bad.wav");
        std::fs::write(&bad, b"RIFFxxxxWAVE").unwrap();
        store.request("bad", bad);
        assert!(wait_for(&store, "bad").result.is_err());
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_budget_is_enforced() {
        let dir = temp_dir("budget");
        let p = dir.join("a.wav");
        std::fs::write(&p, wav16(48000, &sine(440.0, 48000, 48000))).unwrap();
        let store = SampleStore::new(48000, 100_000);
        store.request("big", p);
        let ev = wait_for(&store, "big");
        assert!(ev.result.is_err());
        assert!(store.get("big").is_none());
        assert_eq!(store.used_bytes(), 0);
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn decode_sample_resamples_wav_bytes() {
        let s = decode_sample(&wav16(24000, &sine(440.0, 24000, 2400)), 48000).unwrap();
        assert_eq!((s.channels, s.rate, s.frames()), (1, 48000, 4800));
    }
}
