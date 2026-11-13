//! `RangeSource::start` (ADR 0005 amendment 3): a fresh download probes
//! and starts in one request, its first connection owns the whole file and
//! runs alone until splitting pays; a resume still probes first. The source
//! here makes `start` one request, as a network source does.

mod common;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use common::*;
use whoosh_core::{SourceError, StateStore};
use whoosh_proto::service::Service;
use whoosh_proto::{AddParams, DownloadState, IdsParams};

const LONG: Duration = Duration::from_secs(600);

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn native(data: Bytes, f: impl FnOnce(&mut Cfg)) -> Arc<FaultSource> {
    Arc::new(FaultSource::new(data).cfg(|c| {
        c.native_start = true;
        f(c)
    }))
}

#[tokio::test(start_paused = true)]
async fn a_small_fast_file_takes_one_request() {
    let lab = Lab::new(config()).await;
    // 40 ms on one connection: less than `solo_min_eta`, so nobody joins.
    let data = content(4 * MIB, 101);
    let src = lab.serve(
        URL,
        native(data.clone(), |c| {
            c.per_conn_bps = Some(100 * MIB);
            c.ttfb = ms(20);
        }),
    );
    let id = lab.add(URL).await;
    let (d, took) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert_eq!(src.requests(), 1, "no probe, no second connection");
    assert_eq!(src.starts(), 1);
    assert_eq!(src.served(), 4 * MIB);
    assert_eq!(d.summary.total_bytes, Some(4 * MIB));
    assert!(took < ms(120), "took {took:?}");
    assert!(lab.engine.record(&id).unwrap().resumable);
}

#[tokio::test(start_paused = true)]
async fn a_slow_first_connection_is_joined_at_once() {
    let lab = Lab::new(config()).await;
    // 32 s alone: the others join after the first check.
    let size = 32 * MIB;
    let data = content(size, 102);
    let src = lab.serve(
        URL,
        native(data.clone(), |c| {
            c.per_conn_bps = Some(MIB);
            c.chunk = 16 * KIB as usize;
        }),
    );
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert_eq!(src.starts(), 1);
    assert_eq!(lock(&src.st).probes, 0, "a fresh download does not probe");
    assert!(src.peak() >= 8, "peak {}", src.peak());
    let ranges = lock(&src.st).ranges.clone();
    assert_eq!(ranges[0], None, "the first response is the start");
    assert!(ranges[1..].iter().all(Option::is_some), "{ranges:?}");
    // The first connection's response is dropped where its range ends: the
    // server sends (about) the file once, not the whole file again.
    assert!(src.served() < size + 2 * MIB, "served {}", src.served());
    // Eight connections from the first few milliseconds take 4 s.
    assert!(took < Duration::from_secs(5), "took {took:?}");
}

#[tokio::test(start_paused = true)]
async fn a_high_latency_small_file_needs_one_round_trip() {
    // 500 ms before every response; one connection then takes 40 ms (one
    // 256 KiB chunk per millisecond, the paused clock's resolution).
    async fn run(native_start: bool) -> (Duration, u64) {
        let lab = Lab::new(config()).await;
        let data = content(10 * MIB, 103);
        let src = lab.serve(
            URL,
            Arc::new(FaultSource::new(data.clone()).cfg(|c| {
                c.native_start = native_start;
                c.ttfb = ms(500);
                c.chunk = 256 * KIB as usize;
                c.per_conn_bps = Some(256 * MIB);
            })),
        );
        let id = lab.add(URL).await;
        let (_, took) = finish(&lab.engine, &id, LONG).await;
        assert_same(&lab.file("data.bin").unwrap(), &data);
        (took, src.requests())
    }
    let (took, requests) = run(true).await;
    assert_eq!(requests, 1);
    assert!(took < ms(600), "took {took:?}");
    // The trait's default start (probe, then open) costs a round trip more,
    // and still never splits.
    let (took, requests) = run(false).await;
    assert_eq!(requests, 2);
    assert!(took >= ms(1000) && took < ms(1100), "took {took:?}");
}

#[tokio::test(start_paused = true)]
async fn without_ranges_the_first_response_is_the_download() {
    let lab = Lab::new(config()).await;
    let data = content(6 * MIB, 104);
    let src = lab.serve(
        URL,
        native(data.clone(), |c| {
            c.accepts_ranges = false;
            c.honours_ranges = false;
            c.per_conn_bps = Some(4 * MIB);
        }),
    );
    let id = lab.add(URL).await;
    let (d, _) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert_eq!(src.requests(), 1);
    assert_eq!(src.peak(), 1);
    assert!(!lab.engine.record(&id).unwrap().resumable);
    assert_eq!(d.summary.total_bytes, Some(6 * MIB));
}

#[tokio::test(start_paused = true)]
async fn ignored_ranges_stream_from_the_first_response() {
    // The server advertises ranges but answers `bytes=0-` with a 200: the
    // start says ranges do not work, and nothing is requested again.
    let lab = Lab::new(config()).await;
    let data = content(12 * MIB, 105);
    let src = lab.serve(
        URL,
        native(data.clone(), |c| {
            c.honours_ranges = false;
            c.per_conn_bps = Some(4 * MIB);
        }),
    );
    let id = lab.add(URL).await;
    finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert_eq!(src.requests(), 1);
}

#[tokio::test(start_paused = true)]
async fn unknown_size_streams_from_the_first_response() {
    let lab = Lab::new(config()).await;
    let data = content(5 * MIB + 17, 106);
    let src = lab.serve(
        URL,
        native(data.clone(), |c| {
            c.size_known = false;
            c.per_conn_bps = Some(4 * MIB);
        }),
    );
    let id = lab.add(URL).await;
    let (d, _) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert_eq!(src.requests(), 1);
    assert_eq!(d.summary.total_bytes, Some(data.len() as u64));
}

#[tokio::test(start_paused = true)]
async fn a_resume_probes_first() {
    let lab = Lab::new(config()).await;
    let size = 32 * MIB;
    let data = content(size, 107);
    let src = lab.serve(URL, native(data.clone(), |c| c.per_conn_bps = Some(MIB)));
    let id = lab.add(URL).await;
    tokio::time::sleep(ms(800)).await;
    lab.engine
        .pause(IdsParams {
            ids: vec![id.clone()],
        })
        .await
        .unwrap();
    wait_for(&lab.engine, &id, &[DownloadState::Paused], LONG).await;
    assert_eq!(lock(&src.st).probes, 0);
    lab.engine
        .resume(IdsParams {
            ids: vec![id.clone()],
        })
        .await
        .unwrap();
    finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert_eq!(src.starts(), 1, "only the fresh run starts");
    assert!(
        lock(&src.st).probes >= 1,
        "the resume checked validators first"
    );
    assert!(src.served() < size + 4 * MIB, "served {}", src.served());
}

#[tokio::test(start_paused = true)]
async fn a_first_response_that_breaks_is_picked_up() {
    let lab = Lab::new(config()).await;
    // 62 ms alone at 64 MiB/s (the paused clock's limit for 64 KiB chunks),
    // so it is still alone when it breaks.
    let data = content(4 * MIB, 108);
    let src = lab.serve(
        URL,
        Arc::new(
            FaultSource::new(data.clone())
                .cfg(|c| {
                    c.native_start = true;
                    c.per_conn_bps = Some(100 * MIB);
                })
                .script(|n, _| ReqPlan {
                    reset_after: (n == 1).then_some(MIB + 5),
                    ..ReqPlan::default()
                }),
        ),
    );
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    eprintln!("reset while alone: took {took:?}");
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert_eq!(src.starts(), 1);
    // What is left (3 MiB, 47 ms) is less than a new connection is worth:
    // one replacement takes it over alone, and since nothing else was
    // running it does not wait out the retry backoff.
    assert_eq!(src.requests(), 2);
    assert!(took < ms(90), "took {took:?}");
}

#[tokio::test(start_paused = true)]
async fn a_first_response_that_stalls_is_hedged() {
    let lab = Lab::new(config()).await;
    // Fast and small, so it runs alone; then it stops for a minute.
    let data = content(4 * MIB, 109);
    let src = lab.serve(
        URL,
        Arc::new(
            FaultSource::new(data.clone())
                .cfg(|c| {
                    c.native_start = true;
                    c.per_conn_bps = Some(100 * MIB);
                })
                // Late: the rest would still take it only ~10 ms, so its
                // time-to-finish estimate alone would keep it running alone.
                .script(|n, _| ReqPlan {
                    stall: (n == 1).then_some((7 * MIB / 2, Duration::from_secs(60))),
                    ..ReqPlan::default()
                }),
        ),
    );
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    eprintln!("stall while alone: took {took:?}");
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert!(src.requests() >= 2);
    // Silence ends the solo run within 20 ms, and the stalled range is
    // hedged in the endgame (60 ms of silence) rather than after the 250 ms
    // stall limit.
    assert!(took < ms(200), "took {took:?}");
}

#[tokio::test(start_paused = true)]
async fn an_expired_link_at_start_needs_refresh() {
    let lab = Lab::new(config()).await;
    let src = native(content(MIB, 110), |_| {});
    src.expire();
    lab.serve(URL, src.clone());
    let id = lab.add(URL).await;
    let d = wait_for(&lab.engine, &id, &[DownloadState::NeedsRefresh], LONG).await;
    assert_eq!(d.summary.error.unwrap().code, "link_expired");
    assert_eq!(src.starts(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_throttled_start_is_retried_after_retry_after() {
    let lab = Lab::new(config()).await;
    let data = content(2 * MIB, 111);
    let src = native(data.clone(), |_| {});
    src.fail_next_probe(SourceError::Throttled {
        message: "429".into(),
        retry_after: Some(Duration::from_secs(3)),
    });
    lab.serve(URL, src.clone());
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert!(took >= Duration::from_secs(3), "took {took:?}");
    assert!(took < Duration::from_secs(4), "took {took:?}");
    assert_eq!(src.starts(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_file_that_changes_under_the_first_response_restarts_cleanly() {
    let lab = Lab::new(config()).await;
    let v1 = content(16 * MIB, 112);
    let v2 = content(16 * MIB, 113);
    let src = lab.serve(
        URL,
        Arc::new(
            FaultSource::with_versions(vec![v1, v2.clone()])
                .cfg(|c| {
                    c.native_start = true;
                    c.change_after = Some(5 * MIB);
                    c.per_conn_bps = Some(8 * MIB);
                })
                // Responses end early, so new requests see the change.
                .script(|_, _| ReqPlan {
                    reset_after: Some(MIB),
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
async fn a_pinned_count_is_still_a_ceiling_for_a_small_file() {
    let lab = Lab::new(config()).await;
    let data = content(4 * MIB, 114);
    let src = lab.serve(
        URL,
        native(data.clone(), |c| c.per_conn_bps = Some(200 * MIB)),
    );
    let id = lab
        .engine
        .add(AddParams {
            connections: Some(4),
            ..add_params(URL)
        })
        .await
        .unwrap()
        .id;
    finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert_eq!(src.requests(), 1);
}

#[tokio::test(start_paused = true)]
async fn an_empty_file_takes_one_request() {
    let lab = Lab::new(config()).await;
    let src = lab.serve(URL, native(Bytes::new(), |_| {}));
    let id = lab.add(URL).await;
    finish(&lab.engine, &id, LONG).await;
    assert_eq!(lab.file("data.bin").unwrap().len(), 0);
    assert_eq!(src.requests(), 1);
}

/// labserver's per-conn-throttle and slow-drip on the paused clock: 10 MiB
/// at 2 MB/s and ~3.2 MB/s per connection (16 KiB chunks), one millisecond
/// to the first byte. `jump`: the first connection's solo rate may send the
/// count to 16 at once; `fast`: the fast first climb. Returns how long it
/// took and the peak connection count.
async fn short_throttled(per_conn: u64, jump: bool, fast: bool) -> (Duration, usize) {
    let mut cfg = config();
    if !jump {
        cfg.tuning.solo_window = 0;
    }
    if !fast {
        cfg.tuning.tune_fast_scaling = f64::INFINITY;
    }
    let lab = Lab::new(cfg).await;
    let data = content(10 * MIB, 115);
    let src = lab.serve(
        URL,
        native(data.clone(), |c| {
            c.per_conn_bps = Some(per_conn);
            c.chunk = 16 * KIB as usize;
            c.ttfb = ms(1);
        }),
    );
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    (took, src.peak())
}

#[tokio::test(start_paused = true)]
async fn a_short_per_connection_limited_download_climbs_at_once() {
    for per_conn in [2_000_000, 3_200_000] {
        let (careful, careful_peak) = short_throttled(per_conn, false, false).await;
        let (fast, fast_peak) = short_throttled(per_conn, false, true).await;
        let (jump, jump_peak) = short_throttled(per_conn, true, true).await;
        eprintln!(
            "{per_conn} B/s per connection: careful {careful:?} (peak {careful_peak}), \
             fast climb {fast:?} (peak {fast_peak}), jump {jump:?} (peak {jump_peak})"
        );
        // Careful windows end with the download: it never leaves 8.
        assert!(careful_peak <= 9, "{careful_peak}");
        // The fast climb reaches 16 within a few windows...
        assert!(fast_peak >= 16, "{fast_peak}");
        assert!(
            fast.as_secs_f64() < careful.as_secs_f64() * 0.8,
            "{fast:?} vs {careful:?}"
        );
        // ...the jump from the first connection's solo rate at once.
        assert!(jump_peak >= 16, "{jump_peak}");
        assert!(
            jump.as_secs_f64() < fast.as_secs_f64() * 0.9,
            "{jump:?} vs {fast:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_shared_limit_does_not_jump() {
    // 20 MB/s for everyone at 1 ms: one connection alone gets 20 KB per
    // round trip, over TCP's initial window, so nothing says "limited per
    // connection": the first plan is 8, not 16.
    let lab = Lab::new(config()).await;
    let data = content(10 * MIB, 117);
    let src = lab.serve(
        URL,
        native(data.clone(), |c| {
            c.global_bps = Some(20_000_000);
            c.chunk = 16 * KIB as usize;
            c.ttfb = ms(1);
        }),
    );
    let id = lab.add(URL).await;
    tokio::time::sleep(ms(60)).await;
    assert!(src.peak() <= 8, "peak {} before any climb", src.peak());
    finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
}

#[tokio::test(start_paused = true)]
async fn a_link_the_first_connection_fills_drops_back_to_it() {
    // 64 MiB/s for everyone (a fast server close by): the first connection
    // alone already gets it all. The others join, deliver nothing more in
    // their first window, and the download goes on with one connection
    // instead of trying 16.
    let lab = Lab::new(config()).await;
    let data = content(64 * MIB, 119);
    let src = lab.serve(
        URL,
        native(data.clone(), |c| {
            c.global_bps = Some(64 * MIB);
            c.ttfb = ms(1);
        }),
    );
    let id = lab.add(URL).await;
    let (_, took) = finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert!(took < Duration::from_millis(1100), "took {took:?}");
    assert!(src.peak() <= 8, "peak {}", src.peak());
    let p = lab
        .state
        .host_profile("lab.test:443")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.best_connections, Some(1), "{p:?}");
}

#[tokio::test(start_paused = true)]
async fn a_capped_server_refuses_one_wave_of_the_jump() {
    // 2 MB/s per connection says "limited per connection", but only two are
    // allowed: the jump to 16 opens 8 at a time, so 6 are refused, not 14.
    let lab = Lab::new(config()).await;
    let data = content(8 * MIB, 118);
    let src = lab.serve(
        URL,
        native(data.clone(), |c| {
            c.per_conn_bps = Some(2_000_000);
            c.chunk = 16 * KIB as usize;
            c.ttfb = ms(1);
            c.cap = Some((2, Cap::Throttle(Some(Duration::from_secs(1)))));
        }),
    );
    let id = lab.add(URL).await;
    finish(&lab.engine, &id, LONG).await;
    assert_same(&lab.file("data.bin").unwrap(), &data);
    assert!(src.peak() <= 2);
    // The start, a wave of 8 (6 refused), then the pieces two at a time.
    assert!(src.opens() <= 18, "opens {}", src.opens());
}
