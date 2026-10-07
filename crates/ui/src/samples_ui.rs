// SPDX-License-Identifier: GPL-3.0-or-later
//! Getting samples into a project (SPEC 15.1, 15.3, 17.2): choose or drop
//! WAV files, copy them into the project's `samples/` folder on a worker
//! thread (checking the pack index hash for pack sounds), then register
//! them and make or change a sampler channel. Everything undoable is one
//! undo step; errors are toasts.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::gdk;
use gtk::gio;
use gtk::prelude::*;

use protocol::beats::SampleMode;
use protocol::edit::Edit;
use protocol::ids::ChannelId;
use protocol::model::SampleRef;

use crate::app::App;
use crate::channels::{self, NewChannel, SamplerSetup};
use crate::soundlib::{self, Kit, Piece, Source};

/// One file to bring in.
#[derive(Clone, Debug)]
pub struct ImportItem {
    pub path: PathBuf,
    /// Keep the file where it is and record it in the per-machine registry
    /// (17.2) instead of copying it into the project.
    pub local_only: bool,
    /// The hash a pack index lists for this file, if any.
    pub expect_sha256: Option<String>,
}

impl ImportItem {
    pub fn file(path: PathBuf) -> ImportItem {
        ImportItem {
            path,
            local_only: false,
            expect_sha256: None,
        }
    }

    pub fn piece(piece: &Piece, source: Source) -> ImportItem {
        ImportItem {
            path: piece.path.clone(),
            local_only: source == Source::UserFolder,
            expect_sha256: piece.sha256.clone(),
        }
    }
}

/// Copies or records one file. Runs on a worker thread.
fn import_one(home: &Path, item: &ImportItem) -> Result<SampleRef, String> {
    if let Some(want) = &item.expect_sha256 {
        let bytes = std::fs::read(&item.path)
            .map_err(|e| format!("Cannot read {}: {e}", item.path.display()))?;
        if !doc::sha256::sha256_hex(&bytes).eq_ignore_ascii_case(want) {
            return Err(format!(
                "{} does not match the pack index, so it was not added",
                item.path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            ));
        }
    }
    doc::samples::import_sample(home, &item.path, item.local_only).map_err(|e| e.to_string())
}

/// Imports `items` off the GTK thread, then calls `then` there with one
/// result per item. Without a project folder to hold the samples it reports
/// an error for every item.
pub fn import(
    app: &Rc<App>,
    items: Vec<ImportItem>,
    then: impl FnOnce(Vec<Result<SampleRef, String>>) + 'static,
) {
    let Some(home) = app.session.borrow().sample_home().map(Path::to_path_buf) else {
        let n = items.len();
        then(vec![
            Err(
                "There is no project folder for samples yet".to_string()
            );
            n
        ]);
        return;
    };
    let any_local = items.iter().any(|i| i.local_only);
    let a = app.clone();
    app.tasks.spawn(
        "import-samples",
        move || {
            items
                .iter()
                .map(|i| import_one(&home, i))
                .collect::<Vec<_>>()
        },
        move |results| {
            if any_local {
                a.session.borrow_mut().reload_local_samples();
            }
            then(results);
        },
    );
}

/// Shows the first error of a batch as a toast. Returns the good refs.
fn good(app: &App, results: Vec<Result<SampleRef, String>>) -> Vec<SampleRef> {
    let mut out = Vec::new();
    let mut shown = false;
    for r in results {
        match r {
            Ok(s) => out.push(s),
            Err(e) if !shown => {
                app.toast(&e);
                shown = true;
            }
            Err(_) => {}
        }
    }
    out
}

/// The setup for a piece of a kit: pitched when the kit gives it a root
/// key, a one-shot otherwise.
pub fn piece_setup(piece: &Piece, sample: SampleRef) -> SamplerSetup {
    SamplerSetup {
        sample: Some(sample),
        name: piece.name.clone(),
        root_key: piece.root_key.unwrap_or(60),
        mode: if piece.root_key.is_some() {
            SampleMode::Pitched
        } else {
            SampleMode::OneShot
        },
        choke_group: piece.choke_group,
        gain_db: piece.gain_db,
        pan: piece.pan,
    }
}

/// A new sampler channel for each WAV file (a one-shot at middle C).
pub fn add_channels_from_files(app: &Rc<App>, paths: Vec<PathBuf>) {
    let wavs: Vec<PathBuf> = paths.into_iter().filter(|p| is_audio_file(p)).collect();
    if wavs.is_empty() {
        app.toast("Only WAV, FLAC, Ogg, MP3 and WavPack files can be used as sounds");
        return;
    }
    let a = app.clone();
    import(
        app,
        wavs.into_iter().map(ImportItem::file).collect(),
        move |results| {
            let refs = good(&a, results);
            let n = refs.len();
            for r in refs {
                channels::add(&a, NewChannel::Sampler(SamplerSetup::one_shot(r)));
            }
            if n > 0 {
                let a2 = a.clone();
                a.toast_action(
                    &if n == 1 {
                        "Added a sound".to_string()
                    } else {
                        format!("Added {n} sounds")
                    },
                    "Undo",
                    move || a2.undo(),
                );
            }
        },
    );
}

/// One piece of a pack or folder as a new sampler channel.
pub fn add_piece(app: &Rc<App>, kit: &Kit, piece: &Piece) {
    let (a, p) = (app.clone(), piece.clone());
    import(
        app,
        vec![ImportItem::piece(piece, kit.source)],
        move |results| {
            if let Some(r) = good(&a, results).into_iter().next() {
                let name = p.name.clone();
                if channels::add(&a, NewChannel::Sampler(piece_setup(&p, r))).is_some() {
                    let a2 = a.clone();
                    a.toast_action(&format!("Added {name}"), "Undo", move || a2.undo());
                }
            }
        },
    );
}

/// Every piece of a kit as channels on one new track.
pub fn add_kit(app: &Rc<App>, kit: &Kit) {
    let (a, kit2) = (app.clone(), kit.clone());
    let items = kit
        .pieces
        .iter()
        .map(|p| ImportItem::piece(p, kit.source))
        .collect();
    import(app, items, move |results| {
        let mut setups = Vec::new();
        let mut failed = 0;
        for (p, r) in kit2.pieces.iter().zip(results) {
            match r {
                Ok(s) => setups.push(piece_setup(p, s)),
                Err(e) => {
                    if failed == 0 {
                        a.toast(&e);
                    }
                    failed += 1;
                }
            }
        }
        let n = setups.len();
        if !channels::add_kit(&a, &format!("{} Kit", kit2.title), setups).is_empty() {
            let a2 = a.clone();
            let skipped = if failed > 0 {
                format!(" ({failed} skipped)")
            } else {
                String::new()
            };
            a.toast_action(
                &format!("Added {n} sounds from {}{skipped}", kit2.title),
                "Undo",
                move || a2.undo(),
            );
        }
    });
}

/// Puts a new sample on an existing sampler channel: one undo step.
pub fn set_channel_sample(app: &Rc<App>, channel: ChannelId, item: ImportItem) {
    let a = app.clone();
    import(app, vec![item], move |results| {
        if let Some(r) = good(&a, results).into_iter().next() {
            let hash = r.hash.clone();
            let grouped = a.gesture_begin("Change sample");
            let run = |e: Vec<Edit>| {
                if grouped {
                    a.gesture_edit(e)
                } else {
                    a.edit(e)
                }
            };
            run(vec![Edit::AddSample { sample: r }]);
            run(vec![Edit::SetSamplerSample {
                channel,
                sample: Some(hash),
            }]);
            if grouped {
                a.gesture_end();
            }
        }
    });
}

/// Whether `p` has an extension audiofile can decode (any case).
pub fn is_audio_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(audiofile::is_supported_ext)
}

fn wav_filters() -> gio::ListStore {
    let f = gtk::FileFilter::new();
    f.set_name(Some("Sounds"));
    for e in audiofile::EXTENSIONS {
        f.add_suffix(e);
    }
    for m in [
        "audio/x-wav",
        "audio/wav",
        "audio/flac",
        "audio/x-flac",
        "audio/ogg",
        "audio/mpeg",
        "audio/x-wavpack",
    ] {
        f.add_mime_type(m);
    }
    let store = gio::ListStore::new::<gtk::FileFilter>();
    store.append(&f);
    store
}

fn window_of(w: &impl IsA<gtk::Widget>) -> Option<gtk::Window> {
    w.root().and_then(|r| r.downcast::<gtk::Window>().ok())
}

/// "Sampler..." in the Add Channel menu: choose WAV files, one channel each.
pub fn choose_for_new_channels(parent: &impl IsA<gtk::Widget>, app: &Rc<App>) {
    let dialog = gtk::FileDialog::builder()
        .title("Choose Sounds")
        .filters(&wav_filters())
        .build();
    let a = app.clone();
    dialog.open_multiple(
        window_of(parent).as_ref(),
        gio::Cancellable::NONE,
        move |res| {
            if let Ok(files) = res {
                let paths: Vec<PathBuf> = (0..files.n_items())
                    .filter_map(|i| files.item(i).and_downcast::<gio::File>())
                    .filter_map(|f| f.path())
                    .collect();
                add_channels_from_files(&a, paths);
            }
        },
    );
}

/// "Choose Sample..." on the Sound page of a sampler channel.
pub fn choose_for_channel(parent: &impl IsA<gtk::Widget>, app: &Rc<App>, channel: ChannelId) {
    let dialog = gtk::FileDialog::builder()
        .title("Choose a Sound")
        .filters(&wav_filters())
        .build();
    let a = app.clone();
    dialog.open(
        window_of(parent).as_ref(),
        gio::Cancellable::NONE,
        move |res| {
            if let Ok(f) = res
                && let Some(p) = f.path()
            {
                if is_audio_file(&p) {
                    set_channel_sample(&a, channel, ImportItem::file(p));
                } else {
                    a.toast("Only WAV, FLAC, Ogg, MP3 and WavPack files can be used as sounds");
                }
            }
        },
    );
}

/// Accepts WAV files dropped from the file manager on `widget`: each
/// becomes a sampler channel (SPEC 15.8.4, 15.1).
pub fn install_drop_target(widget: &impl IsA<gtk::Widget>, app: &Rc<App>) {
    let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    let a = app.clone();
    target.connect_drop(move |_, value, _, _| {
        let Ok(list) = value.get::<gdk::FileList>() else {
            return false;
        };
        let paths: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
        if paths.is_empty() {
            return false;
        }
        add_channels_from_files(&a, paths);
        true
    });
    widget.add_controller(target);
}

/// Where the folders the user added to the sound browser are listed.
/// Every kit LibreDAW can use: installed packs, then the user's folders.
pub fn library(app: &App) -> Vec<Kit> {
    let mut kits = soundlib::discover(&soundlib::default_roots());
    for dir in load_folders(app) {
        if let Some(k) = soundlib::scan_folder(&dir) {
            kits.push(k);
        }
    }
    kits
}

pub fn folders_file(app: &App) -> PathBuf {
    app.dirs.config.join("libredaw").join("sound-folders.toml")
}

pub fn load_folders(app: &App) -> Vec<PathBuf> {
    std::fs::read_to_string(folders_file(app))
        .map(|t| soundlib::parse_folders(&t))
        .unwrap_or_default()
}

pub fn save_folders(app: &App, folders: &[PathBuf]) -> std::io::Result<()> {
    let f = folders_file(app);
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = f.with_extension("toml.tmp");
    std::fs::write(&tmp, soundlib::emit_folders(folders))?;
    std::fs::rename(&tmp, &f)
}
