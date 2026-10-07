// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::bundle::{
    Keep, PROJECT_FILE, SaveStep, load, save, save_autosave, save_full, save_keeping,
    save_with_hook,
};
use crate::document::{Document, apply};
use crate::history::{Author, Editor, Scope, Submitted};
use crate::sha256::sha256_hex;
use protocol::beats::SampleMode;
use protocol::edit::{Edit, NewInstrument};
use protocol::ids::TrackId;

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "libredaw-samples-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn bundle(&self) -> PathBuf {
        self.0.join("Song.ldaw")
    }
    fn registry(&self) -> PathBuf {
        self.0
            .join("data")
            .join("libredaw")
            .join(LOCAL_SAMPLES_FILE)
    }
    /// A WAV-looking file with recognizable content.
    fn wav(&self, name: &str, seed: u8) -> PathBuf {
        let p = self.0.join("lib").join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, wav_bytes(seed)).unwrap();
        p
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 4 KiB: a RIFF/WAVE header, then bytes from a small generator, so the
/// content is distinctive and differs per seed.
fn wav_bytes(seed: u8) -> Vec<u8> {
    let mut v = b"RIFF\x00\x10\x00\x00WAVEfmt ".to_vec();
    let mut x = 0x1234_5678u32 ^ (seed as u32).wrapping_mul(0x9E37_79B1);
    while v.len() < 4096 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        v.push((x >> 8) as u8);
    }
    v
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn all_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd {
        let p = e.unwrap().path();
        if p.is_dir() {
            all_files(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn doc_with(refs: &[SampleRef]) -> Document {
    let mut d = Document::new();
    for s in refs {
        d = apply(&d, &Edit::AddSample { sample: s.clone() }).unwrap().0;
    }
    d
}

fn sample_files(bundle: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(bundle.join(SAMPLES_DIR))
        .map(|rd| {
            rd.map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

// ---------------------------------------------------------------------------

#[test]
fn registry_round_trips_awkward_paths() {
    let t = Tmp::new();
    let mut r = LocalSamples::load(&t.registry());
    assert!(r.entries.is_empty());
    r.entries.insert(
        sha256_hex(b"a"),
        PathBuf::from("/music/with \"quotes\" and \\ backslash/kick (1).wav"),
    );
    r.entries.insert(
        sha256_hex(b"b"),
        PathBuf::from("/m\u{fc}sic/\u{1f941}/tab\tnew\nline.wav"),
    );
    r.entries
        .insert(sha256_hex(b"c"), PathBuf::from("/plain.wav"));
    r.save().unwrap();
    let back = LocalSamples::load(&t.registry());
    assert_eq!(back, r);
    // Saving again writes the same bytes.
    let text = fs::read_to_string(t.registry()).unwrap();
    back.save().unwrap();
    assert_eq!(fs::read_to_string(t.registry()).unwrap(), text);
    assert!(!t.registry().with_extension("toml.tmp").exists());
}

#[test]
fn registry_tolerates_junk() {
    let t = Tmp::new();
    fs::create_dir_all(t.registry().parent().unwrap()).unwrap();
    let h = sha256_hex(b"ok");
    fs::write(
        t.registry(),
        format!(
            "garbage\n[[sample]]\nhash = \"nothex\"\npath = \"/x\"\n[[sample]]\nhash = \"{h}\"\npath = \"relative.wav\"\n[[sample]]\nhash = \"{h}\"\npath = \"/ok.wav\" # c\n[[sample]]\nhash = \"{h}\"\n"
        ),
    )
    .unwrap();
    let r = LocalSamples::load(&t.registry());
    assert_eq!(r.entries.len(), 1);
    assert_eq!(r.get(&h), Some(Path::new("/ok.wav")));
}

#[test]
fn import_copies_into_the_bundle_by_hash() {
    let t = Tmp::new();
    let src = t.wav("Kick 01.wav", 1);
    let bytes = fs::read(&src).unwrap();
    let r = import_sample_with(&t.bundle(), &src, false, &t.registry()).unwrap();
    assert_eq!(r.hash, sha256_hex(&bytes));
    assert_eq!(
        (r.orig_name.as_str(), r.size, r.local_only),
        ("Kick 01.wav", 4096, false)
    );
    let stored = t.bundle().join(SAMPLES_DIR).join(format!("{}.wav", r.hash));
    assert_eq!(fs::read(&stored).unwrap(), bytes);
    assert_eq!(
        sample_files(&t.bundle()),
        [format!("{}.wav", r.hash)],
        "no tmp left"
    );
    assert!(
        !t.registry().exists(),
        "bundle samples are not registered locally"
    );
    // The same content from another file name: one stored file.
    let twin = t.wav("copy of kick.wav", 1);
    let r2 = import_sample_with(&t.bundle(), &twin, false, &t.registry()).unwrap();
    assert_eq!(
        (r2.hash.as_str(), r2.orig_name.as_str()),
        (r.hash.as_str(), "copy of kick.wav")
    );
    assert_eq!(sample_files(&t.bundle()).len(), 1);
    // Different content: another file.
    let other = t.wav("snare.wav", 2);
    let r3 = import_sample_with(&t.bundle(), &other, false, &t.registry()).unwrap();
    assert_ne!(r3.hash, r.hash);
    assert_eq!(sample_files(&t.bundle()).len(), 2);
}

#[test]
fn an_existing_sample_is_never_overwritten() {
    let t = Tmp::new();
    let src = t.wav("k.wav", 1);
    let r = import_sample_with(&t.bundle(), &src, false, &t.registry()).unwrap();
    let stored = t.bundle().join(SAMPLES_DIR).join(sample_file_name(&r.hash));
    // Mark the stored file so an overwrite would show.
    let mut perm = fs::metadata(&stored).unwrap().permissions();
    perm.set_readonly(true);
    fs::set_permissions(&stored, perm).unwrap();
    let before = fs::metadata(&stored).unwrap().modified().unwrap();
    let again = import_sample_with(&t.bundle(), &src, false, &t.registry()).unwrap();
    assert_eq!(again, r);
    assert_eq!(fs::metadata(&stored).unwrap().modified().unwrap(), before);
    // A stored file of the wrong size under that name is reported, not replaced.
    fs::remove_file(&stored).unwrap();
    fs::write(&stored, b"short").unwrap();
    assert!(matches!(
        import_sample_with(&t.bundle(), &src, false, &t.registry()),
        Err(BundleError::SampleConflict(h)) if h == r.hash
    ));
    assert_eq!(fs::read(&stored).unwrap(), b"short");
    assert_eq!(sample_files(&t.bundle()).len(), 1, "tmp copy removed");
}

#[test]
fn import_rejects_what_is_not_a_wav() {
    let t = Tmp::new();
    let dir = t.0.join("lib");
    fs::create_dir_all(&dir).unwrap();
    let mut cases: Vec<(PathBuf, Vec<u8>)> = vec![
        (dir.join("a.wav"), b"RIFF0000AVI LIST".to_vec()),
        (dir.join("b.wav"), b"RIFF".to_vec()),
        (dir.join("c.wav"), Vec::new()),
        (dir.join("d.mp3"), b"ID3\x04 not a wav at all".to_vec()),
    ];
    for (p, b) in cases.drain(..) {
        fs::write(&p, b).unwrap();
        assert!(
            matches!(
                import_sample_with(&t.bundle(), &p, false, &t.registry()),
                Err(BundleError::NotWav(_))
            ),
            "{}",
            p.display()
        );
    }
    assert!(matches!(
        import_sample_with(&t.bundle(), &dir, false, &t.registry()),
        Err(BundleError::NotWav(_))
    ));
    assert!(matches!(
        import_sample_with(&t.bundle(), &dir.join("missing.wav"), false, &t.registry()),
        Err(BundleError::Io { .. })
    ));
    let long = dir.join(format!("{}.wav", "x".repeat(200)));
    fs::write(&long, wav_bytes(1)).unwrap();
    assert!(matches!(
        import_sample_with(&t.bundle(), &long, false, &t.registry()),
        Err(BundleError::BadName(_))
    ));
    assert!(sample_files(&t.bundle()).is_empty());
}

#[test]
fn local_only_import_records_hash_and_absolute_path_and_copies_nothing() {
    let t = Tmp::new();
    let src = t.wav("Bought Kick.wav", 3);
    let r = import_sample_with(&t.bundle(), &src, true, &t.registry()).unwrap();
    assert!(r.local_only);
    assert_eq!(r.hash, sha256_hex(&fs::read(&src).unwrap()));
    assert!(sample_files(&t.bundle()).is_empty());
    assert!(!t.bundle().join(SAMPLES_DIR).exists() || sample_files(&t.bundle()).is_empty());
    let reg = LocalSamples::load(&t.registry());
    let got = reg.get(&r.hash).expect("registered");
    assert!(got.is_absolute());
    assert_eq!(fs::canonicalize(&src).unwrap(), got);
    // Importing again does not change the file.
    let text = fs::read_to_string(t.registry()).unwrap();
    import_sample_with(&t.bundle(), &src, true, &t.registry()).unwrap();
    assert_eq!(fs::read_to_string(t.registry()).unwrap(), text);
    // A second sample is added next to the first.
    let src2 = t.wav("Other.wav", 4);
    let r2 = import_sample_with(&t.bundle(), &src2, true, &t.registry()).unwrap();
    assert_eq!(LocalSamples::load(&t.registry()).entries.len(), 2);
    assert_ne!(r.hash, r2.hash);
}

/// Test 17.2: Save and autosave never contain a byte of a local_only file.
#[test]
fn save_and_autosave_never_contain_a_local_only_file() {
    let t = Tmp::new();
    let local_src = t.wav("Bought Kick.wav", 5);
    let shared_src = t.wav("Own Snare.wav", 6);
    let local_bytes = fs::read(&local_src).unwrap();
    let shared_bytes = fs::read(&shared_src).unwrap();
    let local = import_sample_with(&t.bundle(), &local_src, true, &t.registry()).unwrap();
    let shared = import_sample_with(&t.bundle(), &shared_src, false, &t.registry()).unwrap();

    let mut d = doc_with(&[local.clone(), shared.clone()]);
    for s in [&local, &shared] {
        d = apply(
            &d,
            &Edit::AddChannel {
                name: s.orig_name.clone(),
                instrument: NewInstrument::Sampler {
                    sample: Some(s.hash.clone()),
                    mode: SampleMode::OneShot,
                },
                root_key: 60,
                track: TrackId::MASTER,
            },
        )
        .unwrap()
        .0;
    }
    save(&t.bundle(), &d).unwrap();
    save_autosave(&t.bundle(), &d).unwrap();
    save_keeping(&t.bundle(), &d, &Keep::default()).unwrap();

    let mut files = Vec::new();
    all_files(&t.bundle(), &mut files);
    assert!(files.len() >= 4, "{files:?}");
    let probe = &local_bytes[1000..1300];
    let (mut found_shared, mut found_local) = (false, false);
    for f in &files {
        let b = fs::read(f).unwrap();
        found_local |= contains(&b, probe) || contains(&b, &local_bytes[..64]);
        found_shared |= contains(&b, &shared_bytes[1000..1300]);
        assert_ne!(
            f.file_name().unwrap().to_string_lossy(),
            sample_file_name(&local.hash),
            "{}",
            f.display()
        );
    }
    assert!(
        !found_local,
        "a byte range of the local-only file is in the bundle"
    );
    assert!(
        found_shared,
        "the probe finds a bundled sample, so it can detect a leak"
    );

    // The project names the local sample by hash only and git ignores it.
    let text = fs::read_to_string(t.bundle().join(PROJECT_FILE)).unwrap();
    assert!(text.contains(&local.hash) && text.contains("local_only = true"));
    let ignore = fs::read_to_string(t.bundle().join(".gitignore")).unwrap();
    assert!(ignore.contains(&format!("/samples/{}.wav", local.hash)));
    assert!(!ignore.contains(&shared.hash));
    assert_eq!(sample_files(&t.bundle()), [sample_file_name(&shared.hash)]);
}

#[test]
fn collection_keeps_what_documents_history_and_autosave_reference() {
    let t = Tmp::new();
    let mut refs = Vec::new();
    for (i, name) in ["a.wav", "b.wav", "c.wav", "d.wav"].iter().enumerate() {
        let src = t.wav(name, 10 + i as u8);
        refs.push(import_sample_with(&t.bundle(), &src, false, &t.registry()).unwrap());
    }
    let file = |i: usize| sample_file_name(&refs[i].hash);
    // A leftover import tmp file is ours to remove.
    fs::write(t.bundle().join(SAMPLES_DIR).join(".import-1-1.tmp"), b"x").unwrap();
    // And a file that is not ours stays.
    fs::write(t.bundle().join(SAMPLES_DIR).join("notes.txt"), b"x").unwrap();

    // The autosave names c; the history names d; the document names a.
    save_autosave(&t.bundle(), &doc_with(&[refs[2].clone()])).unwrap();
    let mut keep = Keep::default();
    keep.add_project(&doc_with(&[refs[3].clone()]).project);
    let r = save_full(
        &t.bundle(),
        &doc_with(&[refs[0].clone()]),
        &keep,
        &mut |_| true,
    )
    .unwrap();
    assert_eq!(r.samples_deleted, [".import-1-1.tmp".to_string(), file(1)]);
    let mut want = vec![file(0), file(2), file(3), "notes.txt".to_string()];
    want.sort();
    assert_eq!(sample_files(&t.bundle()), want);

    // Without the history and with the autosave cleared, only a stays.
    crate::bundle::clear_autosave(&t.bundle()).unwrap();
    let r = save(&t.bundle(), &doc_with(&[refs[0].clone()])).unwrap();
    let mut gone = vec![file(2), file(3)];
    gone.sort();
    assert_eq!(r.samples_deleted, gone);
    assert_eq!(
        sample_files(&t.bundle()),
        [file(0), "notes.txt".to_string()]
    );

    // An autosave copy never collects samples (they belong to the bundle).
    let r = save_autosave(&t.bundle(), &Document::new()).unwrap();
    assert!(r.samples_deleted.is_empty());
    assert_eq!(sample_files(&t.bundle()).len(), 2);
    assert!(!t.bundle().join(AUTOSAVE_DIR).join(SAMPLES_DIR).exists());
}

#[test]
fn history_keeps_samples_that_undo_removed() {
    let t = Tmp::new();
    let src = t.wav("k.wav", 20);
    let r = import_sample_with(&t.bundle(), &src, false, &t.registry()).unwrap();
    let mut e = Editor::new(Document::new(), true);
    let Submitted::Applied(_) = e
        .submit(
            Author::User,
            None,
            vec![Edit::AddSample { sample: r.clone() }],
            0,
        )
        .unwrap()
    else {
        panic!()
    };
    e.undo(&Scope::Any).unwrap();
    assert!(e.document().project.samples.is_empty());
    // A save that only knows the document would collect the file; with the
    // history's keep set the redo still finds it.
    save_keeping(&t.bundle(), e.document(), &e.history().keep()).unwrap();
    assert_eq!(sample_files(&t.bundle()), [sample_file_name(&r.hash)]);
    e.redo(&Scope::Any).unwrap();
    assert_eq!(e.document().project.samples, std::slice::from_ref(&r));
    save(&t.bundle(), e.document()).unwrap();
    assert_eq!(sample_files(&t.bundle()), [sample_file_name(&r.hash)]);
    let w = save(&t.bundle(), &Document::new()).unwrap();
    assert_eq!(w.samples_deleted, [sample_file_name(&r.hash)]);
}

#[test]
fn a_crash_while_collecting_samples_loses_nothing_referenced() {
    let t = Tmp::new();
    let a = import_sample_with(&t.bundle(), &t.wav("a.wav", 1), false, &t.registry()).unwrap();
    let b = import_sample_with(&t.bundle(), &t.wav("b.wav", 2), false, &t.registry()).unwrap();
    let d = doc_with(std::slice::from_ref(&a));
    let mut n = 0;
    let crash = save_with_hook(&t.bundle(), &d, &mut |s| {
        n += 1;
        !matches!(s, SaveStep::SampleDeleted(_))
    });
    assert!(matches!(
        crash,
        Err(BundleError::Crashed(SaveStep::SampleDeleted(_)))
    ));
    assert!(n > 0);
    let l = load(&t.bundle()).unwrap();
    assert_eq!(l.doc.project.samples, std::slice::from_ref(&a));
    assert!(
        t.bundle()
            .join(SAMPLES_DIR)
            .join(sample_file_name(&a.hash))
            .is_file()
    );
    // The next save finishes the collection of b.
    save(&t.bundle(), &d).unwrap();
    assert_eq!(sample_files(&t.bundle()), [sample_file_name(&a.hash)]);
    let _ = b;
}

#[test]
fn a_missing_sample_is_a_placeholder_and_the_project_still_loads_and_saves() {
    let t = Tmp::new();
    let src = t.wav("k.wav", 30);
    let gone = import_sample_with(&t.bundle(), &src, false, &t.registry()).unwrap();
    let local_src = t.wav("l.wav", 31);
    let local = import_sample_with(&t.bundle(), &local_src, true, &t.registry()).unwrap();
    let ok_src = t.wav("o.wav", 32);
    let fine = import_sample_with(&t.bundle(), &ok_src, false, &t.registry()).unwrap();

    let d = doc_with(&[gone.clone(), local.clone(), fine.clone()]);
    save(&t.bundle(), &d).unwrap();
    // Lose the bundle copy and the local file.
    fs::remove_file(
        t.bundle()
            .join(SAMPLES_DIR)
            .join(sample_file_name(&gone.hash)),
    )
    .unwrap();
    fs::remove_file(&local_src).unwrap();

    let l = load(&t.bundle()).unwrap();
    assert_eq!(l.doc.project.samples.len(), 3);
    let reg = LocalSamples::load(&t.registry());
    let mut missing = missing_samples(&t.bundle(), &l.doc.project, &reg);
    missing.sort();
    let mut want = vec![gone.hash.clone(), local.hash.clone()];
    want.sort();
    assert_eq!(missing, want);
    // Saving keeps the references and what is still there.
    let r = save(&t.bundle(), &l.doc).unwrap();
    assert!(r.samples_deleted.is_empty());
    let again = load(&t.bundle()).unwrap();
    assert_eq!(again.doc.project.samples, d.project.samples);
    assert_eq!(sample_files(&t.bundle()), [sample_file_name(&fine.hash)]);
    // And an autosave of the damaged project works too.
    save_autosave(&t.bundle(), &l.doc).unwrap();
}

#[test]
fn resolve_finds_bundle_copies_and_registered_local_files() {
    let t = Tmp::new();
    let shared =
        import_sample_with(&t.bundle(), &t.wav("s.wav", 40), false, &t.registry()).unwrap();
    let local_src = t.wav("l.wav", 41);
    let local = import_sample_with(&t.bundle(), &local_src, true, &t.registry()).unwrap();
    let reg = LocalSamples::load(&t.registry());
    let p = resolve_sample(&t.bundle(), &shared, &reg).unwrap();
    assert_eq!(
        p,
        t.bundle()
            .join(SAMPLES_DIR)
            .join(sample_file_name(&shared.hash))
    );
    assert_eq!(
        resolve_sample(&t.bundle(), &local, &reg).unwrap(),
        fs::canonicalize(&local_src).unwrap()
    );
    // An autosave copy looks in the parent bundle.
    assert_eq!(
        resolve_sample(&t.bundle().join(AUTOSAVE_DIR), &shared, &reg).unwrap(),
        p
    );
    // Without a registry entry a local sample is missing.
    let empty = LocalSamples::load(&t.0.join("nowhere.toml"));
    assert!(resolve_sample(&t.bundle(), &local, &empty).is_none());
}

#[test]
fn gitignore_block_is_managed_and_keeps_user_lines() {
    let t = Tmp::new();
    let b = t.bundle();
    fs::create_dir_all(&b).unwrap();
    let h1 = sha256_hex(b"1");
    let h2 = sha256_hex(b"2");
    // Nothing to ignore: no file.
    update_gitignore(&b, &[]).unwrap();
    assert!(!b.join(".gitignore").exists());
    update_gitignore(&b, &[&h2, &h1, &h1]).unwrap();
    let text = fs::read_to_string(b.join(".gitignore")).unwrap();
    assert_eq!(text, gitignore_block(&[&h1, &h2]));
    assert_eq!(text.matches(&h1).count(), 1);
    assert!(text.contains("/.autosave/"));
    // Idempotent.
    update_gitignore(&b, &[&h1, &h2]).unwrap();
    assert_eq!(fs::read_to_string(b.join(".gitignore")).unwrap(), text);
    // User lines before and after survive an update and a removal.
    fs::write(b.join(".gitignore"), format!("mine\n{text}after\n")).unwrap();
    update_gitignore(&b, &[&h1]).unwrap();
    let t2 = fs::read_to_string(b.join(".gitignore")).unwrap();
    assert!(t2.starts_with("mine\n") && t2.contains("after\n") && !t2.contains(&h2));
    update_gitignore(&b, &[]).unwrap();
    assert_eq!(
        fs::read_to_string(b.join(".gitignore")).unwrap(),
        "mine\nafter\n"
    );
    // A file that only held our block is removed with it.
    fs::write(b.join(".gitignore"), &text).unwrap();
    update_gitignore(&b, &[]).unwrap();
    assert!(!b.join(".gitignore").exists());
}

#[test]
fn sample_file_names_parse_only_our_form() {
    let h = sha256_hex(b"x");
    assert_eq!(
        parse_sample_file_name(&sample_file_name(&h)),
        Some(h.as_str())
    );
    for bad in [
        "x.wav",
        "notes.txt",
        &format!("{h}.WAV"),
        &format!("{}.wav", h.to_uppercase()),
        &format!("{h}.wav.tmp"),
    ] {
        assert_eq!(parse_sample_file_name(bad), None, "{bad}");
    }
    assert_eq!(
        hashes_in_text(&format!("hash = \"{h}\"\nx = \"short\"\n")).len(),
        1
    );
}
