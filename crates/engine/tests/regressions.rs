//! Regression tests for defects found in review (2026-11-10).

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use common::*;
use vayu_core::{Journal, SourceError, StorageProvider};
use vayu_engine::Tuning;
use vayu_proto::service::Service;
use vayu_proto::{
    DownloadState, Header, IdsParams, ListParams, RefreshUrlParams, RemoveParams, RequestContext,
};

const LONG: Duration = Duration::from_secs(600);

async fn plant(lab: &Lab, name: &str, data: Bytes) {
    let part = Path::new(DIR).join(format!("{name}.vayu-part"));
    let s = lab.storage.open(&part).await.unwrap();
    s.write_at(0, data).await.unwrap();
}

/// An engine over `CrashFs` whose `preallocate` only grows files, so the
/// engine itself must get rid of stale bytes (the contract now has the
/// store truncate, but the engine does not rely on it).
async fn growing_store() -> (
    Arc<vayu_engine::Engine>,
    Arc<vayu_core::testing::MemSourceFactory>,
    Arc<CrashFs>,
) {
    let sources = Arc::new(vayu_core::testing::MemSourceFactory::default());
    let fs = Arc::new(CrashFs::growing());
    let engine = vayu_engine::Engine::start(
        vayu_engine::EngineDeps {
            sources: sources.clone(),
            storage: fs.clone(),
            journal: Arc::new(vayu_core::testing::MemJournal::default()),
            state: Arc::new(vayu_core::testing::MemStateStore::default()),
        },
        config(),
    )
    .await
    .unwrap();
    (engine, sources, fs)
}

/// #1: a stale `<name>.vayu-part` must never end up inside a new file.
#[tokio::test(start_paused = true)]
async fn a_stale_part_file_is_not_reused() {
    let (engine, sources, fs) = growing_store().await;
    // Leftover of another, bigger file with the same name.
    let stale = fs
        .open(&Path::new(DIR).join("data.bin.vayu-part"))
        .await
        .unwrap();
    stale.write_at(0, content(10 * MIB, 99)).await.unwrap();
    let data = content(2 * MIB, 1);
    sources.insert(URL, Arc::new(FaultSource::new(data.clone())));
    let id = engine.add(add_params(URL)).await.unwrap().id;
    finish(&engine, &id, LONG).await;
    assert_same(&fs.finished()[&Path::new(DIR).join("data.bin")], &data);
}

#[tokio::test(start_paused = true)]
async fn removing_but_keeping_files_does_not_poison_the_next_download() {
    let (engine, sources, fs) = growing_store().await;
    let old_url = "https://lab.test/old/setup.exe";
    let new_url = "https://lab.test/new/setup.exe";
    sources.insert(
        old_url,
        // Still running when removed (16 × 1 MiB/s would finish 16 MiB).
        Arc::new(FaultSource::new(content(64 * MIB, 2)).cfg(|c| c.per_conn_bps = Some(MIB))),
    );
    let old = engine.add(add_params(old_url)).await.unwrap().id;
    tokio::time::sleep(Duration::from_secs(2)).await;
    engine
        .remove(RemoveParams {
            ids: vec![old],
            delete_files: false,
        })
        .await
        .unwrap();
    let data = content(MIB, 3);
    sources.insert(new_url, Arc::new(FaultSource::new(data.clone())));
    let id = engine.add(add_params(new_url)).await.unwrap().id;
    let (d, _) = finish(&engine, &id, LONG).await;
    assert_eq!(d.summary.filename, "setup.exe");
    assert_same(&fs.finished()[&Path::new(DIR).join("setup.exe")], &data);
}

#[tokio::test(start_paused = true)]
async fn removing_but_keeping_files_does_not_poison_the_next_download_mem() {
    let lab = Lab::new(config()).await;
    let old_url = "https://lab.test/old/setup.exe";
    let new_url = "https://lab.test/new/setup.exe";
    lab.serve(
        old_url,
        // Still running when removed (16 × 1 MiB/s would finish 16 MiB).
        Arc::new(FaultSource::new(content(64 * MIB, 2)).cfg(|c| c.per_conn_bps = Some(MIB))),
    );
    let old = lab.add(old_url).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    lab.engine
        .remove(RemoveParams {
            ids: vec![old],
            delete_files: false,
        })
        .await
        .unwrap();
    let data = content(MIB, 3);
    lab.serve(new_url, Arc::new(FaultSource::new(data.clone())));
    let id = lab.add(new_url).await;
    let (d, _) = finish(&lab.engine, &id, LONG).await;
    assert_eq!(d.summary.filename, "setup.exe");
    assert_same(&lab.file("setup.exe").unwrap(), &data);
}

#[tokio::test(start_paused = true)]
async fn an_empty_download_does_not_keep_stale_bytes() {
    let lab = Lab::new(config()).await;
    plant(&lab, "data.bin", content(MIB, 4)).await;
    lab.serve(URL, Arc::new(FaultSource::new(Bytes::new())));
    let id = lab.add(URL).await;
    finish(&lab.engine, &id, LONG).await;
    assert_eq!(lab.file("data.bin").unwrap().len(), 0);
}

/// #2: names are reserved atomically; many downloads whose probes all
/// return the same name, on a multi-thread runtime, never share a part file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_probes_never_share_a_name() {
    let lab = Lab::new(config()).await;
    let mut s = lab.engine.settings().await.unwrap();
    s.max_active_downloads = 64;
    lab.engine.set_settings(s).await.unwrap();
    let mut want = Vec::new();
    for i in 0..24u64 {
        let url = format!("https://lab.test/get?id={i}");
        let data = content(256 * KIB + i * 1000, 100 + i);
        lab.serve(
            &url,
            Arc::new(FaultSource::new(data.clone()).cfg(|c| c.filename = Some("same.zip".into()))),
        );
        want.push((lab.add(&url).await, data));
    }
    let t0 = std::time::Instant::now();
    loop {
        let all = lab.engine.list(ListParams::default()).await.unwrap();
        if all.iter().all(|d| d.state == DownloadState::Completed) {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "not done");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let mut names = std::collections::HashSet::new();
    for (id, data) in want {
        let name = lab.engine.record(&id).unwrap().filename;
        assert!(names.insert(name.clone()), "{name} used twice");
        assert_same(&lab.file(&name).unwrap(), &data);
    }
}

/// #3: refreshing the URL of a running download restarts it on the new URL.
#[tokio::test(start_paused = true)]
async fn refresh_url_while_running_uses_the_new_url() {
    let lab = Lab::new(config()).await;
    let data = content(8 * MIB, 5);
    let old = lab.serve(
        URL,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| c.per_conn_bps = Some(MIB))),
    );
    let id = lab.add(URL).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let fresh_url = "https://lab.test/files/data.bin?sig=2";
    let fresh = lab.serve(
        fresh_url,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| c.per_conn_bps = Some(4 * MIB))),
    );
    lab.engine
        .refresh_url(RefreshUrlParams {
            id: id.clone(),
            url: fresh_url.into(),
            request: None,
        })
        .await
        .unwrap();
    let old_opens = old.opens();
    finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert_eq!(old.opens(), old_opens, "the old URL is not used again");
    assert!(fresh.served() > 0);
    assert_eq!(lab.engine.record(&id).unwrap().url, fresh_url);
}

/// #4: a resume that lands while a stopping run saves its host profile
/// still wins.
#[tokio::test(start_paused = true)]
async fn a_resume_while_a_pause_finishes_is_not_lost() {
    let (lab, slow) = slow_lab(config()).await;
    let data = content(8 * MIB, 6);
    lab.serve(
        URL,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| c.per_conn_bps = Some(MIB))),
    );
    let id = lab.add(URL).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    *lock(&slow.profile_delay) = Duration::from_secs(1);
    let engine = lab.engine.clone();
    let ids = IdsParams {
        ids: vec![id.clone()],
    };
    let pause = tokio::spawn({
        let ids = ids.clone();
        async move { engine.pause(ids).await }
    });
    // The run has read "pause" and is saving its host profile.
    tokio::time::sleep(Duration::from_millis(300)).await;
    lab.engine.resume(ids).await.unwrap();
    pause.await.unwrap().unwrap();
    *lock(&slow.profile_delay) = Duration::ZERO;
    finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
}

/// #5: an absurd `Retry-After` neither panics nor parks the download.
#[tokio::test(start_paused = true)]
async fn huge_retry_after_is_clamped() {
    let lab = Lab::new(config()).await;
    let data = content(MIB, 7);
    let src = Arc::new(FaultSource::new(data.clone()));
    src.fail_next_probe(SourceError::Throttled {
        message: "429".into(),
        retry_after: Some(Duration::MAX),
    });
    lab.serve(URL, src.clone());
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    assert!(took <= Duration::from_secs(31), "took {took:?}");
    // The same on a connection (not the probe).
    let lab = Lab::new(config()).await;
    lab.serve(
        URL,
        Arc::new(FaultSource::new(data.clone()).script(|n, _| ReqPlan {
            fail: (n == 1).then(|| SourceError::Throttled {
                message: "503".into(),
                retry_after: Some(Duration::from_secs(86_400)),
            }),
            ..ReqPlan::default()
        })),
    );
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    assert!(took <= Duration::from_secs(31), "took {took:?}");
}

#[tokio::test(start_paused = true)]
async fn a_panicking_run_fails_and_frees_its_slot() {
    let lab = Lab::new(config()).await;
    let mut s = lab.engine.settings().await.unwrap();
    s.max_active_downloads = 1;
    lab.engine.set_settings(s).await.unwrap();
    let bad = "https://lab.test/boom.bin";
    lab.sources.insert(bad, Arc::new(PanicSource));
    let data = content(MIB, 8);
    lab.serve(URL, Arc::new(FaultSource::new(data.clone())));
    let a = lab.add(bad).await;
    let b = lab.add(URL).await;
    let d = wait_for(&lab.engine, &a, &[DownloadState::Failed], LONG).await;
    assert_eq!(d.summary.error.unwrap().code, "internal");
    finish(&lab.engine, &b, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    // And it can be resumed (it is no longer marked running).
    lab.engine
        .resume(IdsParams {
            ids: vec![a.clone()],
        })
        .await
        .unwrap();
    wait_for(&lab.engine, &a, &[DownloadState::Failed], LONG).await;
}

/// #6: everything captured for one download is forgotten once it completes.
#[tokio::test(start_paused = true)]
async fn post_bodies_and_referers_are_cleared() {
    let lab = Lab::new(config()).await;
    lab.serve(URL, Arc::new(FaultSource::new(content(KIB, 9))));
    let mut p = add_params(URL);
    p.request = Some(RequestContext {
        method: Some("POST".into()),
        body_base64: Some("dG9rZW49c2VjcmV0".into()),
        referer: Some("https://lab.test/account?session=x".into()),
        cookie: Some("a=b".into()),
        headers: vec![Header {
            name: "Accept".into(),
            value: "*/*".into(),
        }],
        user_agent: Some("ua".into()),
    });
    let id = lab.engine.add(p).await.unwrap().id;
    finish(&lab.engine, &id, LONG).await;
    let req = lab.engine.record(&id).unwrap().request.unwrap();
    assert_eq!(req.body_base64, None);
    assert_eq!(req.referer, None);
    assert_eq!(req.cookie, None);
    assert_eq!(req.method.as_deref(), Some("POST"));
    assert_eq!(req.headers.len(), 1);
}

/// #7: a download being removed is never started by the queue.
#[tokio::test(start_paused = true)]
async fn remove_cannot_race_the_queue() {
    let (lab, slow) = slow_lab(config()).await;
    let mut s = lab.engine.settings().await.unwrap();
    s.max_active_downloads = 1;
    lab.engine.set_settings(s).await.unwrap();
    let a_url = "https://lab.test/a.bin";
    let b_url = "https://lab.test/b.bin";
    lab.serve(
        a_url,
        Arc::new(FaultSource::new(content(8 * MIB, 10)).cfg(|c| c.per_conn_bps = Some(MIB))),
    );
    let b_src = lab.serve(b_url, Arc::new(FaultSource::new(content(4 * MIB, 11))));
    let a = lab.add(a_url).await;
    let b = lab.add(b_url).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    // Saves now take a while: A's final save holds the save lock while the
    // queue moves on to B, exactly when `remove(B)` runs.
    *lock(&slow.save_delay) = Duration::from_millis(200);
    let engine = lab.engine.clone();
    let pause_a = tokio::spawn(async move { engine.pause(IdsParams { ids: vec![a] }).await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    lab.engine
        .remove(RemoveParams {
            ids: vec![b.clone()],
            delete_files: true,
        })
        .await
        .unwrap();
    pause_a.await.unwrap().unwrap();
    *lock(&slow.save_delay) = Duration::ZERO;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(b_src.opens(), 0, "B never ran");
    assert!(lab.journal.load(&b).await.unwrap().is_none());
    assert!(
        lab.storage
            .in_progress(&Path::new(DIR).join("b.bin.vayu-part"))
            .is_none()
    );
}

/// #8: a silent response without ranges is dropped and retried.
#[tokio::test(start_paused = true)]
async fn a_silent_stream_is_restarted() {
    let mut cfg = config();
    cfg.tuning = Tuning {
        stream_idle_timeout: Duration::from_secs(5),
        ..Tuning::default()
    };
    let lab = Lab::new(cfg).await;
    let data = content(3 * MIB, 12);
    let src = lab.serve(
        URL,
        Arc::new(
            FaultSource::new(data.clone())
                .cfg(|c| {
                    c.accepts_ranges = false;
                    c.honours_ranges = false;
                })
                .script(|n, _| ReqPlan {
                    stall: (n == 1).then_some((MIB, Duration::from_secs(3600))),
                    ttfb: if n == 2 {
                        Duration::from_secs(3600)
                    } else {
                        Duration::ZERO
                    },
                    ..ReqPlan::default()
                }),
        ),
    );
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert_eq!(src.opens(), 3);
    assert!(took < Duration::from_secs(15), "took {took:?}");
}
