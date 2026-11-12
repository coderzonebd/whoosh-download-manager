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
    /// Set by the engine before it moves the finished file into place
    /// (`Storage::finish`): the file it will create (a name that was free
    /// when the intent was written) and its exact length. Written just
    /// before the move, or earlier (when the transfer starts) so that
    /// finishing needs no commit of its own; a later checkpoint without it
    /// withdraws it. After a crash between that move and the record update,
    /// a part file that is gone and a file of this name and length mean the
    /// download already finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finishing: Option<Finishing>,
}

/// The file a finishing download is about to become (see
/// [`Checkpoint::finishing`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finishing {
    /// File name inside the download's directory (after collision
    /// numbering, e.g. `name (1).ext`).
    pub name: String,
    /// Exact length of the finished file, in bytes.
    pub len: u64,
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

    /// The last committed checkpoint, or `None` for a fresh download. A
    /// journal with no valid checkpoint (a crash during the first commit,
    /// damage, an oversized file) also returns `Ok(None)`, with a warning
    /// logged: the download starts over rather than failing.
    fn load<'a>(&'a self, id: &'a DownloadId) -> BoxFuture<'a, io::Result<Option<Checkpoint>>>;

    /// Forget a download (completed or removed). Missing is not an error.
    fn remove<'a>(&'a self, id: &'a DownloadId) -> BoxFuture<'a, io::Result<()>>;
}
