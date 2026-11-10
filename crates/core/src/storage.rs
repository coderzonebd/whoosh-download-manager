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
pub trait Storage: Send + Sync {
    /// Reserve `len` bytes so later writes never fail for space and the file
    /// is not fragmented (`fallocate`, `F_PREALLOCATE`,
    /// `SetFileInformationByHandle`). Called once, when the size is known;
    /// never called for files of unknown size, which simply grow.
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
    /// [`io::ErrorKind::AlreadyExists`] and the engine picks another name.
    /// The storage is unusable afterwards.
    fn finish<'a>(&'a self, final_path: &'a Path) -> BoxFuture<'a, io::Result<()>>;
}

/// Opens in-progress files.
pub trait StorageProvider: Send + Sync {
    /// Open the in-progress file at `path`, creating it (and missing parent
    /// directories) if needed. Existing contents are kept: this is how a
    /// download resumes. The engine names in-progress files
    /// `<final name>.vayu-part`.
    fn open<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<Arc<dyn Storage>>>;

    /// Delete an in-progress or finished file (`download.remove` with
    /// `deleteFiles`). A missing file is not an error.
    fn remove<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<()>>;
}
