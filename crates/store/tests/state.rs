//! `SqliteStateStore`: the `StateStore` contract, persistence, file
//! permissions and corruption recovery.
#![cfg(feature = "sqlite")]

use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use futures::future::join_all;
use vayu_core::source::Protocol;
use vayu_core::{DownloadRecord, HostProfile, StateStore};
use vayu_proto::{Checksum, DownloadState, ErrorInfo, Header, RequestContext};
use vayu_store::SqliteStateStore;

/// A realistic record (~700 bytes of JSON, with a cookie).
fn record(i: u64) -> DownloadRecord {
    DownloadRecord {
        id: format!("dl_{i:08}"),
        url: format!("https://cdn{}.example.com/releases/v{i}/archive-{i}.tar.zst?sig=abcdef0123456789&exp=1762780000", i % 7),
        request: Some(RequestContext {
            method: None,
            headers: vec![Header {
                name: "Accept-Language".into(),
                value: "en-US,en;q=0.9".into(),
            }],
            cookie: Some(format!("session=s{i}x9f8e7d6c5b4a; csrftoken=tok{i}; _ga=GA1.2.123456789.1762780000")),
            referer: Some("https://example.com/downloads".into()),
            user_agent: Some("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0 Safari/537.36".into()),
            body_base64: None,
        }),
        mirrors: vec![],
        filename: format!("archive-{i}.tar.zst"),
        filename_locked: i.is_multiple_of(4),
        directory: "/home/user/Downloads/Compressed".into(),
        category: Some("compressed".into()),
        queue: Some("main".into()),
        state: if i.is_multiple_of(3) {
            DownloadState::Completed
        } else {
            DownloadState::Paused
        },
        total_bytes: Some(1_000_000 + i),
        downloaded_bytes: i * 1000,
        resumable: true,
        checksum: i.is_multiple_of(5).then(|| Checksum {
            algorithm: "sha256".into(),
            value: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
        }),
        added_at: 1_762_780_000_000 + i as i64,
        completed_at: i.is_multiple_of(3).then_some(1_762_790_000_000 + i as i64),
        error: i.is_multiple_of(11).then(|| ErrorInfo {
            code: "disk_full".into(),
            message: "No space left on device".into(),
        }),
    }
}

fn host(name: &str, conns: u16) -> HostProfile {
    HostProfile {
        host: name.into(),
        best_connections: Some(conns),
        max_ok_connections: Some(conns * 2),
        accepts_ranges: Some(true),
        protocol: Some(Protocol::Http2),
        per_connection_bps: Some(12_000_000),
        updated_at: 1_762_780_000_000,
    }
}

fn sorted(mut v: Vec<DownloadRecord>) -> Vec<DownloadRecord> {
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v
}

fn siblings(path: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn roundtrip_and_persistence() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("data/state.db");
    let s = SqliteStateStore::open(&path).await.unwrap();
    assert!(s.load_downloads().await.unwrap().is_empty());
    for i in 0..3 {
        s.save_download(&record(i)).await.unwrap();
    }
    let mut changed = record(1);
    changed.state = DownloadState::Downloading;
    changed.request = None;
    s.save_download(&changed).await.unwrap();
    s.delete_downloads(&["dl_00000000".into(), "missing".into()])
        .await
        .unwrap();
    s.delete_downloads(&[]).await.unwrap();

    assert_eq!(s.get_value("settings").await.unwrap(), None);
    s.set_value("settings", r#"{"maxConnections":16}"#)
        .await
        .unwrap();
    s.set_value("settings", r#"{"maxConnections":32}"#)
        .await
        .unwrap();
    s.set_value("queues", "[]").await.unwrap();
    let e = s.set_value("bad", "{not json").await.unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);

    assert_eq!(s.host_profile("cdn.example.com:443").await.unwrap(), None);
    s.save_host_profile(&host("cdn.example.com:443", 8))
        .await
        .unwrap();
    s.save_host_profile(&host("cdn.example.com:443", 6))
        .await
        .unwrap();

    let expect = sorted(vec![changed.clone(), record(2)]);
    assert_eq!(sorted(s.load_downloads().await.unwrap()), expect);
    s.close().await.unwrap();

    let s = SqliteStateStore::open(&path).await.unwrap();
    assert_eq!(sorted(s.load_downloads().await.unwrap()), expect);
    assert_eq!(
        s.get_value("settings").await.unwrap().as_deref(),
        Some(r#"{"maxConnections":32}"#)
    );
    assert_eq!(s.get_value("queues").await.unwrap().as_deref(), Some("[]"));
    assert_eq!(s.get_value("bad").await.unwrap(), None);
    assert_eq!(
        s.host_profile("cdn.example.com:443").await.unwrap(),
        Some(host("cdn.example.com:443", 6))
    );
    // Dropping without close is fine too.
    drop(s);
    let s = SqliteStateStore::open(&path).await.unwrap();
    assert_eq!(s.load_downloads().await.unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bursts_of_saves_are_batched_and_all_land() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("state.db");
    let s = SqliteStateStore::open(&path).await.unwrap();
    let records: Vec<_> = (0..2000).map(record).collect();
    let results = join_all(records.iter().map(|r| s.save_download(r))).await;
    assert!(results.iter().all(Result::is_ok));
    // Interleave reads and writes in one burst: reads see earlier writes.
    let mut r5 = record(5);
    r5.downloaded_bytes = 42;
    let (w, l) = futures::join!(s.save_download(&r5), s.load_downloads());
    w.unwrap();
    assert_eq!(l.unwrap().len(), 2000);
    s.close().await.unwrap();
    let s = SqliteStateStore::open(&path).await.unwrap();
    let all = sorted(s.load_downloads().await.unwrap());
    assert_eq!(all.len(), 2000);
    assert_eq!(all[5].downloaded_bytes, 42);
    assert_eq!(all[1999], record(1999));
}

#[tokio::test]
async fn loads_ten_thousand_downloads_quickly() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("state.db");
    let s = SqliteStateStore::open(&path).await.unwrap();
    let records: Vec<_> = (0..10_000).map(record).collect();
    for chunk in records.chunks(500) {
        let r = join_all(chunk.iter().map(|r| s.save_download(r))).await;
        assert!(r.iter().all(Result::is_ok));
    }
    s.close().await.unwrap();
    let s = SqliteStateStore::open(&path).await.unwrap();
    let t = Instant::now();
    let all = s.load_downloads().await.unwrap();
    let took = t.elapsed();
    assert_eq!(all.len(), 10_000);
    eprintln!("load_downloads(10,000): {took:?} (this build profile)");
    // The 200 ms target is for release builds (see examples/state_bench.rs);
    // debug builds parse JSON several times slower.
    assert!(took.as_millis() < 2000, "{took:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn files_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("vayu");
    let path = dir.join("state.db");
    let s = SqliteStateStore::open(&path).await.unwrap();
    s.save_download(&record(1)).await.unwrap();
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&path), 0o600);
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    let shm = PathBuf::from(format!("{}-shm", path.display()));
    assert!(wal.exists() && shm.exists(), "{:?}", siblings(&path));
    assert_eq!(mode(&wal), 0o600);
    assert_eq!(mode(&shm), 0o600);
    s.close().await.unwrap();

    // A database someone made world-readable is narrowed on open.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let s = SqliteStateStore::open(&path).await.unwrap();
    assert_eq!(mode(&path), 0o600);
    assert_eq!(s.load_downloads().await.unwrap(), vec![record(1)]);
}

#[tokio::test]
async fn garbage_is_moved_aside_with_its_wal() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("state.db");
    let garbage = b"this is not an SQLite database\n".repeat(200);
    std::fs::write(&path, &garbage).unwrap();
    std::fs::write(format!("{}-wal", path.display()), b"stale wal").unwrap();
    std::fs::write(format!("{}-shm", path.display()), b"stale shm").unwrap();

    let s = SqliteStateStore::open(&path).await.unwrap();
    assert!(s.load_downloads().await.unwrap().is_empty());
    s.save_download(&record(7)).await.unwrap();
    assert_eq!(s.load_downloads().await.unwrap(), vec![record(7)]);

    let names = siblings(&path);
    let aside: Vec<_> = names
        .iter()
        .filter(|n| n.starts_with("state.db.corrupt-"))
        .collect();
    let main = aside
        .iter()
        .find(|n| !n.ends_with("-wal"))
        .expect("moved aside");
    assert!(main.ends_with('Z'), "timestamped: {main}");
    assert_eq!(std::fs::read(d.path().join(main)).unwrap(), garbage);
    // The old WAL must never be replayed into the fresh database: it is
    // moved aside with the database if it still exists (SQLite itself
    // discards one too short to be valid when the failed connection closes).
    if let Some(wal) = aside.iter().find(|n| n.ends_with("-wal")) {
        assert_eq!(std::fs::read(d.path().join(wal)).unwrap(), b"stale wal");
    }
    let live_wal = std::fs::read(format!("{}-wal", path.display())).unwrap_or_default();
    assert!(!live_wal.starts_with(b"stale wal"));
}

#[tokio::test]
async fn damaged_pages_fail_quick_check_and_are_moved_aside() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("state.db");
    let s = SqliteStateStore::open(&path).await.unwrap();
    let records: Vec<_> = (0..3000).map(record).collect();
    join_all(records.iter().map(|r| s.save_download(r))).await;
    s.close().await.unwrap();

    // Overwrite a few b-tree pages in the middle with junk; page 1 (the
    // header) stays intact, so only quick_check can tell.
    let mut bytes = std::fs::read(&path).unwrap();
    let pages = bytes.len() / 4096;
    assert!(pages > 50, "{pages} pages");
    for p in [pages / 3, pages / 2, pages * 2 / 3] {
        bytes[p * 4096..(p + 1) * 4096].fill(0x55);
    }
    std::fs::write(&path, &bytes).unwrap();

    let s = SqliteStateStore::open(&path).await.unwrap();
    assert!(s.load_downloads().await.unwrap().is_empty());
    let names = siblings(&path);
    assert!(
        names.iter().any(|n| n.starts_with("state.db.corrupt-")),
        "{names:?}"
    );
}

#[tokio::test]
async fn a_newer_schema_is_refused_not_discarded() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("state.db");
    let s = SqliteStateStore::open(&path).await.unwrap();
    s.save_download(&record(1)).await.unwrap();
    s.close().await.unwrap();
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.pragma_update(None, "user_version", 99).unwrap();
    }
    let e = SqliteStateStore::open(&path).await.unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    assert!(e.to_string().contains("99"), "{e}");
    assert!(siblings(&path).iter().all(|n| !n.contains("corrupt")));
}

#[tokio::test]
async fn an_unreadable_row_is_skipped_not_fatal() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("state.db");
    let s = SqliteStateStore::open(&path).await.unwrap();
    s.save_download(&record(1)).await.unwrap();
    s.close().await.unwrap();
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute(
            "INSERT INTO downloads (id, added_at, json) VALUES ('x', 0, '{\"from\":\"the future\"}')",
            [],
        )
        .unwrap();
    }
    let s = SqliteStateStore::open(&path).await.unwrap();
    assert_eq!(s.load_downloads().await.unwrap(), vec![record(1)]);
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_database_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let target = d.path().join("precious");
    std::fs::write(&target, b"do not touch").unwrap();
    let path = d.path().join("state.db");
    std::os::unix::fs::symlink(&target, &path).unwrap();
    let e = SqliteStateStore::open(&path).await.unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");
    assert_eq!(std::fs::read(&target).unwrap(), b"do not touch");

    // A planted -wal link is refused too.
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&target, format!("{}-wal", path.display())).unwrap();
    let e = SqliteStateStore::open(&path).await.unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");
    assert_eq!(std::fs::read(&target).unwrap(), b"do not touch");
}

/// `filename_locked` (contract amendment 1) is stored with the record;
/// rows written before it existed read as `false`.
#[tokio::test]
async fn filename_locked_round_trips_and_old_rows_read_false() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("state.db");
    let s = SqliteStateStore::open(&path).await.unwrap();
    assert!(record(4).filename_locked && !record(1).filename_locked);
    s.save_download(&record(4)).await.unwrap();
    s.save_download(&record(1)).await.unwrap();
    s.close().await.unwrap();
    {
        // A row as an older build wrote it: no `filename_locked` key.
        let mut old = serde_json::to_value(record(8)).unwrap();
        old.as_object_mut()
            .unwrap()
            .remove("filename_locked")
            .unwrap();
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute(
            "INSERT INTO downloads (id, added_at, json) VALUES (?1, 0, ?2)",
            [record(8).id, old.to_string()],
        )
        .unwrap();
    }
    let s = SqliteStateStore::open(&path).await.unwrap();
    let all = sorted(s.load_downloads().await.unwrap());
    let mut old8 = record(8);
    old8.filename_locked = false;
    assert_eq!(all, vec![record(1), record(4), old8]);
}

fn rows_in_main_file_alone(path: &Path) -> i64 {
    // Copy only `state.db`, as if the WAL had been lost.
    let d = tempfile::tempdir().unwrap();
    let copy = d.path().join("alone.db");
    std::fs::copy(path, &copy).unwrap();
    let c = rusqlite::Connection::open(&copy).unwrap();
    c.query_row("SELECT count(*) FROM downloads", [], |r| r.get(0))
        .unwrap()
}

fn rows_after_power_loss(path: &Path) -> i64 {
    // What survives a power cut once synced: the database and its WAL (the
    // shared-memory index is rebuilt from the WAL).
    let d = tempfile::tempdir().unwrap();
    let copy = d.path().join("image.db");
    std::fs::copy(path, &copy).unwrap();
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    if wal.exists() {
        std::fs::copy(&wal, format!("{}-wal", copy.display())).unwrap();
    }
    let c = rusqlite::Connection::open(&copy).unwrap();
    c.query_row("SELECT count(*) FROM downloads", [], |r| r.get(0))
        .unwrap()
}

/// `sync` (contract amendment 2): every save that resolved, and any queued
/// in the same burst, is in the files that survive a power cut: the
/// database and its WAL, with the WAL fdatasynced.
#[tokio::test]
async fn sync_covers_every_resolved_save() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("state.db");
    let s = SqliteStateStore::open(&path).await.unwrap();
    s.sync().await.unwrap(); // nothing to do is fine
    s.save_download(&record(1)).await.unwrap();
    let r2 = record(2);
    let (saved, synced) = futures::join!(s.save_download(&r2), s.sync());
    saved.unwrap();
    synced.unwrap();
    assert_eq!(rows_after_power_loss(&path), 2);
    s.save_download(&record(3)).await.unwrap();
    s.sync().await.unwrap();
    assert_eq!(rows_after_power_loss(&path), 3);
    // Several syncs at once all succeed.
    let rs = join_all((0..5).map(|_| s.sync())).await;
    assert!(rs.iter().all(Result::is_ok));
}

/// Syncing the WAL needs no checkpoint, so a reader in another connection
/// no longer stands in its way.
#[tokio::test]
async fn a_reader_does_not_block_sync() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("state.db");
    let s = SqliteStateStore::open(&path).await.unwrap();
    s.save_download(&record(1)).await.unwrap();
    s.sync().await.unwrap(); // the fresh database's one full checkpoint
    let reader = rusqlite::Connection::open(&path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let n: i64 = reader
        .query_row("SELECT count(*) FROM downloads", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    s.save_download(&record(2)).await.unwrap();
    s.sync().await.unwrap();
    assert_eq!(rows_after_power_loss(&path), 2);
    reader.execute_batch("COMMIT").unwrap();
}

/// The one checkpoint a fresh database needs can still be blocked by
/// another connection: then `sync` fails rather than claim durability.
#[tokio::test]
async fn a_blocked_first_checkpoint_fails_sync() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("state.db");
    let s = SqliteStateStore::open(&path).await.unwrap();
    s.save_download(&record(1)).await.unwrap();
    let reader = rusqlite::Connection::open(&path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let _: i64 = reader
        .query_row("SELECT count(*) FROM downloads", [], |r| r.get(0))
        .unwrap();
    s.save_download(&record(2)).await.unwrap();
    let e = s.sync().await.unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::ResourceBusy, "{e}");
    reader.execute_batch("COMMIT").unwrap();
    drop(reader);
    s.sync().await.unwrap();
    assert_eq!(rows_in_main_file_alone(&path), 2);
}
