// SPDX-License-Identifier: GPL-3.0-or-later
//! Which of the two processes runs: each holds a lock on its own file in
//! the data directory for as long as it lives (`tray.lock`, `app.lock`).
//! The system releases a lock when its process ends, crashed or not, so a
//! lock never goes stale.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

/// Held by the tray process.
pub const TRAY: &str = "tray.lock";
/// Held by the app (`wdm-desktop`) while it runs, that is while a window
/// is open: the tray then leaves the notifications and the prompt window
/// to it.
pub const APP: &str = "app.lock";

fn file(dir: &Path, name: &str) -> std::io::Result<(PathBuf, File)> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    Ok((path, f))
}

/// Takes the lock `name` in `dir` for this process: `Some` (keep it alive)
/// when it was free, `None` when another process holds it.
pub fn take(dir: &Path, name: &str) -> Option<File> {
    let (_, f) = file(dir, name).ok()?;
    match f.try_lock() {
        Ok(()) => Some(f),
        Err(TryLockError::WouldBlock) => None,
        // No locking on this file system: act as if it was free.
        Err(TryLockError::Error(_)) => Some(f),
    }
}

/// Another process holds the lock `name` in `dir`.
pub fn held(dir: &Path, name: &str) -> bool {
    let Ok((_, f)) = file(dir, name) else {
        return false;
    };
    // Dropping `f` releases a lock taken here.
    matches!(f.try_lock(), Err(TryLockError::WouldBlock))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_is_held_while_its_file_lives() {
        let dir = std::env::temp_dir().join(format!("wdm-locks-{}", std::process::id()));
        assert!(!held(&dir, TRAY));
        let mine = take(&dir, TRAY).expect("free");
        assert!(held(&dir, TRAY));
        assert!(take(&dir, TRAY).is_none());
        // The app's lock is another one.
        assert!(!held(&dir, APP));
        drop(mine);
        assert!(!held(&dir, TRAY));
        assert!(take(&dir, TRAY).is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}
