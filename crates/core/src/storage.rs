//! Where bytes go.

use std::io;
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use futures::future::BoxFuture;

/// One in-progress file that accepts writes at arbitrary offsets, in any
/// order, from many segments at once.
///
/// Errors are plain [`io::Error`]s; the engine maps
/// [`io::ErrorKind::StorageFull`] to the `disk_full` download error.
///
/// Error model: `write_at` resolves when data is *accepted* (an
/// implementation may also write it out before resolving), so an error
/// writing it out surfaces on that call or a later one (`write_at`, `flush`
/// or `finish`). Write and fsync errors are sticky: once one is reported, every
/// later `flush` and `finish` fails too, until the file is removed and
/// opened again. A failed `flush` means every write since the last
/// successful flush may be lost. A future dropped before it resolves (a
/// cancelled `write_at`, `flush` or `finish`) is safe and never loses an
/// error.
pub trait Storage: Send + Sync {
    /// Reserve `len` bytes so later writes never fail for space and the file
    /// is not fragmented (`fallocate`, `F_PREALLOCATE`,
    /// `SetFileInformationByHandle`), and leave the file exactly `len` bytes
    /// long: a longer file left by an earlier attempt is truncated, so it
    /// cannot finish with stale bytes at the end. Called when the size is
    /// known; never called for files of unknown size, which simply grow.
    fn preallocate(&self, len: u64) -> BoxFuture<'_, io::Result<()>>;

    /// Write `data` at `offset`. Resolves once the data is accepted into a
    /// bounded buffer, so a fast network cannot outrun a slow disk
    /// (backpressure). Data is durable only after [`Storage::flush`].
    fn write_at(&self, offset: u64, data: Bytes) -> BoxFuture<'_, io::Result<()>>;

    /// Read back bytes; used to verify segment tails after a crash. Sees
    /// every write that has resolved, flushed or not.
    fn read_at(&self, offset: u64, len: usize) -> BoxFuture<'_, io::Result<Bytes>>;

    /// Make every resolved write durable (write out buffers, then fsync or
    /// the platform equivalent). The engine calls this before every journal
    /// commit, so it must be cheap enough to run every couple of seconds.
    fn flush(&self) -> BoxFuture<'_, io::Result<()>>;

    /// Flush, close, and move the finished file to `final_path` without
    /// overwriting anything: if `final_path` exists, fail with
    /// [`io::ErrorKind::AlreadyExists`] and the engine picks another name
    /// (the storage stays usable for that retry). After a successful finish
    /// the storage is unusable: writes fail.
    fn finish<'a>(&'a self, final_path: &'a Path) -> BoxFuture<'a, io::Result<()>>;

    /// Like [`Storage::flush`], but only hands every resolved write to the
    /// operating system, without forcing it to disk: the bytes survive a
    /// killed process, not a power cut. Used instead of `flush` under
    /// `Durability::ProcessSafe`. The default flushes, which is stronger.
    fn write_out(&self) -> BoxFuture<'_, io::Result<()>> {
        self.flush()
    }

    /// Like [`Storage::finish`], but without forcing the data to disk before
    /// the move (`Durability::ProcessSafe`); the same no-overwrite rule
    /// applies. The default is `finish`, which is stronger.
    fn finish_unsynced<'a>(&'a self, final_path: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        self.finish(final_path)
    }
}

/// Opens in-progress files.
pub trait StorageProvider: Send + Sync {
    /// Open the in-progress file at `path`, creating it (and missing parent
    /// directories) if needed. Existing contents are kept: this is how a
    /// download resumes. The engine names in-progress files
    /// `<final name>.vayu-part`.
    ///
    /// Opening a path that is already open returns the same live storage.
    /// After that storage finished (or the path was removed), opening the
    /// path starts a new file, and writes through an old handle never reach
    /// it.
    fn open<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<Arc<dyn Storage>>>;

    /// The length of the file at `path` (in-progress or finished), or `None`
    /// when there is none. Lets the engine tell, after a crash between
    /// [`Storage::finish`] and the journal update, that the part file was
    /// already moved into place instead of downloading it again.
    fn file_len<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<Option<u64>>>;

    /// Delete an in-progress or finished file (`download.remove` with
    /// `deleteFiles`). A missing file is not an error.
    fn remove<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<()>>;
}
