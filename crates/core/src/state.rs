//! Durable application state: the download list, settings, and what we
//! learned about each host.

use std::io;

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use vayu_proto::{Checksum, DownloadId, DownloadState, ErrorInfo, RequestContext, UnixMillis};

use crate::source::Protocol;

/// One download as stored. The engine keeps the live copy in memory and
/// saves it on every state change and every few seconds while running;
/// byte-exact progress lives in the [`crate::Journal`], not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadRecord {
    pub id: DownloadId,
    pub url: String,
    /// How to request it. Holds secrets (cookies): the engine clears
    /// `cookie` and `Authorization`-like headers once the download completes.
    pub request: Option<RequestContext>,
    pub mirrors: Vec<String>,
    pub filename: String,
    /// The user chose `filename` (`AddParams::filename`), so a probe must not
    /// replace it with the server's name.
    #[serde(default)]
    pub filename_locked: bool,
    pub directory: String,
    pub category: Option<String>,
    pub queue: Option<String>,
    pub state: DownloadState,
    pub total_bytes: Option<u64>,
    pub downloaded_bytes: u64,
    pub resumable: bool,
    pub checksum: Option<Checksum>,
    pub added_at: UnixMillis,
    pub completed_at: Option<UnixMillis>,
    pub error: Option<ErrorInfo>,
}

/// What we learned about a host, so the next download from it starts at the
/// optimum instead of probing again (Speed Doctrine #3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostProfile {
    /// `host:port`, lower-case.
    pub host: String,
    /// The connection count that gave the best throughput last time.
    pub best_connections: Option<u16>,
    /// The highest count that did not trigger throttling (429/503/resets).
    pub max_ok_connections: Option<u16>,
    pub accepts_ranges: Option<bool>,
    pub protocol: Option<Protocol>,
    /// Typical throughput of one connection, bytes per second.
    pub per_connection_bps: Option<u64>,
    pub updated_at: UnixMillis,
}

/// Persistence for everything except byte progress.
///
/// Security: the store holds cookies and must be readable by the user only
/// (0600 on Unix; the per-user data directory's ACL on Windows).
pub trait StateStore: Send + Sync {
    /// Every download, in no particular order (the engine sorts).
    fn load_downloads(&self) -> BoxFuture<'_, io::Result<Vec<DownloadRecord>>>;

    /// Insert or replace by `id`. Called often; implementations batch
    /// writes (e.g. SQLite WAL with `synchronous=NORMAL`) rather than fsync
    /// each call.
    fn save_download<'a>(&'a self, record: &'a DownloadRecord) -> BoxFuture<'a, io::Result<()>>;

    /// Missing ids are ignored.
    fn delete_downloads<'a>(&'a self, ids: &'a [DownloadId]) -> BoxFuture<'a, io::Result<()>>;

    /// A small JSON document by key: `"settings"`, `"queues"`,
    /// `"categories"`, … The engine owns the schema of each value.
    fn get_value<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<Option<String>>>;

    /// Insert or replace. `json` must be valid JSON; anything else is
    /// rejected with [`io::ErrorKind::InvalidInput`].
    fn set_value<'a>(&'a self, key: &'a str, json: &'a str) -> BoxFuture<'a, io::Result<()>>;

    fn host_profile<'a>(&'a self, host: &'a str) -> BoxFuture<'a, io::Result<Option<HostProfile>>>;

    fn save_host_profile<'a>(&'a self, profile: &'a HostProfile) -> BoxFuture<'a, io::Result<()>>;
}
