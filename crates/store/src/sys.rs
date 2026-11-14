//! The platform layer: positional I/O, preallocation, no-replace rename,
//! directory fsync, file identity and error mapping.
//!
//! This is the only module allowed to use `unsafe` (the workspace denies
//! `unsafe_code`). Linux and macOS need none: `rustix` and `nix` wrap the
//! calls safely. Windows needs a few blocks, each with a `// SAFETY:`
//! comment and listed in the crate README.
#![allow(unsafe_code)]

use std::fs::File;
use std::io::{self, IoSlice};
use std::path::{Path, PathBuf};

// ------------------------------------------------------------------ errors

/// Normalise errors the engine must treat as "the disk is full".
///
/// `ENOSPC` and Windows' `ERROR_DISK_FULL`/`ERROR_HANDLE_DISK_FULL` already
/// decode to [`io::ErrorKind::StorageFull`]; a quota (`EDQUOT`,
/// `ERROR_DISK_QUOTA_EXCEEDED`) means the same to the user, so it is mapped
/// too. The original error is kept as the source.
pub(crate) fn map_err(e: io::Error) -> io::Error {
    match e.kind() {
        io::ErrorKind::QuotaExceeded => io::Error::new(io::ErrorKind::StorageFull, e),
        _ => e,
    }
}

// ------------------------------------------------------------ positional I/O

/// Write all of `buf` at `offset` without moving a shared cursor (Unix) or
/// from the single thread that owns the handle (Windows).
pub(crate) fn write_all_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    #[cfg(test)]
    faults::check(file, faults::Op::Write)?;
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::write_all_at(file, buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let (mut buf, mut offset) = (buf, offset);
        while !buf.is_empty() {
            match file.seek_write(buf, offset) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    buf = &buf[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// Write the slices back to back starting at `offset`. On Linux this is one
/// `pwritev` per 1024 slices (`IOV_MAX`), which is how adjacent buffered
/// writes are coalesced without copying them.
pub(crate) fn write_vectored_at(
    file: &File,
    bufs: &mut [IoSlice<'_>],
    offset: u64,
) -> io::Result<()> {
    #[cfg(test)]
    faults::check(file, faults::Op::Write)?;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        const IOV_MAX: usize = 1024;
        let (mut bufs, mut offset) = (bufs, offset);
        while !bufs.is_empty() {
            let n = bufs.len().min(IOV_MAX);
            match rustix::io::pwritev(file, &bufs[..n], offset) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(written) => {
                    offset += written as u64;
                    IoSlice::advance_slices(&mut bufs, written);
                }
                Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let mut offset = offset;
        for b in bufs.iter() {
            write_all_at(file, b, offset)?;
            offset += b.len() as u64;
        }
        Ok(())
    }
}

/// Read into `buf` from `offset` until it is full or the file ends; returns
/// the number of bytes read.
pub(crate) fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    let mut done = 0;
    while done < buf.len() {
        #[cfg(unix)]
        let r = std::os::unix::fs::FileExt::read_at(file, &mut buf[done..], offset + done as u64);
        #[cfg(windows)]
        let r =
            std::os::windows::fs::FileExt::seek_read(file, &mut buf[done..], offset + done as u64);
        match r {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(done)
}

/// `fdatasync` (`FlushFileBuffers` on Windows).
pub(crate) fn sync_data(file: &File) -> io::Result<()> {
    #[cfg(test)]
    faults::check(file, faults::Op::Sync)?;
    file.sync_data()
}

/// Test hook: make writes or syncs of one file fail with a given OS error,
/// on every OS (`/dev/full` and `/dev/null` do that only on Linux). Files
/// are told apart by [`file_id`], so tests running side by side do not
/// see each other's faults.
#[cfg(test)]
pub(crate) mod faults {
    use std::fs::File;
    use std::io;
    use std::sync::Mutex;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Op {
        Write,
        Sync,
    }

    /// (file id, operation, raw OS error).
    type Fault = ((u64, u64), Op, i32);

    static FAULTS: Mutex<Vec<Fault>> = Mutex::new(Vec::new());

    fn faults() -> std::sync::MutexGuard<'static, Vec<Fault>> {
        FAULTS.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// From now on, `op` on the file at `path` fails with the raw OS
    /// error `code`.
    pub(crate) fn inject(path: &std::path::Path, op: Op, code: i32) {
        let id = super::file_id(&File::open(path).unwrap()).unwrap();
        faults().push((id, op, code));
    }

    /// Removes every fault of the file at `path`.
    pub(crate) fn clear(path: &std::path::Path) {
        let id = super::file_id(&File::open(path).unwrap()).unwrap();
        faults().retain(|(i, _, _)| *i != id);
    }

    pub(super) fn check(file: &File, op: Op) -> io::Result<()> {
        let faults = faults();
        if faults.is_empty() {
            return Ok(());
        }
        let Some(id) = super::file_id(file) else {
            return Ok(());
        };
        match faults.iter().find(|(i, o, _)| *i == id && *o == op) {
            Some((_, _, code)) => Err(io::Error::from_raw_os_error(*code)),
            None => Ok(()),
        }
    }
}

// ------------------------------------------------------------ identity

/// What identifies `file` on its filesystem: (device, inode) on Unix,
/// (volume serial number, file index) on Windows. `None` if unknown.
pub(crate) fn file_id(file: &File) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        file.metadata().ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        // SAFETY: an all-zero BY_HANDLE_FILE_INFORMATION is a valid value
        // (plain integers and FILETIMEs).
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: `file` is an open handle borrowed for the whole call, and
        // `info` is a writable BY_HANDLE_FILE_INFORMATION on our stack that
        // outlives it; the call does not retain the pointer.
        let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) };
        (ok != 0).then(|| {
            let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
            (u64::from(info.dwVolumeSerialNumber), index)
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        None
    }
}

// ------------------------------------------------------------ page cache

/// Linux: `posix_fadvise(POSIX_FADV_DONTNEED)` over the whole file: the
/// kernel drops the file's clean pages from the page cache (and starts
/// writeback of any dirty ones without waiting; data is never lost). Best
/// effort; a no-op elsewhere.
pub(crate) fn drop_cache(file: &File) {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let _ = rustix::fs::fadvise(file, 0, None, rustix::fs::Advice::DontNeed);
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = file;
    }
}

// ------------------------------------------------------------ preallocation

/// How [`preallocate`] reserved the space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Prealloc {
    /// Blocks are reserved, so later writes in range cannot fail for space.
    Reserved,
    /// The filesystem cannot reserve space (FAT on some kernels, network and
    /// FUSE filesystems, …): the file was only extended, sparsely.
    SizeOnly,
}

/// Reserve `[0, len)` and leave the file exactly `len` bytes long: a tail
/// left by an earlier, larger version of the file is cut off, because no
/// byte past the known size can belong to the download.
pub(crate) fn preallocate(file: &File, len: u64) -> io::Result<Prealloc> {
    let cur = file.metadata()?.len();
    let reserved = len == 0 || reserve(file, cur, len)?;
    if file.metadata()?.len() != len {
        file.set_len(len)?;
    }
    Ok(if reserved {
        Prealloc::Reserved
    } else {
        Prealloc::SizeOnly
    })
}

/// Returns `Ok(false)` when the filesystem cannot reserve space at all.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn reserve(file: &File, _cur: u64, len: u64) -> io::Result<bool> {
    use rustix::fs::{FallocateFlags, fallocate};
    use rustix::io::Errno;
    // Mode 0 allocates (unwritten extents on ext4/XFS/btrfs: no zeroes are
    // written), fills holes inside the range and never shrinks the file.
    // Deliberately not `posix_fallocate`: glibc emulates it by writing zeroes
    // when the filesystem has no fallocate, which would stall for minutes.
    loop {
        match fallocate(file, FallocateFlags::empty(), 0, len) {
            Ok(()) => return Ok(true),
            Err(Errno::INTR) => {}
            Err(e) if e == Errno::OPNOTSUPP || e == Errno::NOSYS => return Ok(false),
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(target_vendor = "apple")]
fn reserve(file: &File, cur: u64, len: u64) -> io::Result<bool> {
    use nix::errno::Errno;
    use nix::fcntl::{FcntlArg, fcntl};
    if len <= cur {
        return Ok(true);
    }
    // F_PEOFPOSMODE counts from the physical end of the file, so ask only for
    // what is missing (asking for `len` again would fail with ENOSPC on a
    // nearly full disk when resuming a large, mostly written file).
    let missing =
        i64::try_from(len - cur).map_err(|_| io::Error::from(io::ErrorKind::FileTooLarge))?;
    let mut store = libc::fstore_t {
        fst_flags: libc::F_ALLOCATECONTIG | libc::F_ALLOCATEALL,
        fst_posmode: libc::F_PEOFPOSMODE,
        fst_offset: 0,
        fst_length: missing,
        fst_bytesalloc: 0,
    };
    // Contiguous first; any layout if the free space is fragmented.
    let mut r = fcntl(file, FcntlArg::F_PREALLOCATE(&mut store));
    if r.is_err() {
        store.fst_flags = libc::F_ALLOCATEALL;
        r = fcntl(file, FcntlArg::F_PREALLOCATE(&mut store));
    }
    match r {
        Ok(_) => Ok(true),
        Err(Errno::ENOTSUP | Errno::EOPNOTSUPP | Errno::EINVAL) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

#[cfg(windows)]
fn reserve(file: &File, _cur: u64, len: u64) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ALLOCATION_INFO, FileAllocationInfo, SetFileInformationByHandle,
    };
    let info = FILE_ALLOCATION_INFO {
        AllocationSize: i64::try_from(len)
            .map_err(|_| io::Error::from(io::ErrorKind::FileTooLarge))?,
    };
    // SAFETY: `file` is an open handle borrowed for the whole call, so the
    // handle stays valid; `info` is a properly initialised
    // FILE_ALLOCATION_INFO on our stack that outlives the call, and the size
    // passed is exactly its size. The call does not retain the pointer.
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileAllocationInfo,
            (&raw const info).cast(),
            size_of::<FILE_ALLOCATION_INFO>() as u32,
        )
    };
    if ok != 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    // ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, ERROR_INVALID_PARAMETER:
    // the filesystem or redirector cannot reserve space.
    match e.raw_os_error() {
        Some(1 | 50 | 87) => Ok(false),
        _ => Err(e),
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple",
    windows
)))]
fn reserve(_file: &File, _cur: u64, _len: u64) -> io::Result<bool> {
    Ok(false)
}

// ------------------------------------------------------- no-replace rename

/// Move `from` to `to` atomically, failing with
/// [`io::ErrorKind::AlreadyExists`] instead of replacing an existing `to`.
///
/// - Linux: `renameat2(RENAME_NOREPLACE)`; macOS: `renameatx_np(RENAME_EXCL)`.
/// - Windows: `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING`
///   (`std::fs::rename` would replace).
/// - Where the filesystem cannot do an exclusive rename (some network and
///   FUSE filesystems, other Unixes): [`link_then_unlink`], and where it has
///   no hard links either (FAT, exFAT): [`claim_then_rename`].
pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    {
        use rustix::fs::{CWD, RenameFlags, renameat_with};
        use rustix::io::Errno;
        match renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
            Ok(()) => Ok(()),
            Err(e)
                if e == Errno::INVAL
                    || e == Errno::NOSYS
                    || e == Errno::OPNOTSUPP
                    || e == Errno::NOTSUP =>
            {
                link_then_unlink(from, to)
            }
            Err(e) => Err(e.into()),
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
        let (from_w, to_w) = (wide(from)?, wide(to)?);
        // SAFETY: both pointers come from NUL-terminated UTF-16 buffers with
        // no interior NULs (checked by `wide`) that live until the call
        // returns; MoveFileExW only reads them and does not retain them.
        let ok = unsafe { MoveFileExW(from_w.as_ptr(), to_w.as_ptr(), MOVEFILE_WRITE_THROUGH) };
        if ok != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(all(
        unix,
        not(any(target_os = "linux", target_os = "android", target_vendor = "apple"))
    ))]
    {
        link_then_unlink(from, to)
    }
}

/// Fallback 1: `link(2)` fails with `EEXIST` rather than replacing, so it
/// claims the name atomically; then drop the old name. A crash in between
/// leaves both names on the same inode, which is harmless.
#[cfg(unix)]
pub(crate) fn link_then_unlink(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::hard_link(from, to) {
        Ok(()) => {
            if let Err(e) = std::fs::remove_file(from) {
                // The file is complete under its final name; a second name
                // for it is not worth failing the download over.
                tracing::warn!(from = %from.display(), error = %e, "could not remove the in-progress name after linking");
            }
            Ok(())
        }
        Err(e) if no_hard_links(&e) => claim_then_rename(from, to),
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn no_hard_links(e: &io::Error) -> bool {
    use rustix::io::Errno;
    Errno::from_io_error(e).is_some_and(|e| {
        e == Errno::PERM || e == Errno::OPNOTSUPP || e == Errno::NOTSUP || e == Errno::NOSYS
    })
}

/// Fallback 2, for filesystems without hard links: claim the name with an
/// exclusive create, then rename over our own empty placeholder. Exclusive
/// with respect to every other create-new or no-replace writer; a program
/// that blindly overwrites `to` in the instant between the two steps would
/// be replaced. A crash in between leaves an empty `to` next to the intact
/// in-progress file.
#[cfg(unix)]
pub(crate) fn claim_then_rename(from: &Path, to: &Path) -> io::Result<()> {
    drop(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(to)?,
    );
    if let Err(e) = std::fs::rename(from, to) {
        let _ = std::fs::remove_file(to);
        return Err(e);
    }
    Ok(())
}

/// A NUL-terminated wide path for Win32, with the `\\?\` prefix when the
/// absolute path is too long for the legacy `MAX_PATH` limit.
#[cfg(windows)]
fn wide(p: &Path) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let abs = std::path::absolute(p)?;
    let mut v: Vec<u16> = abs.as_os_str().encode_wide().collect();
    if v.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains a NUL character",
        ));
    }
    let s = |t: &str| t.encode_utf16().collect::<Vec<u16>>();
    if v.len() >= 248 && !v.starts_with(&s(r"\\?\")) {
        v = if v.starts_with(&s(r"\\")) {
            let mut u = s(r"\\?\UNC\");
            u.extend_from_slice(&v[2..]);
            u
        } else {
            let mut u = s(r"\\?\");
            u.extend_from_slice(&v);
            u
        };
    }
    v.push(0);
    Ok(v)
}

// ------------------------------------------------------------ directories

/// Make a directory entry change (create, rename) durable.
///
/// # Windows: a no-op, and why that is safe
///
/// Windows has no directory fsync that works for unprivileged handles on
/// every filesystem, and none is needed for what this crate relies on.
/// Every caller syncs a directory for one of two reasons, and both are met
/// another way:
///
/// 1. **A file created in it** (a journal file, the database, its WAL, a
///    part file, and the directories `create_dirs` made above them) must
///    still be there after a power cut once its first sync returns. On
///    NTFS, creating a file and adding its name to the directory are
///    metadata changes written to the volume's log (`$LogFile`) before
///    they are applied, and the log is one ordered stream: making any
///    record durable makes every earlier record durable too. Each of those
///    files is created, then grown by a write (a journal file from zero to
///    its first slot, the WAL by its first frames, a part file by
///    preallocation), and then `FlushFileBuffers`ed (`sync_data`). For that
///    flush to keep its promise (the data readable after a crash) the new
///    size must be durable, and the size lives in the log after the
///    creation records, so the flush commits the log past them. On FAT and
///    exFAT the size is stored in the file's directory entry itself, so
///    flushing the file writes that entry. A later process's first commit
///    may not grow its journal file, but by then an earlier commit did, and
///    its flush already made the name durable (a journal file is only
///    written by commits, each flushed before the next one starts).
/// 2. **A rename into it** (`finish`, a corrupt database moved aside) must
///    be durable once it returns. [`rename_noreplace`] passes
///    `MOVEFILE_WRITE_THROUGH`, documented as "does not return until the
///    file is actually moved on the disk"; and as in (1), committing the
///    rename's log record also commits every earlier one on that volume,
///    including directories created for the move.
///
/// What this does not cover: power-loss behaviour of filesystems without
/// such ordering (ReFS keeps metadata copy-on-write, which we expect to
/// behave the same but have not verified); the order of changes on two
/// different volumes, which no caller depends on; and one corner of (1): a
/// WAL file that a crashed process created and never synced, which SQLite
/// then overwrites in place without growing it. There the argument rests on
/// `FlushFileBuffers` committing the whole NTFS log, not just the records
/// of the file's size, which we expect but cannot show without a power-cut
/// test rig.
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        match File::open(dir).and_then(|d| d.sync_all()) {
            // Some filesystems (certain FUSE and network ones) cannot fsync
            // a directory; there is nothing better to do there.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
                ) =>
            {
                Ok(())
            }
            r => r,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Who may read the directories [`create_dirs`] makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirMode {
    /// The process's default (0777 minus umask on Unix; the inherited ACL
    /// on Windows).
    Default,
    /// Owner-only: 0700 on Unix; on Windows a protected DACL for the user,
    /// `SYSTEM` and `Administrators`, which files created inside inherit
    /// (see `whoosh_proto::winacl`).
    Private,
}

/// Create `dir` and its missing parents. Returns the directories whose
/// entries changed (the parent of every directory created), which must be
/// fsynced before anything inside the new directories can be called
/// durable. Existing directories are left alone.
pub(crate) fn create_dirs(dir: &Path, mode: DirMode) -> io::Result<Vec<PathBuf>> {
    let missing: Vec<&Path> = dir
        .ancestors()
        .take_while(|a| !a.as_os_str().is_empty() && std::fs::symlink_metadata(a).is_err())
        .collect();
    if missing.is_empty() {
        return Ok(Vec::new());
    }
    #[cfg(windows)]
    if mode == DirMode::Private {
        whoosh_proto::winacl::create_private_dir(dir)?;
    }
    #[cfg(windows)]
    let done = mode == DirMode::Private;
    #[cfg(not(windows))]
    let done = false;
    if !done {
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        if mode == DirMode::Private {
            std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        }
        b.create(dir)?;
    }
    let mut changed: Vec<PathBuf> = missing.iter().map(|d| parent_or_cwd(d)).collect();
    changed.dedup();
    Ok(changed)
}

/// The directory that holds `p`'s entry (`.` for a bare relative name).
pub(crate) fn parent_or_cwd(p: &Path) -> PathBuf {
    match p.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// [`sync_dir`] each directory, stopping at the first failure.
pub(crate) fn sync_dirs(dirs: &[PathBuf]) -> io::Result<()> {
    dirs.iter().try_for_each(|d| sync_dir(d))
}

/// `fdatasync` a file, then `fsync` the directories whose entries its
/// creation changed. In this order on purpose: on a journaling filesystem
/// the data sync's journal commit already carries the new entries, so the
/// directory syncs after it cost at most a device cache flush. Run side by
/// side they cost two journal commits instead (measured on ext4/NVMe: a
/// 4 KiB file 1.85 ms in order, 3.3 ms side by side; 1 MiB 4.1 vs 5.9 ms).
pub(crate) fn sync_data_and_dirs(file: &File, dirs: &[PathBuf]) -> io::Result<()> {
    file.sync_data()?;
    sync_dirs(dirs)
}

/// Open without following a symbolic link in the last path component, so a
/// link planted where an in-progress file, journal or database belongs
/// cannot redirect our writes (or a truncation) to its target. Links in
/// parent directories are followed as usual.
///
/// Unix: `O_NOFOLLOW`. Windows: `FILE_FLAG_OPEN_REPARSE_POINT`, then refuse
/// the handle if it is a link.
pub(crate) fn open_nofollow(opts: &mut std::fs::OpenOptions, path: &Path) -> io::Result<File> {
    let refuse = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is a symbolic link; refusing to follow it",
                path.display()
            ),
        )
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
        match opts.open(path) {
            Err(e) if rustix::io::Errno::from_io_error(&e) == Some(rustix::io::Errno::LOOP) => {
                Err(refuse())
            }
            r => r,
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        opts.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let f = opts.open(path)?;
        if f.metadata()?.file_type().is_symlink() {
            return Err(refuse());
        }
        Ok(f)
    }
}

/// Open options for a file only the user may read: 0600 on Unix. On
/// Windows the file inherits the ACL of its directory, which
/// [`create_dirs`] with [`DirMode::Private`] makes owner-only.
pub(crate) fn private_file_options() -> std::fs::OpenOptions {
    #[allow(unused_mut)]
    let mut o = std::fs::OpenOptions::new();
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn disk_full_and_quota_map_to_storage_full() {
        let full = map_err(io::Error::from_raw_os_error(
            rustix::io::Errno::NOSPC.raw_os_error(),
        ));
        assert_eq!(full.kind(), io::ErrorKind::StorageFull);
        let quota = map_err(io::Error::from_raw_os_error(
            rustix::io::Errno::DQUOT.raw_os_error(),
        ));
        assert_eq!(quota.kind(), io::ErrorKind::StorageFull);
        let other = map_err(io::Error::from_raw_os_error(
            rustix::io::Errno::ACCESS.raw_os_error(),
        ));
        assert_eq!(other.kind(), io::ErrorKind::PermissionDenied);
    }

    #[cfg(windows)]
    #[test]
    fn disk_full_and_quota_map_to_storage_full() {
        // ERROR_DISK_FULL, ERROR_HANDLE_DISK_FULL, ERROR_DISK_QUOTA_EXCEEDED.
        for code in [112, 39, 1295] {
            let e = map_err(io::Error::from_raw_os_error(code));
            assert_eq!(e.kind(), io::ErrorKind::StorageFull, "code {code}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn fallbacks_never_replace() {
        let d = tempfile::tempdir().unwrap();
        let (a, b, c) = (d.path().join("a"), d.path().join("b"), d.path().join("c"));
        std::fs::write(&a, b"aaa").unwrap();
        std::fs::write(&b, b"bbb").unwrap();
        for f in [link_then_unlink, claim_then_rename, rename_noreplace] {
            let e = f(&a, &b).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(std::fs::read(&a).unwrap(), b"aaa");
            assert_eq!(std::fs::read(&b).unwrap(), b"bbb");
        }
        link_then_unlink(&a, &c).unwrap();
        assert!(!a.exists());
        claim_then_rename(&c, &a).unwrap();
        assert!(!c.exists());
        rename_noreplace(&a, &c).unwrap();
        assert_eq!(std::fs::read(&c).unwrap(), b"aaa");
        assert!(!a.exists());
    }

    #[test]
    fn preallocate_sets_the_exact_size() {
        let d = tempfile::tempdir().unwrap();
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(d.path().join("f"))
            .unwrap();
        write_all_at(&f, &[7u8; 100], 0).unwrap();
        preallocate(&f, 1 << 20).unwrap();
        assert_eq!(f.metadata().unwrap().len(), 1 << 20);
        let mut buf = [0u8; 101];
        assert_eq!(read_at(&f, &mut buf, 0).unwrap(), 101);
        assert_eq!(&buf[..100], &[7u8; 100]);
        assert_eq!(buf[100], 0);
        preallocate(&f, 50).unwrap();
        assert_eq!(f.metadata().unwrap().len(), 50);
    }
}
