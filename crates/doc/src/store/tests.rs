// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::document::{Document, apply, apply_batch};
use crate::starter::starter_project;
use protocol::edit::{Edit, NewInstrument};
use protocol::ids::TrackId;
use std::sync::atomic::{AtomicU32, Ordering};

pub(crate) struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new() -> TempDir {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "libredaw-store-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn tempo(d: &Document, bpm: f64) -> Document {
    apply(d, &Edit::SetTempo { bpm }).unwrap().0
}

fn commit(s: &mut HistoryStore, d: &Document, parent: Option<&str>, ms: u64) -> String {
    s.write_commit(&d.project, d.next_id, parent, "main", "user", "edit", ms)
        .unwrap()
}

/// A document with a CLAP channel that names a state blob.
fn clap_doc() -> Document {
    let (d, _) = apply(
        &Document::new(),
        &Edit::AddChannel {
            name: "Surge".into(),
            instrument: NewInstrument::Clap {
                plugin_id: "a.b".into(),
                preset: None,
            },
            root_key: 60,
            track: TrackId::MASTER,
        },
    )
    .unwrap();
    let inst = match &d.project.channels[0].instrument {
        protocol::model::Instrument::Clap(r) => r.instance,
        _ => unreachable!(),
    };
    apply(
        &d,
        &Edit::CommitPluginState {
            instance: inst,
            state_file: format!("{}-1.bin", inst.0),
        },
    )
    .unwrap()
    .0
}

#[test]
fn commits_round_trip_through_a_reopened_store() {
    let t = TempDir::new();
    let d = starter_project();
    let mut s = HistoryStore::open(&t.0).unwrap();
    assert!(s.is_empty());
    let a = commit(&mut s, &d, None, 1000);
    let d2 = tempo(&d, 140.0);
    let b = commit(&mut s, &d2, Some(&a), 2000);
    assert_ne!(a, b);
    assert_eq!(a.len(), 64);
    s.write_head("main", &b).unwrap();

    let mut s = HistoryStore::open(&t.0).unwrap();
    assert_eq!(s.commit_count(), 2);
    assert_eq!(s.head(), Some(("main", b.as_str())));
    let (p, next) = s.load_project(&a).unwrap();
    assert_eq!(p, *d.project);
    assert_eq!(next, d.next_id);
    let (p2, _) = s.load_project(&b).unwrap();
    assert_eq!(p2.tempo_bpm, 140.0);
    assert_eq!(s.commit(&b).unwrap().parent.as_deref(), Some(a.as_str()));
}

#[test]
fn identical_commits_have_the_same_id_and_unchanged_nodes_are_shared() {
    let t = TempDir::new();
    let d = starter_project();
    let mut s = HistoryStore::open(&t.0).unwrap();
    let a = commit(&mut s, &d, None, 1000);
    assert_eq!(commit(&mut s, &d, None, 1000), a);
    let files = |sub: &str| {
        fs::read_dir(t.0.join(HISTORY_DIR).join(sub))
            .unwrap()
            .count()
    };
    let objects = files("objects");
    // 4 channels + 4 patterns + 5 tracks + root.
    assert_eq!(objects, 14);
    // A tempo change adds only a new root object and a commit.
    commit(&mut s, &tempo(&d, 90.0), Some(&a), 2000);
    assert_eq!(files("objects"), objects + 1);
    assert_eq!(files("commits"), 2);
    // Touching one pattern adds that pattern plus the root.
    let pat = d.project.patterns[0].id;
    let d3 = apply(
        &d,
        &Edit::SetStep {
            pattern: pat,
            step: 3,
            on: true,
            vel: None,
        },
    )
    .unwrap()
    .0;
    commit(&mut s, &d3, Some(&a), 3000);
    assert_eq!(files("objects"), objects + 3);
}

#[test]
fn keep_names_every_sample_and_blob_any_commit_references() {
    let t = TempDir::new();
    let d = clap_doc();
    let blob = match &d.project.channels[0].instrument {
        protocol::model::Instrument::Clap(r) => r.state_file.clone().unwrap(),
        _ => unreachable!(),
    };
    let with_sample = apply(
        &d,
        &Edit::AddSample {
            sample: SampleRef {
                hash: "ab".repeat(32),
                orig_name: "k.wav".into(),
                size: 1,
                local_only: false,
            },
        },
    )
    .unwrap()
    .0;
    let mut s = HistoryStore::open(&t.0).unwrap();
    let a = commit(&mut s, &with_sample, None, 1);
    // The newest state no longer has either.
    commit(&mut s, &Document::new(), Some(&a), 2);
    assert!(s.keep().blobs.contains(&blob));
    assert!(s.keep().samples.contains(&"ab".repeat(32)));
    // And after reopening, from disk alone.
    let s = HistoryStore::open(&t.0).unwrap();
    assert!(s.keep().blobs.contains(&blob));
    assert!(s.keep().samples.contains(&"ab".repeat(32)));
}

#[test]
fn damaged_or_partial_files_are_ignored() {
    let t = TempDir::new();
    let d = starter_project();
    let mut s = HistoryStore::open(&t.0).unwrap();
    let a = commit(&mut s, &d, None, 1);
    let b = commit(&mut s, &tempo(&d, 100.0), Some(&a), 2);
    let path =
        t.0.join(HISTORY_DIR)
            .join("commits")
            .join(format!("{b}.toml"));
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("100", "101").replace("edit", "edat")).unwrap();
    fs::write(t.0.join(HISTORY_DIR).join("commits").join("junk.toml"), "x").unwrap();
    fs::write(
        t.0.join(HISTORY_DIR)
            .join("commits")
            .join(format!("{a}.toml.tmp")),
        "x",
    )
    .unwrap();
    let s = HistoryStore::open(&t.0).unwrap();
    assert_eq!(
        s.commit_count(),
        1,
        "the altered commit no longer matches its hash"
    );
    assert!(s.commit(&a).is_some());
}

/// Writes a small history with a branch, a version, and HEAD, through the
/// persister. Returns the batches.
fn batches(d0: &Document) -> Vec<PendingBatch> {
    let d1 = tempo(d0, 130.0);
    let d2 = tempo(&d1, 140.0);
    let c = |entry, project: &Document, parent, branch: &str| PendingCommit {
        entry,
        project: project.project.clone(),
        next_id: project.next_id,
        parent,
        branch: branch.into(),
        author: "user".into(),
        description: format!("entry {entry}"),
        unix_ms: 1000 + entry,
    };
    vec![
        PendingBatch {
            commits: vec![c(0, d0, None, "main"), c(1, &d1, Some(0), "main")],
            branches: vec![PendingBranch {
                id: "main".into(),
                name: "Main".into(),
                head: 1,
                base: 0,
                author: "user".into(),
                archived: false,
            }],
            versions: vec![("First Draft".into(), 1)],
            head: Some(("main".into(), 1)),
        },
        PendingBatch {
            commits: vec![c(2, &d2, Some(1), "alt")],
            branches: vec![PendingBranch {
                id: "alt".into(),
                name: "Darker".into(),
                head: 2,
                base: 1,
                author: "agent:x".into(),
                archived: false,
            }],
            versions: vec![],
            head: Some(("alt".into(), 2)),
        },
    ]
}

/// Whatever a crash leaves, the store opens, every commit loads, and refs
/// only name commits that exist.
fn assert_consistent(dir: &Path) {
    let mut s = HistoryStore::open(dir).unwrap();
    let hashes: Vec<String> = s.commits().map(|(h, _)| h.clone()).collect();
    for h in &hashes {
        s.load_project(h).unwrap_or_else(|e| panic!("{h}: {e}"));
    }
    for b in s.branch_infos() {
        assert!(s.commit(&b.head).is_some() && s.commit(&b.base).is_some());
    }
    for v in s.versions() {
        assert!(s.commit(&v.commit).is_some());
    }
    if let Some((_, c)) = s.head() {
        assert!(s.commit(c).is_some());
    }
}

#[test]
fn a_crash_between_any_two_writes_leaves_a_consistent_store() {
    let d0 = starter_project();
    let all = batches(&d0);
    // First count the steps of an uninterrupted run.
    let steps = {
        let t = TempDir::new();
        let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let n2 = n.clone();
        let mut s = HistoryStore::open(&t.0).unwrap();
        s.set_hook(Some(Box::new(move |_| {
            n2.fetch_add(1, Ordering::Relaxed);
            true
        })));
        let mut p = Persister::new(s, HashMap::new());
        for b in &all {
            p.write(b).unwrap();
        }
        n.load(Ordering::Relaxed)
    };
    assert!(steps > 8, "{steps}");
    for stop_at in 1..=steps {
        let t = TempDir::new();
        let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let n2 = n.clone();
        let mut s = HistoryStore::open(&t.0).unwrap();
        s.set_hook(Some(Box::new(move |_| {
            n2.fetch_add(1, Ordering::Relaxed) + 1 < stop_at
        })));
        let mut p = Persister::new(s, HashMap::new());
        let mut crashed = false;
        for b in &all {
            if let Err(e) = p.write(b) {
                assert!(matches!(e, StoreError::Crashed(_)), "{e}");
                crashed = true;
                break;
            }
        }
        assert!(crashed, "step {stop_at} should have crashed");
        assert_consistent(&t.0);
        // Restarting from what is on disk and writing everything again
        // (commits are content addressed, so repeats are harmless) completes.
        let s = HistoryStore::open(&t.0).unwrap();
        let mut p = Persister::new(s, HashMap::new());
        for b in &all {
            p.write(b).unwrap();
        }
        assert_consistent(&t.0);
        let s = HistoryStore::open(&t.0).unwrap();
        assert_eq!(s.commit_count(), 3);
        assert_eq!(s.branch_infos().len(), 2);
        assert_eq!(s.versions().count(), 1);
    }
}

#[test]
fn compact_keeps_named_versions_heads_and_recent_commits() {
    let t = TempDir::new();
    let d = starter_project();
    let mut s = HistoryStore::open(&t.0).unwrap();
    let mut parent: Option<String> = None;
    let mut ids = Vec::new();
    for i in 0..6u64 {
        let h = commit(
            &mut s,
            &tempo(&d, 100.0 + i as f64),
            parent.as_deref(),
            1000 + i,
        );
        parent = Some(h.clone());
        ids.push(h);
    }
    s.write_version("Keeper", &ids[1]).unwrap();
    s.write_head("main", &ids[5]).unwrap();
    s.write_branch(&BranchMeta {
        id: "main".into(),
        name: "Main".into(),
        head: ids[5].clone(),
        base: ids[0].clone(),
        author: "user".into(),
        archived: false,
    })
    .unwrap();
    // Keep commits from time 1004 on: indexes 4 and 5, plus version 1, the
    // branch base 0, and the head 5.
    let r = s.compact(1004).unwrap();
    assert_eq!(r.commits_removed, 2, "indexes 2 and 3 go");
    assert!(r.objects_removed >= 2);
    let mut s = HistoryStore::open(&t.0).unwrap();
    assert_eq!(s.commit_count(), 4);
    for i in [0, 1, 4, 5] {
        s.load_project(&ids[i]).unwrap();
    }
    // The commit after a dropped one is a root as far as the tree shows.
    let nodes = s.nodes(None, 0);
    let n4 = nodes.iter().find(|n| n.commit == ids[4]).unwrap();
    assert_eq!(n4.parent, None);
    assert_eq!(
        nodes
            .iter()
            .find(|n| n.commit == ids[1])
            .unwrap()
            .name
            .as_deref(),
        Some("Keeper")
    );
    assert_consistent(&t.0);
    // Nothing named by a kept commit was deleted: an apply of every kept
    // project still works.
    apply_batch(
        &Document::from_project(s.load_project(&ids[5]).unwrap().0, 100),
        &[],
    )
    .unwrap();
}

#[test]
fn slugs_are_safe_file_names() {
    assert_eq!(slugify("Version A: darker!"), "version-a-darker");
    assert_eq!(slugify("../../etc"), "etc");
    assert_eq!(slugify("   "), "version");
    assert_eq!(slugify(&"x".repeat(100)).len(), 40);
}
