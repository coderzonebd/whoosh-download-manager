//! Logging: `tracing` to `<data dir>/logs/vayu-daemon.<date>.log` (daily
//! rotation, the last 7 files kept), plus stderr when running in the
//! foreground. `VAYU_LOG` sets the filter (`info` by default; the syntax is
//! `tracing-subscriber`'s `EnvFilter`, e.g. `VAYU_LOG=vayu_daemon=debug`).
//!
//! Secrets: the daemon never logs request params (they carry cookies) or
//! the token, at any level. The `log` crate is deliberately *not* bridged
//! into `tracing`, because the WebSocket library logs whole frames (hello
//! included) at trace level through it.

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;

pub const LOG_ENV: &str = "VAYU_LOG";
pub const LOG_FILE_PREFIX: &str = "vayu-daemon";
pub const MAX_LOG_FILES: usize = 7;

/// Keeps the background log writer alive; drop it last (it flushes).
#[derive(Debug)]
pub struct LogGuard {
    _file: Option<WorkerGuard>,
}

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("cannot create the log directory {0}: {1}")]
    Dir(std::path::PathBuf, std::io::Error),
    #[error("cannot open the log file: {0}")]
    File(String),
    #[error("a global logger is already installed")]
    AlreadySet,
}

fn filter() -> EnvFilter {
    let base = std::env::var(LOG_ENV).unwrap_or_else(|_| "info".to_owned());
    // Belt and braces for the secret rule above, should anyone bridge `log`.
    EnvFilter::new(format!("{base},tungstenite=off,tokio_tungstenite=off"))
}

/// Installs the global subscriber. Without `data_dir` (or if the log
/// directory cannot be created) only stderr is used, and only when
/// `foreground` is set.
pub fn init(data_dir: Option<&Path>, foreground: bool) -> Result<LogGuard, LogError> {
    let (file_layer, guard) = match data_dir {
        Some(dir) => {
            let logs = crate::paths::log_dir(dir);
            crate::runtime::create_data_dir(&logs).map_err(|e| LogError::Dir(logs.clone(), e))?;
            let appender = RollingFileAppender::builder()
                .rotation(Rotation::DAILY)
                .filename_prefix(LOG_FILE_PREFIX)
                .filename_suffix("log")
                .max_log_files(MAX_LOG_FILES)
                .build(&logs)
                .map_err(|e| LogError::File(e.to_string()))?;
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let layer = tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer);
            (Some(layer), Some(guard))
        }
        None => (None, None),
    };
    let stderr_layer = foreground.then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_target(false)
    });
    let subscriber = tracing_subscriber::registry()
        .with(filter())
        .with(file_layer)
        .with(stderr_layer);
    tracing::subscriber::set_global_default(subscriber).map_err(|_| LogError::AlreadySet)?;
    Ok(LogGuard { _file: guard })
}
