//! `Service` semantics: list/get/errors, names, categories, checksums,
//! queues, speed limit, events, settings, secrets, shutdown and resume.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use common::*;
use futures::{FutureExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio::time::Instant;
use vayu_core::{Journal, StateStore, Storage};
use vayu_engine::{Engine, SETTINGS_KEY};
use vayu_proto::service::{Notification, Service};
use vayu_proto::{
    AddParams, Checksum, DownloadState, Header, IdParams, IdsParams, ListParams, PreconnectParams,
    ProbeParams, RefreshUrlParams, RemoveParams, RequestContext, error_code,
};

const LONG: Duration = Duration::from_secs(600);

fn url(i: usize) -> String {
    format!("https://lab.test/f/file{i}.bin")
}

fn slow(data: Bytes, bps: u64) -> Arc<FaultSource> {
    Arc::new(FaultSource::new(data).cfg(|c| c.per_conn_bps = Some(bps)))
}

#[tokio::test(start_paused = true)]
async fn list_is_newest_first_with_filters_and_paging() {
    let lab = Lab::new(config()).await;
    let mut ids = Vec::new();
    for i in 0..5 {
        lab.serve(&url(i), Arc::new(FaultSource::new(content(KIB, i as u64))));
        let mut p = add_params(&url(i));
        p.start = false;
        if i == 2 {
            p.category = Some("work".into());
        }
        ids.push(lab.engine.add(p).await.unwrap().id);
    }
    let all = lab.engine.list(ListParams::default()).await.unwrap();
    let got: Vec<_> = all.iter().map(|s| s.id.clone()).collect();
    let mut want = ids.clone();
    want.reverse();
    assert_eq!(got, want, "newest first");
    assert!(all.iter().all(|s| s.state == DownloadState::Paused));

    let page = lab
        .engine
        .list(ListParams {
            offset: Some(1),
            limit: Some(2),
            ..ListParams::default()
        })
        .await
        .unwrap();
    assert_eq!(
        page.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        want[1..3].to_vec()
    );
    let work = lab
        .engine
        .list(ListParams {
            category: Some("work".into()),
            ..ListParams::default()
        })
        .await
        .unwrap();
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].id, ids[2]);
    let none = lab
        .engine
        .list(ListParams {
            states: vec![DownloadState::Completed],
            ..ListParams::default()
        })
        .await
        .unwrap();
    assert!(none.is_empty());

    // Start one and check `get` carries segments while it runs.
    lab.serve(&url(9), slow(content(8 * MIB, 9), MIB));
    let id = lab.add(&url(9)).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let d = detail(&lab.engine, &id).await;
    assert_eq!(d.summary.state, DownloadState::Downloading);
    assert!(!d.segments.is_empty());
    assert!(d.summary.connections >= 1);
    assert!(d.summary.speed_bps > 0);
}

#[tokio::test(start_paused = true)]
async fn errors_use_the_protocol_codes() {
    let lab = Lab::new(config()).await;
    let nf = |r: Result<_, vayu_proto::service::ServiceError>| r.unwrap_err().code;
    assert_eq!(
        nf(lab
            .engine
            .get(IdParams { id: "nope".into() })
            .await
            .map(|_| ())),
        error_code::NOT_FOUND
    );
    assert_eq!(
        nf(lab
            .engine
            .pause(IdsParams {
                ids: vec!["nope".into()]
            })
            .await),
        error_code::NOT_FOUND
    );
    assert_eq!(
        nf(lab
            .engine
            .remove(RemoveParams {
                ids: vec!["nope".into()],
                delete_files: false
            })
            .await),
        error_code::NOT_FOUND
    );
    const BAD: i32 = vayu_proto::service::ServiceError::INVALID_PARAMS;
    lab.serve(URL, Arc::new(FaultSource::new(content(MIB, 1))));
    let bad = |f: fn(&mut AddParams)| {
        let mut p = add_params(URL);
        f(&mut p);
        p
    };
    for p in [
        bad(|p| p.directory = Some("relative/dir".into())),
        bad(|p| p.filename = Some("..".into())),
        bad(|p| {
            p.checksum = Some(Checksum {
                algorithm: "crc32".into(),
                value: "00".into(),
            })
        }),
        bad(|p| {
            p.checksum = Some(Checksum {
                algorithm: "sha256".into(),
                value: "abcd".into(),
            })
        }),
        bad(|p| p.url = "https://unknown.test/x".into()),
        bad(|p| p.url = "  ".into()),
    ] {
        assert_eq!(nf(lab.engine.add(p).await.map(|_| ())), BAD);
    }

    let id = lab.add(URL).await;
    finish(&lab.engine, &id, LONG).await;
    let ids = IdsParams {
        ids: vec![id.clone()],
    };
    assert_eq!(
        nf(lab.engine.pause(ids.clone()).await),
        error_code::INVALID_STATE
    );
    assert_eq!(nf(lab.engine.resume(ids).await), error_code::INVALID_STATE);
    assert_eq!(
        nf(lab
            .engine
            .refresh_url(RefreshUrlParams {
                id,
                url: URL.into(),
                request: None
            })
            .await),
        error_code::INVALID_STATE
    );
    let mut s = lab.engine.settings().await.unwrap();
    s.max_active_downloads = 0;
    assert_eq!(nf(lab.engine.set_settings(s).await.map(|_| ())), BAD);
}

#[tokio::test(start_paused = true)]
async fn file_names_follow_the_priority_and_never_collide() {
    let lab = Lab::new(config()).await;
    // Content-Disposition wins over the URL; traversal is defused.
    let cd = "https://lab.test/dl?id=7";
    lab.serve(
        cd,
        Arc::new(
            FaultSource::new(content(KIB, 1))
                .cfg(|c| c.filename = Some("../../Report final.pdf".into())),
        ),
    );
    let id = lab.add(cd).await;
    let (d, _) = finish(&lab.engine, &id, LONG).await;
    assert_eq!(d.summary.filename, "Report final.pdf");
    assert_eq!(d.summary.category.as_deref(), Some("documents"));
    assert!(lab.file("Report final.pdf").is_some());

    // The user's name wins over everything.
    let mut p = add_params(cd);
    p.filename = Some("mine.iso".into());
    let id = lab.engine.add(p).await.unwrap().id;
    let (d, _) = finish(&lab.engine, &id, LONG).await;
    assert_eq!(d.summary.filename, "mine.iso");
    assert_eq!(d.summary.category.as_deref(), Some("programs"));

    // URL path, percent-decoded (Bangla included).
    let bn = "https://lab.test/files/%E0%A6%AC%E0%A6%BE%E0%A6%82%E0%A6%B2%E0%A6%BE%20file.zip?x=1";
    lab.serve(bn, Arc::new(FaultSource::new(content(KIB, 2))));
    let id = lab.add(bn).await;
    let (d, _) = finish(&lab.engine, &id, LONG).await;
    assert_eq!(d.summary.filename, "বাংলা file.zip");

    // Two at once with the same name get different part files; a
    // finished file of that name already exists, so numbering continues.
    let data = content(2 * MIB, 3);
    lab.serve(URL, slow(data.clone(), 4 * MIB));
    let a = lab.add(URL).await;
    let b = lab.add(URL).await;
    let (da, _) = finish(&lab.engine, &a, LONG).await;
    let (db, _) = finish(&lab.engine, &b, LONG).await;
    let mut names = vec![da.summary.filename, db.summary.filename];
    names.sort();
    assert_eq!(names, vec!["data (1).bin", "data.bin"]);
    let c = lab.add(URL).await;
    let (dc, _) = finish(&lab.engine, &c, LONG).await;
    assert_eq!(dc.summary.filename, "data (2).bin");
    for n in ["data.bin", "data (1).bin", "data (2).bin"] {
        assert_same(&lab.file(n).unwrap(), &data);
    }
}

#[tokio::test(start_paused = true)]
async fn clients_hear_the_servers_name_when_it_arrives() {
    // Added under the URL's name (`dl`), the server's name comes with the
    // first response: clients get the whole summary again as `updated`.
    let lab = Lab::new(config()).await;
    let u = "https://lab.test/download.php?file=x";
    lab.serve(
        u,
        Arc::new(
            FaultSource::new(content(KIB, 1)).cfg(|c| c.filename = Some("Episode 5.mp4".into())),
        ),
    );
    let mut events = lab.engine.subscribe();
    let id = lab.add(u).await;
    finish(&lab.engine, &id, LONG).await;
    let (mut added, mut updated) = (None, Vec::new());
    while let Some(Some(n)) = events.next().now_or_never() {
        match n {
            Notification::DownloadAdded(s) if s.id == id => added = Some(s.filename),
            Notification::DownloadUpdated(s) if s.id == id => updated.push(s),
            _ => {}
        }
    }
    assert_eq!(added.as_deref(), Some("download.php"));
    let last = updated.last().expect("an updated event");
    assert_eq!(last.filename, "Episode 5.mp4");
    assert_eq!(last.category.as_deref(), Some("video"));
    assert_eq!(last.total_bytes, Some(KIB));
}

#[tokio::test(start_paused = true)]
async fn categories_come_from_the_extension_unless_given() {
    let lab = Lab::new(config()).await;
    let u = "https://lab.test/v/movie.mkv";
    lab.serve(u, Arc::new(FaultSource::new(content(KIB, 1))));
    let id = lab.add(u).await;
    assert_eq!(
        detail(&lab.engine, &id).await.summary.category.as_deref(),
        Some("video")
    );
    let mut p = add_params(u);
    p.category = Some("Series".into());
    let id = lab.engine.add(p).await.unwrap().id;
    assert_eq!(
        detail(&lab.engine, &id).await.summary.category.as_deref(),
        Some("Series")
    );
}

#[tokio::test(start_paused = true)]
async fn checksums_are_verified() {
    let lab = Lab::new(config()).await;
    let data = content(3 * MIB, 4);
    lab.serve(URL, Arc::new(FaultSource::new(data.clone())));
    let good = hex::encode(Sha256::digest(&data));
    let mut p = add_params(URL);
    p.checksum = Some(Checksum {
        algorithm: "sha256".into(),
        value: good.to_uppercase(),
    });
    let mut events = lab.engine.subscribe();
    let id = lab.engine.add(p).await.unwrap().id;
    finish(&lab.engine, &id, LONG).await;
    let mut saw_verifying = false;
    while let Some(Some(n)) = events.next().now_or_never() {
        if let Notification::DownloadState(s) = n
            && s.id == id
            && s.state == DownloadState::Verifying
        {
            saw_verifying = true;
        }
    }
    assert!(saw_verifying);

    let mut p = add_params(URL);
    p.filename = Some("bad.bin".into());
    p.checksum = Some(Checksum {
        algorithm: "md5".into(),
        value: "00".repeat(16),
    });
    let id = lab.engine.add(p).await.unwrap().id;
    let d = wait_for(&lab.engine, &id, &[DownloadState::Failed], LONG).await;
    assert_eq!(d.summary.error.unwrap().code, "checksum_mismatch");
    assert!(lab.file("bad.bin").is_none());
    let cp = lab.journal.load(&id).await.unwrap().unwrap();
    assert!(cp.done.is_empty(), "a retry starts clean");
}

#[tokio::test(start_paused = true)]
async fn the_main_queue_runs_max_active_in_order() {
    let lab = Lab::new(config()).await;
    let mut s = lab.engine.settings().await.unwrap();
    s.max_active_downloads = 1;
    lab.engine.set_settings(s).await.unwrap();
    let mut ids = Vec::new();
    for i in 0..3 {
        lab.serve(&url(i), slow(content(2 * MIB, i as u64), 4 * MIB));
        ids.push(lab.add(&url(i)).await);
    }
    let t0 = Instant::now();
    let mut max_active = 0;
    loop {
        let st = lab.engine.stats().await.unwrap();
        max_active = max_active.max(st.active);
        let all = lab.engine.list(ListParams::default()).await.unwrap();
        if all.iter().all(|d| d.state == DownloadState::Completed) {
            break;
        }
        assert!(t0.elapsed() < LONG);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(max_active, 1);
    let done: Vec<i64> = ids
        .iter()
        .map(|id| lab.engine.record(id).unwrap().completed_at.unwrap())
        .collect();
    assert!(done.windows(2).all(|w| w[0] <= w[1]), "FIFO: {done:?}");
}

#[tokio::test(start_paused = true)]
async fn named_queues_run_one_at_a_time() {
    let lab = Lab::new(config()).await;
    let mut ids = Vec::new();
    for i in 0..3 {
        lab.serve(&url(i), slow(content(2 * MIB, i as u64), 2 * MIB));
        let mut p = add_params(&url(i));
        p.start = false;
        let id = lab.engine.add(p).await.unwrap().id;
        lab.engine
            .set_queue(&id, Some("night".into()))
            .await
            .unwrap();
        ids.push(id);
    }
    lab.engine
        .resume(IdsParams { ids: ids.clone() })
        .await
        .unwrap();
    let t0 = Instant::now();
    loop {
        let running = lab
            .engine
            .list(ListParams::default())
            .await
            .unwrap()
            .iter()
            .filter(|d| matches!(d.state, DownloadState::Probing | DownloadState::Downloading))
            .count();
        assert!(running <= 1, "{running} running in one named queue");
        if ids
            .iter()
            .all(|id| lab.engine.record(id).unwrap().state == DownloadState::Completed)
        {
            break;
        }
        assert!(t0.elapsed() < LONG);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(start_paused = true)]
async fn the_global_speed_limit_holds() {
    let lab = Lab::new(config()).await;
    let mut s = lab.engine.settings().await.unwrap();
    s.speed_limit_bps = Some(MIB);
    lab.engine.set_settings(s).await.unwrap();
    let data = content(4 * MIB, 5);
    lab.serve(URL, Arc::new(FaultSource::new(data.clone())));
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    let secs = took.as_secs_f64();
    assert!((3.6..=4.6).contains(&secs), "took {secs}");
}

#[tokio::test(start_paused = true)]
async fn events_are_batched_and_every_subscriber_gets_all() {
    let lab = Lab::new(config()).await;
    let data = content(16 * MIB, 6);
    lab.serve(URL, slow(data, MIB));
    let collect = |mut s: futures::stream::BoxStream<'static, Notification>| {
        tokio::spawn(async move {
            let mut out = Vec::new();
            while let Some(n) = s.next().await {
                let done = matches!(&n, Notification::DownloadState(e) if e.state == DownloadState::Completed);
                out.push((Instant::now(), n));
                if done {
                    break;
                }
            }
            out
        })
    };
    let a = collect(lab.engine.subscribe());
    let b = collect(lab.engine.subscribe());
    let id = lab.add(URL).await;
    let a = a.await.unwrap();
    let b = b.await.unwrap();

    let states = |v: &[(Instant, Notification)]| -> Vec<DownloadState> {
        v.iter()
            .filter_map(|(_, n)| match n {
                Notification::DownloadState(s) if s.id == id => Some(s.state),
                _ => None,
            })
            .collect()
    };
    assert!(matches!(a[0].1, Notification::DownloadAdded(ref s) if s.id == id));
    assert_eq!(
        states(&a),
        vec![
            DownloadState::Probing,
            DownloadState::Downloading,
            DownloadState::Completed
        ]
    );
    assert_eq!(states(&a), states(&b));

    let progress: Vec<(Instant, &vayu_proto::ProgressEvent)> = a
        .iter()
        .filter_map(|(t, n)| match n {
            Notification::DownloadProgress(p) => Some((*t, p)),
            _ => None,
        })
        .collect();
    assert!(progress.len() >= 4, "{}", progress.len());
    // ≤ 4 per second: consecutive batches at least ~250 ms apart (the
    // final one, sent when the download ends, may come sooner).
    for w in progress[..progress.len() - 1].windows(2) {
        assert!(w[1].0 - w[0].0 >= Duration::from_millis(240));
    }
    // Segments at most once per second per download.
    let with_segments: Vec<Instant> = progress
        .iter()
        .filter(|(_, p)| p.items.iter().any(|i| i.segments.is_some()))
        .map(|(t, _)| *t)
        .collect();
    assert!(!with_segments.is_empty());
    for w in with_segments.windows(2) {
        assert!(w[1] - w[0] >= Duration::from_millis(990));
    }
    let stats = a
        .iter()
        .filter(|(_, n)| matches!(n, Notification::Stats(_)))
        .count();
    assert!(stats >= 1);
}

#[tokio::test(start_paused = true)]
async fn settings_persist_and_are_broadcast() {
    let lab = Lab::new(config()).await;
    let mut events = lab.engine.subscribe();
    let mut s = lab.engine.settings().await.unwrap();
    assert_eq!(s.default_directory, DIR);
    s.max_connections_per_download = 8;
    s.speed_limit_bps = Some(0);
    s.theme = "dark".into();
    let stored = lab.engine.set_settings(s.clone()).await.unwrap();
    assert_eq!(stored.speed_limit_bps, None, "0 means unlimited");
    match events.next().await.unwrap() {
        Notification::Settings(e) => assert_eq!(e, stored),
        other => panic!("{other:?}"),
    }
    let json = lab.state.get_value(SETTINGS_KEY).await.unwrap().unwrap();
    assert!(json.contains("\"theme\":\"dark\""), "{json}");
    let again = Engine::start(lab.deps(), config()).await.unwrap();
    assert_eq!(again.settings().await.unwrap(), stored);
}

#[tokio::test(start_paused = true)]
async fn secrets_are_cleared_once_complete() {
    let lab = Lab::new(config()).await;
    lab.serve(URL, Arc::new(FaultSource::new(content(MIB, 7))));
    let mut p = add_params(URL);
    p.request = Some(RequestContext {
        cookie: Some("session=s3cret".into()),
        headers: vec![
            Header {
                name: "Authorization".into(),
                value: "Bearer x".into(),
            },
            Header {
                name: "Accept".into(),
                value: "*/*".into(),
            },
        ],
        referer: Some("https://lab.test/".into()),
        ..RequestContext::default()
    });
    let id = lab.engine.add(p).await.unwrap().id;
    let running = lab.engine.record(&id).unwrap();
    assert!(
        running.request.unwrap().cookie.is_some(),
        "kept while needed"
    );
    finish(&lab.engine, &id, LONG).await;
    let stored = lab
        .state
        .load_downloads()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.id == id)
        .unwrap();
    let req = stored.request.unwrap();
    assert_eq!(req.cookie, None);
    assert_eq!(req.headers.len(), 1);
    assert_eq!(req.headers[0].name, "Accept");
    assert_eq!(req.referer, None, "the referer is cleared too");
}

#[tokio::test(start_paused = true)]
async fn shutdown_commits_and_a_new_engine_resumes() {
    let lab = Lab::new(config()).await;
    let size = 24 * MIB;
    let data = content(size, 8);
    let src = lab.serve(URL, slow(data.clone(), MIB));
    let id = lab.add(URL).await;
    let mut events = lab.engine.subscribe();
    tokio::time::sleep(Duration::from_millis(900)).await;
    lab.engine.shutdown().await;
    let cp = lab.journal.load(&id).await.unwrap().unwrap();
    assert!(cp.done.len() >= MIB, "{}", cp.done.len());
    let rec = lab
        .state
        .load_downloads()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.id == id)
        .unwrap();
    assert_eq!(
        rec.state,
        DownloadState::Queued,
        "resumes at the next start"
    );
    assert_eq!(rec.downloaded_bytes, cp.done.len());
    // The subscription ends.
    let rest: Vec<_> = events.by_ref().collect().await;
    assert!(rest.len() < 100);
    let mut p = add_params(URL);
    p.filename = Some("late.bin".into());
    assert_eq!(
        lab.engine.add(p).await.unwrap_err().code,
        error_code::INVALID_STATE
    );
    let served = src.served();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(src.served(), served, "nothing runs after shutdown");

    let again = Engine::start(lab.deps(), config()).await.unwrap();
    finish(&again, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert!(src.served() - served <= size - cp.done.len() + 2 * MIB);
}

#[tokio::test(start_paused = true)]
async fn remove_stops_and_deletes_files() {
    let lab = Lab::new(config()).await;
    lab.serve(URL, slow(content(16 * MIB, 9), MIB));
    let id = lab.add(URL).await;
    let mut events = lab.engine.subscribe();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let part = Path::new(DIR).join("data.bin.vayu-part");
    assert!(lab.storage.in_progress(&part).is_some());
    lab.engine
        .remove(RemoveParams {
            ids: vec![id.clone()],
            delete_files: true,
        })
        .await
        .unwrap();
    assert!(lab.storage.in_progress(&part).is_none());
    assert!(lab.journal.load(&id).await.unwrap().is_none());
    assert!(lab.state.load_downloads().await.unwrap().is_empty());
    assert_eq!(
        lab.engine
            .get(IdParams { id: id.clone() })
            .await
            .unwrap_err()
            .code,
        error_code::NOT_FOUND
    );
    loop {
        match events.next().await.unwrap() {
            Notification::DownloadRemoved(r) => {
                assert_eq!(r.ids, vec![id.clone()]);
                break;
            }
            Notification::DownloadState(s) => assert_ne!(s.state, DownloadState::Paused),
            _ => {}
        }
    }
    // A finished file is deleted too.
    let u = "https://lab.test/small.bin";
    lab.serve(u, Arc::new(FaultSource::new(content(KIB, 1))));
    let id = lab.add(u).await;
    finish(&lab.engine, &id, LONG).await;
    assert!(lab.file("small.bin").is_some());
    lab.engine
        .remove(RemoveParams {
            ids: vec![id],
            delete_files: true,
        })
        .await
        .unwrap();
    assert!(lab.file("small.bin").is_none());
}

async fn corrupt_tail_then_resume(validators: bool) -> (Bytes, Bytes, u64) {
    let lab = Lab::new(config()).await;
    let size = 16 * MIB;
    let data = content(size, 10);
    let src = lab.serve(
        URL,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| {
            c.per_conn_bps = Some(MIB);
            c.validators = validators;
        })),
    );
    let id = lab.add(URL).await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    lab.engine
        .pause(IdsParams {
            ids: vec![id.clone()],
        })
        .await
        .unwrap();
    let cp = lab.journal.load(&id).await.unwrap().unwrap();
    let r = cp.done.ranges()[0];
    // Damage the last 100 KiB of the first journalled range, as a lying
    // disk cache would after a power cut.
    let part = lab
        .storage
        .in_progress(&Path::new(DIR).join("data.bin.vayu-part"))
        .unwrap();
    let from = r.end.saturating_sub(100 * KIB).max(r.start);
    part.write_at(from, Bytes::from(vec![0u8; (r.end - from) as usize]))
        .await
        .unwrap();
    let before = src.served();
    lab.engine
        .resume(IdsParams {
            ids: vec![id.clone()],
        })
        .await
        .unwrap();
    finish(&lab.engine, &id, LONG).await;
    (lab.file("data.bin").unwrap(), data, src.served() - before)
}

#[tokio::test(start_paused = true)]
async fn tail_verification_repairs_damaged_ranges() {
    let (got, want, resent) = corrupt_tail_then_resume(true).await;
    assert_same(&got, &want);
    assert!(
        resent < want.len() as u64,
        "resumed, not restarted: {resent}"
    );
}

#[tokio::test(start_paused = true)]
async fn without_validators_a_tail_mismatch_restarts() {
    let (got, want, resent) = corrupt_tail_then_resume(false).await;
    assert_same(&got, &want);
    assert!(resent >= want.len() as u64, "restarted: {resent}");
}

#[tokio::test(start_paused = true)]
async fn probe_and_preconnect() {
    let lab = Lab::new(config()).await;
    lab.serve(
        URL,
        Arc::new(FaultSource::new(content(3 * MIB, 11)).cfg(|c| {
            c.content_type = Some("application/zip".into());
        })),
    );
    let p = lab
        .engine
        .probe(ProbeParams {
            url: URL.into(),
            request: None,
        })
        .await
        .unwrap();
    assert_eq!(p.total_bytes, Some(3 * MIB));
    assert!(p.resumable);
    assert_eq!(p.filename, "data.bin");
    assert_eq!(p.final_url, URL);
    lab.engine
        .preconnect(PreconnectParams { url: URL.into() })
        .await
        .unwrap();
    tokio::task::yield_now().await;
    assert_eq!(lab.sources.preconnects(), vec![URL.to_string()]);
    assert!(lab.engine.capabilities().contains(&"http".to_string()));
}

#[tokio::test(start_paused = true)]
async fn add_without_start_waits_for_resume() {
    let lab = Lab::new(config()).await;
    let src = lab.serve(URL, Arc::new(FaultSource::new(content(MIB, 12))));
    let mut p = add_params(URL);
    p.start = false;
    let id = lab.engine.add(p).await.unwrap().id;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(src.opens(), 0);
    assert_eq!(
        detail(&lab.engine, &id).await.summary.state,
        DownloadState::Paused
    );
    lab.engine
        .resume(IdsParams {
            ids: vec![id.clone()],
        })
        .await
        .unwrap();
    finish(&lab.engine, &id, LONG).await;
}

#[tokio::test(start_paused = true)]
async fn a_resume_during_a_pause_wins() {
    let lab = Lab::new(config()).await;
    let data = content(8 * MIB, 13);
    lab.serve(URL, slow(data.clone(), MIB));
    let id = lab.add(URL).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let ids = IdsParams {
        ids: vec![id.clone()],
    };
    let (p, r) = tokio::join!(lab.engine.pause(ids.clone()), lab.engine.resume(ids));
    p.unwrap();
    r.unwrap();
    finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
}

async fn lose_part_file_then_resume(validators: bool) {
    let lab = Lab::new(config()).await;
    // Still running at the pause.
    let size = 32 * MIB;
    let data = content(size, 14);
    lab.serve(
        URL,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| {
            c.per_conn_bps = Some(MIB);
            c.validators = validators;
        })),
    );
    let id = lab.add(URL).await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    lab.engine
        .pause(IdsParams {
            ids: vec![id.clone()],
        })
        .await
        .unwrap();
    assert!(
        !lab.journal
            .load(&id)
            .await
            .unwrap()
            .unwrap()
            .done
            .is_empty()
    );
    // The part file disappears (deleted, or renamed by a finish whose
    // journal update was lost).
    vayu_core::StorageProvider::remove(&*lab.storage, &Path::new(DIR).join("data.bin.vayu-part"))
        .await
        .unwrap();
    lab.engine
        .resume(IdsParams {
            ids: vec![id.clone()],
        })
        .await
        .unwrap();
    finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
}

#[tokio::test(start_paused = true)]
async fn a_missing_part_file_is_downloaded_again() {
    lose_part_file_then_resume(true).await;
    lose_part_file_then_resume(false).await;
}
