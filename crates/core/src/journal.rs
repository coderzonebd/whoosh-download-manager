//! The crash-safe record of progress.

use std::io;

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use vayu_proto::DownloadId;

use crate::RangeSet;
use crate::source::RemoteMeta;

/// Everything needed to resume a download after the process, or the whole
/// machine, stopped without warning.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// What the file looked like when the download started; compared with a
    /// fresh probe on resume.
    pub meta: RemoteMeta,
    /// Ranges whose bytes were flushed to storage before this checkpoint.
    pub done: RangeSet,
}

/// Persists [`Checkpoint`]s, one series per download.
///
/// After a crash at any instant (power cut included), `load` returns either
/// the previous or the new checkpoint, never a torn mix and never a range
/// that was not committed. Implementations detect torn or corrupted data
/// (checksums) and fall back to the last good checkpoint.
pub trait Journal: Send + Sync {
    /// Called only after [`crate::Storage::flush`] has returned, so every
    /// range in `done` is really on disk. Durable when it resolves. The
    /// engine commits at most every 2 s or 64 MiB per download.
    fn commit<'a>(
        &'a self,
        id: &'a DownloadId,
        cp: &'a Checkpoint,
    ) -> BoxFuture<'a, io::Result<()>>;

    /// The last committed checkpoint, or `None` for a fresh download.
    fn load<'a>(&'a self, id: &'a DownloadId) -> BoxFuture<'a, io::Result<Option<Checkpoint>>>;

    /// Forget a download (completed or removed). Missing is not an error.
    fn remove<'a>(&'a self, id: &'a DownloadId) -> BoxFuture<'a, io::Result<()>>;
}
