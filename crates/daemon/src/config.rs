//! [`DaemonConfig`]: everything `serve` needs besides the service.

use std::path::PathBuf;
use std::time::Duration;

/// `Origin` values allowed at the WebSocket upgrade besides "no Origin"
/// (PROTOCOL.md §1): the desktop window's webview on each platform.
pub fn default_allowed_origins() -> Vec<String> {
    [
        "tauri://localhost",
        "http://tauri.localhost",
        "https://tauri.localhost",
    ]
    .map(String::from)
    .to_vec()
}

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Holds `runtime.json`, `daemon.lock` and `logs/`.
    pub data_dir: PathBuf,
    /// TCP port on 127.0.0.1. `0` picks a free one.
    pub port: u16,
    /// When `port` is taken, listen on a free port instead of failing.
    pub port_fallback: bool,
    /// Reported in `HelloResult::daemon_version` and `runtime.json`.
    pub version: String,
    /// Refuse to start when another daemon owns `data_dir`.
    pub single_instance: bool,
    /// See [`default_allowed_origins`].
    pub allowed_origins: Vec<String>,
    /// A connection must complete `session.hello` within this (else 4408).
    pub hello_timeout: Duration,
    /// Before `session.hello` completes, a connection may send at most this
    /// many bytes (the hello itself, in practice); more closes it (1009)
    /// without the daemon ever buffering or parsing the rest.
    pub max_hello_bytes: usize,
    /// WebSocket connections that may wait for their hello at once; further
    /// upgrades get HTTP 503 until one finishes or times out.
    pub max_pending_hellos: usize,
    /// Largest WebSocket message (and frame) accepted from a client; larger
    /// ones close the connection with 1009.
    pub max_message_bytes: usize,
    /// The daemon pings every client this often...
    pub ping_interval: Duration,
    /// ...and drops one it has heard nothing from for `ping_interval +
    /// pong_timeout`.
    pub pong_timeout: Duration,
    /// Lossless notifications queued for one client before it is considered
    /// too slow and closed with 4429.
    pub max_pending_events: usize,
    /// Requests one client may have in flight; further requests are not read
    /// until one is answered.
    pub max_inflight_requests: usize,
}

impl DaemonConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            port: vayu_proto::DEFAULT_PORT,
            port_fallback: true,
            version: crate::VERSION.to_owned(),
            single_instance: true,
            allowed_origins: default_allowed_origins(),
            hello_timeout: Duration::from_secs(10),
            max_hello_bytes: 64 * 1024,
            max_pending_hellos: 8,
            max_message_bytes: 16 * 1024 * 1024,
            ping_interval: Duration::from_secs(20),
            pong_timeout: Duration::from_secs(20),
            max_pending_events: 4096,
            max_inflight_requests: 32,
        }
    }
}
