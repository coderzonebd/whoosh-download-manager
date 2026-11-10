//! Fault scenarios, mirroring labserver's, on the paused clock.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use vayu_core::{Journal, SourceError, StateStore};
use vayu_engine::Tuning;
use vayu_proto::service::Service;
use vayu_proto::{DownloadState, IdsParams, RefreshUrlParams};

const LONG: Duration = Duration::from_secs(600);

fn host() -> &'static str {
    "lab.test:443"
}

#[tokio::test(start_paused = true)]
async fn clean_download_is_exact_and_cleans_up() {
    let lab = Lab::new(config()).await;
    let data = content(8 * MIB + 123, 1);
    let src = lab.serve(
        URL,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| c.per_conn_bps = Some(64 * MIB))),
    );
    let id = lab.add(URL).await;
    let (d, _) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert_eq!(d.summary.downloaded_bytes, data.len() as u64);
    assert_eq!(d.summary.total_bytes, Some(data.len() as u64));
    assert!(d.summary.completed_at.is_some());
    assert!(
        lab.journal.load(&id).await.unwrap().is_none(),
        "journal removed"
    );
    assert!(src.peak() >= 2, "used several connections: {}", src.peak());
    let rec = lab.engine.record(&id).unwrap();
    assert_eq!(rec.state, DownloadState::Completed);
    assert!(rec.resumable);
}

#[tokio::test(start_paused = true)]
async fn per_connection_throttle_climbs_to_many_connections() {
    let lab = Lab::new(config()).await;
    let size = 48 * MIB;
    let data = content(size, 2);
    let src = lab.serve(
        URL,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| c.per_conn_bps = Some(2 * MIB))),
    );
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    // One connection would take 24 s; 16 take 1.5 s.
    assert!(src.peak() >= 12, "peak {}", src.peak());
    assert!(took < Duration::from_secs(4), "took {took:?}");
    let p = lab.state.host_profile(host()).await.unwrap().unwrap();
    assert!(p.best_connections.unwrap() >= 12, "{p:?}");
}

#[tokio::test(start_paused = true)]
async fn global_throttle_settles_on_few_connections() {
    let lab = Lab::new(config()).await;
    let size = 24 * MIB;
    let data = content(size, 3);
    lab.serve(
        URL,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| c.global_bps = Some(8 * MIB))),
    );
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert!(took < Duration::from_millis(3300), "took {took:?}");
    let p = lab.state.host_profile(host()).await.unwrap().unwrap();
    assert!(p.best_connections.unwrap() <= 2, "{p:?}");
}

#[tokio::test(start_paused = true)]
async fn connection_cap_with_retry_after_is_respected() {
    let lab = Lab::new(config()).await;
    let size = 16 * MIB;
    let data = content(size, 4);
    let src = lab.serve(
        URL,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| {
            c.cap = Some((2, Cap::Throttle(Some(Duration::from_secs(1)))));
            c.per_conn_bps = Some(4 * MIB);
        })),
    );
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    // Two connections at 4 MiB/s: 2 s is the floor.
    assert!(took < Duration::from_millis(2600), "took {took:?}");
    assert!(src.opens() < 16, "opens {}", src.opens());
    let p = lab.state.host_profile(host()).await.unwrap().unwrap();
    assert_eq!(p.max_ok_connections, Some(2), "{p:?}");
}

#[tokio::test(start_paused = true)]
async fn connection_cap_with_resets_is_learned() {
    let lab = Lab::new(config()).await;
    let size = 16 * MIB;
    let data = content(size, 5);
    let src = lab.serve(
        URL,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| {
            c.cap = Some((2, Cap::Reset));
            c.per_conn_bps = Some(4 * MIB);
        })),
    );
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert!(took < Duration::from_millis(2800), "took {took:?}");
    assert!(src.opens() < 24, "opens {}", src.opens());
}

#[tokio::test(start_paused = true)]
async fn random_resets_are_retried() {
    let lab = Lab::new(config()).await;
    let size = 32 * MIB;
    let data = content(size, 6);
    let src = lab.serve(
        URL,
        Arc::new(
            FaultSource::new(data.clone())
                .cfg(|c| c.per_conn_bps = Some(16 * MIB))
                .script(|n, _| ReqPlan {
                    reset_after: (n % 3 == 0).then_some(700 * KIB),
                    ..ReqPlan::default()
                }),
        ),
    );
    let id = lab.add(URL).await;
    finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert!(src.opens() > 4);
}

#[tokio::test(start_paused = true)]
async fn a_stalled_connection_is_replaced_quickly() {
    let lab = Lab::new(config()).await;
    let size = 64 * MIB;
    let data = content(size, 7);
    let src = lab.serve(
        URL,
        Arc::new(
            FaultSource::new(data.clone())
                .cfg(|c| c.per_conn_bps = Some(100 * MIB))
                .script(|n, _| ReqPlan {
                    stall: (n == 2).then_some((256 * KIB, Duration::from_secs(8))),
                    ..ReqPlan::default()
                }),
        ),
    );
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    // Four connections at 100 MiB/s need 0.16 s; the stall is 8 s.
    assert!(took < Duration::from_millis(600), "took {took:?}");
    assert!(src.opens() >= 5);
}

#[tokio::test(start_paused = true)]
async fn a_single_stalled_connection_is_hedged() {
    let mut cfg = config();
    cfg.tuning = Tuning {
        fixed_connections: Some(1),
        ..Tuning::default()
    };
    let lab = Lab::new(cfg).await;
    let data = content(4 * MIB, 8);
    lab.serve(
        URL,
        Arc::new(
            FaultSource::new(data.clone())
                .cfg(|c| c.per_conn_bps = Some(8 * MIB))
                .script(|n, _| ReqPlan {
                    stall: (n == 1).then_some((MIB, Duration::from_secs(30))),
                    ..ReqPlan::default()
                }),
        ),
    );
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert!(took < Duration::from_secs(2), "took {took:?}");
}

#[tokio::test(start_paused = true)]
async fn lying_ranges_fall_back_to_one_connection() {
    let lab = Lab::new(config()).await;
    let data = content(12 * MIB, 9);
    let src = lab.serve(
        URL,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| c.honours_ranges = false)),
    );
    let id = lab.add(URL).await;
    let (d, _) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    let ranges = lock(&src.st).ranges.clone();
    assert_eq!(*ranges.last().unwrap(), None, "finished on a plain GET");
    assert_eq!(d.summary.total_bytes, Some(12 * MIB));
}

#[tokio::test(start_paused = true)]
async fn no_ranges_uses_one_connection_from_the_start() {
    let lab = Lab::new(config()).await;
    let data = content(6 * MIB, 10);
    let src = lab.serve(
        URL,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| {
            c.accepts_ranges = false;
            c.honours_ranges = false;
        })),
    );
    let id = lab.add(URL).await;
    let (d, _) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert_eq!(src.peak(), 1);
    assert_eq!(src.opens(), 1);
    assert!(!lab.engine.record(&id).unwrap().resumable);
    assert_eq!(d.summary.total_bytes, Some(6 * MIB));
}

#[tokio::test(start_paused = true)]
async fn unknown_size_streams_and_learns_the_size() {
    let lab = Lab::new(config()).await;
    let data = content(5 * MIB + 17, 11);
    lab.serve(
        URL,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| c.size_known = false)),
    );
    let id = lab.add(URL).await;
    let (d, _) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert_eq!(d.summary.total_bytes, Some(data.len() as u64));
}

#[tokio::test(start_paused = true)]
async fn unknown_size_stream_restarts_after_a_reset() {
    let lab = Lab::new(config()).await;
    let data = content(3 * MIB, 12);
    let src = lab.serve(
        URL,
        Arc::new(
            FaultSource::new(data.clone())
                .cfg(|c| c.size_known = false)
                .script(|n, _| ReqPlan {
                    reset_after: (n == 1).then_some(MIB),
                    ..ReqPlan::default()
                }),
        ),
    );
    let id = lab.add(URL).await;
    finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert_eq!(src.opens(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_file_that_changes_once_restarts_and_never_mixes() {
    let lab = Lab::new(config()).await;
    let v1 = content(16 * MIB, 13);
    let v2 = content(16 * MIB, 14);
    let src = lab.serve(
        URL,
        Arc::new(
            FaultSource::with_versions(vec![v1, v2.clone()])
                .cfg(|c| {
                    c.change_after = Some(5 * MIB);
                    c.per_conn_bps = Some(8 * MIB);
                })
                // Responses end early, so new requests see the change.
                .script(|_, _| ReqPlan {
                    reset_after: Some(3 * MIB),
                    ..ReqPlan::default()
                }),
        ),
    );
    let id = lab.add(URL).await;
    finish(&lab.engine, &id, LONG).await;
    assert_eq!(src.version(), 1);
    assert_same(&lab.file("data.bin").unwrap(), &v2);
}

#[tokio::test(start_paused = true)]
async fn responses_in_flight_finish_their_version() {
    let lab = Lab::new(config()).await;
    let v1 = content(16 * MIB, 25);
    let v2 = content(16 * MIB, 26);
    lab.serve(
        URL,
        Arc::new(
            FaultSource::with_versions(vec![v1.clone(), v2.clone()]).cfg(|c| {
                c.change_after = Some(5 * MIB);
                c.per_conn_bps = Some(8 * MIB);
            }),
        ),
    );
    let id = lab.add(URL).await;
    finish(&lab.engine, &id, LONG).await;
    let got = lab.file("data.bin").unwrap();
    assert!(got == v1 || got == v2, "the file is exactly one version");
}

#[tokio::test(start_paused = true)]
async fn a_file_that_keeps_changing_fails_with_remote_changed() {
    let lab = Lab::new(config()).await;
    let vs: Vec<_> = (0..4).map(|i| content(16 * MIB, 20 + i)).collect();
    lab.serve(
        URL,
        Arc::new(
            FaultSource::with_versions(vs)
                .cfg(|c| {
                    c.change_after = Some(5 * MIB);
                    c.per_conn_bps = Some(8 * MIB);
                })
                .script(|_, _| ReqPlan {
                    reset_after: Some(3 * MIB),
                    ..ReqPlan::default()
                }),
        ),
    );
    let id = lab.add(URL).await;
    let d = wait_for(
        &lab.engine,
        &id,
        &[DownloadState::Failed, DownloadState::Completed],
        LONG,
    )
    .await;
    assert_eq!(d.summary.state, DownloadState::Failed);
    assert_eq!(d.summary.error.unwrap().code, "remote_changed");
    assert!(lab.file("data.bin").is_none(), "no file of mixed versions");
}

#[tokio::test(start_paused = true)]
async fn slow_first_byte_does_not_cause_a_hedging_storm() {
    let lab = Lab::new(config()).await;
    let data = content(16 * MIB, 15);
    let src = lab.serve(
        URL,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| {
            c.ttfb = Duration::from_millis(500);
            c.per_conn_bps = Some(4 * MIB);
        })),
    );
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert!(src.opens() <= 24, "opens {}", src.opens());
    // Four connections alone: 0.5 s + 1 s.
    assert!(took < Duration::from_millis(2600), "took {took:?}");
}

#[tokio::test(start_paused = true)]
async fn an_expired_link_waits_for_a_refresh_and_resumes() {
    let lab = Lab::new(config()).await;
    let size = 16 * MIB;
    let data = content(size, 16);
    lab.serve(
        URL,
        Arc::new(
            FaultSource::new(data.clone())
                .cfg(|c| {
                    c.expire_after = Some(6 * MIB);
                    c.per_conn_bps = Some(4 * MIB);
                })
                .script(|_, _| ReqPlan {
                    reset_after: Some(2 * MIB),
                    ..ReqPlan::default()
                }),
        ),
    );
    let id = lab.add(URL).await;
    let d = wait_for(&lab.engine, &id, &[DownloadState::NeedsRefresh], LONG).await;
    assert_eq!(d.summary.error.unwrap().code, "link_expired");
    let cp = lab.journal.load(&id).await.unwrap().unwrap();
    assert!(cp.done.len() >= 4 * MIB, "progress kept: {}", cp.done.len());

    let fresh_url = "https://lab.test/files/data.bin?sig=new";
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
    finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert!(
        fresh.served() <= size - cp.done.len() + MIB,
        "continued from the journal: served {} after {} were journalled",
        fresh.served(),
        cp.done.len()
    );
}

#[tokio::test(start_paused = true)]
async fn a_refreshed_link_to_a_different_file_restarts() {
    let lab = Lab::new(config()).await;
    let old = content(8 * MIB, 17);
    let new = content(8 * MIB, 18);
    lab.serve(
        URL,
        Arc::new(
            FaultSource::new(old)
                .cfg(|c| {
                    c.expire_after = Some(3 * MIB);
                    c.per_conn_bps = Some(2 * MIB);
                })
                .script(|_, _| ReqPlan {
                    reset_after: Some(MIB),
                    ..ReqPlan::default()
                }),
        ),
    );
    let id = lab.add(URL).await;
    wait_for(&lab.engine, &id, &[DownloadState::NeedsRefresh], LONG).await;
    let other = "https://lab.test/other.bin";
    // Same size, different ETag ("v2"): the validators conflict.
    let src = Arc::new(FaultSource::with_versions(vec![new.clone(), new.clone()]));
    lock(&src.st).version = 1;
    lab.serve(other, src);
    lab.engine
        .refresh_url(RefreshUrlParams {
            id: id.clone(),
            url: other.into(),
            request: None,
        })
        .await
        .unwrap();
    finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &new);
}

#[tokio::test(start_paused = true)]
async fn pause_and_resume_continue_from_the_journal() {
    let lab = Lab::new(config()).await;
    let size = 32 * MIB;
    let data = content(size, 19);
    let src = lab.serve(
        URL,
        Arc::new(FaultSource::new(data.clone()).cfg(|c| c.per_conn_bps = Some(MIB))),
    );
    let id = lab.add(URL).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    lab.engine
        .pause(IdsParams {
            ids: vec![id.clone()],
        })
        .await
        .unwrap();
    let d = detail(&lab.engine, &id).await;
    assert_eq!(d.summary.state, DownloadState::Paused);
    let cp = lab.journal.load(&id).await.unwrap().unwrap();
    assert!(cp.done.len() >= 2 * MIB, "{}", cp.done.len());
    assert_eq!(cp.done.len(), d.summary.downloaded_bytes);
    let served_before = src.served();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(src.served(), served_before, "nothing runs while paused");

    lab.engine
        .resume(IdsParams {
            ids: vec![id.clone()],
        })
        .await
        .unwrap();
    finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    let resent = src.served() - served_before;
    assert!(
        resent <= size - cp.done.len() + 2 * MIB,
        "resent {resent}, journal had {}",
        cp.done.len()
    );
}

#[tokio::test(start_paused = true)]
async fn throttled_probe_honours_retry_after() {
    let lab = Lab::new(config()).await;
    let data = content(2 * MIB, 21);
    let src = Arc::new(FaultSource::new(data.clone()));
    src.fail_next_probe(SourceError::Throttled {
        message: "429".into(),
        retry_after: Some(Duration::from_secs(3)),
    });
    lab.serve(URL, src);
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    assert!(took >= Duration::from_secs(3), "took {took:?}");
    assert!(took < Duration::from_secs(4), "took {took:?}");
}

#[tokio::test(start_paused = true)]
async fn a_dead_server_exhausts_the_error_budget() {
    let lab = Lab::new(config()).await;
    lab.serve(
        URL,
        Arc::new(
            FaultSource::new(content(4 * MIB, 22)).script(|_, _| ReqPlan {
                fail: Some(SourceError::Transient("connection refused".into())),
                ..ReqPlan::default()
            }),
        ),
    );
    let id = lab.add(URL).await;
    let d = wait_for(&lab.engine, &id, &[DownloadState::Failed], LONG).await;
    assert_eq!(d.summary.error.unwrap().code, "network");
}

#[tokio::test(start_paused = true)]
async fn an_expired_link_at_probe_needs_refresh() {
    let lab = Lab::new(config()).await;
    let src = Arc::new(FaultSource::new(content(MIB, 23)));
    src.expire();
    lab.serve(URL, src);
    let id = lab.add(URL).await;
    let d = wait_for(&lab.engine, &id, &[DownloadState::NeedsRefresh], LONG).await;
    assert_eq!(d.summary.error.unwrap().code, "link_expired");
}

#[tokio::test(start_paused = true)]
async fn fatal_errors_fail_at_once() {
    let lab = Lab::new(config()).await;
    let src = Arc::new(FaultSource::new(content(MIB, 24)));
    src.fail_next_probe(SourceError::Fatal("404 Not Found".into()));
    lab.serve(URL, src);
    let id = lab.add(URL).await;
    let d = wait_for(&lab.engine, &id, &[DownloadState::Failed], LONG).await;
    assert_eq!(d.summary.error.unwrap().code, "fatal");
}

#[tokio::test(start_paused = true)]
async fn empty_files_complete() {
    let lab = Lab::new(config()).await;
    lab.serve(URL, Arc::new(FaultSource::new(bytes::Bytes::new())));
    let id = lab.add(URL).await;
    finish(&lab.engine, &id, LONG).await;
    assert_eq!(lab.file("data.bin").unwrap().len(), 0);
}

#[tokio::test(start_paused = true)]
async fn empty_files_complete_without_ranges() {
    let lab = Lab::new(config()).await;
    lab.serve(
        URL,
        Arc::new(FaultSource::new(bytes::Bytes::new()).cfg(|c| {
            c.accepts_ranges = false;
            c.honours_ranges = false;
        })),
    );
    let id = lab.add(URL).await;
    finish(&lab.engine, &id, Duration::from_secs(10)).await;
    assert_eq!(lab.file("data.bin").unwrap().len(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_body_longer_than_the_probe_is_never_cut() {
    let lab = Lab::new(config()).await;
    // No ranges, no validators, chunked: only the probe's length is known,
    // and the body is longer.
    lab.serve(
        URL,
        Arc::new(FaultSource::new(content(5 * MIB, 30)).cfg(|c| {
            c.accepts_ranges = false;
            c.honours_ranges = false;
            c.size_known = false;
            c.validators = false;
            c.probe_size = Some(4 * MIB);
        })),
    );
    let id = lab.add(URL).await;
    let d = wait_for(
        &lab.engine,
        &id,
        &[DownloadState::Failed, DownloadState::Completed],
        LONG,
    )
    .await;
    assert_eq!(d.summary.state, DownloadState::Failed);
    assert_eq!(d.summary.error.unwrap().code, "remote_changed");
    assert!(lab.file("data.bin").is_none(), "no truncated file");
}

#[tokio::test(start_paused = true)]
async fn stalled_hedges_time_out_and_the_owner_is_replaced() {
    let mut cfg = config();
    cfg.tuning = Tuning {
        fixed_connections: Some(1),
        ..Tuning::default()
    };
    let lab = Lab::new(cfg).await;
    let data = content(4 * MIB, 31);
    let src = lab.serve(
        URL,
        Arc::new(
            FaultSource::new(data.clone())
                .cfg(|c| c.per_conn_bps = Some(8 * MIB))
                .script(|n, _| ReqPlan {
                    // The owner goes silent for an hour; every hedge hangs
                    // before its headers; only a replacement gets through.
                    stall: (n == 1).then_some((MIB, Duration::from_secs(3600))),
                    ttfb: if (2..=4).contains(&n) {
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
    assert!(took < Duration::from_secs(10), "took {took:?}");
    assert!(src.opens() >= 5, "opens {}", src.opens());
}
