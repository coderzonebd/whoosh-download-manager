//! `vayu-proto`: the daemon's local API contract.
//!
//! Every client — desktop window, CLI, browser extension (through the native
//! messaging host), web remote, MCP server — speaks JSON-RPC 2.0 to the
//! daemon using these types. The human-readable spec is
//! `docs/api/PROTOCOL.md`; the TypeScript mirror is
//! `packages/api-client/src/types.ts`. All three change together, in one
//! commit.
//!
//! Wire format: camelCase field names, `null`-free (optional fields are
//! omitted), byte counts as integers, timestamps as Unix milliseconds.

use serde::{Deserialize, Serialize};

pub mod auth;
pub mod runtime;
pub mod service;

/// Bumped on any breaking change to methods, params, results or events.
pub const PROTOCOL_VERSION: u32 = 1;

/// Default TCP port on 127.0.0.1. The real port is in the runtime file.
pub const DEFAULT_PORT: u16 = 47210;

/// WebSocket path on the daemon.
pub const RPC_PATH: &str = "/rpc";

/// JSON-RPC method names.
pub mod method {
    pub const SESSION_HELLO: &str = "session.hello";
    /// Stops the daemon (authenticated; the daemon's own, not `Service`).
    pub const DAEMON_SHUTDOWN: &str = "daemon.shutdown";

    pub const DOWNLOAD_PROBE: &str = "download.probe";
    pub const DOWNLOAD_ADD: &str = "download.add";
    pub const DOWNLOAD_LIST: &str = "download.list";
    pub const DOWNLOAD_GET: &str = "download.get";
    pub const DOWNLOAD_PAUSE: &str = "download.pause";
    pub const DOWNLOAD_RESUME: &str = "download.resume";
    pub const DOWNLOAD_REMOVE: &str = "download.remove";
    pub const DOWNLOAD_REFRESH_URL: &str = "download.refreshUrl";

    pub const NET_PRECONNECT: &str = "net.preconnect";

    pub const STATS_GET: &str = "stats.get";
    pub const SETTINGS_GET: &str = "settings.get";
    pub const SETTINGS_SET: &str = "settings.set";
}

/// Server-to-client notification method names.
pub mod event {
    pub const DOWNLOAD_ADDED: &str = "event.download.added";
    pub const DOWNLOAD_PROGRESS: &str = "event.download.progress";
    pub const DOWNLOAD_STATE: &str = "event.download.state";
    pub const DOWNLOAD_REMOVED: &str = "event.download.removed";
    pub const STATS: &str = "event.stats";
    /// The full `Settings` object, sent to every client after any change.
    pub const SETTINGS: &str = "event.settings";
}

/// Answered by `vayu-nmhost` itself, never by the daemon (PROTOCOL.md §1).
pub mod host {
    pub const NM_STATUS: &str = "nm.status";
    /// Sent by the host whenever its daemon connection goes up or down.
    pub const NM_STATUS_EVENT: &str = "event.nm.status";
    /// Request ids starting with this prefix are reserved for the host.
    pub const RESERVED_ID_PREFIX: &str = "nmhost:";
}

/// Built-in category ids. Users can add their own; any other string is a
/// user-defined category.
pub mod category {
    pub const VIDEO: &str = "video";
    pub const MUSIC: &str = "music";
    pub const DOCUMENTS: &str = "documents";
    pub const COMPRESSED: &str = "compressed";
    pub const PROGRAMS: &str = "programs";
    pub const IMAGES: &str = "images";
    pub const OTHER: &str = "other";
    pub const BUILT_IN: [&str; 7] = [VIDEO, MUSIC, DOCUMENTS, COMPRESSED, PROGRAMS, IMAGES, OTHER];
}

/// JSON-RPC error codes beyond the standard -32700..-32600 range.
pub mod error_code {
    pub const UNAUTHORIZED: i32 = -32001;
    pub const NOT_FOUND: i32 = -32002;
    pub const INVALID_STATE: i32 = -32003;
    pub const IO: i32 = -32004;
    pub const NETWORK: i32 = -32005;
    pub const UNSUPPORTED: i32 = -32006;
    /// Native host only: daemon not running, token rejected, or the
    /// connection was lost before the answer.
    pub const DAEMON_UNAVAILABLE: i32 = -32007;
    /// Native host only: the response would exceed the browser's 1 MiB limit.
    pub const MESSAGE_TOO_LARGE: i32 = -32008;
}

/// WebSocket close codes (PROTOCOL.md §1).
pub mod close_code {
    /// Bad or missing token or nonce, or a first message that is not a hello.
    /// Final: the client must not retry with the same token.
    pub const UNAUTHORIZED: u16 = 4401;
    /// No `session.hello` within 10 s. Retryable.
    pub const HELLO_TIMEOUT: u16 = 4408;
    /// Incompatible protocol version. Final.
    pub const PROTOCOL_MISMATCH: u16 = 4426;
    /// The client fell more than 4096 lossless notifications behind.
    /// Retryable (reconnect and re-read state).
    pub const TOO_SLOW: u16 = 4429;
}

/// Opaque to clients. Daemons generate ids matching `[a-z0-9_-]{1,64}`
/// (vayu-engine: `dl_` + lower-case hex); storage relies on that to keep
/// file names short and portable.
pub type DownloadId = String;
pub type UnixMillis = i64;

// ---------------------------------------------------------------- session

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloParams {
    pub token: String,
    /// A fresh random value per connection, 32–128 lower-case hex characters
    /// ([`auth::new_nonce`]). The daemon answers with [`HelloResult::proof`].
    pub nonce: String,
    pub protocol: u32,
    pub client: ClientInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    /// e.g. "desktop", "cli", "extension-chromium", "extension-firefox", "web-remote".
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloResult {
    pub daemon_version: String,
    pub protocol: u32,
    /// Feature flags the daemon supports, e.g. "http", "torrent", "media", "preconnect".
    pub capabilities: Vec<String>,
    /// [`auth::hello_proof`] of the client's nonce. Clients must check it with
    /// [`auth::verify_hello_proof`] before sending anything else.
    pub proof: String,
}

// ---------------------------------------------------------------- requests

/// How to make the request exactly as the browser would have.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestContext {
    /// Defaults to GET.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<Header>,
    /// Full `Cookie` header value for the request URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// Request body for POST-triggered downloads, base64-encoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_base64: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checksum {
    /// "sha256", "sha512", "sha1", "md5".
    pub algorithm: String,
    /// Lowercase hex.
    pub value: String,
}

// ---------------------------------------------------------------- download.*

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeParams {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<RequestContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    /// The server honours ranges, so the download can be split and resumed.
    pub resumable: bool,
    /// Best filename guess (Content-Disposition, then URL, then MIME type).
    pub filename: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub final_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddParams {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<RequestContext>,
    /// Overrides the detected filename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Absolute directory; defaults to the category's or the global folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Start now (true, default) or only add to the list.
    #[serde(default = "default_true")]
    pub start: bool,
    /// Extra URLs serving the identical file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mirrors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<Checksum>,
    /// Use exactly this many connections (1–64) instead of learning the
    /// count; still lowered when the server throttles. Out of range →
    /// invalid params.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connections: Option<u32>,
    /// A named queue (runs one download at a time); the main queue when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
}

/// Bounds of [`AddParams::connections`].
pub const MAX_CONNECTIONS_PER_DOWNLOAD: u32 = 64;

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddResult {
    pub id: DownloadId,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListParams {
    /// Only these states; all when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<DownloadState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Skip this many results (newest first). Defaults to 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
    /// Return at most this many. Defaults to all; extensions must page
    /// because native messaging responses are limited to 1 MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdParams {
    pub id: DownloadId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdsParams {
    pub ids: Vec<DownloadId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveParams {
    pub ids: Vec<DownloadId>,
    /// Also delete the downloaded (or partial) files.
    #[serde(default)]
    pub delete_files: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshUrlParams {
    pub id: DownloadId,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<RequestContext>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Queued,
    Probing,
    Downloading,
    Paused,
    /// Link expired; waiting for `download.refreshUrl` (the extension does this itself).
    NeedsRefresh,
    Verifying,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorInfo {
    /// Stable machine-readable code, e.g. "remote_changed", "disk_full", "link_expired".
    pub code: String,
    /// Human-readable, already localised by the daemon when possible.
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadSummary {
    pub id: DownloadId,
    pub url: String,
    pub filename: String,
    pub directory: String,
    pub state: DownloadState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    pub downloaded_bytes: u64,
    pub speed_bps: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_secs: Option<u64>,
    pub connections: u32,
    pub resumable: bool,
    /// A built-in id from [`category`] or a user-defined name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// The queue this download belongs to; absent means the main queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    pub added_at: UnixMillis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<UnixMillis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentInfo {
    pub start: u64,
    pub end: u64,
    pub downloaded_bytes: u64,
    pub speed_bps: u64,
    /// Which source serves this segment: host, mirror, or peer label.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadDetail {
    #[serde(flatten)]
    pub summary: DownloadSummary,
    pub segments: Vec<SegmentInfo>,
}

// ---------------------------------------------------------------- net.*

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreconnectParams {
    pub url: String,
}

// ---------------------------------------------------------------- stats / settings

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub download_bps: u64,
    pub upload_bps: u64,
    pub active: u32,
    pub queued: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub default_directory: String,
    pub max_active_downloads: u32,
    /// Upper bound; the engine learns the best count per host below this.
    pub max_connections_per_download: u32,
    /// Global download limit; absent means unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_limit_bps: Option<u64>,
    pub language: String,
    /// "system", "light", "dark".
    pub theme: String,
}

// ---------------------------------------------------------------- events

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressItem {
    pub id: DownloadId,
    pub downloaded_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    pub speed_bps: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_secs: Option<u64>,
    pub connections: u32,
    /// Per-segment progress as `[start, end, downloadedBytes]`, included at
    /// most once per second per download, so UIs need not poll `download.get`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segments: Option<Vec<(u64, u64, u64)>>,
}

/// `event.download.progress`: batched, at most 4 per second.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub items: Vec<ProgressItem>,
}

/// `event.download.state`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateEvent {
    pub id: DownloadId,
    pub state: DownloadState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
}

/// `event.download.removed`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedEvent {
    pub ids: Vec<DownloadId>,
}

/// Result of `nm.status` and payload of `event.nm.status` (native host only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NmStatus {
    pub daemon_running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_version: Option<String>,
    pub host_version: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn add_params_default_to_start_and_omit_empty_fields() {
        let p: AddParams =
            serde_json::from_value(json!({ "url": "https://example.com/a.iso" })).unwrap();
        assert!(p.start);
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            json!({ "url": "https://example.com/a.iso", "start": true })
        );
    }

    #[test]
    fn states_are_snake_case_on_the_wire() {
        assert_eq!(
            serde_json::to_value(DownloadState::NeedsRefresh).unwrap(),
            json!("needs_refresh")
        );
    }

    #[test]
    fn detail_flattens_summary() {
        let d = DownloadDetail {
            summary: DownloadSummary {
                id: "dl_1".into(),
                url: "https://example.com/a".into(),
                filename: "a".into(),
                directory: "/tmp".into(),
                state: DownloadState::Downloading,
                total_bytes: Some(10),
                downloaded_bytes: 5,
                speed_bps: 1,
                eta_secs: None,
                connections: 2,
                resumable: true,
                category: None,
                queue: None,
                added_at: 0,
                completed_at: None,
                error: None,
            },
            segments: vec![],
        };
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["downloadedBytes"], json!(5));
        assert_eq!(v["segments"], json!([]));
    }

    #[test]
    fn progress_segments_are_compact_triples() {
        let p = ProgressItem {
            id: "dl_1".into(),
            downloaded_bytes: 7,
            total_bytes: None,
            speed_bps: 0,
            eta_secs: None,
            connections: 1,
            segments: Some(vec![(0, 10, 7)]),
        };
        assert_eq!(
            serde_json::to_value(&p).unwrap()["segments"],
            json!([[0, 10, 7]])
        );
    }

    #[test]
    fn nm_status_matches_the_host() {
        let s: NmStatus =
            serde_json::from_value(json!({ "daemonRunning": false, "hostVersion": "0.0.1" }))
                .unwrap();
        assert!(!s.daemon_running);
        assert_eq!(s.daemon_version, None);
    }
}
