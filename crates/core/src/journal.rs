//! The crash-safe record of progress.

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::io;

use crate::ByteRange;
use crate::source::RemoteMeta;

/// Everything needed to resume a download after the process, or the whole
/// machine, stopped without warning.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Validators captured at the start; compared with a fresh probe on resume.
    pub meta: RemoteMeta,
    /// Ranges whose bytes were flushed to storage before this checkpoint.
    pub done: Vec<ByteRange>,
}

/// Persists [`Checkpoint`]s atomically: after a crash, `load` returns either
/// the previous or the new checkpoint, never a torn mix of both.
pub trait Journal: Send + Sync {
    /// Called only after [`crate::storage::Storage::flush`] has returned, so
    /// every range in `done` is really on disk.
    fn commit(&self, checkpoint: &Checkpoint) -> BoxFuture<'_, io::Result<()>>;

    /// The last committed checkpoint, or `None` for a fresh download.
    fn load(&self) -> BoxFuture<'_, io::Result<Option<Checkpoint>>>;
}
