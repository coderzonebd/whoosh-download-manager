//! The engine side of contract amendment 1 (ADR 0005): per-download
//! connection counts, named queues at add time, `filename_locked`, and
//! `StorageProvider::file_len` to recognise a file finished just before a
//! crash.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use common::*;
use sha2::{Digest, Sha256};
use vayu_core::testing::{MemJournal, MemSourceFactory, MemStateStore};
use vayu_core::{
    ByteRange, Checkpoint, DownloadRecord, Journal, RangeSet, RemoteMeta, StateStore,
    StorageProvider,
};
use vayu_engine::{CONNECTIONS_KEY, Engine, EngineDeps};
use vayu_proto::service::{Service, ServiceError};
use vayu_proto::{Checksum, DownloadState, IdsParams, ListParams};

const LONG: Duration = Duration::from_secs(600);

#[tokio::test(start_paused = true)]
async fn a_pinned_connection_count_is_used_instead_of_learning() {
    let lab = Lab::new(config()).await;
    // Learning would climb to 16 here (per-connection throttle).
    let src = lab.serve(
        URL,
        Arc::new(FaultSource::new(content(24 * MIB, 1)).cfg(|c| c.per_conn_bps = Some(2 * MIB))),
    );
    let mut p = add_params(URL);
    p.connections = Some(3);
    let id = lab.engine.add(p).await.unwrap().id;
    finish(&lab.engine, &id, LONG).await;
    assert_eq!(src.peak(), 3);
    // Nothing learned from a pinned run.
    let profile = lab.state.host_profile("lab.test:443").await.unwrap();
    assert!(
        profile
            .as_ref()
            .is_none_or(|p| p.best_connections.is_none()),
        "{profile:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_pinned_count_may_exceed_the_settings_cap_but_yields_to_throttling() {
    let lab = Lab::new(config()).await;
    let mut s = lab.engine.settings().await.unwrap();
    s.max_connections_per_download = 2;
    lab.engine.set_settings(s).await.unwrap();
    let wide = lab.serve(
        URL,
        Arc::new(FaultSource::new(content(16 * MIB, 2)).cfg(|c| c.per_conn_bps = Some(MIB))),
    );
    let mut p = add_params(URL);
    p.connections = Some(6);
    let id = lab.engine.add(p).await.unwrap().id;
    finish(&lab.engine, &id, LONG).await;
    assert_eq!(wide.peak(), 6);

    let capped_url = "https://lab.test/capped.bin";
    let capped = lab.serve(
        capped_url,
        Arc::new(FaultSource::new(content(8 * MIB, 3)).cfg(|c| {
            c.per_conn_bps = Some(4 * MIB);
            c.cap = Some((2, Cap::Throttle(Some(Duration::from_millis(500)))));
        })),
    );
    let mut p = add_params(capped_url);
    p.connections = Some(8);
    let id = lab.engine.add(p).await.unwrap().id;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    assert_eq!(capped.peak(), 2);
    assert!(
        capped.opens() < 16,
        "lowered, not hammering: {}",
        capped.opens()
    );
    assert!(took < Duration::from_secs(3), "took {took:?}");
}

#[tokio::test(start_paused = true)]
async fn connection_counts_are_validated_and_survive_a_restart() {
    let lab = Lab::new(config()).await;
    for bad in [0u32, 65, 1000] {
        let mut p = add_params(URL);
        p.connections = Some(bad);
        let e = lab.engine.add(p).await.unwrap_err();
        assert_eq!(e.code, ServiceError::INVALID_PARAMS, "{bad}");
    }
    let src = lab.serve(
        URL,
        Arc::new(FaultSource::new(content(12 * MIB, 4)).cfg(|c| c.per_conn_bps = Some(2 * MIB))),
    );
    let mut p = add_params(URL);
    p.connections = Some(5);
    p.start = false;
    let id = lab.engine.add(p).await.unwrap().id;
    let json = lab.state.get_value(CONNECTIONS_KEY).await.unwrap().unwrap();
    assert!(json.contains(&format!("\"{id}\":5")), "{json}");
    lab.engine.shutdown().await;

    let again = Engine::start(lab.deps(), config()).await.unwrap();
    again
        .resume(IdsParams {
            ids: vec![id.clone()],
        })
        .await
        .unwrap();
    finish(&again, &id, LONG).await;
    assert_eq!(src.peak(), 5);
    again
        .remove(vayu_proto::RemoveParams {
            ids: vec![id.clone()],
            delete_files: false,
        })
        .await
        .unwrap();
    let json = lab.state.get_value(CONNECTIONS_KEY).await.unwrap().unwrap();
    assert!(!json.contains(&id), "forgotten on remove: {json}");
}

#[tokio::test(start_paused = true)]
async fn a_queue_can_be_chosen_at_add_time() {
    let lab = Lab::new(config()).await;
    let mut ids = Vec::new();
    for i in 0..3u64 {
        let url = format!("https://lab.test/q/{i}.bin");
        lab.serve(
            &url,
            Arc::new(
                FaultSource::new(content(2 * MIB, 10 + i)).cfg(|c| c.per_conn_bps = Some(4 * MIB)),
            ),
        );
        let mut p = add_params(&url);
        p.queue = Some("night".into());
        ids.push(lab.engine.add(p).await.unwrap().id);
    }
    let mut p = add_params("https://lab.test/q/main.bin");
    p.queue = Some(String::new());
    lab.serve(
        "https://lab.test/q/main.bin",
        Arc::new(FaultSource::new(content(MIB, 20))),
    );
    let main = lab.engine.add(p).await.unwrap().id;
    assert_eq!(
        lab.engine.record(&main).unwrap().queue,
        None,
        "empty = main queue"
    );
    assert_eq!(
        lab.engine.record(&ids[0]).unwrap().queue.as_deref(),
        Some("night")
    );
    let t0 = tokio::time::Instant::now();
    loop {
        let all = lab.engine.list(ListParams::default()).await.unwrap();
        let running = all
            .iter()
            .filter(|d| d.queue.as_deref() == Some("night"))
            .filter(|d| matches!(d.state, DownloadState::Probing | DownloadState::Downloading))
            .count();
        assert!(running <= 1, "{running} running in one named queue");
        if all.iter().all(|d| d.state == DownloadState::Completed) {
            break;
        }
        assert!(t0.elapsed() < LONG);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_locked_name_is_never_replaced_by_the_server_name() {
    let lab = Lab::new(config()).await;
    let u = "https://lab.test/get?id=1";
    lab.serve(
        u,
        Arc::new(
            FaultSource::new(content(KIB, 30)).cfg(|c| c.filename = Some("server.zip".into())),
        ),
    );
    let mut p = add_params(u);
    p.filename = Some("mine.zip".into());
    let locked = lab.engine.add(p).await.unwrap().id;
    let free = lab.add(u).await;
    assert!(lab.engine.record(&locked).unwrap().filename_locked);
    assert!(!lab.engine.record(&free).unwrap().filename_locked);
    let (dl, _) = finish(&lab.engine, &locked, LONG).await;
    let (df, _) = finish(&lab.engine, &free, LONG).await;
    assert_eq!(dl.summary.filename, "mine.zip");
    assert_eq!(df.summary.filename, "server.zip");

    // A stored record that is locked keeps its name across a restart even
    // before any byte was journalled (no heuristic involved).
    let state = Arc::new(MemStateStore::default());
    let sources = Arc::new(MemSourceFactory::default());
    let storage = Arc::new(vayu_core::testing::MemStorageProvider::default());
    sources.insert(
        u,
        Arc::new(
            FaultSource::new(content(KIB, 31)).cfg(|c| c.filename = Some("server.zip".into())),
        ),
    );
    let mut rec = record("dl_locked", u, "get");
    rec.filename_locked = true;
    rec.state = DownloadState::Queued;
    state.save_download(&rec).await.unwrap();
    let engine = Engine::start(
        EngineDeps {
            sources,
            storage: storage.clone(),
            journal: Arc::new(MemJournal::default()),
            state,
        },
        config(),
    )
    .await
    .unwrap();
    let (d, _) = finish(&engine, "dl_locked", LONG).await;
    assert_eq!(d.summary.filename, "get");
}

fn record(id: &str, url: &str, name: &str) -> DownloadRecord {
    DownloadRecord {
        id: id.into(),
        url: url.into(),
        request: None,
        mirrors: vec![],
        filename: name.into(),
        filename_locked: false,
        directory: DIR.into(),
        category: None,
        queue: None,
        state: DownloadState::Downloading,
        total_bytes: None,
        downloaded_bytes: 0,
        resumable: true,
        checksum: None,
        added_at: 1,
        completed_at: None,
        error: None,
    }
}

struct Crashed {
    fs: Arc<CrashFs>,
    src: Arc<FaultSource>,
    deps: EngineDeps,
    data: Bytes,
}

/// The state a crash leaves right after `Storage::finish` moved the file
/// into place but before the journal and the record were updated: the
/// journal claims the whole file, the part file is gone, the record still
/// says downloading. `existing` are files already in the directory.
async fn crashed_after_finish(
    final_name: &str,
    final_bytes: Option<Bytes>,
    existing: &[(&str, Bytes)],
    checksum: Option<Checksum>,
) -> Crashed {
    let data = content(3 * MIB, 40);
    let fs = Arc::new(CrashFs::default());
    for (name, bytes) in existing {
        finish_file(&fs, name, bytes.clone()).await;
    }
    finish_file(&fs, final_name, final_bytes.unwrap_or_else(|| data.clone())).await;
    let journal = Arc::new(MemJournal::default());
    let size = data.len() as u64;
    journal
        .commit(
            &"dl_crashed".to_string(),
            &Checkpoint {
                meta: RemoteMeta {
                    size: Some(size),
                    accepts_ranges: true,
                    etag: Some("\"v1\"".into()),
                    ..RemoteMeta::default()
                },
                done: RangeSet::from_ranges(ByteRange::new(0, size)),
            },
        )
        .await
        .unwrap();
    let state = Arc::new(MemStateStore::default());
    let mut rec = record("dl_crashed", URL, "data.bin");
    rec.checksum = checksum;
    state.save_download(&rec).await.unwrap();
    let sources = Arc::new(MemSourceFactory::default());
    let src = Arc::new(FaultSource::new(data.clone()));
    sources.insert(URL, src.clone());
    Crashed {
        deps: EngineDeps {
            sources,
            storage: fs.clone(),
            journal,
            state,
        },
        fs,
        src,
        data,
    }
}

async fn finish_file(fs: &CrashFs, name: &str, bytes: Bytes) {
    let tmp = Path::new(DIR).join(format!("{name}.tmp-setup"));
    let s = fs.open(&tmp).await.unwrap();
    s.write_at(0, bytes).await.unwrap();
    s.finish(&Path::new(DIR).join(name)).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_file_finished_before_a_crash_is_adopted() {
    let c = crashed_after_finish("data.bin", None, &[], None).await;
    let engine = Engine::start(c.deps.clone(), config()).await.unwrap();
    let (d, _) = finish(&engine, "dl_crashed", LONG).await;
    assert_eq!(d.summary.filename, "data.bin");
    assert_eq!(d.summary.downloaded_bytes, c.data.len() as u64);
    assert_eq!(c.src.opens(), 0, "nothing downloaded again");
    assert_eq!(c.fs.finished().len(), 1);
    assert!(
        c.deps
            .journal
            .load(&"dl_crashed".to_string())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(start_paused = true)]
async fn the_numbered_name_finish_chose_is_adopted() {
    let other = content(3 * MIB, 41); // same length, someone else's file
    let c = crashed_after_finish("data (1).bin", None, &[("data.bin", other)], None).await;
    let engine = Engine::start(c.deps.clone(), config()).await.unwrap();
    let (d, _) = finish(&engine, "dl_crashed", LONG).await;
    assert_eq!(d.summary.filename, "data (1).bin");
    assert_eq!(c.src.opens(), 0);
    assert_eq!(c.fs.finished().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_finished_file_of_the_wrong_length_is_not_adopted() {
    let c = crashed_after_finish("data.bin", Some(content(MIB, 42)), &[], None).await;
    let engine = Engine::start(c.deps.clone(), config()).await.unwrap();
    let (d, _) = finish(&engine, "dl_crashed", LONG).await;
    assert_eq!(d.summary.filename, "data (1).bin");
    assert!(c.src.opens() > 0, "downloaded again");
    assert_same(
        &c.fs.finished()[&Path::new(DIR).join("data (1).bin")],
        &c.data,
    );
}

#[tokio::test(start_paused = true)]
async fn the_checksum_decides_whether_a_finished_file_is_adopted() {
    let data = content(3 * MIB, 40);
    let good = Checksum {
        algorithm: "sha256".into(),
        value: hex::encode(Sha256::digest(&data)),
    };
    // Right length, right digest: adopted.
    let c = crashed_after_finish("data.bin", None, &[], Some(good.clone())).await;
    let engine = Engine::start(c.deps.clone(), config()).await.unwrap();
    let (d, _) = finish(&engine, "dl_crashed", LONG).await;
    assert_eq!(d.summary.filename, "data.bin");
    assert_eq!(c.src.opens(), 0);

    // Right length, other bytes: the digest fails, so it is downloaded
    // again (and verified) under the next free name.
    let c = crashed_after_finish("data.bin", Some(content(3 * MIB, 43)), &[], Some(good)).await;
    let engine = Engine::start(c.deps.clone(), config()).await.unwrap();
    let (d, _) = finish(&engine, "dl_crashed", LONG).await;
    assert_eq!(d.summary.filename, "data (1).bin");
    assert!(c.src.opens() > 0);
    assert_same(
        &c.fs.finished()[&Path::new(DIR).join("data (1).bin")],
        &c.data,
    );
}
