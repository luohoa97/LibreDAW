// SPDX-License-Identifier: GPL-3.0-or-later
//! Socket directory, single-owner lock, and stale socket handling (17.1).
//!
//! The directory is mode 0700 and owned by us. A lock file in it is held
//! with `flock` for as long as the DAW serves the socket, so a second DAW
//! gets `AlreadyRunning`. Whoever holds the lock owns the socket path: a
//! socket file found while we hold the lock is stale and is removed.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use crate::state::ControlStartError;

pub const SOCKET_NAME: &str = "control.sock";
pub const LOCK_NAME: &str = "control.lock";

pub struct Bound {
    pub listener: UnixListener,
    pub socket_path: PathBuf,
    /// Held for the life of the server; dropping it releases the flock.
    pub lock: File,
}

pub fn bind(dir: &Path) -> Result<Bound, ControlStartError> {
    prepare_dir(dir).map_err(ControlStartError::Io)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(dir.join(LOCK_NAME))
        .map_err(ControlStartError::Io)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(ControlStartError::AlreadyRunning),
        Err(TryLockError::Error(e)) => return Err(ControlStartError::Io(e)),
    }
    let socket_path = dir.join(SOCKET_NAME);
    remove_stale(&socket_path).map_err(ControlStartError::Io)?;
    let listener = UnixListener::bind(&socket_path).map_err(ControlStartError::Io)?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))
        .map_err(ControlStartError::Io)?;
    Ok(Bound {
        listener,
        socket_path,
        lock,
    })
}

fn prepare_dir(dir: &Path) -> io::Result<()> {
    if let Some(parent) = dir.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::other("socket directory is not a directory"));
    }
    let me = fs::metadata("/proc/self")?.uid();
    if meta.uid() != me {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "socket directory is owned by another user",
        ));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Called only while holding the lock: anything at the socket path is a
/// leftover of a DAW that died without cleaning up.
fn remove_stale(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_socket() => fs::remove_file(path),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "something that is not a socket is in the way",
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
