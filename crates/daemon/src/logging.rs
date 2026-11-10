//! Logging: `tracing` to `<data dir>/logs/vayu-daemon.<date>.log` (daily
//! rotation, the last 7 files kept), plus stderr when running in the
//! foreground. `VAYU_LOG` sets the filter (`info` by default; the syntax is
//! `tracing-subscriber`'s `EnvFilter`, e.g. `VAYU_LOG=vayu_daemon=debug`).
//!
//! Secrets: the daemon never logs request params (they carry cookies) or
//! the token, at any level. The `log` crate is deliberately *not* bridged
//! into `tracing`, because the WebSocket library logs whole frames (hello
//! included) at trace level through it.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing_appender::non_blocking::WorkerGuard;
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
    #[error("cannot use the log directory {0}: {1}")]
    Dir(PathBuf, String),
    #[error("a global logger is already installed")]
    AlreadySet,
}

fn filter() -> EnvFilter {
    let base = std::env::var(LOG_ENV).unwrap_or_else(|_| "info".to_owned());
    // Belt and braces for the secret rule above, should anyone bridge `log`.
    EnvFilter::new(format!("{base},tungstenite=off,tokio_tungstenite=off"))
}

/// Installs the global subscriber. Without `data_dir` (or if the data or
/// log directory is not private and cannot be made so) only stderr is used,
/// and only when `foreground` is set.
pub fn init(data_dir: Option<&Path>, foreground: bool) -> Result<LogGuard, LogError> {
    let (file_layer, guard) = match data_dir {
        Some(dir) => {
            let logs = crate::paths::log_dir(dir);
            let fail = |e: String| LogError::Dir(logs.clone(), e);
            crate::runtime::create_data_dir(&logs).map_err(|e| fail(e.to_string()))?;
            crate::runtime::secure_dir(dir).map_err(fail)?;
            crate::runtime::secure_dir(&logs).map_err(fail)?;
            let (writer, guard) = tracing_appender::non_blocking(DailyFile::new(logs.clone()));
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

/// `<dir>/vayu-daemon.YYYY-MM-DD.log` (UTC), a new file each day, created
/// owner-only (0600) whatever the umask; only the newest
/// [`MAX_LOG_FILES`] are kept.
#[derive(Debug)]
pub struct DailyFile {
    dir: PathBuf,
    current: Option<(i64, File)>,
}

impl DailyFile {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, current: None }
    }

    fn file(&mut self) -> io::Result<&mut File> {
        let day = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| (d.as_secs() / 86_400) as i64)
            .unwrap_or(0);
        if self.current.as_ref().is_none_or(|(d, _)| *d != day) {
            self.current = None;
            let path = self.dir.join(file_name(day));
            let mut options = OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = options.open(&path)?;
            #[cfg(unix)]
            {
                // A file left by an older build may be wider; tighten it.
                use std::os::unix::fs::PermissionsExt;
                if file.metadata()?.permissions().mode() & 0o077 != 0 {
                    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                }
            }
            prune(&self.dir);
            self.current = Some((day, file));
        }
        match &mut self.current {
            Some((_, file)) => Ok(file),
            None => Err(io::Error::other("no log file")),
        }
    }
}

impl Write for DailyFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file()?.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.current {
            Some((_, file)) => file.flush(),
            None => Ok(()),
        }
    }
}

fn file_name(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{LOG_FILE_PREFIX}.{y:04}-{m:02}-{d:02}.log")
}

/// Deletes all but the newest `MAX_LOG_FILES` log files (names sort by date).
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with(&format!("{LOG_FILE_PREFIX}.")) && n.ends_with(".log")
            })
        })
        .collect();
    logs.sort();
    let excess = logs.len().saturating_sub(MAX_LOG_FILES);
    for old in &logs[..excess] {
        let _ = std::fs::remove_file(old);
    }
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian
/// (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(20_767), (2026, 11, 10));
        assert_eq!(file_name(20_767), "vayu-daemon.2026-11-10.log");
    }

    #[test]
    fn files_are_private_and_pruned() {
        let dir = std::env::temp_dir().join(format!("vayu-daemon-logs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for day in 0..10 {
            std::fs::write(dir.join(file_name(day)), b"old").unwrap();
        }
        std::fs::write(dir.join("unrelated.txt"), b"keep").unwrap();
        let mut w = DailyFile::new(dir.clone());
        writeln!(w, "hello").unwrap();
        w.flush().unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names.len(), MAX_LOG_FILES + 1, "{names:?}");
        assert!(names.contains(&"unrelated.txt".to_owned()));
        assert!(!names.contains(&file_name(0)), "oldest pruned: {names:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let today = names.iter().rev().find(|n| n.ends_with(".log")).unwrap();
            let mode = std::fs::metadata(dir.join(today))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
