//! `RangeSource::start`: the probe's metadata and the body from offset 0
//! in one request, over HTTP/1.1, HTTP/1.1+TLS, HTTP/2 and HTTP/3.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use support::*;
use whoosh_core::{RangeSource, RemoteMeta, SourceError, SourceFactory};
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
    start_clean,
    start_ranges_ignored,
    start_no_length_no_validators,
    start_redirects_and_gates,
    start_expiring_url,
    start_changing_file,
    start_empty_and_names,
    start_cancel,
    start_saves_a_round_trip,
);

/// `start` on a fresh source must learn exactly what `probe` learns (on
/// another fresh source), and its body must be the entity from offset 0.
async fn start_like_probe(
    lab: &Lab,
    sc: &str,
    file: &str,
    make: impl Fn() -> Arc<dyn RangeSource>,
) -> RemoteMeta {
    let probed = make().probe().await.unwrap();
    let (meta, s) = make().start().await.unwrap();
    assert_eq!(
        meta, probed,
        "{sc}/{file}: start learnt something else than probe"
    );
    let (bytes, e) = read_all(s).await;
    assert!(e.is_none(), "{sc}/{file}: {e:?}");
    if let Some(n) = meta.size {
        assert_eq!(bytes.len() as u64, n, "{sc}/{file}");
    }
    lab.verify(sc, file, 0, &bytes);
    meta
}

async fn start_clean(t: T) {
    let lab = Lab::new(t).await;
    let m = start_like_probe(&lab, "clean", "10m.bin", || lab.source("clean", "10m.bin")).await;
    assert_eq!(m.size, Some(10 * MIB));
    assert!(m.accepts_ranges);
    assert_eq!(m.protocol, Some(t.protocol()));
    let st = lab.lab.stats("clean").unwrap();
    assert_eq!(st.requests, 2, "probe + start: start is one request");
}

/// `200` to `bytes=0-`: the whole entity; like `probe`, no range support
/// unless a real 206 was seen (`lying-ranges` advertises `Accept-Ranges`).
async fn start_ranges_ignored(t: T) {
    let lab = Lab::new(t).await;
    for sc in ["no-range", "lying-ranges"] {
        let m = start_like_probe(&lab, sc, "10m.bin", || lab.source(sc, "10m.bin")).await;
        assert!(!m.accepts_ranges, "{sc}");
        assert_eq!(m.size, Some(10 * MIB), "{sc}");
    }
}

async fn start_no_length_no_validators(t: T) {
    let lab = Lab::new(t).await;
    let m = start_like_probe(&lab, "no-length", "10m.bin", || {
        lab.source("no-length", "10m.bin")
    })
    .await;
    assert_eq!((m.size, m.accepts_ranges), (Some(10 * MIB), true));
    let m = start_like_probe(&lab, "no-validators", "10m.bin", || {
        lab.source("no-validators", "10m.bin")
    })
    .await;
    assert_eq!(
        (m.etag.as_deref(), m.last_modified.as_deref()),
        (None, None)
    );
}

async fn start_redirects_and_gates(t: T) {
    let lab = Lab::new(t).await;
    let m = start_like_probe(&lab, "redirect-chain", "1m.bin", || {
        lab.source("redirect-chain", "1m.bin")
    })
    .await;
    assert_eq!(
        m.final_url.as_deref(),
        Some(lab.url("redirect-chain", "_hop/3/1m.bin").as_str())
    );
    let cookie = RequestContext {
        cookie: Some("whoosh_session=lab-ok".into()),
        ..Default::default()
    };
    start_like_probe(&lab, "cookie-required", "1m.bin", || {
        lab.source_with("cookie-required", "1m.bin", &cookie)
    })
    .await;
    let referer = RequestContext {
        referer: Some("https://downloads.example.test/page".into()),
        ..Default::default()
    };
    start_like_probe(&lab, "referer-required", "1m.bin", || {
        lab.source_with("referer-required", "1m.bin", &referer)
    })
    .await;
    for sc in ["cookie-required", "referer-required"] {
        let e = lab.source(sc, "1m.bin").start().await.err().unwrap();
        assert!(matches!(e, SourceError::LinkExpired(_)), "{sc}: {e:?}");
    }
}

async fn start_expiring_url(t: T) {
    let lab = Lab::new(t).await;
    let path = lab
        .lab
        .mint_path("expiring-url", "1m.bin", Some(Duration::from_secs(3)))
        .unwrap();
    let url = lab.origin_url(&path);
    start_like_probe(&lab, "expiring-url", "1m.bin", || {
        lab.factory.source(&url, None).unwrap()
    })
    .await;
    sleep_past_expiry(&path).await;
    let e = lab
        .factory
        .source(&url, None)
        .unwrap()
        .start()
        .await
        .err()
        .unwrap();
    assert!(matches!(e, SourceError::LinkExpired(_)), "{e:?}");
}

/// The file changes after 32 MiB served: the `start` response keeps the
/// version it began with, and later requests with its validators fail.
async fn start_changing_file(t: T) {
    let (sc, f) = ("changing-file", "100m.bin");
    let lab = Lab::new(t).await;
    let src = lab.source(sc, f);
    let (m, mut s) = src.start().await.unwrap();
    assert_eq!(m.etag, lab.lab.etag(sc, f, 0));
    let mut got = Vec::new();
    while (got.len() as u64) < 40 * MIB {
        got.extend_from_slice(&s.next().await.unwrap().unwrap());
    }
    drop(s);
    lab.verify(sc, f, 0, &got); // version 0 throughout
    assert_eq!(lab.lab.current_version(sc, f), Some(1));
    let e = open_err(&*src, Some(r(50 * MIB, 51 * MIB)), m.validators()).await;
    assert_eq!(e, SourceError::RemoteChanged);
}

async fn start_empty_and_names(t: T) {
    let lab = Lab::new(t).await;
    let m = start_like_probe(&lab, "clean", "empty.bin", || {
        lab.source("clean", "empty.bin")
    })
    .await;
    assert_eq!((m.size, m.accepts_ranges), (Some(0), false));
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
        let m = start_like_probe(&lab, "bangla-filename", file, || {
            lab.source("bangla-filename", file)
        })
        .await;
        assert_eq!(m.filename.as_deref(), Some(want), "{file}");
    }
}

/// The engine drops the `start` stream where its range ends: the request
/// ends on the server promptly, and the source keeps working.
async fn start_cancel(t: T) {
    let lab = Lab::new(t).await;
    let src = lab.source("clean", "1g.bin");
    let (m, mut s) = src.start().await.unwrap();
    let first = s.next().await.unwrap().unwrap();
    lab.verify("clean", "1g.bin", 0, &first);
    drop(s);
    let gone = eventually(Duration::from_secs(5), || async {
        lab.lab.stats("clean").unwrap().active_requests == 0
    })
    .await;
    assert!(gone, "still active: {:?}", lab.lab.stats("clean"));
    assert!(lab.lab.stats("clean").unwrap().bytes_sent < 512 * MIB);
    // The next request works (no connection with an unread body reused).
    let b = fetch(&*src, Some(r(5 * MIB, 6 * MIB)), m.validators()).await;
    lab.verify("clean", "1g.bin", 5 * MIB, &b);
}

/// 500 ms before every response head: `start` needs one response where
/// `probe` + `open` need two. The request count is the deterministic
/// witness; the times are printed for the record and checked only as lower
/// bounds (load can only add to them).
async fn start_saves_a_round_trip(t: T) {
    let lab = Lab::new(t).await;
    // Warm the connection pool so both sides pay the same connection costs.
    lab.source("clean", "1m.bin").probe().await.unwrap();
    let requests = || lab.lab.stats("high-latency").unwrap().requests;
    let t0 = Instant::now();
    let src = lab.source("high-latency", "1m.bin");
    let m = src.probe().await.unwrap();
    let mut s = src.open(open_req(None, m.validators())).await.unwrap();
    s.next().await.unwrap().unwrap();
    let two = t0.elapsed();
    drop(s);
    assert_eq!(requests(), 2);
    let t0 = Instant::now();
    let (_, mut s) = lab.source("high-latency", "1m.bin").start().await.unwrap();
    s.next().await.unwrap().unwrap();
    let one = t0.elapsed();
    assert_eq!(requests(), 3, "start is one request");
    eprintln!("{t:?}: first byte after probe+open {two:?}, after start {one:?}");
    assert!(two >= Duration::from_millis(990), "{two:?}");
    assert!(one >= Duration::from_millis(490), "{one:?}");
}
