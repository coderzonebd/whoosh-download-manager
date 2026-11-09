//! Integration tests: a real server on an ephemeral port, real sockets.

mod support;

use http::{Method, StatusCode, Version};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use support::{Client, eventually, get, is_reset, lab, pct_decode};
use vayu_labserver::content::hex;
use vayu_labserver::{ByteRange, LabConfig, LabServer};

const MIB: u64 = 1 << 20;

fn r(start: u64, end: u64) -> ByteRange {
    ByteRange::new(start, end).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_body_and_single_ranges_are_byte_exact() {
    let lab = lab(&[("clean", "")]).await;
    let addr = lab.http_addr();
    let f = lab.file("clean", "1m.bin").unwrap();
    let mut c = Client::h1(addr).await;

    let full = c.get("/s/clean/1m.bin", &[]).await;
    assert_eq!(full.status, StatusCode::OK);
    assert!(full.error.is_none());
    assert_eq!(full.header("content-length"), Some("1048576"));
    assert_eq!(full.header("accept-ranges"), Some("bytes"));
    assert_eq!(
        full.header("content-type"),
        Some("application/octet-stream")
    );
    assert!(full.header("etag").is_some() && full.header("last-modified").is_some());
    assert_eq!(full.body.len() as u64, MIB);
    assert_eq!(f.verify(0, &full.body), Ok(()));

    let size = MIB;
    for (spec, want) in [
        ("bytes=0-0", r(0, 1)),
        ("bytes=65530-65545", r(65530, 65546)), // crosses a 64 KiB block
        ("bytes=1000-", r(1000, size)),
        ("bytes=-500", r(size - 500, size)),
        ("bytes=0-99999999", r(0, size)),
        ("bytes=1048575-1048575", r(size - 1, size)),
    ] {
        let p = c.get("/s/clean/1m.bin", &[("range", spec)]).await;
        assert_eq!(p.status, StatusCode::PARTIAL_CONTENT, "{spec}");
        assert_eq!(
            p.header("content-range").unwrap(),
            format!("bytes {}-{}/{size}", want.start, want.end - 1),
            "{spec}"
        );
        assert_eq!(p.header("content-length").unwrap(), want.len().to_string());
        assert_eq!(p.body, f.bytes(want), "{spec}");
    }

    let bad = c
        .get("/s/clean/1m.bin", &[("range", "bytes=1048576-")])
        .await;
    assert_eq!(bad.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(bad.header("content-range"), Some("bytes */1048576"));
    assert!(bad.body.is_empty());

    let multi = c
        .get("/s/clean/1m.bin", &[("range", "bytes=0-1,5-6")])
        .await;
    assert_eq!(multi.status, StatusCode::OK, "multi-range is ignored");
    assert_eq!(multi.body.len() as u64, MIB);

    let head = c.head("/s/clean/1m.bin").await;
    assert_eq!(head.status, StatusCode::OK);
    assert_eq!(head.header("content-length"), Some("1048576"));
    assert!(head.body.is_empty());

    let empty = c.get("/s/clean/empty.bin", &[]).await;
    assert_eq!((empty.status, empty.body.len()), (StatusCode::OK, 0));
    assert_eq!(empty.header("content-length"), Some("0"));
    let empty_range = c.get("/s/clean/empty.bin", &[("range", "bytes=0-")]).await;
    assert_eq!(empty_range.status, StatusCode::RANGE_NOT_SATISFIABLE);

    assert_eq!(
        c.get("/s/clean/nope.bin", &[]).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.get("/s/nope/1m.bin", &[]).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.request(Method::POST, "/s/clean/1m.bin", &[]).await.status,
        StatusCode::METHOD_NOT_ALLOWED
    );

    // Same name, same bytes, in any scenario; and the library agrees.
    assert_eq!(f, vayu_labserver::VirtualFile::named("1m.bin", MIB));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sha256_sidecar_and_digest_endpoint_match_content() {
    let lab = lab(&[("clean", "")]).await;
    let addr = lab.http_addr();
    let body = get(addr, "/s/clean/10m.bin", &[]).await.body;
    let want = hex(&Sha256::digest(&body));

    let side = get(addr, "/s/clean/10m.bin.sha256", &[]).await;
    assert_eq!(side.status, StatusCode::OK);
    assert_eq!(
        String::from_utf8(side.body).unwrap(),
        format!("{want}  10m.bin\n")
    );

    let f = lab.file("clean", "10m.bin").unwrap();
    assert_eq!(f.sha256_hex(f.full()), want);

    let d = get(
        addr,
        "/_lab/digest?scenario=clean&file=10m.bin&start=5&end=70000",
        &[],
    )
    .await;
    let v: serde_json::Value = serde_json::from_slice(&d.body).unwrap();
    assert_eq!(
        v["sha256"].as_str().unwrap(),
        hex(&Sha256::digest(&body[5..70000]))
    );
    assert_eq!(v["version"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn range_modes_ignore_advertise_and_chunked() {
    let lab = lab(&[
        (
            "ign",
            "[ranges]\nmode = \"ignore\"\n[[files]]\nname = \"f\"\nsize = 300000",
        ),
        (
            "lie",
            "[ranges]\nmode = \"advertise-ignore\"\n[[files]]\nname = \"f\"\nsize = 300000",
        ),
        (
            "chunk",
            "[ranges]\nchunked = true\n[[files]]\nname = \"f\"\nsize = 300000",
        ),
    ])
    .await;
    let addr = lab.http_addr();
    let f = lab.file("ign", "f").unwrap();

    let ign = get(addr, "/s/ign/f", &[("range", "bytes=10-20")]).await;
    assert_eq!(ign.status, StatusCode::OK);
    assert_eq!(ign.header("accept-ranges"), None);
    assert_eq!(ign.body.len(), 300_000);
    assert_eq!(f.verify(0, &ign.body), Ok(()));

    let lie = get(addr, "/s/lie/f", &[("range", "bytes=10-20")]).await;
    assert_eq!(lie.status, StatusCode::OK);
    assert_eq!(lie.header("accept-ranges"), Some("bytes"));
    assert_eq!(lie.body.len(), 300_000);

    let ch = get(addr, "/s/chunk/f", &[]).await;
    assert_eq!(ch.status, StatusCode::OK);
    assert_eq!(ch.header("content-length"), None);
    assert_eq!(ch.header("transfer-encoding"), Some("chunked"));
    assert_eq!(f.verify(0, &ch.body), Ok(()));
    assert_eq!(ch.body.len(), 300_000);

    let chr = get(addr, "/s/chunk/f", &[("range", "bytes=100-")]).await;
    assert_eq!(chr.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(chr.header("content-range"), Some("bytes 100-299999/300000"));
    assert_eq!(chr.header("content-length"), None);
    assert_eq!(f.verify(100, &chr.body), Ok(()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validators_and_conditional_requests() {
    let lab = lab(&[
        ("v", "[[files]]\nname = \"f\"\nsize = 1000"),
        ("nov", "[validators]\netag = false\nlast_modified = false\n[[files]]\nname = \"f\"\nsize = 1000"),
    ])
    .await;
    let addr = lab.http_addr();
    let first = get(addr, "/s/v/f", &[]).await;
    let etag = first.header("etag").unwrap().to_owned();
    let lm = first.header("last-modified").unwrap().to_owned();
    assert_eq!(lab.etag("v", "f", 0).unwrap(), etag);
    assert_eq!(lm, "Thu, 01 Jan 2026 00:00:00 GMT");

    assert_eq!(
        get(addr, "/s/v/f", &[("if-none-match", &etag)])
            .await
            .status,
        StatusCode::NOT_MODIFIED
    );
    assert_eq!(
        get(addr, "/s/v/f", &[("if-modified-since", &lm)])
            .await
            .status,
        StatusCode::NOT_MODIFIED
    );
    assert_eq!(
        get(addr, "/s/v/f", &[("if-match", "\"nope\"")])
            .await
            .status,
        StatusCode::PRECONDITION_FAILED
    );
    let ok_match = get(
        addr,
        "/s/v/f",
        &[("if-match", &etag), ("range", "bytes=0-9")],
    )
    .await;
    assert_eq!(ok_match.status, StatusCode::PARTIAL_CONTENT);
    let ir = get(
        addr,
        "/s/v/f",
        &[("if-range", &etag), ("range", "bytes=0-9")],
    )
    .await;
    assert_eq!(ir.status, StatusCode::PARTIAL_CONTENT);
    let ir_old = get(
        addr,
        "/s/v/f",
        &[("if-range", "\"old\""), ("range", "bytes=0-9")],
    )
    .await;
    assert_eq!(
        ir_old.status,
        StatusCode::OK,
        "If-Range mismatch sends the whole file"
    );
    let ir_date = get(addr, "/s/v/f", &[("if-range", &lm), ("range", "bytes=0-9")]).await;
    assert_eq!(ir_date.status, StatusCode::PARTIAL_CONTENT);

    let nov = get(addr, "/s/nov/f", &[]).await;
    assert_eq!(nov.header("etag"), None);
    assert_eq!(nov.header("last-modified"), None);
    let nov_ir = get(
        addr,
        "/s/nov/f",
        &[("if-range", &etag), ("range", "bytes=0-9")],
    )
    .await;
    assert_eq!(nov_ir.status, StatusCode::OK);
}

fn within(actual: f64, target: f64, tol: f64) -> bool {
    (actual - target).abs() <= target * tol
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn per_connection_throttle_is_accurate_and_per_connection() {
    let lab = lab(&[(
        "t",
        "[bandwidth]\nper_conn = \"4MB/s\"\n[[files]]\nname = \"f\"\nsize = \"6MB\"",
    )])
    .await;
    let addr = lab.http_addr();
    let f = lab.file("t", "f").unwrap();

    let one = get(addr, "/s/t/f", &[]).await;
    assert_eq!(f.verify(0, &one.body), Ok(()));
    let rate = one.body.len() as f64 / one.elapsed.as_secs_f64();
    assert!(within(rate, 4e6, 0.10), "single connection: {rate:.0} B/s");

    // Two connections each get the full per-connection rate.
    let (a, b) = tokio::join!(get(addr, "/s/t/f", &[]), get(addr, "/s/t/f", &[]));
    for x in [&a, &b] {
        let rate = x.body.len() as f64 / x.elapsed.as_secs_f64();
        assert!(
            within(rate, 4e6, 0.10),
            "parallel connection: {rate:.0} B/s"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn global_throttle_is_shared_by_everyone() {
    let lab = lab(&[(
        "g",
        "[bandwidth]\nglobal = \"4MB/s\"\n[[files]]\nname = \"f\"\nsize = \"3MB\"",
    )])
    .await;
    let addr = lab.http_addr();
    let t0 = Instant::now();
    let (a, b) = tokio::join!(get(addr, "/s/g/f", &[]), get(addr, "/s/g/f", &[]));
    let total = (a.body.len() + b.body.len()) as f64;
    assert_eq!(total, 6e6);
    let rate = total / t0.elapsed().as_secs_f64();
    assert!(within(rate, 4e6, 0.10), "aggregate {rate:.0} B/s");
}

const CAPPED: &str = "[bandwidth]\nper_conn = \"500kB/s\"\n[[files]]\nname = \"f\"\nsize = \"2MB\"\n[limits]\nmax_conns_per_ip = 2\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_caps_refuse_with_503_429_or_reset() {
    let s503 = format!("{CAPPED}refuse = \"503\"\nretry_after_secs = 3");
    let s429 = format!("{CAPPED}refuse = \"429\"");
    let srst = format!("{CAPPED}refuse = \"reset\"");
    let lab = lab(&[("c503", &s503), ("c429", &s429), ("crst", &srst)]).await;
    let addr = lab.http_addr();

    for (sc, expect) in [
        ("c503", Some(StatusCode::SERVICE_UNAVAILABLE)),
        ("c429", Some(StatusCode::TOO_MANY_REQUESTS)),
        ("crst", None),
    ] {
        let path = format!("/s/{sc}/f");
        // Two slow downloads hold both slots; their bodies stay unread.
        let mut c1 = Client::h1(addr).await;
        let mut c2 = Client::h1(addr).await;
        let h1 = c1.send(Method::GET, &path, &[]).await.unwrap();
        let h2 = c2.send(Method::GET, &path, &[]).await.unwrap();
        assert_eq!((h1.status(), h2.status()), (StatusCode::OK, StatusCode::OK));

        let mut c3 = Client::h1(addr).await;
        match expect {
            Some(status) => {
                let third = c3.get(&path, &[]).await;
                assert_eq!(third.status, status, "{sc}");
                let want_retry = if sc == "c503" { "3" } else { "1" };
                assert_eq!(third.header("retry-after"), Some(want_retry), "{sc}");
                assert_eq!(third.header("connection"), Some("close"), "{sc}");
            }
            None => {
                let err = c3.send(Method::GET, &path, &[]).await.unwrap_err();
                assert!(
                    is_reset(&err),
                    "{sc}: expected RST, got {}",
                    support::describe(&err)
                );
            }
        }
        let st = lab.stats(sc).unwrap();
        assert_eq!(st.refused, 1, "{sc}");
        assert_eq!(st.active_requests, 2, "{sc}");
        assert_eq!(st.clients["127.0.0.1"].peak_active_requests, 2, "{sc}");

        // Closing the slow downloads frees the slots.
        drop((h1, h2, c1, c2));
        assert!(
            eventually(Duration::from_secs(3), || lab
                .stats(sc)
                .unwrap()
                .active_requests
                == 0)
            .await,
            "{sc}: slots not released"
        );
        let after = get(addr, &format!("/s/{sc}/f"), &[("range", "bytes=0-99")]).await;
        assert_eq!(after.status, StatusCode::PARTIAL_CONTENT, "{sc}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resets_after_n_bytes_are_real_tcp_resets() {
    let lab = lab(&[
        (
            "rst",
            "[resets]\nafter_bytes = \"1MiB\"\nmax = 1\n[[files]]\nname = \"f\"\nsize = \"4MiB\"",
        ),
        (
            "rnd",
            "[resets]\nprob_per_mb = 1.0\nmax = 3\n[[files]]\nname = \"f\"\nsize = \"2MiB\"",
        ),
    ])
    .await;
    let addr = lab.http_addr();
    let f = lab.file("rst", "f").unwrap();

    let broken = get(addr, "/s/rst/f", &[]).await;
    assert_eq!(broken.status, StatusCode::OK);
    let err = broken.error.as_ref().expect("body must fail");
    assert!(
        is_reset(err),
        "expected ECONNRESET, got {}",
        support::describe(err)
    );
    assert!(
        broken.body.len() as u64 <= MIB,
        "got {} bytes",
        broken.body.len()
    );
    assert_eq!(
        f.verify(0, &broken.body),
        Ok(()),
        "bytes before the reset are intact"
    );
    assert_eq!(lab.stats("rst").unwrap().resets_injected, 1);

    // Budget used up: the next download completes.
    let ok = get(addr, "/s/rst/f", &[]).await;
    assert!(ok.error.is_none());
    assert_eq!(ok.body.len() as u64, 4 * MIB);

    // prob_per_mb = 1: every response dies within its first MiB, up to max.
    // The reset point can be byte 0, so the RST may also beat the head.
    for i in 0..3 {
        let mut c = Client::h1(addr).await;
        match c.send(Method::GET, "/s/rnd/f", &[]).await {
            Ok(resp) => {
                let x = support::read(resp, Instant::now()).await;
                assert!(x.error.as_ref().is_some_and(|e| is_reset(e)), "request {i}");
                assert!((x.body.len() as u64) < MIB);
            }
            Err(e) => assert!(is_reset(&e), "request {i}: {}", support::describe(&e)),
        }
    }
    let ok = get(addr, "/s/rnd/f", &[]).await;
    assert!(ok.error.is_none());
    assert_eq!(lab.stats("rnd").unwrap().resets_injected, 3);

    // Resetting the stats restores the budget.
    lab.reset(Some("rnd"));
    assert!(get(addr, "/s/rnd/f", &[]).await.error.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalls_pause_mid_stream_then_finish() {
    let lab = lab(&[(
        "st",
        "[stalls]\nafter_bytes = \"256KiB\"\nduration_ms = 1200\n[[files]]\nname = \"f\"\nsize = \"1MiB\"",
    )])
    .await;
    let addr = lab.http_addr();
    let x = get(addr, "/s/st/f", &[]).await;
    assert!(x.error.is_none());
    assert_eq!(lab.file("st", "f").unwrap().verify(0, &x.body), Ok(()));
    assert_eq!(x.body.len() as u64, MIB);
    assert!(
        x.max_gap >= Duration::from_millis(1100),
        "max gap {:?}",
        x.max_gap
    );
    assert!(
        x.ttfb < Duration::from_millis(500),
        "stall is mid-body, not before it"
    );
    assert_eq!(lab.stats("st").unwrap().stalls_injected, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn latency_ttfb_and_per_chunk() {
    let lab = lab(&[
        ("ttfb", "[latency]\nttfb_ms = 300\n[[files]]\nname = \"f\"\nsize = 1000"),
        (
            "drip",
            "[latency]\nchunk_ms = 25\nchunk_size = \"64KiB\"\n[[files]]\nname = \"f\"\nsize = \"512KiB\"",
        ),
    ])
    .await;
    let addr = lab.http_addr();
    let t = get(addr, "/s/ttfb/f", &[]).await;
    assert!(t.ttfb >= Duration::from_millis(300), "{:?}", t.ttfb);
    let d = get(addr, "/s/drip/f", &[]).await;
    assert!(
        d.elapsed >= Duration::from_millis(200),
        "8 chunks x 25 ms: {:?}",
        d.elapsed
    );
    assert_eq!(lab.file("drip", "f").unwrap().verify(0, &d.body), Ok(()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changing_file_switches_content_and_etag() {
    let lab = lab(&[
        (
            "chg",
            "[changing]\nafter_bytes = \"1MiB\"\nmax_changes = 2\n[[files]]\nname = \"f\"\nsize = \"2MiB\"",
        ),
        (
            "tick",
            "[changing]\nafter_secs = 0.5\n[[files]]\nname = \"f\"\nsize = 1000",
        ),
    ])
    .await;
    let addr = lab.http_addr();
    let base = lab.file("chg", "f").unwrap();

    let v0 = get(addr, "/s/chg/f", &[]).await;
    let e0 = v0.header("etag").unwrap().to_owned();
    assert_eq!(v0.header("x-lab-version"), Some("0"));
    assert_eq!(
        base.verify(0, &v0.body),
        Ok(()),
        "in-flight response keeps its version"
    );

    // 2 MiB served → version 2.
    assert_eq!(lab.current_version("chg", "f"), Some(2));
    let v2 = get(addr, "/s/chg/f", &[]).await;
    let e2 = v2.header("etag").unwrap().to_owned();
    assert_ne!(e0, e2);
    assert_eq!(e2, lab.etag("chg", "f", 2).unwrap());
    assert_ne!(v0.header("last-modified"), v2.header("last-modified"));
    assert_eq!(base.version(2).verify(0, &v2.body), Ok(()));
    assert!(base.verify(0, &v2.body).is_err(), "content really changed");

    // A resume with the old validator gets the whole new file, not a splice.
    let resume = get(
        addr,
        "/s/chg/f",
        &[("if-range", &e0), ("range", "bytes=100-")],
    )
    .await;
    assert_eq!(resume.status, StatusCode::OK);
    let strict = get(
        addr,
        "/s/chg/f",
        &[("if-match", &e0), ("range", "bytes=100-")],
    )
    .await;
    assert_eq!(strict.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(
        lab.current_version("chg", "f"),
        Some(2),
        "max_changes caps it"
    );

    let a = get(addr, "/s/tick/f", &[])
        .await
        .header("etag")
        .unwrap()
        .to_owned();
    tokio::time::sleep(Duration::from_millis(600)).await;
    let b = get(addr, "/s/tick/f", &[])
        .await
        .header("etag")
        .unwrap()
        .to_owned();
    assert_ne!(a, b, "time-based change");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_urls_expire_and_can_be_reminted() {
    let lab = lab(&[(
        "exp",
        "[signed_url]\nttl_secs = 1\n[[files]]\nname = \"f\"\nsize = 1000",
    )])
    .await;
    let addr = lab.http_addr();
    let f = lab.file("exp", "f").unwrap();

    let unsigned = get(addr, "/s/exp/f", &[]).await;
    assert_eq!(unsigned.status, StatusCode::FORBIDDEN);

    let mint = get(addr, "/_lab/mint?scenario=exp&file=f", &[]).await;
    assert_eq!(mint.status, StatusCode::OK);
    let m: serde_json::Value = serde_json::from_slice(&mint.body).unwrap();
    let path = m["path"].as_str().unwrap().to_owned();
    assert!(
        m["url"]
            .as_str()
            .unwrap()
            .starts_with(&format!("http://{addr}/s/exp/f?expires="))
    );

    let ok = get(addr, &path, &[]).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(f.verify(0, &ok.body), Ok(()));

    let tampered = path.replace("sig=", "sig=0");
    assert_eq!(
        get(addr, &tampered, &[]).await.status,
        StatusCode::FORBIDDEN
    );
    let other = lab
        .mint_path("exp", "f", Some(Duration::from_secs(60)))
        .unwrap();
    assert_eq!(get(addr, &other, &[]).await.status, StatusCode::OK);

    // ttl 1 s: two seconds later the first URL is dead, a fresh one works.
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let expired = get(addr, &path, &[]).await;
    assert_eq!(expired.status, StatusCode::FORBIDDEN);
    assert_eq!(expired.body, b"link expired\n");
    let fresh = lab.mint_path("exp", "f", None).unwrap();
    assert_eq!(get(addr, &fresh, &[]).await.status, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cookie_and_referer_gates() {
    let lab = lab(&[
        (
            "ck",
            "[access]\ncookie = \"sess=ok\"\n[[files]]\nname = \"f\"\nsize = 10",
        ),
        (
            "rf",
            "[access]\nreferer = \"https://site.test/\"\n[[files]]\nname = \"f\"\nsize = 10",
        ),
    ])
    .await;
    let addr = lab.http_addr();
    assert_eq!(
        get(addr, "/s/ck/f", &[]).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get(addr, "/s/ck/f", &[("cookie", "sess=bad")]).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get(addr, "/s/ck/f", &[("cookie", "a=1; sess=ok; b=2")])
            .await
            .status,
        StatusCode::OK
    );
    let login = get(addr, "/_lab/login?scenario=ck", &[]).await;
    assert_eq!(login.header("set-cookie"), Some("sess=ok; Path=/"));

    assert_eq!(
        get(addr, "/s/rf/f", &[]).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get(addr, "/s/rf/f", &[("referer", "https://evil.test/")])
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get(
            addr,
            "/s/rf/f",
            &[("referer", "https://site.test/page?id=1")]
        )
        .await
        .status,
        StatusCode::OK
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redirect_chain_keeps_query_and_ends_at_file() {
    let lab = lab(&[
        ("rd", "[redirects]\ncount = 3\n[[files]]\nname = \"f\"\nsize = 10"),
        ("rda", "[redirects]\ncount = 1\nstatus = 307\nabsolute = true\n[[files]]\nname = \"f\"\nsize = 10"),
    ])
    .await;
    let addr = lab.http_addr();
    let mut c = Client::h1(addr).await;
    let mut path = "/s/rd/f?x=1".to_owned();
    let mut hops = 0;
    loop {
        let x = c.get(&path, &[]).await;
        if x.status == StatusCode::OK {
            assert_eq!(x.body.len(), 10);
            break;
        }
        assert_eq!(x.status, StatusCode::FOUND);
        path = x.header("location").unwrap().to_owned();
        hops += 1;
        assert_eq!(path, format!("/s/rd/_hop/{hops}/f?x=1"));
    }
    assert_eq!(hops, 3);
    assert_eq!(
        c.get("/s/rd/_hop/4/f", &[]).await.status,
        StatusCode::NOT_FOUND
    );

    let a = c.get("/s/rda/f", &[]).await;
    assert_eq!(a.status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        a.header("location").unwrap(),
        format!("http://{addr}/s/rda/_hop/1/f")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_disposition_variants_from_builtin_scenario() {
    let lab = LabServer::start(LabConfig::default()).await.unwrap();
    let addr = lab.http_addr();
    let bn = get(
        addr,
        "/s/bangla-filename/bangla.bin",
        &[("range", "bytes=0-0")],
    )
    .await;
    let cd = bn.header("content-disposition").unwrap();
    assert!(cd.starts_with("attachment; filename=\"bangla-report.pdf\"; filename*=UTF-8''"));
    let encoded = cd.split("UTF-8''").nth(1).unwrap();
    assert_eq!(
        String::from_utf8(pct_decode(encoded)).unwrap(),
        "বাংলা প্রতিবেদন ২০২৬.pdf"
    );

    let plain = get(
        addr,
        "/s/bangla-filename/plain.bin",
        &[("range", "bytes=0-0")],
    )
    .await;
    assert_eq!(
        plain.header("content-disposition"),
        Some(r#"attachment; filename="annual \"final\" report.pdf""#)
    );

    let latin1 = get(
        addr,
        "/s/bangla-filename/latin1.bin",
        &[("range", "bytes=0-0")],
    )
    .await;
    let raw = latin1
        .headers
        .get("content-disposition")
        .unwrap()
        .as_bytes();
    assert_eq!(raw, b"attachment; filename=\"r\xe9sum\xe9.pdf\"");
    assert!(std::str::from_utf8(raw).is_err(), "deliberately not UTF-8");

    let rawu = get(
        addr,
        "/s/bangla-filename/raw-utf8.bin",
        &[("range", "bytes=0-0")],
    )
    .await;
    let raw = rawu.headers.get("content-disposition").unwrap().as_bytes();
    assert_eq!(
        std::str::from_utf8(raw).unwrap(),
        "attachment; filename=\"বাংলা.txt\""
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http2_over_tls_h1_over_tls_and_h2c() {
    let lab = lab(&[("clean", "[[files]]\nname = \"f\"\nsize = \"3MiB\"")]).await;
    let tls_addr = lab.https_addr().unwrap();
    let cert = lab.cert_der().unwrap().to_vec();
    let f = lab.file("clean", "f").unwrap();

    let mut h2 = Client::tls(tls_addr, &cert, &[b"h2", b"http/1.1"]).await;
    let full = h2.get("/s/clean/f", &[]).await;
    assert_eq!(full.version, Version::HTTP_2);
    assert_eq!(full.status, StatusCode::OK);
    assert_eq!(f.verify(0, &full.body), Ok(()));
    assert_eq!(full.body.len() as u64, 3 * MIB);
    let part = h2
        .get("/s/clean/f", &[("range", "bytes=70000-200000")])
        .await;
    assert_eq!(part.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(part.body, f.bytes(r(70000, 200001)));
    let head = h2.head("/s/clean/f").await;
    assert_eq!(head.header("content-length"), Some("3145728"));
    assert!(head.body.is_empty());

    let mut h1 = Client::tls(tls_addr, &cert, &[b"http/1.1"]).await;
    let x = h1.get("/s/clean/f", &[("range", "bytes=-10")]).await;
    assert_eq!(x.version, Version::HTTP_11);
    assert_eq!(x.body, f.bytes(r(3 * MIB - 10, 3 * MIB)));

    let mut h2c = Client::h2c(lab.http_addr()).await;
    let y = h2c.get("/s/clean/f", &[("range", "bytes=0-65536")]).await;
    assert_eq!(y.version, Version::HTTP_2);
    assert_eq!(y.body, f.bytes(r(0, 65537)));

    let pem = get(lab.http_addr(), "/_lab/cert.pem", &[]).await;
    assert_eq!(
        String::from_utf8(pem.body).unwrap(),
        lab.cert_pem().unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peak_connection_and_request_counts() {
    let lab = lab(&[(
        "p",
        "[bandwidth]\nper_conn = \"2MB/s\"\n[[files]]\nname = \"f\"\nsize = \"400kB\"",
    )])
    .await;
    let addr = lab.http_addr();

    // Keep-alive: three requests, one connection.
    let mut c = Client::h1(addr).await;
    for _ in 0..3 {
        c.get("/s/p/f", &[("range", "bytes=0-999")]).await;
    }
    let st = lab.stats("p").unwrap();
    assert_eq!(st.requests, 3);
    assert_eq!(st.status[&206], 3);
    assert_eq!(st.clients["127.0.0.1"].peak_open_connections, 1);
    assert_eq!(st.clients["127.0.0.1"].peak_active_requests, 1);
    assert_eq!(st.open_connections, 1);
    drop(c);
    assert!(
        eventually(Duration::from_secs(2), || lab
            .stats("p")
            .unwrap()
            .open_connections
            == 0)
        .await
    );

    // Three parallel connections, each mid-download at the same time.
    lab.reset(None);
    let (a, b, d) = tokio::join!(
        get(addr, "/s/p/f", &[]),
        get(addr, "/s/p/f", &[]),
        get(addr, "/s/p/f", &[])
    );
    assert!(a.error.is_none() && b.error.is_none() && d.error.is_none());
    let st = lab.stats("p").unwrap();
    assert_eq!(st.peak_open_connections, 3);
    assert_eq!(st.max_client_peak_connections(), 3);
    assert_eq!(st.max_client_peak_requests(), 3);
    assert_eq!(st.bytes_sent, 1_200_000);

    // HTTP/2: three streams share one connection. Peaks restart from what is
    // open at reset time, so first let the server see the H1 connections close.
    assert!(
        eventually(Duration::from_secs(2), || lab
            .stats("p")
            .unwrap()
            .open_connections
            == 0)
        .await
    );
    lab.reset(None);
    let c = Client::h2c(addr).await;
    let c = std::sync::Arc::new(tokio::sync::Mutex::new(c));
    let mut futs = Vec::new();
    for _ in 0..3 {
        let c = c.clone();
        futs.push(tokio::spawn(async move {
            let resp = c
                .lock()
                .await
                .send(Method::GET, "/s/p/f", &[])
                .await
                .unwrap();
            support::read(resp, Instant::now()).await
        }));
    }
    for f in futs {
        assert_eq!(f.await.unwrap().body.len(), 400_000);
    }
    let st = lab.stats("p").unwrap();
    assert_eq!(st.max_client_peak_connections(), 1);
    assert_eq!(st.max_client_peak_requests(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_api_lists_reports_and_resets() {
    let lab = LabServer::start(LabConfig::default()).await.unwrap();
    let addr = lab.http_addr();
    assert_eq!(get(addr, "/_lab/health", &[]).await.body, b"ok\n");

    let list: serde_json::Value =
        serde_json::from_slice(&get(addr, "/_lab/scenarios", &[]).await.body).unwrap();
    let names: Vec<_> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_owned())
        .collect();
    for n in [
        "clean",
        "per-conn-throttle",
        "conn-cap-2",
        "flaky",
        "stall",
        "high-latency",
        "no-range",
        "no-length",
        "changing-file",
        "expiring-url",
        "cookie-required",
        "bangla-filename",
    ] {
        assert!(
            names.iter().any(|x| x == n),
            "{n} missing from /_lab/scenarios"
        );
    }
    let clean = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "clean")
        .unwrap();
    assert!(
        clean["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "/s/clean/1g.bin")
    );

    get(addr, "/s/clean/1m.bin", &[("range", "bytes=0-9")]).await;
    let st: vayu_labserver::ScenarioStats =
        serde_json::from_slice(&get(addr, "/_lab/stats/clean", &[]).await.body).unwrap();
    assert_eq!(st.requests, 1);
    assert_eq!(st.bytes_sent, 10);
    let all: serde_json::Value =
        serde_json::from_slice(&get(addr, "/_lab/stats", &[]).await.body).unwrap();
    assert_eq!(all["clean"]["requests"], 1);

    let reset = Client::h1(addr)
        .await
        .request(Method::POST, "/_lab/reset", &[])
        .await;
    assert_eq!(reset.status, StatusCode::OK);
    assert_eq!(lab.stats("clean").unwrap().requests, 0);
    assert_eq!(
        get(addr, "/_lab/nope", &[]).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(addr, "/_lab/stats/nope", &[]).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(get(addr, "/", &[]).await.status, StatusCode::OK);
}
