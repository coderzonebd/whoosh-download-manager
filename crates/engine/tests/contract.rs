//! The engine side of contract amendment 1 (ADR 0005): per-download
//! connection counts, named queues at add time and `filename_locked`.
//! (`StorageProvider::file_len`, recognising a file finished just before a
//! crash, is exercised in `tests/finish.rs`.)

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use vayu_core::testing::{MemJournal, MemSourceFactory, MemStateStore};
use vayu_core::{DownloadRecord, StateStore};
use vayu_engine::{CONNECTIONS_KEY, Engine, EngineDeps};
use vayu_proto::service::{Service, ServiceError};
use vayu_proto::{DownloadState, IdsParams, ListParams};

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
