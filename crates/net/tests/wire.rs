//! What goes on the wire and how every kind of response is classified,
//! against a raw HTTP/1.1 fixture server that records each request.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use support::*;
use vayu_core::{SourceError, SourceFactory, Validators};
use vayu_proto::{Header, RequestContext};

const BODY: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!?";

fn v(etag: Option<&str>, lm: Option<&str>, size: Option<u64>) -> Validators {
    Validators {
        etag: etag.map(Into::into),
        last_modified: lm.map(Into::into),
        size,
    }
}

/// Parses `bytes=a-b` from a request.
fn range_of(s: &Seen) -> Option<(usize, usize)> {
    let (a, b) = s.header("range")?.strip_prefix("bytes=")?.split_once('-')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// A well-behaved server for BODY with a strong ETag.
fn honest(s: &Seen) -> Reply {
    let total = BODY.len();
    match range_of(s) {
        Some((a, b)) => Reply::new(206)
            .header("Content-Range", &format!("bytes {a}-{b}/{total}"))
            .header("ETag", "\"v1\"")
            .header("Last-Modified", "Tue, 01 Sep 2026 00:00:00 GMT")
            .body(&BODY[a..=b]),
        None => Reply::new(200)
            .header("ETag", "\"v1\"")
            .header("Last-Modified", "Tue, 01 Sep 2026 00:00:00 GMT")
            .body(BODY),
    }
}

fn hdr(name: &str, value: &str) -> Header {
    Header {
        name: name.into(),
        value: value.into(),
    }
}

// ------------------------------------------------------------- requests

#[tokio::test]
async fn request_is_built_from_the_context() {
    let fx = Fixture::start(honest).await;
    let f = plain_factory(|_| {});
    let ctx = RequestContext {
        method: None,
        headers: vec![
            hdr("Connection", "keep-alive, X-Hop"),
            hdr("X-Hop", "1"),
            hdr("Keep-Alive", "timeout=5"),
            hdr("Transfer-Encoding", "chunked"),
            hdr("TE", "trailers"),
            hdr("Upgrade", "websocket"),
            hdr("Proxy-Authorization", "Basic eA=="),
            hdr("Host", "evil.example"),
            hdr("Content-Length", "123"),
            hdr("Range", "bytes=50-"),
            hdr("If-None-Match", "\"old\""),
            hdr("Accept-Encoding", "gzip, deflate, br"),
            hdr("Accept-Language", "bn-BD,bn;q=0.9"),
            hdr("Sec-Fetch-Mode", "navigate"),
        ],
        cookie: Some("sid=abc".into()),
        referer: Some("https://site.example/page".into()),
        user_agent: Some("Mozilla/5.0 Test".into()),
        body_base64: None,
    };
    let src = f.source(&fx.url("/file.bin?x=1#frag"), Some(&ctx)).unwrap();
    let m = src.probe().await.unwrap();
    assert_eq!(m.size, Some(BODY.len() as u64));
    let s = fx.last();
    assert_eq!(s.method, "GET");
    assert_eq!(s.target, "/file.bin?x=1", "no fragment on the wire");
    assert_eq!(s.count("host"), 1);
    assert_eq!(s.header("host").unwrap(), fx.addr.to_string());
    assert_eq!(s.header("range"), Some("bytes=0-0"));
    assert_eq!(s.header("accept-encoding"), Some("identity"));
    assert_eq!(s.header("cookie"), Some("sid=abc"));
    assert_eq!(s.header("referer"), Some("https://site.example/page"));
    assert_eq!(s.header("user-agent"), Some("Mozilla/5.0 Test"));
    assert_eq!(s.header("accept-language"), Some("bn-BD,bn;q=0.9"));
    assert_eq!(s.header("sec-fetch-mode"), Some("navigate"));
    for gone in [
        "x-hop",
        "keep-alive",
        "transfer-encoding",
        "te",
        "upgrade",
        "proxy-authorization",
        "content-length",
        "if-none-match",
    ] {
        assert_eq!(s.header(gone), None, "{gone} was forwarded");
    }
}

#[tokio::test]
async fn default_user_agent_and_identity_on_every_request() {
    let fx = Fixture::start(honest).await;
    let f = plain_factory(|_| {});
    let src = f.source(&fx.url("/f"), None).unwrap();
    let m = src.probe().await.unwrap();
    fetch(&*src, Some(r(3, 9)), m.validators()).await;
    fetch(&*src, None, m.validators()).await;
    for s in fx.requests() {
        assert_eq!(s.header("user-agent"), Some(vayu_net::DEFAULT_USER_AGENT));
        assert_eq!(s.header("accept-encoding"), Some("identity"));
        assert_eq!(s.header("accept"), Some("*/*"));
    }
}

#[tokio::test]
async fn post_with_body() {
    let fx = Fixture::start(honest).await;
    let f = plain_factory(|_| {});
    let ctx = RequestContext {
        method: Some("POST".into()),
        headers: vec![hdr("Content-Type", "application/x-www-form-urlencoded")],
        body_base64: Some("YT0xJmI9Mg==".into()), // a=1&b=2
        ..Default::default()
    };
    let src = f.source(&fx.url("/export"), Some(&ctx)).unwrap();
    let m = src.probe().await.unwrap();
    fetch(&*src, Some(r(0, 10)), m.validators()).await;
    for s in fx.requests() {
        assert_eq!(s.method, "POST");
        assert_eq!(s.body, b"a=1&b=2");
        assert_eq!(s.header("content-length"), Some("7"));
        assert_eq!(
            s.header("content-type"),
            Some("application/x-www-form-urlencoded")
        );
    }
}

#[tokio::test]
async fn url_credentials_become_basic_auth() {
    let fx = Fixture::start(honest).await;
    let f = plain_factory(|_| {});
    let url = format!("http://user:p%40ss@{}/f", fx.addr);
    f.source(&url, None).unwrap().probe().await.unwrap();
    let s = fx.last();
    assert_eq!(s.header("authorization"), Some("Basic dXNlcjpwQHNz"));
    assert_eq!(s.target, "/f");
}

// ------------------------------------------------- contract points 2, 3

#[tokio::test]
async fn if_range_only_with_a_strong_etag() {
    let fx = Fixture::start(honest).await;
    let src = plain_factory(|_| {}).source(&fx.url("/f"), None).unwrap();
    let b = fetch(&*src, Some(r(10, 20)), v(Some("\"v1\""), None, None)).await;
    assert_eq!(b, &BODY[10..20]);
    let s = fx.last();
    assert_eq!(s.header("range"), Some("bytes=10-19"));
    assert_eq!(s.header("if-range"), Some("\"v1\""));
    // A weak ETag is compared but never sent as If-Range.
    let e = open_err(&*src, Some(r(10, 20)), v(Some("W/\"v1\""), None, None)).await;
    assert_eq!(e, SourceError::RemoteChanged, "W/\"v1\" != \"v1\"");
    assert_eq!(fx.last().header("if-range"), None);
    // Last-Modified alone: no If-Range either.
    fetch(
        &*src,
        Some(r(0, 5)),
        v(None, Some("Tue, 01 Sep 2026 00:00:00 GMT"), Some(64)),
    )
    .await;
    assert_eq!(fx.last().header("if-range"), None);
}

#[tokio::test]
async fn wrong_content_range_is_ranges_unsupported() {
    let fx = Fixture::start(|s: &Seen| match range_of(s) {
        Some((a, b)) => Reply::new(206)
            .header("Content-Range", &format!("bytes {}-{}/64", a + 1, b + 1))
            .body(&BODY[a + 1..=b + 1]),
        None => Reply::new(200).body(BODY),
    })
    .await;
    let src = plain_factory(|_| {}).source(&fx.url("/f"), None).unwrap();
    let e = open_err(&*src, Some(r(10, 20)), Validators::default()).await;
    assert_eq!(e, SourceError::RangesUnsupported);
}

#[tokio::test]
async fn missing_content_range_is_ranges_unsupported() {
    let fx = Fixture::start(|_s: &Seen| Reply::new(206).body(&BODY[..10])).await;
    let src = plain_factory(|_| {}).source(&fx.url("/f"), None).unwrap();
    let e = open_err(&*src, Some(r(0, 10)), Validators::default()).await;
    assert_eq!(e, SourceError::RangesUnsupported);
}

#[tokio::test]
async fn a_long_body_is_cut_at_the_end_of_the_range() {
    // Content-Range says 10 bytes, the body has 20.
    let fx = Fixture::start(|_s: &Seen| {
        Reply::new(206)
            .header("Content-Range", "bytes 0-9/64")
            .body(&BODY[..20])
    })
    .await;
    let src = plain_factory(|_| {}).source(&fx.url("/f"), None).unwrap();
    let b = fetch(&*src, Some(r(0, 10)), Validators::default()).await;
    assert_eq!(b, &BODY[..10]);
}

#[tokio::test]
async fn a_short_body_is_transient() {
    let fx = Fixture::start(|_s: &Seen| {
        Reply::new(206)
            .header("Content-Range", "bytes 0-19/64")
            .length(Some(20))
            .body(&BODY[..12])
            .close()
    })
    .await;
    let src = plain_factory(|_| {}).source(&fx.url("/f"), None).unwrap();
    let s = src
        .open(open_req(Some(r(0, 20)), Validators::default()))
        .await
        .unwrap();
    let (b, e) = read_all(s).await;
    assert_eq!(b, &BODY[..12], "the bytes that came are correct");
    assert!(matches!(e, Some(SourceError::Transient(_))), "{e:?}");
}

#[tokio::test]
async fn short_whole_file_is_transient_when_the_size_is_known() {
    // No Content-Length, connection closes after 30 bytes; the probe said 64.
    let fx = Fixture::start(|_s: &Seen| Reply::new(200).length(None).body(&BODY[..30])).await;
    let src = plain_factory(|_| {}).source(&fx.url("/f"), None).unwrap();
    let s = src
        .open(open_req(None, v(None, None, Some(64))))
        .await
        .unwrap();
    let (b, e) = read_all(s).await;
    assert_eq!(b.len(), 30);
    assert!(
        matches!(&e, Some(SourceError::Transient(m)) if m.contains("34 bytes early")),
        "{e:?}"
    );
    // Size unknown: read until the server closes.
    let s = src
        .open(open_req(None, Validators::default()))
        .await
        .unwrap();
    let (b, e) = read_all(s).await;
    assert_eq!((b.len(), e), (30, None));
}

#[tokio::test]
async fn changed_validators_are_remote_changed_before_any_byte() {
    let fx = Fixture::start(honest).await;
    let src = plain_factory(|_| {}).source(&fx.url("/f"), None).unwrap();
    for expect in [
        v(None, Some("Mon, 31 Aug 2026 00:00:00 GMT"), None),
        v(None, None, Some(65)), // Content-Range total is 64
        v(Some("\"v0\""), None, None),
    ] {
        let e = open_err(&*src, Some(r(0, 10)), expect.clone()).await;
        assert_eq!(e, SourceError::RemoteChanged, "{expect:?}");
        let e = open_err(&*src, None, expect.clone()).await;
        assert_eq!(e, SourceError::RemoteChanged, "unranged {expect:?}");
    }
}

#[tokio::test]
async fn ok_to_a_ranged_request_tells_ignored_ranges_from_a_new_version() {
    let etag = Arc::new(std::sync::Mutex::new("\"v1\"".to_string()));
    let e2 = etag.clone();
    let fx = Fixture::start(move |_s: &Seen| {
        Reply::new(200)
            .header("ETag", &e2.lock().unwrap())
            .body(BODY)
    })
    .await;
    let src = plain_factory(|_| {}).source(&fx.url("/f"), None).unwrap();
    let expect = v(Some("\"v1\""), None, Some(64));
    assert_eq!(
        open_err(&*src, Some(r(5, 10)), expect.clone()).await,
        SourceError::RangesUnsupported
    );
    *etag.lock().unwrap() = "\"v2\"".into();
    assert_eq!(
        open_err(&*src, Some(r(5, 10)), expect).await,
        SourceError::RemoteChanged
    );
}

#[tokio::test]
async fn a_clamped_range_of_a_shorter_file_is_remote_changed() {
    // The file shrank to 62 bytes: the server clamps 60-69 to 60-61.
    let fx = Fixture::start(|_s: &Seen| {
        Reply::new(206)
            .header("Content-Range", "bytes 60-61/62")
            .body(&BODY[60..62])
    })
    .await;
    let src = plain_factory(|_| {}).source(&fx.url("/f"), None).unwrap();
    let e = open_err(&*src, Some(r(60, 64)), v(None, None, Some(64))).await;
    assert_eq!(e, SourceError::RemoteChanged);
    // Same size, so the same version: just a range the server did not honour.
    let e = open_err(&*src, Some(r(60, 64)), v(None, None, Some(62))).await;
    assert_eq!(e, SourceError::RangesUnsupported);
}

#[tokio::test]
async fn an_empty_body_ends_without_chunks() {
    let fx = Fixture::start(|_s: &Seen| Reply::new(200).header("ETag", "\"e\"")).await;
    let src = plain_factory(|_| {})
        .source(&fx.url("/empty"), None)
        .unwrap();
    let mut s = src
        .open(open_req(None, Validators::default()))
        .await
        .unwrap();
    use futures::StreamExt;
    assert!(s.next().await.is_none());
}

/// Hostile `Content-Range` values: no arithmetic overflow (debug builds
/// panic on one), just `RangesUnsupported`.
#[tokio::test]
async fn extreme_content_range_is_ranges_unsupported_not_a_panic() {
    let fx = Fixture::start(|_s: &Seen| {
        Reply::new(206)
            .header("Content-Range", "bytes 0-18446744073709551615/*")
            .body(&BODY[..10])
    })
    .await;
    let f = plain_factory(|_| {});
    let src = f.source(&fx.url("/ranged"), None).unwrap();
    let e = open_err(&*src, Some(r(0, 10)), Validators::default()).await;
    assert_eq!(e, SourceError::RangesUnsupported);
    let src = f.source(&fx.url("/unranged"), None).unwrap();
    let e = open_err(&*src, None, Validators::default()).await;
    assert_eq!(e, SourceError::RangesUnsupported);
}

/// A remote `Retry-After` is capped (an engine computes `now + it`).
#[tokio::test]
async fn huge_retry_after_is_capped() {
    let fx =
        Fixture::start(|_s: &Seen| Reply::new(429).header("Retry-After", "18446744073709551615"))
            .await;
    let e = plain_factory(|_| {})
        .source(&fx.url("/"), None)
        .unwrap()
        .probe()
        .await
        .unwrap_err();
    match e {
        SourceError::Throttled {
            retry_after: Some(d),
            ..
        } => {
            assert_eq!(d, Duration::from_secs(3600));
            let _ = Instant::now() + d;
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn unsatisfiable_range_is_remote_changed() {
    let fx =
        Fixture::start(|_s: &Seen| Reply::new(416).header("Content-Range", "bytes */32")).await;
    let src = plain_factory(|_| {}).source(&fx.url("/f"), None).unwrap();
    assert_eq!(
        open_err(&*src, Some(r(40, 50)), Validators::default()).await,
        SourceError::RemoteChanged
    );
}

// --------------------------------------------------------- classification

#[tokio::test]
async fn statuses_are_classified() {
    let later = httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(30));
    let fx = Fixture::start(move |s: &Seen| {
        let code: u16 = s.target.trim_start_matches('/').parse().unwrap();
        let mut r = Reply::new(code).body("nope");
        if code == 503 {
            r = r.header("Retry-After", &later);
        }
        if code == 429 {
            r = r.header("Retry-After", "7");
        }
        r
    })
    .await;
    let f = plain_factory(|_| {});
    let probe = |code: u16| {
        let src = f.source(&fx.url(&format!("/{code}")), None).unwrap();
        async move { src.probe().await.unwrap_err() }
    };
    for code in [401, 403, 410] {
        assert!(
            matches!(probe(code).await, SourceError::LinkExpired(_)),
            "{code}"
        );
    }
    for code in [400, 404, 405, 451] {
        assert!(matches!(probe(code).await, SourceError::Fatal(_)), "{code}");
    }
    for code in [408, 500, 502, 504] {
        assert!(
            matches!(probe(code).await, SourceError::Transient(_)),
            "{code}"
        );
    }
    match probe(429).await {
        SourceError::Throttled { retry_after, .. } => {
            assert_eq!(retry_after, Some(Duration::from_secs(7)))
        }
        e => panic!("{e:?}"),
    }
    match probe(503).await {
        SourceError::Throttled {
            retry_after: Some(d),
            ..
        } => assert!(
            d > Duration::from_secs(25) && d <= Duration::from_secs(30),
            "{d:?}"
        ),
        e => panic!("{e:?}"),
    }
    // Error bodies are read, so the one connection is reused throughout.
    assert_eq!(fx.accepts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn refused_connections_are_throttled_and_dns_failures_transient() {
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let f = plain_factory(|_| {});
    let e = f
        .source(&format!("http://{closed}/f"), None)
        .unwrap()
        .probe()
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            SourceError::Throttled {
                retry_after: None,
                ..
            }
        ),
        "{e:?}"
    );
    let e = f
        .source("http://no-such-host.invalid/f", None)
        .unwrap()
        .probe()
        .await
        .unwrap_err();
    assert!(matches!(e, SourceError::Transient(_)), "{e:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn untrusted_certificate_is_fatal() {
    let lab = vayu_labserver::LabServer::start(vayu_labserver::LabConfig::default())
        .await
        .unwrap();
    // OS roots only: the lab's self-signed certificate is unknown.
    let f = vayu_net::HttpSourceFactory::new(vayu_net::NetConfig::default());
    let url = lab.https_url("clean", "1m.bin").unwrap();
    let e = f.source(&url, None).unwrap().probe().await.unwrap_err();
    assert!(
        matches!(&e, SourceError::Fatal(m) if m.contains("certificate") || m.contains("Certificate")),
        "{e:?}"
    );
}

#[tokio::test]
async fn bad_urls_are_fatal_and_source_does_no_io() {
    let f = plain_factory(|_| {});
    for bad in [
        "not a url",
        "ftp://example.com/f",
        "file:///etc/passwd",
        "http://",
    ] {
        assert!(
            matches!(f.source(bad, None), Err(SourceError::Fatal(_))),
            "{bad}"
        );
    }
    let bad_body = RequestContext {
        body_base64: Some("***".into()),
        ..Default::default()
    };
    assert!(matches!(
        f.source("http://example.com/", Some(&bad_body)),
        Err(SourceError::Fatal(_))
    ));
    let t0 = Instant::now();
    let _ = f.source("http://10.255.255.1/unroutable", None).unwrap();
    assert!(t0.elapsed() < Duration::from_millis(50));
    // Preconnect never fails, even for nonsense.
    f.preconnect("not a url").await;
    f.preconnect("ftp://example.com/").await;
}

// -------------------------------------------------------------- redirects

#[tokio::test]
async fn cross_origin_redirect_drops_cookie_and_authorization() {
    let target = Fixture::start(honest).await;
    let to = target.url("/file");
    let start = Fixture::start(move |s: &Seen| match s.target.as_str() {
        "/away" => Reply::new(302).header("Location", &to),
        "/here" => Reply::new(307).header("Location", "/file"),
        _ => honest(s),
    })
    .await;
    let f = plain_factory(|_| {});
    let ctx = RequestContext {
        cookie: Some("sid=secret".into()),
        headers: vec![hdr("Authorization", "Bearer token"), hdr("X-Other", "kept")],
        ..Default::default()
    };
    // Same origin: credentials stay.
    let m = f
        .source(&start.url("/here"), Some(&ctx))
        .unwrap()
        .probe()
        .await
        .unwrap();
    assert_eq!(m.final_url.as_deref(), Some(start.url("/file").as_str()));
    let last = start.last();
    assert_eq!(
        (last.target.as_str(), last.header("cookie")),
        ("/file", Some("sid=secret"))
    );
    assert_eq!(last.header("authorization"), Some("Bearer token"));
    // Another origin (another port): dropped there, and for the rest of
    // the source's life.
    let src = f.source(&start.url("/away"), Some(&ctx)).unwrap();
    let m = src.probe().await.unwrap();
    assert_eq!(m.final_url.as_deref(), Some(target.url("/file").as_str()));
    assert_eq!(start.last().header("cookie"), Some("sid=secret"));
    fetch(&*src, Some(r(0, 8)), m.validators()).await;
    let seen = target.requests();
    assert_eq!(seen.len(), 2);
    for s in seen {
        assert_eq!(s.header("cookie"), None);
        assert_eq!(s.header("authorization"), None);
        assert_eq!(s.header("x-other"), Some("kept"));
    }
}

/// The browser's `Referer` reaches another origin only as the page's
/// origin, and not at all after a downgrade to `http`.
#[tokio::test]
async fn cross_origin_redirect_trims_the_referer() {
    let target = Fixture::start(honest).await;
    let to = target.url("/file");
    let start = Fixture::start(move |_s: &Seen| Reply::new(302).header("Location", &to)).await;
    let f = plain_factory(|_| {});
    for (page, want) in [
        (
            "http://site.example/dl/page?token=secret",
            Some("http://site.example/"),
        ),
        ("https://site.example/dl/page?token=secret", None),
    ] {
        let ctx = RequestContext {
            referer: Some(page.into()),
            ..Default::default()
        };
        f.source(&start.url("/away"), Some(&ctx))
            .unwrap()
            .probe()
            .await
            .unwrap();
        assert_eq!(
            start.last().header("referer"),
            Some(page),
            "first hop as the browser sent it"
        );
        assert_eq!(target.last().header("referer"), want, "{page}");
    }
}

#[tokio::test]
async fn redirect_methods_follow_browsers() {
    let fx = Fixture::start(|s: &Seen| match s.target.as_str() {
        "/302" => Reply::new(302).header("Location", "/file"),
        "/303" => Reply::new(303).header("Location", "/file"),
        "/307" => Reply::new(307).header("Location", "/file"),
        "/308" => Reply::new(308).header("Location", "/file"),
        _ => honest(s),
    })
    .await;
    let f = plain_factory(|_| {});
    let ctx = RequestContext {
        method: Some("POST".into()),
        body_base64: Some("eA==".into()),
        headers: vec![hdr("Content-Type", "text/plain")],
        ..Default::default()
    };
    for (path, method, body) in [
        ("/302", "GET", &b""[..]),
        ("/303", "GET", b""),
        ("/307", "POST", b"x"),
        ("/308", "POST", b"x"),
    ] {
        f.source(&fx.url(path), Some(&ctx))
            .unwrap()
            .probe()
            .await
            .unwrap();
        let s = fx.last();
        assert_eq!((s.method.as_str(), &s.body[..]), (method, body), "{path}");
        assert_eq!(
            s.header("content-type").is_some(),
            method == "POST",
            "{path}"
        );
    }
}

#[tokio::test]
async fn at_most_ten_redirects() {
    let fx = Fixture::start(|s: &Seen| {
        let n: i32 = s.target.trim_start_matches("/r").parse().unwrap();
        if n == 10 {
            honest(s)
        } else {
            Reply::new(302).header("Location", &format!("/r{}", n + 1))
        }
    })
    .await;
    let f = plain_factory(|_| {});
    let m = f
        .source(&fx.url("/r0"), None)
        .unwrap()
        .probe()
        .await
        .unwrap();
    assert_eq!(m.final_url.as_deref(), Some(fx.url("/r10").as_str()));
    let e = f
        .source(&fx.url("/r-1"), None)
        .unwrap()
        .probe()
        .await
        .unwrap_err();
    assert!(
        matches!(&e, SourceError::Fatal(m) if m.contains("redirects")),
        "{e:?}"
    );
    let e = plain_factory(|c| c.max_redirects = 0)
        .source(&fx.url("/r9"), None)
        .unwrap()
        .probe()
        .await
        .unwrap_err();
    assert!(matches!(e, SourceError::Fatal(_)), "{e:?}");
}

#[tokio::test]
async fn redirect_to_another_scheme_is_fatal() {
    let fx =
        Fixture::start(|_s: &Seen| Reply::new(302).header("Location", "ftp://example.com/f")).await;
    let e = plain_factory(|_| {})
        .source(&fx.url("/"), None)
        .unwrap()
        .probe()
        .await
        .unwrap_err();
    assert!(matches!(e, SourceError::Fatal(_)), "{e:?}");
}

/// The end of a redirect chain is reused; when it stops working (a signed
/// CDN URL expired), the chain is followed again from the original URL.
#[tokio::test]
async fn expired_redirect_target_is_followed_again() {
    let version = Arc::new(AtomicUsize::new(1));
    let uses = Arc::new(AtomicUsize::new(0));
    let (v2, u2) = (version.clone(), uses.clone());
    let fx = Fixture::start(move |s: &Seen| {
        if s.target == "/dl" {
            let n = v2.load(Ordering::SeqCst);
            return Reply::new(302).header("Location", &format!("/signed?v={n}"));
        }
        let q = query(&s.target);
        let n: usize = q["v"].parse().unwrap();
        // Each signed URL works twice, then expires.
        if n != v2.load(Ordering::SeqCst) || u2.fetch_add(1, Ordering::SeqCst) >= 2 {
            v2.store(n + 1, Ordering::SeqCst);
            u2.store(0, Ordering::SeqCst);
            return Reply::new(403);
        }
        honest(s)
    })
    .await;
    let src = plain_factory(|_| {}).source(&fx.url("/dl"), None).unwrap();
    let m = src.probe().await.unwrap();
    for i in 0..4 {
        let b = fetch(&*src, Some(r(i, i + 4)), m.validators()).await;
        assert_eq!(b, &BODY[i as usize..i as usize + 4]);
    }
    let targets: Vec<String> = fx.requests().into_iter().map(|s| s.target).collect();
    assert_eq!(
        targets,
        [
            "/dl",
            "/signed?v=1", // probe
            "/signed?v=1", // open 1 (cached target)
            "/signed?v=1",
            "/dl",
            "/signed?v=2", // open 2: expired, followed again
            "/signed?v=2", // open 3
            "/signed?v=2",
            "/dl",
            "/signed?v=3", // open 4
        ]
    );
}

// ------------------------------------------------------------- pooling

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keep_alive_and_preconnect() {
    let fx = Fixture::start(honest).await;
    let f = plain_factory(|_| {});
    f.preconnect(&fx.url("/f")).await;
    assert!(
        eventually(Duration::from_secs(2), || async {
            fx.accepts.load(Ordering::SeqCst) == 1
        })
        .await
    );
    f.preconnect(&fx.url("/f")).await; // already warm: nothing new
    let src = f.source(&fx.url("/f"), None).unwrap();
    let m = src.probe().await.unwrap();
    for i in 0..5 {
        fetch(&*src, Some(r(i, i + 10)), m.validators()).await;
    }
    fetch(&*src, None, m.validators()).await;
    assert_eq!(fx.accepts.load(Ordering::SeqCst), 1);
    assert!(fx.requests().iter().all(|s| s.conn == 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_connections_expire() {
    let fx = Fixture::start(honest).await;
    let f = plain_factory(|c| c.pool_idle_timeout = Duration::from_millis(200));
    let src = f.source(&fx.url("/f"), None).unwrap();
    let m = src.probe().await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    fetch(&*src, Some(r(0, 4)), m.validators()).await;
    assert_eq!(fx.accepts.load(Ordering::SeqCst), 2);
}

/// A pooled connection the server closed while idle is replaced
/// transparently: the request never left, so this is not a retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_closed_idle_connection_is_replaced() {
    let fx = Fixture::start(|s: &Seen| honest(s).silent_close()).await;
    let src = plain_factory(|_| {}).source(&fx.url("/f"), None).unwrap();
    let m = src.probe().await.unwrap();
    for i in 0..3 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        fetch(&*src, Some(r(i, i + 4)), m.validators()).await;
    }
    assert_eq!(fx.requests().len(), 4, "each request sent exactly once");
    assert_eq!(fx.accepts.load(Ordering::SeqCst), 4);
}
