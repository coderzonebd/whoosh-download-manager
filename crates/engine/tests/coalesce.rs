//! Write gathering: one `write_at` per ~512 KiB per connection instead of
//! one per network chunk, without changing what counts as written.

mod common;

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use common::*;
use futures::future::BoxFuture;
use vayu_core::testing::{MemJournal, MemSourceFactory, MemStateStore, MemStorageProvider};
use vayu_core::{Storage, StorageProvider};
use vayu_engine::{Engine, EngineConfig, EngineDeps, Tuning};
use vayu_proto::service::Service;

const LONG: Duration = Duration::from_secs(600);

/// Counts `write_at` calls and their sizes.
#[derive(Default)]
struct Counts {
    writes: AtomicU64,
    largest: AtomicU64,
}

struct Counting {
    inner: Arc<dyn Storage>,
    counts: Arc<Counts>,
}

impl Storage for Counting {
    fn preallocate(&self, len: u64) -> BoxFuture<'_, io::Result<()>> {
        self.inner.preallocate(len)
    }
    fn write_at(&self, offset: u64, data: Bytes) -> BoxFuture<'_, io::Result<()>> {
        self.counts.writes.fetch_add(1, Ordering::Relaxed);
        self.counts
            .largest
            .fetch_max(data.len() as u64, Ordering::Relaxed);
        self.inner.write_at(offset, data)
    }
    fn read_at(&self, offset: u64, len: usize) -> BoxFuture<'_, io::Result<Bytes>> {
        self.inner.read_at(offset, len)
    }
    fn flush(&self) -> BoxFuture<'_, io::Result<()>> {
        self.inner.flush()
    }
    fn finish<'a>(&'a self, p: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        self.inner.finish(p)
    }
}

struct CountingProvider {
    inner: MemStorageProvider,
    counts: Arc<Counts>,
}

impl StorageProvider for CountingProvider {
    fn open<'a>(&'a self, p: &'a Path) -> BoxFuture<'a, io::Result<Arc<dyn Storage>>> {
        Box::pin(async move {
            let inner = self.inner.open(p).await?;
            Ok(Arc::new(Counting {
                inner,
                counts: self.counts.clone(),
            }) as Arc<dyn Storage>)
        })
    }
    fn file_len<'a>(&'a self, p: &'a Path) -> BoxFuture<'a, io::Result<Option<u64>>> {
        self.inner.file_len(p)
    }
    fn remove<'a>(&'a self, p: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        self.inner.remove(p)
    }
}

struct Rig {
    engine: Arc<Engine>,
    sources: Arc<MemSourceFactory>,
    storage: Arc<CountingProvider>,
}

async fn rig(tuning: Tuning) -> Rig {
    let sources = Arc::new(MemSourceFactory::default());
    let storage = Arc::new(CountingProvider {
        inner: MemStorageProvider::default(),
        counts: Arc::default(),
    });
    let mut cfg: EngineConfig = config();
    cfg.tuning = tuning;
    let engine = Engine::start(
        EngineDeps {
            sources: sources.clone(),
            storage: storage.clone(),
            journal: Arc::new(MemJournal::default()),
            state: Arc::new(MemStateStore::default()),
        },
        cfg,
    )
    .await
    .unwrap();
    Rig {
        engine,
        sources,
        storage,
    }
}

fn small_chunks(data: Bytes, bps: u64) -> Arc<FaultSource> {
    Arc::new(FaultSource::new(data).cfg(|c| {
        c.chunk = 16 * KIB as usize;
        c.per_conn_bps = Some(bps);
    }))
}

#[tokio::test(start_paused = true)]
async fn network_chunks_are_gathered_into_few_writes() {
    let r = rig(Tuning {
        fixed_connections: Some(4),
        ..Tuning::default()
    })
    .await;
    let size = 16 * MIB;
    let data = content(size, 1);
    r.sources.insert(URL, small_chunks(data.clone(), 8 * MIB));
    let id = r.engine.add(add_params(URL)).await.unwrap().id;
    finish(&r.engine, &id, LONG).await;
    assert_same(
        &r.storage
            .inner
            .finished(&Path::new(DIR).join("data.bin"))
            .unwrap(),
        &data,
    );
    let writes = r.storage.counts.writes.load(Ordering::Relaxed);
    let chunks = size / (16 * KIB);
    // 16 MiB / 512 KiB = 32 full writes, plus one partial per range end.
    let gather = Tuning::default().write_coalesce as u64;
    // Full gathers, plus one partial write per range end.
    assert!(
        writes <= size / gather + 16,
        "{writes} writes for {chunks} chunks"
    );
    assert!(r.storage.counts.largest.load(Ordering::Relaxed) <= gather + 16 * KIB);
}

#[tokio::test(start_paused = true)]
async fn gathering_can_be_turned_off() {
    let r = rig(Tuning {
        fixed_connections: Some(4),
        write_coalesce: 0,
        ..Tuning::default()
    })
    .await;
    let size = 4 * MIB;
    let data = content(size, 2);
    r.sources.insert(URL, small_chunks(data.clone(), 8 * MIB));
    let id = r.engine.add(add_params(URL)).await.unwrap().id;
    finish(&r.engine, &id, LONG).await;
    assert_eq!(
        r.storage.counts.writes.load(Ordering::Relaxed),
        size / (16 * KIB),
        "one write per chunk"
    );
}

#[tokio::test(start_paused = true)]
async fn a_slow_connection_still_writes_within_the_delay() {
    let r = rig(Tuning {
        fixed_connections: Some(1),
        ..Tuning::default()
    })
    .await;
    // 64 KiB/s in 16 KiB chunks: a chunk every 250 ms, far below 512 KiB
    // per write; the 200 ms delay must still write them out promptly.
    let size = MIB;
    let data = content(size, 3);
    let src = small_chunks(data.clone(), 64 * KIB);
    r.sources.insert(URL, src.clone());
    let id = r.engine.add(add_params(URL)).await.unwrap().id;
    let mut worst_lag = 0;
    for _ in 0..25 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let written = detail(&r.engine, &id).await.summary.downloaded_bytes;
        worst_lag = worst_lag.max(src.served().saturating_sub(written));
    }
    // Received but not yet written: at most a couple of chunks.
    assert!(worst_lag <= 48 * KIB, "lag {worst_lag}");
    finish(&r.engine, &id, LONG).await;
    assert_same(
        &r.storage
            .inner
            .finished(&Path::new(DIR).join("data.bin"))
            .unwrap(),
        &data,
    );
}

#[tokio::test(start_paused = true)]
async fn gathered_bytes_survive_a_hedge_takeover_and_a_pause() {
    let r = rig(Tuning::default()).await;
    let size = 24 * MIB;
    let data = content(size, 4);
    // The second request stalls with ~200 KiB gathered; a hedge takes over
    // and the stalled owner must still write what it gathered.
    r.sources.insert(
        URL,
        Arc::new(
            FaultSource::new(data.clone())
                .cfg(|c| {
                    c.chunk = 16 * KIB as usize;
                    c.per_conn_bps = Some(4 * MIB);
                })
                .script(|n, _| ReqPlan {
                    stall: (n == 2).then_some((200 * KIB, Duration::from_secs(30))),
                    ..ReqPlan::default()
                }),
        ),
    );
    let id = r.engine.add(add_params(URL)).await.unwrap().id;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let ids = vayu_proto::IdsParams {
        ids: vec![id.clone()],
    };
    r.engine.pause(ids.clone()).await.unwrap();
    r.engine.resume(ids).await.unwrap();
    finish(&r.engine, &id, LONG).await;
    assert_same(
        &r.storage
            .inner
            .finished(&Path::new(DIR).join("data.bin"))
            .unwrap(),
        &data,
    );
}
