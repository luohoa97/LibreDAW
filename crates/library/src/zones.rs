// SPDX-License-Identifier: GPL-3.0-or-later
//! Multisampled instruments: samples of one instrument grouped into key
//! zones, so a sampler can play them across the keyboard.
//!
//! The root note of a sample comes from, in order: the file name (`C3`,
//! `A#2`), the keyboard order of an 88-sample set (`Grand Piano 1..88` is
//! A0 to C8), or the pitch of the audio itself (autocorrelation).

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};

use crate::index::{LibraryIndex, SoundEntry};
use crate::pitch::estimate_root;

/// With `decode` false the audio is not read: sets that need it stay unplaced
/// (`root_source` empty) until a scan with `decode` true.
/// Fills `root_note` for tonal multisamples that have none. Returns how
/// many roots were estimated from audio. Entries already decided keep
/// their root, so a warm scan does no work here.
pub fn assign_roots(entries: &mut [SoundEntry], decode: bool) -> usize {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, e) in entries.iter().enumerate() {
        if let Some(g) = &e.group {
            groups.entry(g.clone()).or_default().push(i);
        }
    }
    let mut to_pitch: Vec<usize> = Vec::new();
    for idx in groups.values() {
        let pending: Vec<usize> = idx
            .iter()
            .copied()
            .filter(|&i| entries[i].root_source.is_none())
            .collect();
        if pending.is_empty() {
            continue;
        }
        let mut seqs: Vec<u32> = idx.iter().filter_map(|&i| entries[i].seq).collect();
        seqs.sort_unstable();
        let keyboard = idx.len() == 88 && seqs == (1..=88).collect::<Vec<u32>>();
        for i in pending {
            if keyboard && let Some(s) = entries[i].seq {
                entries[i].root_note = Some(20 + s as u8);
                entries[i].root_source = Some("keyboard-order".into());
            } else {
                to_pitch.push(i);
            }
        }
    }
    if to_pitch.is_empty() || !decode {
        return 0;
    }
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<(usize, Option<u8>)>> = Mutex::new(Vec::new());
    let paths: Vec<_> = to_pitch.iter().map(|&i| entries[i].path.clone()).collect();
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                let mut local = Vec::new();
                loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(p) = paths.get(k) else { break };
                    local.push((k, estimate_root(p)));
                }
                if let Ok(mut o) = out.lock() {
                    o.extend(local);
                }
            });
        }
    });
    let mut n = 0;
    for (k, root) in out.into_inner().unwrap_or_default() {
        let e = &mut entries[to_pitch[k]];
        match root {
            Some(r) => {
                e.root_note = Some(r);
                e.root_source = Some("pitch".into());
                n += 1;
            }
            None => e.root_source = Some("unknown".into()),
        }
    }
    n
}

/// One key range of an instrument.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Zone {
    pub sound_id: String,
    pub root: u8,
    pub lo: u8,
    pub hi: u8,
    /// More samples with the same root (round robin or layers), by id.
    #[serde(default)]
    pub alternates: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instrument {
    pub id: String,
    pub name: String,
    pub role: String,
    pub pack: String,
    /// "name", "keyboard-order" or "pitch": how the roots were found.
    /// "pitch" is an estimate; the UI may say so.
    pub root_source: String,
    /// For estimated roots: true when the roots rise with the file numbers,
    /// which is how these sets are laid out (a sanity check).
    pub ordered: bool,
    pub zones: Vec<Zone>,
}

/// Instruments with at least two placed samples, sorted by name.
pub fn build_instruments(index: &LibraryIndex) -> Vec<Instrument> {
    let mut groups: BTreeMap<&str, Vec<&SoundEntry>> = BTreeMap::new();
    for e in &index.entries {
        if let (Some(g), Some(_)) = (&e.group, e.root_note) {
            groups.entry(g).or_default().push(e);
        }
    }
    let mut out = Vec::new();
    for (g, mut members) in groups {
        members.sort_by_key(|e| (e.root_note, e.rel.clone()));
        let mut roots: Vec<(u8, Vec<&SoundEntry>)> = Vec::new();
        for e in &members {
            let r = e.root_note.unwrap_or(0);
            match roots.last_mut() {
                Some((lr, v)) if *lr == r => v.push(e),
                _ => roots.push((r, vec![e])),
            }
        }
        if roots.len() < 2 {
            continue;
        }
        let mut zones = Vec::new();
        for (i, (root, v)) in roots.iter().enumerate() {
            let lo = if i == 0 {
                0
            } else {
                zones_hi(&roots, i - 1) + 1
            };
            let hi = if i + 1 == roots.len() {
                127
            } else {
                zones_hi(&roots, i)
            };
            zones.push(Zone {
                sound_id: v[0].id.clone(),
                root: *root,
                lo,
                hi,
                alternates: v[1..].iter().map(|e| e.id.clone()).collect(),
            });
        }
        let first = members[0];
        let source = if members
            .iter()
            .any(|e| e.root_source.as_deref() == Some("pitch"))
        {
            "pitch"
        } else {
            first.root_source.as_deref().unwrap_or("name")
        };
        let mut numbered: Vec<(u32, u8)> = members
            .iter()
            .filter_map(|e| Some((e.seq?, e.root_note?)))
            .collect();
        numbered.sort_unstable();
        let ordered = numbered.windows(2).all(|w| w[0].1 <= w[1].1);
        out.push(Instrument {
            id: format!(
                "{}-inst-{}",
                first.id.split('-').next().unwrap_or("lib"),
                &doc::sha256::sha256_hex(g.as_bytes())[..12]
            ),
            name: first
                .instrument
                .clone()
                .unwrap_or_else(|| g.rsplit('/').next().unwrap_or(g).to_string()),
            role: first.role.clone(),
            pack: first.pack.clone(),
            root_source: source.to_string(),
            ordered,
            zones,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    out
}

/// Multisample sets whose samples could not be placed on the keyboard (no
/// note in the file names and no way to read the pitch), with their sample
/// counts. Today this is every numbered set stored as Ogg Vorbis in WAV.
pub fn unplaced_instruments(index: &LibraryIndex) -> Vec<(String, usize)> {
    let mut m: BTreeMap<String, usize> = BTreeMap::new();
    for e in index
        .entries
        .iter()
        .filter(|e| e.group.is_some() && e.root_note.is_none())
    {
        let name = e.instrument.clone().unwrap_or_default();
        *m.entry(format!("{} ({})", name, e.pack)).or_default() += 1;
    }
    m.into_iter().collect()
}

/// Last key of zone `i`: halfway to the next root.
fn zones_hi(roots: &[(u8, Vec<&SoundEntry>)], i: usize) -> u8 {
    let (a, b) = (u16::from(roots[i].0), u16::from(roots[i + 1].0));
    ((a + b) / 2) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Source, scan};
    use crate::testutil::{TempDir, fl_like_tree};

    fn instruments() -> (TempDir, Vec<Instrument>) {
        let t = TempDir::new("zones");
        fl_like_tree(t.path());
        let s = Source {
            root: t.path().join("Packs"),
            label: "FL".into(),
            id_prefix: "fl".into(),
            pack_prefix: "FL Studio".into(),
        };
        let r = scan(&s, &t.path().join("cache")).unwrap();
        let v = build_instruments(&r.index);
        (t, v)
    }

    #[test]
    fn named_notes_make_zones() {
        let (_t, v) = instruments();
        let o = v.iter().find(|i| i.name == "OSTR").unwrap();
        assert_eq!(o.root_source, "name");
        let roots: Vec<(u8, u8, u8)> = o.zones.iter().map(|z| (z.root, z.lo, z.hi)).collect();
        assert_eq!(roots, vec![(36, 0, 39), (43, 40, 45), (48, 46, 127)]);
    }

    #[test]
    fn numbered_samples_are_pitched() {
        let (_t, v) = instruments();
        let j = v.iter().find(|i| i.name == "Jazz Guitar").unwrap();
        assert_eq!(j.root_source, "pitch");
        assert!(j.ordered);
        assert_eq!(
            j.zones.iter().map(|z| z.root).collect::<Vec<_>>(),
            vec![57, 59, 61]
        );
    }

    #[test]
    fn zones_cover_the_keyboard_without_gaps() {
        let (_t, v) = instruments();
        for i in &v {
            assert_eq!(i.zones[0].lo, 0);
            assert_eq!(i.zones.last().unwrap().hi, 127);
            for w in i.zones.windows(2) {
                assert_eq!(w[0].hi + 1, w[1].lo);
            }
        }
    }
}
