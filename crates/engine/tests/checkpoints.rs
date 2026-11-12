//! Durability cadence: checkpoints are spaced by what a flush costs, and a
//! download that finishes right away commits nothing it does not need.

mod common;

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use common::*;
use futures::future::BoxFuture;
use sha2::{Digest, Sha256};
use vayu_core::testing::{MemJournal, MemSourceFactory, MemStateStore, MemStorageProvider};
use vayu_core::{Storage, StorageProvider};
use vayu_engine::{Engine, EngineDeps, Tuning};
use vayu_proto::service::Service;
use vayu_proto::{AddParams, Checksum};

const LONG: Duration = Duration::from_secs(600);

/// Storage whose `flush` takes `delay` (a disk that is the bottleneck),
/// counting flushes.
struct SlowFlush {
    inner: Arc<MemStorageProvider>,
    delay: Duration,
    flushes: Arc<AtomicUsize>,
}

struct SlowFile {
    inner: Arc<dyn Storage>,
    delay: Duration,
    flushes: Arc<AtomicUsize>,
}

impl Storage for SlowFile {
    fn preallocate(&self, len: u64) -> BoxFuture<'_, io::Result<()>> {
        self.inner.preallocate(len)
    }
    fn write_at(&self, offset: u64, data: Bytes) -> BoxFuture<'_, io::Result<()>> {
        self.inner.write_at(offset, data)
    }
    fn read_at(&self, offset: u64, len: usize) -> BoxFuture<'_, io::Result<Bytes>> {
        self.inner.read_at(offset, len)
    }
    fn flush(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            self.flushes.fetch_add(1, Ordering::Relaxed);
            tokio::time::sleep(self.delay).await;
            self.inner.flush().await
        })
    }
    fn finish<'a>(&'a self, final_path: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        self.inner.finish(final_path)
    }
}

impl StorageProvider for SlowFlush {
    fn open<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<Arc<dyn Storage>>> {
        Box::pin(async move {
            let inner = self.inner.open(path).await?;
            Ok(Arc::new(SlowFile {
                inner,
                delay: self.delay,
                flushes: self.flushes.clone(),
            }) as Arc<dyn Storage>)
        })
    }
    fn file_len<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<Option<u64>>> {
        self.inner.file_len(path)
    }
    fn remove<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        self.inner.remove(path)
    }
}

/// 64 MiB on one connection at 2 MiB/s (32 s) behind flushes of `delay`;
/// returns the number of journal commits.
async fn commits_with_flush(delay: Duration) -> usize {
    let sources = Arc::new(MemSourceFactory::default());
    let files = Arc::new(MemStorageProvider::default());
    let journal = Arc::new(MemJournal::default());
    let mut cfg = config();
    cfg.tuning = Tuning {
        fixed_connections: Some(1),
        ..Tuning::default()
    };
    let engine = Engine::start(
        EngineDeps {
            sources: sources.clone(),
            storage: Arc::new(SlowFlush {
                inner: files.clone(),
                delay,
                flushes: Arc::new(AtomicUsize::new(0)),
            }),
            journal: journal.clone(),
            state: Arc::new(MemStateStore::default()),
        },
        cfg,
    )
    .await
    .unwrap();
    let data = content(64 * MIB, 201);
    sources.insert(
        URL,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| c.per_conn_bps = Some(2 * MIB))),
    );
    let id = engine.add(add_params(URL)).await.unwrap().id;
    let (_, took) = finish(&engine, &id, LONG).await;
    assert!(took < Duration::from_secs(36), "took {took:?}");
    assert_same(
        &files.finished(&Path::new(DIR).join("data.bin")).unwrap(),
        &data,
    );
    journal.commits()
}

#[tokio::test(start_paused = true)]
async fn slow_flushes_space_checkpoints_out() {
    // Cheap flushes: one checkpoint every 2 s.
    let fast = commits_with_flush(Duration::from_millis(1)).await;
    assert!((14..=18).contains(&fast), "{fast} commits");
    // A 400 ms flush is next due 8 s later (5 % of the time).
    let slow = commits_with_flush(Duration::from_millis(400)).await;
    assert!((4..=6).contains(&slow), "{slow} commits");
}

#[tokio::test(start_paused = true)]
async fn a_fresh_download_commits_only_its_finish_intent() {
    let lab = Lab::new(config()).await;
    let data = content(2 * MIB, 202);
    lab.serve(URL, Arc::new(FaultSource::new(data.clone())));
    let id = lab.add(URL).await;
    finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    // One commit, when the transfer starts: it carries the finish intent,
    // so finishing commits nothing more. No final checkpoint before the
    // move (the intent's file at its length is what a restart looks for).
    assert_eq!(lab.journal.commits(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_checksum_is_verified_after_a_checkpoint() {
    let lab = Lab::new(config()).await;
    let data = content(2 * MIB, 203);
    lab.serve(URL, Arc::new(FaultSource::new(data.clone())));
    let id = lab
        .engine
        .add(AddParams {
            checksum: Some(Checksum {
                algorithm: "sha256".into(),
                value: hex::encode(Sha256::digest(&data)),
            }),
            ..add_params(URL)
        })
        .await
        .unwrap()
        .id;
    finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    // The early checkpoint (with the intent), the one claiming every byte
    // before the long read (it withdraws the intent), the intent again.
    assert_eq!(lab.journal.commits(), 3);
}

/// A fast download (64 MiB at 256 MiB/s per connection, one connection)
/// behind flushes of 5 ms; returns (flushes, journal commits).
async fn writeback_counts(ranges: bool) -> (usize, usize) {
    let sources = Arc::new(MemSourceFactory::default());
    let files = Arc::new(MemStorageProvider::default());
    let journal = Arc::new(MemJournal::default());
    let flushes = Arc::new(AtomicUsize::new(0));
    let mut cfg = config();
    cfg.tuning = Tuning {
        fixed_connections: Some(1),
        ..Tuning::default()
    };
    let engine = Engine::start(
        EngineDeps {
            sources: sources.clone(),
            storage: Arc::new(SlowFlush {
                inner: files.clone(),
                delay: Duration::from_millis(5),
                flushes: flushes.clone(),
            }),
            journal: journal.clone(),
            state: Arc::new(MemStateStore::default()),
        },
        cfg,
    )
    .await
    .unwrap();
    let data = content(64 * MIB, 204);
    sources.insert(
        URL,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| {
            c.per_conn_bps = Some(256 * MIB);
            c.chunk = 256 * KIB as usize;
            c.accepts_ranges = ranges;
            c.honours_ranges = ranges;
        })),
    );
    let id = engine.add(add_params(URL)).await.unwrap().id;
    finish(&engine, &id, LONG).await;
    assert_same(
        &files.finished(&Path::new(DIR).join("data.bin")).unwrap(),
        &data,
    );
    (flushes.load(Ordering::Relaxed), journal.commits())
}

#[tokio::test(start_paused = true)]
async fn a_fast_transfer_is_flushed_as_it_goes() {
    // 64 MiB in 250 ms: no time-based checkpoint is due, but every 16 MiB
    // of new data is flushed (and committed) while the transfer runs, so
    // the flush before the rename has little left.
    let (flushes, commits) = writeback_counts(true).await;
    assert!(flushes >= 4, "{flushes} flushes");
    assert!(commits >= 4, "{commits} commits");
    // A single stream (no ranges) is flushed too, but journals nothing
    // before its finish intent.
    let (flushes, commits) = writeback_counts(false).await;
    assert!(flushes >= 4, "{flushes} flushes");
    assert_eq!(commits, 1);
}
