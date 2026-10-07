// SPDX-License-Identifier: GPL-3.0-or-later
//! Fixtures for tests: temp directories and synthetic WAV files. Never any
//! real FL Studio content.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        let p = std::env::temp_dir().join(format!(
            "libredaw-library-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 16-bit mono PCM WAV bytes of a sine.
pub fn sine_wav(hz: f64, secs: f64, rate: u32) -> Vec<u8> {
    let n = (f64::from(rate) * secs) as usize;
    let mut d = Vec::with_capacity(n * 2);
    for i in 0..n {
        let s = (2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(rate)).sin() * 0.5;
        d.extend_from_slice(&((s * 32767.0) as i16).to_le_bytes());
    }
    let mut w = Vec::new();
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + d.len() as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(d.len() as u32).to_le_bytes());
    w.extend_from_slice(&d);
    w
}

pub fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

/// A fixture tree shaped like an FL `Packs` folder.
pub fn fl_like_tree(root: &Path) {
    let short = sine_wav(220.0, 0.2, 44100);
    for rel in [
        "Drums/Kicks/909 Kick.wav",
        "Drums/Kicks/808 Kick.wav",
        "Drums/Snares/909 Snare.wav",
        "Drums/Snares/808 Snare.wav",
        "Drums/Hats/909 CH.wav",
        "Drums/Hats/909 OH.wav",
        "Drums/Hats/808 CH.wav",
        "Drums/Hats/808 OH.wav",
        "Drums/Percussion/909 Clap.wav",
        "Drums/Percussion/808 Cowbell.wav",
        "Drums/Percussion/808 Clap.wav",
        "Drums/Cymbals/909 Crash.wav",
        "Drums/Toms/909 Tom.wav",
        "Drums/SFX/Zap SFX.wav",
        "Loops/Funky 120bpm Am.wav",
        "Risers/Riser One.wav",
        "Vocals/Voc Ahh.wav",
        "SFX/FX Whoosh.wav",
        "Legacy/Drums/Kits/Drum Kit 01/FLS_Kick 01.wav",
        "Legacy/Drums/Kits/Drum Kit 01/FLS_Snare 01.wav",
        "Legacy/Drums/Kits/Drum Kit 01/FLS_Hat 01.wav",
    ] {
        write(root, &format!("Packs/{rel}"), &short);
    }
    write(root, "Packs/Drums.nfo", b"Tip=text");
    write(root, "Packs/Instruments/Bass/Bass1.fst", b"proprietary");
    // Named multisample: three notes.
    for n in ["C2", "G2", "C3"] {
        let hz = 65.41
            * if n == "G2" {
                1.5
            } else if n == "C3" {
                2.0
            } else {
                1.0
            };
        write(
            root,
            &format!("Packs/Instruments/Orchestral/Strings Section/OSTR {n}.wav"),
            &sine_wav(hz, 0.6, 44100),
        );
    }
    // Numbered multisample: sines at A3 B3 C#4 (220, 246.94, 277.18 Hz).
    for (i, hz) in [220.0, 246.94, 277.18].iter().enumerate() {
        write(
            root,
            &format!("Packs/Instruments/Guitar/Jazz/Jazz Guitar ({}).wav", i + 1),
            &sine_wav(*hz, 0.6, 44100),
        );
    }
}
