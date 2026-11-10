//! `vayu-daemon`: serves the local API (`docs/api/PROTOCOL.md`) for any
//! [`Service`](vayu_proto::service::Service).
//!
//! ```no_run
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! use std::sync::Arc;
//! use vayu_daemon::{DaemonConfig, mock::{MockOptions, MockService}};
//!
//! let service = MockService::new(MockOptions::default());
//! let config = DaemonConfig::new(vayu_daemon::paths::default_data_dir()?);
//! let daemon = vayu_daemon::serve(service, config).await?;
//! vayu_daemon::shutdown_signal().await;
//! daemon.shutdown().await;
//! # Ok(()) }
//! ```
//!
//! `serve` does, in order:
//! 1. create the data directory (owner-only on Unix);
//! 2. take `daemon.lock` (an OS file lock) — if another process holds it,
//!    fail with [`ServeError::AlreadyRunning`]; if `runtime.json` exists and
//!    its daemon answers `session.hello`, fail the same way; otherwise the
//!    file is stale and gets replaced;
//! 3. listen on `127.0.0.1:<port>` (default 47210, a free port if taken);
//! 4. write `runtime.json` atomically and owner-only, with a fresh token;
//! 5. accept connections: `GET /healthz`, and the WebSocket on `/rpc`.

pub mod client;
pub mod config;
pub mod logging;
pub mod mock;
pub mod outbox;
pub mod paths;
pub mod rpc;
pub mod runtime;
mod server;
pub mod session;
pub mod token;

use std::path::PathBuf;

pub use config::DaemonConfig;
pub use server::{HEALTHZ_PATH, RunningDaemon, serve};

/// This crate's version: `daemonVersion` and `runtime.json`'s `version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("{} (pid {}, port {})",
        "another vayu-daemon is already running for this data directory",
        .pid.map_or("unknown".into(), |p| p.to_string()),
        .port.map_or("unknown".into(), |p| p.to_string()))]
    AlreadyRunning { pid: Option<u32>, port: Option<u16> },
    #[error("data directory {0}: {1}")]
    DataDir(PathBuf, std::io::Error),
    #[error("cannot listen on 127.0.0.1: {0}")]
    Bind(std::io::Error),
    #[error("cannot write {0}: {1}")]
    RuntimeFile(PathBuf, std::io::Error),
    #[error("the OS random number generator failed: {0}")]
    Rng(String),
}

/// Resolves when the process is asked to stop: SIGINT or SIGTERM on Unix;
/// Ctrl-C, Ctrl-Break, closing the console window, log-off or system
/// shutdown on Windows.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
        ) {
            (Ok(mut int), Ok(mut term)) => {
                tokio::select! {
                    _ = int.recv() => tracing::info!("SIGINT received"),
                    _ = term.recv() => tracing::info!("SIGTERM received"),
                }
            }
            _ => {
                tracing::warn!("cannot install signal handlers; falling back to Ctrl-C only");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows;
        let (c, b, close, logoff, shutdown) = (
            windows::ctrl_c(),
            windows::ctrl_break(),
            windows::ctrl_close(),
            windows::ctrl_logoff(),
            windows::ctrl_shutdown(),
        );
        match (c, b, close, logoff, shutdown) {
            (Ok(mut c), Ok(mut b), Ok(mut close), Ok(mut logoff), Ok(mut shutdown)) => {
                tokio::select! {
                    _ = c.recv() => tracing::info!("Ctrl-C received"),
                    _ = b.recv() => tracing::info!("Ctrl-Break received"),
                    _ = close.recv() => tracing::info!("console closed"),
                    _ = logoff.recv() => tracing::info!("user logging off"),
                    _ = shutdown.recv() => tracing::info!("system shutting down"),
                }
            }
            _ => {
                tracing::warn!("cannot install console handlers; falling back to Ctrl-C only");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
