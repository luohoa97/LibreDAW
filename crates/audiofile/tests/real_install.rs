// SPDX-License-Identifier: GPL-3.0-or-later
//! Decodes every audio file of a real FL Studio install, read only, and
//! prints ok, failed and silent counts per format and the total time.
//! `FL_PACKS=<folder>` overrides the default Bottles path.
//! Run: `cargo test -p libredaw-audiofile --release --test real_install -- --ignored --nocapture`

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(audiofile::is_supported_ext)
        {
            out.push(p);
        }
    }
}

#[derive(Default, Debug)]
struct Tally {
    ok: usize,
    failed: usize,
    silent: usize,
    partial: usize,
    /// Decoded length differs from the `fact` chunk by more than a frame.
    short: usize,
}

#[test]
#[ignore = "needs a real FL Studio install"]
fn decode_every_real_file() {
    let packs = std::env::var_os("FL_PACKS").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(
            ".var/app/com.usebottles.bottles/data/bottles/bottles/FL-Studio/drive_c/Program Files/Image-Line/FL Studio 2026/Data/Patches/Packs",
        )
    });
    let mut files = Vec::new();
    walk(&packs, &mut files);
    files.sort();
    println!("{} files under {}", files.len(), packs.display());
    if files.is_empty() {
        return;
    }
    let next = AtomicUsize::new(0);
    let tally: Mutex<BTreeMap<String, Tally>> = Mutex::new(BTreeMap::new());
    let failures: Mutex<Vec<(PathBuf, String)>> = Mutex::new(Vec::new());
    let start = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(p) = files.get(i) else { break };
                    let key = kind(p);
                    let r = audiofile::decode_file(p);
                    let mut t = tally.lock().unwrap();
                    let e = t.entry(key).or_default();
                    match r {
                        Ok(a) => {
                            if let Some(pe) = &a.partial {
                                failures
                                    .lock()
                                    .unwrap()
                                    .push((p.clone(), format!("partial: {pe}")));
                                e.partial += 1;
                            }
                            if let Some(n) = fact(p).and_then(|_| granule(p))
                                && n.abs_diff(a.frames() as u64) > 1
                            {
                                e.short += 1;
                                if e.short <= 3 {
                                    println!(
                                        "frames {} vs granule {n}: {}",
                                        a.frames(),
                                        p.display()
                                    );
                                }
                            }
                            if a.data.iter().all(|v| v.abs() < 1e-6) {
                                e.silent += 1;
                            } else {
                                e.ok += 1;
                            }
                        }
                        Err(err) => {
                            e.failed += 1;
                            failures.lock().unwrap().push((p.clone(), err.to_string()));
                        }
                    }
                }
            });
        }
    });
    println!("total decode time {:?} (4 threads)", start.elapsed());
    for (k, t) in tally.lock().unwrap().iter() {
        println!("{k:<12} {t:?}");
    }
    let f = failures.lock().unwrap();
    for (p, e) in f.iter().take(6) {
        println!("FAILED {}: {e}", p.display());
    }
}

fn granule(p: &Path) -> Option<u64> {
    let b = std::fs::read(p).ok()?;
    let i = b.windows(4).rposition(|w| w == b"OggS")?;
    Some(i64::from_le_bytes(b.get(i + 6..i + 14)?.try_into().ok()?).max(0) as u64)
}

fn fact(p: &Path) -> Option<u64> {
    let mut h = [0u8; 4352];
    let n = std::io::Read::read(&mut std::fs::File::open(p).ok()?, &mut h).ok()?;
    let i = h[..n].windows(4).position(|w| w == b"fact")?;
    Some(u64::from(u32::from_le_bytes(
        h.get(i + 8..i + 12)?.try_into().ok()?,
    )))
}

/// Container kind by content, so Vorbis-in-WAV is told from PCM WAV.
fn kind(p: &Path) -> String {
    let mut h = [0u8; 24];
    if let Ok(mut f) = std::fs::File::open(p) {
        use std::io::Read;
        let _ = f.read(&mut h);
    }
    if &h[0..4] == b"RIFF" {
        let tag = u16::from_le_bytes([h[20], h[21]]);
        format!("wav tag {tag:#06x}")
    } else {
        p.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("?")
            .to_lowercase()
    }
}

/// Compares one file with a reference decode, for example
/// `ffmpeg -i x.ogg -f f32le -acodec pcm_f32le x.raw`:
/// `FL_FILE=... REF_F32=... cargo test --release --test real_install -- --ignored compare_reference --nocapture`
#[test]
#[ignore = "needs FL_FILE and REF_F32"]
fn compare_reference() {
    let (Some(f), Some(r)) = (std::env::var_os("FL_FILE"), std::env::var_os("REF_F32")) else {
        return;
    };
    let a = audiofile::decode_file(Path::new(&f)).unwrap();
    let raw = std::fs::read(r).unwrap();
    let reference: Vec<f32> = raw
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let n = a.data.len().min(reference.len());
    let max_diff = (0..n)
        .map(|i| (a.data[i] - reference[i]).abs())
        .fold(0.0f32, f32::max);
    println!(
        "ours {} values, reference {}, max abs diff {max_diff}",
        a.data.len(),
        reference.len()
    );
    assert!(max_diff < 1e-3);
}
