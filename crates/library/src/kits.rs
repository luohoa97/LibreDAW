// SPDX-License-Identifier: GPL-3.0-or-later
//! Ready-made kits from a library's drum folders: kick, snare, clap, hat,
//! open hat, perc. FL's own `.fst` kit files are plugin state and cannot be
//! read, so kits are built from the one-shots, grouped by the name they
//! share (`909 Kick`, `909 Snare`, ... make the "909" kit) or by the
//! `Kits/<kit>` folders of the Legacy pack.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::index::{LibraryIndex, SoundEntry};

pub const SLOTS: [&str; 6] = ["kick", "snare", "clap", "hat", "open_hat", "perc"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kit {
    pub id: String,
    pub name: String,
    pub pack: String,
    /// Slot name to sound id; only filled slots are present.
    pub slots: BTreeMap<String, String>,
}

fn slot_of(e: &SoundEntry) -> Option<&'static str> {
    let has = |t: &str| e.tags.iter().any(|x| x == t);
    match e.role.as_str() {
        "kick" => Some("kick"),
        "snare" => Some("snare"),
        "clap" => Some("clap"),
        "hat" if has("open") => Some("open_hat"),
        "hat" => Some("hat"),
        "perc" if has("rim") => None,
        "perc" => Some("perc"),
        _ => None,
    }
}

/// "909 Kick" gives "909"; "Attack Hat 01" gives "Attack".
fn family(e: &SoundEntry) -> Option<String> {
    let first = e.name.split_whitespace().next()?;
    (e.name.split_whitespace().count() >= 2).then(|| first.to_string())
}

fn is_drum_pack(e: &SoundEntry) -> bool {
    e.pack.starts_with("FL Studio: Drums")
        || e.pack.ends_with(": Drums")
        || e.pack.contains("(ModeAudio)")
}

fn kit_id(prefix: &str, name: &str) -> String {
    let h = doc::sha256::sha256_hex(name.as_bytes());
    format!("{prefix}-kit-{}", &h[..10])
}

/// Kits with at least a kick, a snare and a hat. Sorted by name.
pub fn build_kits(index: &LibraryIndex) -> Vec<Kit> {
    let mut by_name: BTreeMap<(String, String), BTreeMap<&'static str, &SoundEntry>> =
        BTreeMap::new();
    for e in &index.entries {
        let Some(slot) = slot_of(e) else { continue };
        // Legacy kit folders name their kit; one-shot packs share a name.
        let (name, pack) = if let Some(k) = &e.kit {
            (k.clone(), e.pack.clone())
        } else if is_drum_pack(e)
            && let Some(f) = family(e)
        {
            (f, e.pack.clone())
        } else {
            continue;
        };
        // The first sample (by path) of a slot wins: deterministic.
        by_name
            .entry((name, pack))
            .or_default()
            .entry(slot)
            .or_insert(e);
    }
    // One-shot families shared across packs (a "909" clap sits in
    // Percussion, the kick in Kicks) are merged by name within a pack.
    let prefix = index
        .entries
        .first()
        .and_then(|e| e.id.split('-').next())
        .unwrap_or("lib")
        .to_string();
    let mut kits: Vec<Kit> = by_name
        .into_iter()
        .filter(|(_, s)| s.contains_key("kick") && s.contains_key("snare") && s.contains_key("hat"))
        .map(|((name, pack), s)| Kit {
            id: kit_id(&prefix, &format!("{pack}/{name}")),
            name,
            pack,
            slots: s
                .into_iter()
                .map(|(k, e)| (k.to_string(), e.id.clone()))
                .collect(),
        })
        .collect();
    kits.sort_by(|a, b| a.name.cmp(&b.name).then(a.pack.cmp(&b.pack)));
    kits
}

/// The kit to offer first: "909", then "808", then the first one.
pub fn default_kit(kits: &[Kit]) -> Option<&Kit> {
    ["909", "808", "707"]
        .iter()
        .find_map(|n| kits.iter().find(|k| k.name == *n))
        .or_else(|| kits.first())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Source, scan};
    use crate::testutil::{TempDir, fl_like_tree};

    #[test]
    fn kits_from_families_and_kit_folders() {
        let t = TempDir::new("kits");
        fl_like_tree(t.path());
        let s = Source {
            root: t.path().join("Packs"),
            label: "FL".into(),
            id_prefix: "fl".into(),
            pack_prefix: "FL Studio".into(),
        };
        let r = scan(&s, &t.path().join("cache")).unwrap();
        let kits = build_kits(&r.index);
        let names: Vec<&str> = kits.iter().map(|k| k.name.as_str()).collect();
        assert_eq!(names, vec!["808", "909", "Drum Kit 01"]);
        let k909 = kits.iter().find(|k| k.name == "909").unwrap();
        assert_eq!(k909.slots.len(), 5);
        assert!(k909.slots.contains_key("open_hat") && k909.slots.contains_key("clap"));
        let open = r.index.find(&k909.slots["open_hat"]).unwrap();
        assert_eq!(open.name, "909 OH");
        assert_eq!(default_kit(&kits).unwrap().name, "909");
        // The legacy kit has no clap and no perc.
        let leg = kits.iter().find(|k| k.name == "Drum Kit 01").unwrap();
        assert_eq!(leg.slots.len(), 3);
    }
}
