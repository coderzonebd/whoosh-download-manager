//! `<data dir>/runtime.json` (PROTOCOL.md §1): `{ port, token, pid, version }`.
//!
//! * Written atomically: a new temporary file in the same directory, created
//!   owner-only (0600 on Unix) *before* anything is written to it, filled,
//!   then renamed over `runtime.json` (not fsynced: see [`write_atomic`]).
//!   A reader sees the old file or the new one, never a partial one, and
//!   never a file others can read.
//! * Removed on clean exit, but only if it is still ours (same pid and
//!   token), so a daemon that lost the race never deletes a live daemon's file.
//! * `daemon.lock` in the same directory is held (an OS file lock) for the
//!   daemon's lifetime; it is what makes the daemon single-instance even when
//!   two start at the same moment. The OS releases it if the daemon crashes.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub use whoosh_proto::runtime::{RUNTIME_FILE, RuntimeError, RuntimeInfo};

pub const LOCK_FILE: &str = "daemon.lock";

pub fn runtime_file(data_dir: &Path) -> PathBuf {
    data_dir.join(RUNTIME_FILE)
}

/// Creates a directory and its parents; the new ones are owner-only (0700)
/// on Unix. Use [`secure_dir`] afterwards for one that already existed.
pub fn create_data_dir(dir: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Makes `dir` private, or refuses it (PROTOCOL.md §1): a directory of ours
/// that others could read or write is tightened to 0700; a symlink or a
/// directory owned by someone else is refused, since that user could swap
/// `runtime.json` under us. Always Ok on Windows, where `%LOCALAPPDATA%` is
/// private by inheritance.
pub fn secure_dir(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = std::fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
        if !meta.file_type().is_dir() {
            return Err("not a directory (a symlink is not accepted)".into());
        }
        let me = rustix::process::geteuid().as_raw();
        if meta.uid() != me {
            return Err(format!(
                "owned by uid {}, not by this user (uid {me})",
                meta.uid()
            ));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("cannot make it private (chmod 700): {e}"))?;
        }
    }
    whoosh_proto::runtime::check_private_dir(dir).map_err(|e| e.to_string())
}

/// Creates a new, empty file that only the owner can read or write. The
/// permissions are part of the `open` call, so there is no moment at which
/// the file exists with wider permissions. On Windows the file inherits the
/// ACL of the data directory, which under `%LOCALAPPDATA%` is the user's own.
pub fn create_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Writes `info` to `<dir>/runtime.json` atomically (see the module docs).
pub fn write_atomic(dir: &Path, info: &RuntimeInfo) -> io::Result<()> {
    let target = runtime_file(dir);
    let json = serde_json::to_vec_pretty(info).map_err(io::Error::other)?;
    // Unique per process and attempt, so two writers never share a temp file.
    let tmp = dir.join(format!(
        ".{RUNTIME_FILE}.{}.{}.tmp",
        std::process::id(),
        unique_suffix()
    ));
    // No fsync: the rename alone makes the switch atomic, and durability
    // buys nothing here. After a crash or power loss the daemon is gone, so
    // the file is stale whatever it holds, and every reader treats a missing,
    // empty or invalid one as "no daemon". The two fsyncs (file, directory)
    // were the slowest part of writing it: 1–4 ms of startup.
    let result = (|| {
        let mut file = create_private_file(&tmp)?;
        file.write_all(&json)?;
        drop(file);
        std::fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn unique_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Reads `<dir>/runtime.json` with [`whoosh_proto::runtime::read_runtime`]'s
/// checks. `Ok(None)` when there is none.
pub fn read(dir: &Path) -> Result<Option<RuntimeInfo>, RuntimeError> {
    match whoosh_proto::runtime::read_runtime(&runtime_file(dir)) {
        Ok(info) => Ok(Some(info)),
        Err(RuntimeError::Missing(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Removes `<dir>/runtime.json` if it still describes `ours`.
pub fn remove_if_ours(dir: &Path, ours: &RuntimeInfo) -> io::Result<bool> {
    match read(dir) {
        Ok(Some(current)) if current.pid == ours.pid && current.token == ours.token => {
            std::fs::remove_file(runtime_file(dir))?;
            Ok(true)
        }
        Ok(_) => Ok(false),
        // Corrupt: not provably someone else's, and a daemon that is exiting
        // must not leave a file pointing at a dead port.
        Err(RuntimeError::Invalid { .. }) => {
            std::fs::remove_file(runtime_file(dir))?;
            Ok(true)
        }
        // Not ours (another owner, a symlink, other permissions): leave it.
        Err(RuntimeError::Insecure { .. }) => Ok(false),
        Err(e) => Err(io::Error::other(e.to_string())),
    }
}

/// The single-instance lock, held until dropped.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
    path: PathBuf,
}

impl InstanceLock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Takes `<dir>/daemon.lock` without waiting. `Ok(None)` means another
/// process holds it.
pub fn try_lock(dir: &Path) -> io::Result<Option<InstanceLock>> {
    let path = dir.join(LOCK_FILE);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(InstanceLock { _file: file, path })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "whoosh-daemon-rt-{name}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        create_data_dir(&dir).unwrap();
        secure_dir(&dir).unwrap();
        dir
    }

    fn info(port: u16) -> RuntimeInfo {
        RuntimeInfo {
            port,
            token: format!("token-{port}"),
            pid: std::process::id(),
            version: "0.0.1".into(),
        }
    }

    #[test]
    fn round_trip_and_debug_redacts_the_token() {
        let dir = temp_dir("roundtrip");
        assert!(read(&dir).unwrap().is_none());
        write_atomic(&dir, &info(1234)).unwrap();
        assert_eq!(read(&dir).unwrap(), Some(info(1234)));
        let debug = format!("{:?}", info(1234));
        assert!(!debug.contains("token-1234"), "{debug}");
        // Exactly the four documented fields.
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(runtime_file(&dir)).unwrap()).unwrap();
        let mut keys: Vec<_> = raw.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["pid", "port", "token", "version"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn private_files_are_0600_from_creation() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("perm");
        // The file is owner-only while still empty, i.e. before any secret
        // is written into it.
        let path = dir.join("x");
        let file = create_private_file(&path).unwrap();
        let meta = file.metadata().unwrap();
        assert_eq!(meta.len(), 0);
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        drop(file);

        // A pre-existing, world-readable runtime.json is replaced by a
        // private one (rename swaps the inode, permissions and all).
        std::fs::write(runtime_file(&dir), b"{}").unwrap();
        std::fs::set_permissions(runtime_file(&dir), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        write_atomic(&dir, &info(1)).unwrap();
        let mode = std::fs::metadata(runtime_file(&dir))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn secure_dir_tightens_ours_and_refuses_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("secure");
        let open = dir.join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o775)).unwrap();
        secure_dir(&open).unwrap();
        let mode = std::fs::metadata(&open).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);

        let link = dir.join("link");
        std::os::unix::fs::symlink(&open, &link).unwrap();
        let err = secure_dir(&link).unwrap_err();
        assert!(err.contains("symlink"), "{err}");
        let file = dir.join("file");
        std::fs::write(&file, b"").unwrap();
        assert!(secure_dir(&file).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn concurrent_readers_never_see_a_partial_file() {
        let dir = temp_dir("atomic");
        write_atomic(&dir, &info(1)).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let (dir, stop) = (dir.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut reads = 0u32;
                while !stop.load(Ordering::Relaxed) {
                    // Must always parse: never missing, never truncated.
                    let got = read(&dir).expect("readable").expect("present");
                    assert_eq!(got.token, format!("token-{}", got.port));
                    reads += 1;
                }
                reads
            })
        };
        for port in 2..100 {
            write_atomic(&dir, &info(port)).unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        assert!(reader.join().unwrap() > 0);
        // No temporary files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n != RUNTIME_FILE)
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn removes_only_its_own_file() {
        let dir = temp_dir("remove");
        let mine = info(10);
        let theirs = RuntimeInfo {
            pid: mine.pid + 1,
            ..info(11)
        };
        write_atomic(&dir, &theirs).unwrap();
        assert!(!remove_if_ours(&dir, &mine).unwrap());
        assert!(runtime_file(&dir).exists());
        write_atomic(&dir, &mine).unwrap();
        assert!(remove_if_ours(&dir, &mine).unwrap());
        assert!(!runtime_file(&dir).exists());
        assert!(!remove_if_ours(&dir, &mine).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lock_is_exclusive_until_dropped() {
        let dir = temp_dir("lock");
        let first = try_lock(&dir).unwrap().expect("first lock");
        assert!(try_lock(&dir).unwrap().is_none());
        drop(first);
        assert!(try_lock(&dir).unwrap().is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
