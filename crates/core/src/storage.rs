//! Where bytes go.

use bytes::Bytes;
use futures::future::BoxFuture;
use std::io;

/// A destination that accepts writes at arbitrary offsets, in any order,
/// from many segments at once.
pub trait Storage: Send + Sync {
    /// Reserve `len` bytes on disk so later writes never fail for space and
    /// the file is not fragmented. Called once, when the size is known.
    fn preallocate(&self, len: u64) -> BoxFuture<'_, io::Result<()>>;

    /// Write `data` at `offset`. May buffer; data is durable only after
    /// [`Storage::flush`] returns.
    fn write_at(&self, offset: u64, data: Bytes) -> BoxFuture<'_, io::Result<()>>;

    /// Read back bytes, used to verify a segment's tail after a crash.
    fn read_at(&self, offset: u64, len: usize) -> BoxFuture<'_, io::Result<Bytes>>;

    /// Make every completed write durable (fsync or the platform equivalent).
    fn flush(&self) -> BoxFuture<'_, io::Result<()>>;
}
