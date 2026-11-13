//! Every labserver scenario, over HTTP/1.1 (`http://`), HTTP/2 (`https://`,
//! ALPN) and HTTP/3 (QUIC). Each `async fn` below runs once per transport.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use support::*;
use whoosh_core::{Protocol, RangeSource, SourceError, SourceFactory, Validators};
use whoosh_labserver::Scenario;
use whoosh_proto::RequestContext;

macro_rules! per_transport {
    ($($name:ident),* $(,)?) => {$(
        mod $name {
            use super::*;
            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn h1() { super::$name(T::H1).await }
            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn h1s() { super::$name(T::H1s).await }
            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn h2() { super::$name(T::H2).await }
            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn h3() { super::$name(T::H3).await }
        }
    )*};
}

per_transport!(
    clean,
    concurrent_opens,
    connection_reuse,
    lying_ranges,
    no_range,
    changing_file,
    expiring_url,
    rate_limited,
    conn_cap_2,
    conn_cap_reset,
    resets_mid_body,
    stall,
    idle_backstop,
    no_length,
    no_validators,
    redirect_chain,
    cookie_required,
    referer_required,
    high_latency,
    bangla_filename,
    empty_file,
    cancel_mid_body,
    cancel_before_head,
);

fn custom(name: &str, toml: &str) -> Option<Vec<Scenario>> {
    Some(vec![Scenario::from_toml(name, toml).unwrap()])
}

async fn clean(t: T) {
    let lab = Lab::new(t).await;
    let src = lab.source("clean", "10m.bin");
    let m = src.probe().await.unwrap();
    assert_eq!(m.size, Some(10 * MIB));
    assert!(m.accepts_ranges);
    assert!(m.etag.as_deref().is_some_and(|e| e.starts_with('"')));
    assert!(m.last_modified.is_some());
    assert_eq!(m.protocol, Some(t.protocol()));
    assert_eq!(
        m.final_url.as_deref(),
        Some(lab.url("clean", "10m.bin").as_str())
    );
    assert_eq!(m.content_type.as_deref(), Some("application/octet-stream"));
    assert_eq!(m.filename, None);
    let v = m.validators();
    for (a, b) in [
        (0, 1),
        (0, 65536),
        (MIB - 3, MIB + 5),
        (10 * MIB - 1, 10 * MIB),
        (3 * MIB, 7 * MIB),
    ] {
        let bytes = fetch(&*src, Some(r(a, b)), v.clone()).await;
        assert_eq!(bytes.len() as u64, b - a, "{a}..{b}");
        lab.verify("clean", "10m.bin", a, &bytes);
    }
    let all = fetch(&*src, None, v.clone()).await;
    assert_eq!(all.len() as u64, 10 * MIB);
    lab.verify("clean", "10m.bin", 0, &all);
    // An empty range is an empty stream, without a request.
    let before = lab.lab.stats("clean").unwrap().requests;
    assert!(fetch(&*src, Some(r(5, 5)), v).await.is_empty());
    assert_eq!(lab.lab.stats("clean").unwrap().requests, before);
}

/// Contract point 1: sixteen requests in flight at once, each its own
/// request on its own connection (HTTP/2 and HTTP/3 too, by default). The
/// client's pool is the witness (labserver's live counts change as fast
/// responses finish under load; only its peaks are stable).
async fn concurrent_opens(t: T) {
    let sc = "per-conn-throttle";
    let lab = Lab::new(t).await;
    let src = lab.source(sc, "100m.bin");
    let v = src.probe().await.unwrap().validators();
    let n = 16u64;
    let len = MIB;
    let opens = (0..n).map(|i| {
        let src = src.clone();
        let v = v.clone();
        async move {
            src.open(open_req(Some(r(i * len, (i + 1) * len)), v))
                .await
                .unwrap()
        }
    });
    let streams = futures::future::join_all(opens).await;
    // All 16 held at once: 16 requests in flight, on 16 connections.
    assert_eq!(
        client_conns(&lab.factory, t),
        (n as usize, n as usize),
        "{:?}",
        lab.factory.pool_stats()
    );
    let st = lab.lab.stats(sc).unwrap();
    assert_eq!(st.peak_open_connections, n, "one connection each: {st:?}");
    let reads = streams.into_iter().map(read_all);
    for (i, (bytes, err)) in futures::future::join_all(reads)
        .await
        .into_iter()
        .enumerate()
    {
        assert!(err.is_none(), "{err:?}");
        assert_eq!(bytes.len() as u64, len);
        lab.verify(sc, "100m.bin", i as u64 * len, &bytes);
    }
}

/// Over TLS, decrypted records that arrived together reach the reader as
/// one chunk, not one chunk per 16 KiB TLS record (each chunk costs a
/// wake-up downstream).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_records_are_coalesced() {
    for t in [T::H1s, T::H2] {
        let lab = Lab::new(t).await;
        let src = lab.source("clean", "10m.bin");
        let m = src.probe().await.unwrap();
        let mut s = src
            .open(open_req(Some(r(0, 10 * MIB)), m.validators()))
            .await
            .unwrap();
        let (mut n, mut biggest, mut all) = (0usize, 0usize, Vec::new());
        while let Some(c) = s.next().await {
            let c = c.unwrap();
            n += 1;
            biggest = biggest.max(c.len());
            all.extend_from_slice(&c);
        }
        lab.verify("clean", "10m.bin", 0, &all);
        assert!(biggest > 16 * 1024, "{t:?}: largest chunk {biggest} B");
        assert!(n < 10 * 64, "{t:?}: {n} chunks for 10 MiB");
    }
}

/// Sequential requests reuse one kept-alive connection.
async fn connection_reuse(t: T) {
    let lab = Lab::new(t).await;
    let src = lab.source("clean", "10m.bin");
    let v = src.probe().await.unwrap().validators();
    for i in 0..5 {
        let b = fetch(&*src, Some(r(i * MIB, i * MIB + 1000)), v.clone()).await;
        lab.verify("clean", "10m.bin", i * MIB, &b);
    }
    let all = fetch(&*src, None, v).await;
    assert_eq!(all.len() as u64, 10 * MIB);
    let st = lab.lab.stats("clean").unwrap();
    assert_eq!(st.requests, 7);
    assert_eq!(st.peak_open_connections, 1, "{st:?}");
}

async fn ranges_ignored(t: T, sc: &str) {
    let lab = Lab::new(t).await;
    let src = lab.source(sc, "10m.bin");
    let m = src.probe().await.unwrap();
    assert!(!m.accepts_ranges, "{sc}: no real 206 was seen");
    assert_eq!(m.size, Some(10 * MIB));
    let e = open_err(&*src, Some(r(MIB, 2 * MIB)), m.validators()).await;
    assert_eq!(e, SourceError::RangesUnsupported);
    let all = fetch(&*src, None, m.validators()).await;
    assert_eq!(all.len() as u64, 10 * MIB);
    lab.verify(sc, "10m.bin", 0, &all);
}

async fn lying_ranges(t: T) {
    ranges_ignored(t, "lying-ranges").await;
}

async fn no_range(t: T) {
    ranges_ignored(t, "no-range").await;
}

/// The file changes after 32 MiB served. The request in flight keeps the
/// old version; every later request with the old validators fails with
/// `RemoteChanged` instead of mixing versions.
async fn changing_file(t: T) {
    let (sc, f) = ("changing-file", "100m.bin");
    let lab = Lab::new(t).await;
    let src = lab.source(sc, f);
    let m = src.probe().await.unwrap();
    assert_eq!(m.etag, lab.lab.etag(sc, f, 0));
    let v0 = m.validators();
    let first = fetch(&*src, Some(r(0, 40 * MIB)), v0.clone()).await;
    lab.verify(sc, f, 0, &first); // all version 0
    assert_eq!(lab.lab.current_version(sc, f), Some(1));
    // Strong ETag: If-Range does not match, the server sends 200 v1.
    let e = open_err(&*src, Some(r(40 * MIB, 41 * MIB)), v0.clone()).await;
    assert_eq!(e, SourceError::RemoteChanged);
    // Without the ETag: no If-Range; the 206 carries a new Last-Modified.
    let lm_only = Validators {
        etag: None,
        ..v0.clone()
    };
    let e = open_err(&*src, Some(r(40 * MIB, 41 * MIB)), lm_only).await;
    assert_eq!(e, SourceError::RemoteChanged);
    // The whole file, unranged.
    assert_eq!(open_err(&*src, None, v0).await, SourceError::RemoteChanged);
}

async fn expiring_url(t: T) {
    let lab = Lab::new(t).await;
    let path = lab
        .lab
        .mint_path("expiring-url", "1m.bin", Some(Duration::from_secs(3)))
        .unwrap();
    let src = lab.factory.source(&lab.origin_url(&path), None).unwrap();
    let m = src.probe().await.unwrap();
    let b = fetch(&*src, Some(r(0, 1000)), m.validators()).await;
    lab.verify("expiring-url", "1m.bin", 0, &b);
    sleep_past_expiry(&path).await;
    let e = open_err(&*src, Some(r(0, 1000)), m.validators()).await;
    assert!(matches!(e, SourceError::LinkExpired(_)), "{e:?}");
    // Unsigned: forbidden at once.
    let e = lab
        .source("expiring-url", "1m.bin")
        .probe()
        .await
        .unwrap_err();
    assert!(matches!(e, SourceError::LinkExpired(_)), "{e:?}");
}

/// Opens `held` long requests (5 MB/s per connection, 20 MiB each) and
/// returns them unread, so they occupy the server's per-IP slots.
async fn hold(
    src: &Arc<dyn RangeSource>,
    v: &Validators,
    held: u64,
) -> Vec<whoosh_core::ByteStream> {
    let mut out = Vec::new();
    for i in 0..held {
        let s = src
            .open(open_req(
                Some(r(i * 20 * MIB, (i + 1) * 20 * MIB)),
                v.clone(),
            ))
            .await
            .unwrap_or_else(|e| panic!("held request {i}: {e:?}"));
        out.push(s);
    }
    out
}

async fn throttled(t: T, sc: &str, cap: u64, retry_after: Option<Duration>) {
    let lab = Lab::new(t).await;
    let src = lab.source(sc, "100m.bin");
    let v = src.probe().await.unwrap().validators();
    let held = hold(&src, &v, cap).await;
    let e = open_err(&*src, Some(r(90 * MIB, 91 * MIB)), v.clone()).await;
    match e {
        SourceError::Throttled {
            retry_after: got, ..
        } => assert_eq!(got, retry_after, "{sc}"),
        other => panic!("{sc}: expected Throttled, got {other:?}"),
    }
    drop(held);
}

/// 429 + `Retry-After: 2` above four requests.
async fn rate_limited(t: T) {
    throttled(t, "rate-limited", 4, Some(Duration::from_secs(2))).await;
}

/// 503 + `Retry-After: 1` above two requests.
async fn conn_cap_2(t: T) {
    throttled(t, "conn-cap-2", 2, Some(Duration::from_secs(1))).await;
}

/// The request above the cap gets its connection reset (HTTP/3: stream
/// reset + CONNECTION_CLOSE) before any response head.
async fn conn_cap_reset(t: T) {
    throttled(t, "conn-cap-reset", 2, None).await;
}

/// A reset in the middle of a body is `Transient`, after exactly the bytes
/// that arrived; nothing is retried behind the engine's back.
async fn resets_mid_body(t: T) {
    let lab = Lab::with(
        t,
        custom("reset", "[resets]\nafter_bytes = \"256KiB\""),
        |_| {},
    )
    .await;
    let src = lab.source("reset", "1m.bin");
    let m = src.probe().await.unwrap();
    for i in 0..3 {
        match src.open(open_req(Some(r(0, MIB)), m.validators())).await {
            Ok(s) => {
                let (bytes, e) = read_all(s).await;
                assert!(matches!(e, Some(SourceError::Transient(_))), "{i}: {e:?}");
                assert!((bytes.len() as u64) <= 256 * 1024, "{}", bytes.len());
                lab.verify("reset", "1m.bin", 0, &bytes);
            }
            // Labserver resets 20 ms after the reset point, and a reset
            // discards what the server has not transmitted yet (a TCP RST
            // or an HTTP/3 RESET_STREAM alike). On a loaded machine that can
            // include the response head: then it is a reset before the
            // head, which the contract calls `Throttled`.
            Err(e) => assert!(
                matches!(
                    e,
                    SourceError::Throttled {
                        retry_after: None,
                        ..
                    }
                ),
                "open {i}: {e:?}; {:?}",
                lab.lab.stats("reset")
            ),
        }
    }
    let st = lab.lab.stats("reset").unwrap();
    assert_eq!(st.requests, 4, "probe + 3 opens, no hidden retries");
}

/// A 1.5 s pause mid-body is not an error.
async fn stall(t: T) {
    let toml = "[stalls]\nafter_bytes = \"256KiB\"\nduration_ms = 1500";
    let lab = Lab::with(t, custom("stall", toml), |_| {}).await;
    let src = lab.source("stall", "1m.bin");
    let m = src.probe().await.unwrap();
    let t0 = Instant::now();
    let b = fetch(&*src, Some(r(0, MIB)), m.validators()).await;
    assert!(
        t0.elapsed() >= Duration::from_millis(1400),
        "{:?}",
        t0.elapsed()
    );
    lab.verify("stall", "1m.bin", 0, &b);
    assert_eq!(lab.lab.stats("stall").unwrap().stalls_injected, 1);
}

/// The idle backstop fires when no byte arrives for `idle_timeout`.
async fn idle_backstop(t: T) {
    // The stall outlasts any plausible scheduling delay, so the error itself
    // proves the backstop fired during it: had it not, the body would
    // complete after the stall, without an error. No wall-clock bound.
    let toml = "[stalls]\nafter_bytes = \"256KiB\"\nduration_ms = 30000";
    let lab = Lab::with(t, custom("stall", toml), |c| {
        c.idle_timeout = Duration::from_millis(700)
    })
    .await;
    let src = lab.source("stall", "1m.bin");
    let m = src.probe().await.unwrap();
    let s = src
        .open(open_req(Some(r(0, MIB)), m.validators()))
        .await
        .unwrap();
    let (bytes, e) = read_all(s).await;
    assert!(
        matches!(&e, Some(SourceError::Transient(m)) if m.contains("no data")),
        "{e:?}"
    );
    lab.verify("stall", "1m.bin", 0, &bytes);
}

async fn no_length(t: T) {
    let lab = Lab::new(t).await;
    let src = lab.source("no-length", "10m.bin");
    let m = src.probe().await.unwrap();
    assert_eq!(m.size, Some(10 * MIB), "size from Content-Range");
    assert!(m.accepts_ranges);
    let b = fetch(&*src, Some(r(MIB, 3 * MIB)), m.validators()).await;
    lab.verify("no-length", "10m.bin", MIB, &b);
    for expect in [m.validators(), Validators::default()] {
        let all = fetch(&*src, None, expect).await;
        assert_eq!(all.len() as u64, 10 * MIB);
        lab.verify("no-length", "10m.bin", 0, &all);
    }
}

async fn no_validators(t: T) {
    let lab = Lab::new(t).await;
    let src = lab.source("no-validators", "10m.bin");
    let m = src.probe().await.unwrap();
    assert_eq!(
        (m.etag.as_deref(), m.last_modified.as_deref()),
        (None, None)
    );
    assert_eq!(m.size, Some(10 * MIB));
    assert!(m.accepts_ranges);
    let b = fetch(&*src, Some(r(2 * MIB, 4 * MIB)), m.validators()).await;
    lab.verify("no-validators", "10m.bin", 2 * MIB, &b);
}

/// Three 302s; the end of the chain is reused by later requests.
async fn redirect_chain(t: T) {
    let lab = Lab::new(t).await;
    let src = lab.source("redirect-chain", "10m.bin");
    let m = src.probe().await.unwrap();
    assert_eq!(
        m.final_url.as_deref(),
        Some(lab.url("redirect-chain", "_hop/3/10m.bin").as_str())
    );
    assert_eq!(m.size, Some(10 * MIB));
    for i in 0..3 {
        let b = fetch(&*src, Some(r(i * MIB, (i + 1) * MIB)), m.validators()).await;
        lab.verify("redirect-chain", "10m.bin", i * MIB, &b);
    }
    let st = lab.lab.stats("redirect-chain").unwrap();
    assert_eq!(st.requests, 4 + 3, "chain followed once: {st:?}");
}

async fn cookie_required(t: T) {
    let lab = Lab::new(t).await;
    let e = lab
        .source("cookie-required", "1m.bin")
        .probe()
        .await
        .unwrap_err();
    assert!(matches!(e, SourceError::LinkExpired(_)), "{e:?}");
    let wrong = RequestContext {
        cookie: Some("whoosh_session=nope".into()),
        ..Default::default()
    };
    let e = lab
        .source_with("cookie-required", "1m.bin", &wrong)
        .probe()
        .await
        .unwrap_err();
    assert!(matches!(e, SourceError::LinkExpired(_)), "{e:?}");
    let ok = RequestContext {
        cookie: Some("theme=dark; whoosh_session=lab-ok".into()),
        ..Default::default()
    };
    let src = lab.source_with("cookie-required", "1m.bin", &ok);
    let m = src.probe().await.unwrap();
    let b = fetch(&*src, Some(r(0, MIB)), m.validators()).await;
    lab.verify("cookie-required", "1m.bin", 0, &b);
}

async fn referer_required(t: T) {
    let lab = Lab::new(t).await;
    let e = lab
        .source("referer-required", "1m.bin")
        .probe()
        .await
        .unwrap_err();
    assert!(matches!(e, SourceError::LinkExpired(_)), "{e:?}");
    let ok = RequestContext {
        referer: Some("https://downloads.example.test/files/page.html".into()),
        ..Default::default()
    };
    let src = lab.source_with("referer-required", "1m.bin", &ok);
    let m = src.probe().await.unwrap();
    let b = fetch(&*src, Some(r(0, MIB)), m.validators()).await;
    lab.verify("referer-required", "1m.bin", 0, &b);
}

/// 500 ms before every response head: `open` resolves only after it.
async fn high_latency(t: T) {
    let lab = Lab::new(t).await;
    let src = lab.source("high-latency", "1m.bin");
    let t0 = Instant::now();
    let m = src.probe().await.unwrap();
    assert!(t0.elapsed() >= Duration::from_millis(480));
    let t0 = Instant::now();
    let s = src
        .open(open_req(Some(r(0, MIB)), m.validators()))
        .await
        .unwrap();
    assert!(
        t0.elapsed() >= Duration::from_millis(480),
        "open waits for the head"
    );
    let (b, e) = read_all(s).await;
    assert!(e.is_none());
    lab.verify("high-latency", "1m.bin", 0, &b);
}

/// Content-Disposition variants, decoded and sanitised.
async fn bangla_filename(t: T) {
    let lab = Lab::new(t).await;
    let plain = if cfg!(windows) {
        "annual _final_ report.pdf"
    } else {
        "annual \"final\" report.pdf"
    };
    for (file, want) in [
        ("bangla.bin", "বাংলা প্রতিবেদন ২০২৬.pdf"),
        ("bangla-star-only.bin", "পরীক্ষা.zip"),
        ("plain.bin", plain),
        ("latin1.bin", "résumé.pdf"),
        ("raw-utf8.bin", "বাংলা.txt"),
        ("traversal.bin", "passwd"),
    ] {
        let m = lab.source("bangla-filename", file).probe().await.unwrap();
        assert_eq!(m.filename.as_deref(), Some(want), "{file}");
    }
}

async fn empty_file(t: T) {
    let lab = Lab::new(t).await;
    let src = lab.source("clean", "empty.bin");
    let m = src.probe().await.unwrap();
    assert_eq!(m.size, Some(0));
    assert!(!m.accepts_ranges);
    assert!(fetch(&*src, None, m.validators()).await.is_empty());
}

/// Contract point 4: dropping a stream mid-body ends the request on the
/// server promptly.
async fn cancel_mid_body(t: T) {
    let lab = Lab::new(t).await;
    let src = lab.source("clean", "1g.bin");
    let m = src.probe().await.unwrap();
    let mut s = src
        .open(open_req(Some(r(0, 1 << 30)), m.validators()))
        .await
        .unwrap();
    let first = s.next().await.unwrap().unwrap();
    lab.verify("clean", "1g.bin", 0, &first);
    assert_eq!(lab.lab.stats("clean").unwrap().active_requests, 1);
    drop(s);
    let gone = eventually(Duration::from_secs(5), || async {
        lab.lab.stats("clean").unwrap().active_requests == 0
    })
    .await;
    assert!(gone, "request still active: {:?}", lab.lab.stats("clean"));
    let st = lab.lab.stats("clean").unwrap();
    assert!(st.bytes_sent < 512 * MIB, "server kept sending: {st:?}");
}

/// Dropping the `open` future before the response head cancels too.
async fn cancel_before_head(t: T) {
    let lab = Lab::new(t).await;
    let src = lab.source("high-latency", "100m.bin");
    let m = src.probe().await.unwrap();
    let f = src.open(open_req(Some(r(0, 100 * MIB)), m.validators()));
    assert!(
        tokio::time::timeout(Duration::from_millis(150), f)
            .await
            .is_err()
    );
    let gone = eventually(Duration::from_secs(5), || async {
        lab.lab.stats("high-latency").unwrap().active_requests == 0
    })
    .await;
    assert!(
        gone,
        "request still active: {:?}",
        lab.lab.stats("high-latency")
    );
    let st = lab.lab.stats("high-latency").unwrap();
    assert!(st.bytes_sent < 50 * MIB, "{st:?}");
}

// ------------------------------------------------------------ one-offs

/// `flaky` itself (random resets, ~1 per 50 MiB): every failure surfaces
/// as `Transient` after correct bytes; the engine sees each one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flaky_builtin_h1() {
    let lab = Lab::new(T::H1).await;
    let src = lab.source("flaky", "100m.bin");
    let m = src.probe().await.unwrap();
    let mut opens = 0;
    let mut errors = 0;
    for _ in 0..6 {
        opens += 1;
        let s = src.open(open_req(None, m.validators())).await.unwrap();
        let (b, e) = read_all(s).await;
        lab.verify("flaky", "100m.bin", 0, &b);
        match e {
            None => assert_eq!(b.len() as u64, 100 * MIB),
            Some(SourceError::Transient(_)) => errors += 1,
            Some(other) => panic!("{other:?}"),
        }
    }
    let st = lab.lab.stats("flaky").unwrap();
    assert!(
        errors >= 1 && st.resets_injected >= 1,
        "no reset in 600 MiB: {st:?}"
    );
    assert_eq!(st.requests, 1 + opens);
}

/// HTTP/3 is learnt from `Alt-Svc` on a TCP response, then used.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alt_svc_switches_to_h3() {
    let lab3 = whoosh_labserver::LabServer::start(whoosh_labserver::LabConfig {
        h3_port: Some(0),
        ..Default::default()
    })
    .await
    .unwrap();
    // A plain factory: no host profile, so it starts on TCP.
    let f = whoosh_net::HttpSourceFactory::new(test_config(&lab3));
    let url = format!(
        "https://localhost:{}/s/clean/1m.bin",
        lab3.https_addr().unwrap().port()
    );
    let src = f.source(&url, None).unwrap();
    let m1 = src.probe().await.unwrap();
    assert_eq!(m1.protocol, Some(Protocol::Http2));
    let m2 = src.probe().await.unwrap();
    assert_eq!(m2.protocol, Some(Protocol::Http3));
    let b = fetch(&*src, Some(r(0, MIB)), m2.validators()).await;
    lab3.file("clean", "1m.bin").unwrap().verify(0, &b).unwrap();
    // Plain http:// never switches, even though Alt-Svc is sent there too.
    let plain = f
        .source(&format!("http://{}/s/clean/1m.bin", lab3.http_addr()), None)
        .unwrap();
    for _ in 0..2 {
        assert_eq!(plain.probe().await.unwrap().protocol, Some(Protocol::Http1));
    }
}

/// QUIC to a port where nobody answers: TCP takes over after the fallback
/// delay, and HTTP/3 is not tried again for a while. The test owns the
/// "dead" UDP port, so it sees QUIC's packets arrive there: that is the
/// witness for "QUIC was tried" and "QUIC was not tried", not wall-clock
/// time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn h3_falls_back_to_tcp() {
    let lab = Lab::new(T::H2).await;
    let dead = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    dead.set_nonblocking(true).unwrap();
    let port = dead.local_addr().unwrap().port();
    // Datagrams waiting on the dead port (QUIC Initials, or the close of
    // an abandoned attempt).
    fn drain(s: &std::net::UdpSocket) -> usize {
        let mut buf = [0u8; 2048];
        std::iter::from_fn(|| s.recv(&mut buf).ok()).count()
    }
    let dead = &dead;
    // An IP origin (in the lab certificate), so the dead socket is the only
    // address QUIC can try (`localhost` would try `::1` first).
    let origin = format!("https://127.0.0.1:{}", lab.lab.https_addr().unwrap().port());
    let f = lab.factory_with(|c| c.h3_fallback_delay = Duration::from_millis(200));
    f.remember_http3(&origin, port, Duration::from_secs(60));
    let src = f.source(&format!("{origin}/s/clean/1m.bin"), None).unwrap();
    let t0 = Instant::now();
    let m = src.probe().await.unwrap();
    assert_eq!(m.protocol, Some(Protocol::Http2));
    // TCP only joined after the delay (a lower bound: load only adds)...
    assert!(
        t0.elapsed() >= Duration::from_millis(190),
        "{:?}",
        t0.elapsed()
    );
    // ...and QUIC was tried first.
    assert!(
        drain(dead) > 0,
        "no QUIC packet reached the advertised port"
    );
    // Let the abandoned attempt fall silent.
    let quiet = eventually(Duration::from_secs(10), || async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        drain(dead) == 0
    })
    .await;
    assert!(quiet, "the abandoned QUIC attempt kept sending");
    // Marked broken: a new connection goes straight to TCP, with no QUIC
    // packet at all (QUIC would have sent its Initial before TCP started).
    let held = src
        .open(open_req(Some(r(0, MIB)), m.validators()))
        .await
        .unwrap();
    let src2 = f
        .source(&format!("{origin}/s/clean/10m.bin"), None)
        .unwrap();
    let m2 = src2.probe().await.unwrap();
    assert_eq!(m2.protocol, Some(Protocol::Http2));
    assert_eq!(f.pool_stats().http2, 2, "{:?}", f.pool_stats());
    assert_eq!(drain(dead), 0, "HTTP/3 was tried again while marked broken");
    drop(held);
}

/// More streams per connection when configured: requests opened one after
/// another fill a connection before the next one is opened. The client's
/// pool counts are exact while the streams are held; labserver's peak
/// connection count (monotonic) confirms the server saw no third one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn h2_streams_per_connection() {
    let sc = "per-conn-throttle";
    for t in [T::H2, T::H3] {
        let lab = Lab::with(t, None, |c| c.max_streams_per_connection = 4).await;
        let src = lab.source(sc, "10m.bin");
        let v = src.probe().await.unwrap().validators();
        let len = 256 * 1024;
        let mut held = Vec::new();
        for i in 0..8 {
            held.push(
                src.open(open_req(Some(r(i * len, (i + 1) * len)), v.clone()))
                    .await
                    .unwrap(),
            );
        }
        assert_eq!(
            client_conns(&lab.factory, t),
            (2, 8),
            "{t:?}: {:?}",
            lab.factory.pool_stats()
        );
        let st = lab.lab.stats(sc).unwrap();
        assert_eq!(st.peak_open_connections, 2, "{t:?}: {st:?}");
        for (i, s) in held.into_iter().enumerate() {
            let (b, e) = read_all(s).await;
            assert!(e.is_none());
            lab.verify(sc, "10m.bin", i as u64 * len, &b);
        }
        assert_eq!(client_conns(&lab.factory, t), (2, 0), "{t:?}");
    }
}

/// A slow response head is a `Transient` error after `idle_timeout`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn head_timeout() {
    // 30 s of server think-time against a 200 ms limit: no race with load.
    let slow = custom("slow-head", "[latency]\nttfb_ms = 30000");
    let lab = Lab::with(T::H1, slow, |c| c.idle_timeout = Duration::from_millis(200)).await;
    let e = lab.source("slow-head", "1m.bin").probe().await.unwrap_err();
    assert!(
        matches!(&e, SourceError::Transient(m) if m.contains("no response")),
        "{e:?}"
    );
}

/// Preconnect over TLS, then the probe and an open use that connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preconnect_https() {
    for t in [T::H2, T::H3] {
        let lab = Lab::new(t).await;
        let url = lab.url("high-latency", "1m.bin");

        lab.factory.preconnect(&url).await;
        let src = lab.factory.source(&url, None).unwrap();
        let m = src.probe().await.unwrap();
        assert_eq!(m.protocol, Some(t.protocol()));
        let st = lab.lab.stats("high-latency").unwrap();
        assert_eq!(st.peak_open_connections, 1, "{t:?}: {st:?}");
    }
}
