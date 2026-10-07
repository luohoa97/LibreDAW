// SPDX-License-Identifier: GPL-3.0-or-later
//! Indexes the owner's real FL Studio install, read only, and prints the
//! numbers. Run with `cargo test -p libredaw-library --test real_install
//! -- --ignored --nocapture`. Nothing is copied; the cache goes to a
//! temp folder.

use std::collections::BTreeMap;
use std::time::Instant;

use library::{Source, build_instruments, build_kits, default_kit, detect_installs, scan};

#[test]
#[ignore = "needs a real FL Studio install"]
fn index_real_install() {
    let t = Instant::now();
    let installs = detect_installs();
    println!("detect: {} install(s) in {:?}", installs.len(), t.elapsed());
    let Some(inst) = installs.first() else {
        println!("no FL Studio install here; nothing to do");
        return;
    };
    println!(
        "{} ({}) at {}",
        inst.name,
        inst.version,
        inst.packs.display()
    );

    let cache = std::env::temp_dir().join(format!("libredaw-library-real-{}", std::process::id()));
    let src = Source::fl_studio(inst);
    let cold = scan(&src, &cache).unwrap();
    println!("cold: {:?} {:?}", cold.stats.elapsed, cold.stats);
    let warm = scan(&src, &cache).unwrap();
    println!("warm: {:?} {:?}", warm.stats.elapsed, warm.stats);
    assert_eq!(cold.index.entries, warm.index.entries);

    for (role, n) in warm.index.counts_by_role() {
        println!("  {role:<11} {n}");
    }
    let mut formats: BTreeMap<&str, usize> = BTreeMap::new();
    for e in &warm.index.entries {
        *formats.entry(e.format.as_str()).or_default() += 1;
    }
    println!("formats: {formats:?}");
    let with_tempo = warm
        .index
        .entries
        .iter()
        .filter(|e| e.tempo_bpm.is_some())
        .count();
    let with_key = warm
        .index
        .entries
        .iter()
        .filter(|e| e.key.is_some())
        .count();
    println!("tempo in name: {with_tempo}, key in name: {with_key}");
    println!("total {} entries", warm.index.entries.len());

    let kits = build_kits(&warm.index);
    println!(
        "{} kits, default {:?}",
        kits.len(),
        default_kit(&kits).map(|k| &k.name)
    );
    for k in kits.iter().take(40) {
        println!(
            "  kit {:<28} {:<34} {} slots",
            k.name,
            k.pack,
            k.slots.len()
        );
    }

    let inst = build_instruments(&warm.index);
    println!("{} multisampled instruments", inst.len());
    for i in &inst {
        println!(
            "  {:<24} {:<10} {:>3} zones  roots from {:<14} ordered={}",
            i.name,
            i.role,
            i.zones.len(),
            i.root_source,
            i.ordered
        );
    }

    for (n, c) in library::unplaced_instruments(&warm.index) {
        println!("  unplaced: {n} x{c}");
    }

    // Check the note-name convention (C4 = MIDI 60) against the audio.
    let mut offsets: BTreeMap<i32, usize> = BTreeMap::new();
    for e in warm
        .index
        .entries
        .iter()
        .filter(|e| e.root_source.as_deref() == Some("name"))
    {
        if let Ok(Some(p)) = library::pitch::estimate_root(&e.path) {
            *offsets
                .entry(i32::from(p) - i32::from(e.root_note.unwrap_or(0)))
                .or_default() += 1;
        }
    }
    println!("pitch minus named root (semitones: count): {offsets:?}");
    let _ = std::fs::remove_dir_all(&cache);
}
