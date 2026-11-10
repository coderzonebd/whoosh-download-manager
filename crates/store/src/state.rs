//! Durable application state in SQLite.
//!
//! One dedicated thread owns the connection and serves requests from a
//! channel. Whatever is queued when it wakes is run as one short
//! transaction (at most 256 requests), so a burst of `save_download` calls
//! costs one WAL append instead of one each; replies go out only after the
//! commit. If the batch fails, it is rolled back and each request is retried
//! on its own, so one bad request cannot fail the others.
//!
//! The database runs in WAL mode with `synchronous=NORMAL`: a commit is
//! atomic and survives a process crash; a power cut can lose the last
//! commits but never corrupts the file. Byte-exact progress lives in the
//! journal, so that is the right trade.
//!
//! It stores cookies, so `secure_delete` is on, and a `TRUNCATE` checkpoint
//! takes replaced or deleted rows out of the `-wal` file: before
//! `delete_downloads` resolves, and within 30 s of any other write.
//!
//! # Schema (version 1, `PRAGMA user_version`)
//!
//! - `downloads(id TEXT PRIMARY KEY, added_at INTEGER, json TEXT)`
//! - `kv(key TEXT PRIMARY KEY, json TEXT)`
//! - `host_profiles(host TEXT PRIMARY KEY, json TEXT)`

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::future::BoxFuture;
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior, params};
use tokio::sync::oneshot;
use vayu_core::{DownloadRecord, HostProfile, StateStore};
use vayu_proto::DownloadId;

use crate::sys;

/// The highest schema version this build understands.
const SCHEMA_VERSION: usize = MIGRATIONS.len();

/// `MIGRATIONS[n]` takes the schema from version `n` to `n + 1`.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE downloads (
        id       TEXT PRIMARY KEY NOT NULL,
        added_at INTEGER NOT NULL,
        json     TEXT NOT NULL
    );
    CREATE TABLE kv (
        key  TEXT PRIMARY KEY NOT NULL,
        json TEXT NOT NULL
    );
    CREATE TABLE host_profiles (
        host TEXT PRIMARY KEY NOT NULL,
        json TEXT NOT NULL
    );
"];

const MAX_BATCH: usize = 256;

/// [`StateStore`] backed by one SQLite database file.
///
/// Security: on Unix the database file is created 0600 before SQLite opens
/// it (SQLite gives its `-wal` and `-shm` files the same mode), and a file
/// that is readable by others is narrowed to 0600 with a warning. On
/// Windows the per-user data directory's ACL applies.
///
/// A database that fails to open as a database or fails `PRAGMA
/// quick_check` is moved aside as `<name>.corrupt-<UTC timestamp>` (with its
/// `-wal`), and a fresh one is started, with a warning. Other errors
/// (permissions, a full disk, a newer schema) are returned instead: they are
/// not a reason to set the user's data aside.
#[derive(Debug)]
pub struct SqliteStateStore {
    path: PathBuf,
    tx: mpsc::Sender<Request>,
}

#[derive(Debug)]
struct Request {
    op: Op,
    reply: oneshot::Sender<io::Result<Output>>,
}

#[derive(Debug)]
enum Op {
    LoadDownloads,
    SaveDownload {
        id: String,
        added_at: i64,
        json: String,
    },
    DeleteDownloads(Vec<String>),
    GetValue(String),
    SetValue {
        key: String,
        json: String,
    },
    HostProfile(String),
    SaveHostProfile {
        host: String,
        json: String,
    },
    Close,
}

impl Op {
    fn writes(&self) -> bool {
        matches!(
            self,
            Op::SaveDownload { .. }
                | Op::DeleteDownloads(_)
                | Op::SetValue { .. }
                | Op::SaveHostProfile { .. }
        )
    }
}

#[derive(Debug)]
enum Output {
    Unit,
    Downloads(Vec<DownloadRecord>),
    Text(Option<String>),
}

impl SqliteStateStore {
    /// Open (or create) the database at `path`, creating missing parent
    /// directories (owner-only on Unix). The database work happens on the
    /// store's own thread; this resolves once it is ready.
    pub async fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let p = path.clone();
        std::thread::Builder::new()
            .name("vayu-store-sqlite".into())
            .spawn(move || match open_db(&p) {
                Ok(conn) => {
                    let _ = ready_tx.send(Ok(()));
                    serve(conn, rx);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })?;
        ready_rx.await.map_err(|_| closed_error())??;
        Ok(Self { path, tx })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Finish queued requests and close the database (checkpointing the
    /// WAL). Dropping the store does the same without waiting.
    pub async fn close(self) -> io::Result<()> {
        self.call(Op::Close).await.map(drop)
    }

    async fn call(&self, op: Op) -> io::Result<Output> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Request { op, reply })
            .map_err(|_| closed_error())?;
        rx.await.map_err(|_| closed_error())?
    }
}

fn closed_error() -> io::Error {
    io::Error::other("the state store is closed")
}

fn unexpected() -> io::Error {
    io::Error::other("state store: unexpected reply")
}

impl StateStore for SqliteStateStore {
    fn load_downloads(&self) -> BoxFuture<'_, io::Result<Vec<DownloadRecord>>> {
        Box::pin(async move {
            match self.call(Op::LoadDownloads).await? {
                Output::Downloads(v) => Ok(v),
                _ => Err(unexpected()),
            }
        })
    }

    fn save_download<'a>(&'a self, record: &'a DownloadRecord) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let op = Op::SaveDownload {
                id: record.id.clone(),
                added_at: record.added_at,
                json: serde_json::to_string(record)?,
            };
            self.call(op).await.map(drop)
        })
    }

    fn delete_downloads<'a>(&'a self, ids: &'a [DownloadId]) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            if ids.is_empty() {
                return Ok(());
            }
            self.call(Op::DeleteDownloads(ids.to_vec())).await.map(drop)
        })
    }

    fn get_value<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<Option<String>>> {
        Box::pin(async move {
            match self.call(Op::GetValue(key.to_owned())).await? {
                Output::Text(v) => Ok(v),
                _ => Err(unexpected()),
            }
        })
    }

    fn set_value<'a>(&'a self, key: &'a str, json: &'a str) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            serde_json::from_str::<serde::de::IgnoredAny>(json)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            let op = Op::SetValue {
                key: key.to_owned(),
                json: json.to_owned(),
            };
            self.call(op).await.map(drop)
        })
    }

    fn host_profile<'a>(&'a self, host: &'a str) -> BoxFuture<'a, io::Result<Option<HostProfile>>> {
        Box::pin(async move {
            match self.call(Op::HostProfile(host.to_owned())).await? {
                Output::Text(None) => Ok(None),
                Output::Text(Some(json)) => serde_json::from_str(&json)
                    .map(Some)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
                _ => Err(unexpected()),
            }
        })
    }

    fn save_host_profile<'a>(&'a self, profile: &'a HostProfile) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let op = Op::SaveHostProfile {
                host: profile.host.clone(),
                json: serde_json::to_string(profile)?,
            };
            self.call(op).await.map(drop)
        })
    }
}

// ------------------------------------------------------------- the thread

/// How long data overwritten or deleted in a commit (a cleared cookie) may
/// stay in the `-wal` file before a `TRUNCATE` checkpoint removes it.
/// Deletions are checkpointed at once.
const SECRET_CHECKPOINT: Duration = if cfg!(test) {
    Duration::from_millis(200)
} else {
    Duration::from_secs(30)
};

type Reply = (oneshot::Sender<io::Result<Output>>, io::Result<Output>);

fn serve(mut conn: Connection, rx: mpsc::Receiver<Request>) {
    // When the WAL started holding data written since the last TRUNCATE.
    let mut dirty_since: Option<Instant> = None;
    loop {
        let first = match dirty_since {
            None => match rx.recv() {
                Ok(r) => r,
                Err(_) => break,
            },
            Some(t) => {
                let wait = (t + SECRET_CHECKPOINT).saturating_duration_since(Instant::now());
                match rx.recv_timeout(wait) {
                    Ok(r) => r,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        truncate_wal(&conn);
                        dirty_since = None;
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        };
        let mut batch = vec![first];
        while batch.len() < MAX_BATCH {
            match rx.try_recv() {
                Ok(r) => batch.push(r),
                Err(_) => break,
            }
        }
        let close = batch
            .iter()
            .position(|r| matches!(r.op, Op::Close))
            .map(|i| batch.split_off(i));
        let wrote = batch.iter().any(|r| r.op.writes());
        let deleted = batch.iter().any(|r| matches!(r.op, Op::DeleteDownloads(_)));
        let replies = run_batch(&mut conn, batch);
        if wrote && dirty_since.is_none() {
            dirty_since = Some(Instant::now());
        }
        // A removed download's cookies leave the WAL before `delete`
        // resolves; anything else within SECRET_CHECKPOINT, even while
        // writes keep coming.
        if deleted || dirty_since.is_some_and(|t| t.elapsed() >= SECRET_CHECKPOINT) {
            truncate_wal(&conn);
            dirty_since = None;
        }
        for (reply, out) in replies {
            let _ = reply.send(out);
        }
        if let Some(mut rest) = close {
            let req = rest.remove(0);
            // Requests queued after `close` are dropped, so their callers
            // see "closed".
            drop(rest);
            let r = conn
                .close()
                .map(|()| Output::Unit)
                .map_err(|(_, e)| to_io(e));
            let _ = req.reply.send(r);
            return;
        }
    }
}

/// Copy the WAL into the database and cut the WAL to zero bytes, so old
/// page images (cookies since replaced or deleted) are no longer in any
/// file; `secure_delete` zeroes them inside the database.
fn truncate_wal(conn: &Connection) {
    let r = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
        r.get::<_, i64>(0)
    });
    match r {
        Ok(0) => {}
        Ok(_) => tracing::debug!("WAL checkpoint was blocked; will retry"),
        Err(e) => tracing::warn!(error = %e, "WAL checkpoint failed"),
    }
}

/// Runs the batch; returns the replies to send once the caller is done.
fn run_batch(conn: &mut Connection, batch: Vec<Request>) -> Vec<Reply> {
    if !batch.iter().any(|r| r.op.writes()) {
        return batch
            .into_iter()
            .map(|r| {
                let out = exec(conn, &r.op).map_err(to_io);
                (r.reply, out)
            })
            .collect();
    }
    let all = (|| {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let outs = batch
            .iter()
            .map(|r| exec(&tx, &r.op))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        tx.commit()?;
        Ok::<_, rusqlite::Error>(outs)
    })();
    match all {
        Ok(outs) => batch
            .into_iter()
            .zip(outs)
            .map(|(r, out)| (r.reply, Ok(out)))
            .collect(),
        Err(e) => {
            if batch.len() > 1 {
                tracing::debug!(error = %e, n = batch.len(), "state batch failed; retrying one by one");
            }
            batch
                .into_iter()
                .map(|r| {
                    let out = if r.op.writes() {
                        conn.transaction_with_behavior(TransactionBehavior::Immediate)
                            .and_then(|tx| {
                                let out = exec(&tx, &r.op)?;
                                tx.commit()?;
                                Ok(out)
                            })
                    } else {
                        exec(conn, &r.op)
                    };
                    (r.reply, out.map_err(to_io))
                })
                .collect()
        }
    }
}

fn exec(conn: &Connection, op: &Op) -> rusqlite::Result<Output> {
    Ok(match op {
        Op::LoadDownloads => {
            let mut st = conn.prepare_cached("SELECT id, json FROM downloads")?;
            let mut rows = st.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let json = row.get_ref(1)?.as_str()?;
                match serde_json::from_str::<DownloadRecord>(json) {
                    Ok(d) => out.push(d),
                    Err(e) => {
                        let id: String = row.get(0)?;
                        tracing::warn!(id, error = %e, "skipping an unreadable download record");
                    }
                }
            }
            Output::Downloads(out)
        }
        Op::SaveDownload { id, added_at, json } => {
            conn.prepare_cached(
                "INSERT INTO downloads (id, added_at, json) VALUES (?1, ?2, ?3)
                 ON CONFLICT(id) DO UPDATE SET added_at = excluded.added_at, json = excluded.json",
            )?
            .execute(params![id, added_at, json])?;
            Output::Unit
        }
        Op::DeleteDownloads(ids) => {
            let mut st = conn.prepare_cached("DELETE FROM downloads WHERE id = ?1")?;
            for id in ids {
                st.execute([id])?;
            }
            Output::Unit
        }
        Op::GetValue(key) => Output::Text(
            conn.prepare_cached("SELECT json FROM kv WHERE key = ?1")?
                .query_row([key], |r| r.get(0))
                .optional()?,
        ),
        Op::SetValue { key, json } => {
            conn.prepare_cached(
                "INSERT INTO kv (key, json) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET json = excluded.json",
            )?
            .execute([key, json])?;
            Output::Unit
        }
        Op::HostProfile(host) => Output::Text(
            conn.prepare_cached("SELECT json FROM host_profiles WHERE host = ?1")?
                .query_row([host], |r| r.get(0))
                .optional()?,
        ),
        Op::SaveHostProfile { host, json } => {
            conn.prepare_cached(
                "INSERT INTO host_profiles (host, json) VALUES (?1, ?2)
                 ON CONFLICT(host) DO UPDATE SET json = excluded.json",
            )?
            .execute([host, json])?;
            Output::Unit
        }
        Op::Close => unreachable!("close is handled by the serve loop"),
    })
}

fn to_io(e: rusqlite::Error) -> io::Error {
    let kind = match e.sqlite_error_code() {
        Some(ErrorCode::DiskFull) => io::ErrorKind::StorageFull,
        Some(ErrorCode::PermissionDenied | ErrorCode::AuthorizationForStatementDenied) => {
            io::ErrorKind::PermissionDenied
        }
        Some(ErrorCode::ReadOnly) => io::ErrorKind::ReadOnlyFilesystem,
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => io::ErrorKind::ResourceBusy,
        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => io::ErrorKind::InvalidData,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, e)
}

// ----------------------------------------------------------------- opening

enum OpenError {
    /// Not a database, or damaged: move it aside.
    Corrupt(String),
    Other(io::Error),
}

impl From<rusqlite::Error> for OpenError {
    fn from(e: rusqlite::Error) -> Self {
        match e.sqlite_error_code() {
            Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => {
                OpenError::Corrupt(e.to_string())
            }
            _ => OpenError::Other(to_io(e)),
        }
    }
}

impl From<io::Error> for OpenError {
    fn from(e: io::Error) -> Self {
        OpenError::Other(e)
    }
}

fn open_db(path: &Path) -> io::Result<Connection> {
    match try_open(path) {
        Ok(c) => Ok(c),
        Err(OpenError::Other(e)) => Err(e),
        Err(OpenError::Corrupt(why)) => {
            let aside = move_aside(path)?;
            tracing::warn!(
                path = %path.display(),
                moved_to = %aside.display(),
                reason = %why,
                "the state database is corrupt; moved it aside and starting fresh"
            );
            try_open(path).map_err(|e| match e {
                OpenError::Other(e) => e,
                OpenError::Corrupt(why) => io::Error::new(io::ErrorKind::InvalidData, why),
            })
        }
    }
}

fn try_open(path: &Path) -> Result<Connection, OpenError> {
    prepare_files(path)?;
    let mut conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(io::Error::other(format!(
            "could not switch the state database to WAL (got {mode})"
        ))
        .into());
    }
    // secure_delete: deleted cookies are overwritten, not left in free pages.
    conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA secure_delete = ON;")?;
    quick_check(&conn)?;
    migrate(&mut conn)?;
    // SQLite has created -wal and -shm by now; make sure of their mode.
    prepare_files(path)?;
    Ok(conn)
}

fn quick_check(conn: &Connection) -> Result<(), OpenError> {
    let mut st = conn.prepare("PRAGMA quick_check")?;
    let rows = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.len() == 1 && rows[0] == "ok" {
        Ok(())
    } else {
        Err(OpenError::Corrupt(format!(
            "quick_check: {}",
            rows.join("; ")
        )))
    }
}

fn migrate(conn: &mut Connection) -> Result<(), OpenError> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let version = usize::try_from(version).unwrap_or(usize::MAX);
    if version > SCHEMA_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the state database has schema version {version}; this build understands up to {SCHEMA_VERSION}"
            ),
        )
        .into());
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = OsString::from(path.as_os_str());
    s.push(suffix);
    s.into()
}

/// Create the parent directory and an owner-only database file before
/// SQLite touches it (fsyncing the directory entries that creates), refuse
/// symbolic links for the database, `-wal` and `-shm`, and narrow any of
/// them that others can read to 0600.
fn prepare_files(path: &Path) -> io::Result<()> {
    let dir = sys::parent_or_cwd(path);
    let mut changed = sys::create_dirs(&dir, sys::DirMode::Private)?;
    let mut opts = sys::private_file_options();
    opts.read(true).write(true).create_new(true);
    match sys::open_nofollow(&mut opts, path) {
        Ok(_) => changed.push(dir),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let mut opts = std::fs::File::options();
            opts.read(true).write(true);
            sys::open_nofollow(&mut opts, path)?;
        }
        Err(e) => return Err(e),
    }
    sys::sync_dirs(&changed)?;
    for p in [
        path.to_path_buf(),
        with_suffix(path, "-wal"),
        with_suffix(path, "-shm"),
    ] {
        let meta = match std::fs::symlink_metadata(&p) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if meta.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is a symbolic link; refusing to follow it", p.display()),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = meta.permissions().mode();
            if mode & 0o077 != 0 {
                tracing::warn!(path = %p.display(), mode = format!("{:o}", mode & 0o777), "state file was readable by others; restricting it to the owner");
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
            }
        }
    }
    Ok(())
}

/// Move a corrupt database (and its WAL, which must never be replayed into
/// the fresh one) to `<path>.corrupt-<UTC timestamp>[-n]`.
fn move_aside(path: &Path) -> io::Result<PathBuf> {
    let stamp = utc_stamp(SystemTime::now());
    let mut aside = None;
    for n in 0..1000 {
        let candidate = if n == 0 {
            with_suffix(path, &format!(".corrupt-{stamp}"))
        } else {
            with_suffix(path, &format!(".corrupt-{stamp}-{n}"))
        };
        match sys::rename_noreplace(path, &candidate) {
            Ok(()) => {
                aside = Some(candidate);
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    let aside =
        aside.ok_or_else(|| io::Error::other("no free name to move the corrupt database to"))?;
    match sys::rename_noreplace(&with_suffix(path, "-wal"), &with_suffix(&aside, "-wal")) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    match std::fs::remove_file(with_suffix(path, "-shm")) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        sys::sync_dir(parent)?;
    }
    Ok(aside)
}

/// `YYYYMMDDTHHMMSSZ`.
fn utc_stamp(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
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
    fn utc_stamps() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        let t = UNIX_EPOCH + Duration::from_secs(1_762_779_845);
        assert_eq!(utc_stamp(t), "20251110T130405Z");
    }

    fn with_cookie(id: &str, cookie: Option<&str>) -> DownloadRecord {
        DownloadRecord {
            id: id.into(),
            url: "https://example.test/f".into(),
            request: cookie.map(|c| vayu_proto::RequestContext {
                cookie: Some(c.into()),
                ..Default::default()
            }),
            mirrors: vec![],
            filename: "f".into(),
            directory: "/d".into(),
            category: None,
            queue: None,
            state: vayu_proto::DownloadState::Downloading,
            total_bytes: None,
            downloaded_bytes: 0,
            resumable: true,
            checksum: None,
            added_at: 0,
            completed_at: None,
            error: None,
        }
    }

    fn files_contain(path: &Path, needle: &[u8]) -> bool {
        [path.to_path_buf(), with_suffix(path, "-wal")]
            .iter()
            .filter_map(|p| std::fs::read(p).ok())
            .any(|b| b.windows(needle.len()).any(|w| w == needle))
    }

    /// `secure_delete` only covers the main database; a cleared or deleted
    /// cookie must also leave the WAL: at once for deletions, within
    /// SECRET_CHECKPOINT for anything else, even while writes continue.
    #[tokio::test]
    async fn cleared_cookies_leave_the_wal() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("state.db");
        let s = SqliteStateStore::open(&path).await.unwrap();

        s.save_download(&with_cookie("a", Some("SECRET-COOKIE-AAAA")))
            .await
            .unwrap();
        assert!(files_contain(&path, b"SECRET-COOKIE-AAAA"));
        s.delete_downloads(&["a".into()]).await.unwrap();
        assert!(!files_contain(&path, b"SECRET-COOKIE-AAAA"));

        s.save_download(&with_cookie("b", Some("SECRET-COOKIE-BBBB")))
            .await
            .unwrap();
        s.save_download(&with_cookie("b", None)).await.unwrap();
        // Keep writing something else; the deadline still holds.
        let t = std::time::Instant::now();
        while t.elapsed() < SECRET_CHECKPOINT * 3 {
            s.set_value("tick", "1").await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!files_contain(&path, b"SECRET-COOKIE-BBBB"));
        assert_eq!(s.load_downloads().await.unwrap().len(), 1);
    }

    #[test]
    fn move_aside_takes_the_wal_and_drops_the_index() {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("state.db");
        std::fs::write(&db, b"db").unwrap();
        std::fs::write(with_suffix(&db, "-wal"), b"wal").unwrap();
        std::fs::write(with_suffix(&db, "-shm"), b"shm").unwrap();
        let first = move_aside(&db).unwrap();
        assert_eq!(std::fs::read(&first).unwrap(), b"db");
        assert_eq!(std::fs::read(with_suffix(&first, "-wal")).unwrap(), b"wal");
        assert!(!db.exists());
        assert!(!with_suffix(&db, "-wal").exists());
        assert!(!with_suffix(&db, "-shm").exists());
        // A second corruption in the same second gets its own name.
        std::fs::write(&db, b"db2").unwrap();
        let second = move_aside(&db).unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&second).unwrap(), b"db2");
        assert_eq!(std::fs::read(&first).unwrap(), b"db");
    }
}
