// SPDX-License-Identifier: GPL-3.0-or-later
//! Test-only helper shared by the plugin-host and engine integration tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Copy the fixture cdylib `so` to `<tmp>/<tag>-<pid>-<n>/nested/test_plugins.clap`
/// and return the scan directory. The path is unique per call and process,
/// the copy is written under a temporary name and renamed into place, so no
/// reader can see a partial file and no loaded file is overwritten or
/// deleted. Files are left in the target tmp dir.
pub fn install_fixture(so: &Path, tmp: &Path, tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = tmp.join(format!("{tag}-{}-{n}", std::process::id()));
    let nested = dir.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let partial = nested.join(".test_plugins.partial");
    std::fs::copy(so, &partial).unwrap();
    std::fs::rename(&partial, nested.join("test_plugins.clap")).unwrap();
    dir
}
