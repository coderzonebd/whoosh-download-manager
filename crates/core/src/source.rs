//! Where bytes come from.

use bytes::Bytes;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::ByteRange;

/// What a source learned about the remote file before downloading.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteMeta {
    /// Total size, when the server states it.
    pub size: Option<u64>,
    /// Whether ranged requests are honoured (a real `206`, not just the header).
    pub accepts_ranges: bool,
    /// Strong or weak validator used to detect that the file changed on resume.
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// Filename from `Content-Disposition` (already decoded), if any.
    pub filename: Option<String>,
    pub content_type: Option<String>,
    /// URL after redirects.
    pub final_url: Option<String>,
}

/// Why a source failed. The scheduler uses the variant to decide whether to
/// retry, back off, hedge, or ask for a fresh link.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// Connection reset, timeout, DNS failure: retry, maybe on another connection.
    #[error("transient network error: {0}")]
    Transient(String),
    /// 429/503 or connection refused under load: lower this host's connection count.
    #[error("server is limiting us: {0}")]
    Throttled(String),
    /// 401/403/410 or an expired signature: the link needs refreshing.
    #[error("link expired or forbidden: {0}")]
    LinkExpired(String),
    /// Validators no longer match: the remote file changed since we started.
    #[error("remote file changed")]
    RemoteChanged,
    /// The server ignored our `Range` header.
    #[error("server does not support ranges")]
    RangesUnsupported,
    #[error("{0}")]
    Fatal(String),
}

/// One stream of bytes for one requested range. Chunks arrive in order and
/// together cover exactly the requested range unless an error ends the stream.
pub type ByteStream = BoxStream<'static, Result<Bytes, SourceError>>;

/// Anything that can deliver ranges of a remote file.
pub trait RangeSource: Send + Sync {
    /// A short, stable label for logs and per-source statistics.
    fn label(&self) -> &str;

    /// Learn size, validators and range support. Implementations should use
    /// the cheapest request the protocol allows.
    fn probe(&self) -> BoxFuture<'_, Result<RemoteMeta, SourceError>>;

    /// Open a stream for `range`. `None` means "from the start, whole file"
    /// and is used when the source does not support ranges.
    fn open(&self, range: Option<ByteRange>) -> BoxFuture<'_, Result<ByteStream, SourceError>>;
}
