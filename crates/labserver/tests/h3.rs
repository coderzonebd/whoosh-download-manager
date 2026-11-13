//! HTTP/3 integration tests: a real QUIC endpoint on an ephemeral UDP port,
//! a quinn + h3 client.

mod support;

use bytes::{Buf, Bytes};
use http::{HeaderMap, Method, Request, StatusCode, Version};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::{Client, eventually, get};
use whoosh_labserver::{H3_RESET_CODE, H3_RESET_REASON, LabConfig, LabServer, Scenario};

const MIB: u64 = 1 << 20;

/// A server with only these `(name, toml)` scenarios and HTTP/3 on.
async fn h3_lab(scenarios: &[(&str, &str)]) -> LabServer {
    let scs = scenarios
        .iter()
        .map(|(n, t)| Scenario::from_toml(n, t).unwrap())
        .collect();
    LabServer::start(LabConfig {
        h3_port: Some(0),
        ..LabConfig::with_scenarios(scs)
    })
    .await
    .unwrap()
}

/// One QUIC connection; requests on it are HTTP/3 streams.
struct H3 {
    send: h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    conn: quinn::Connection,
    origin: String,
    _driver: tokio::task::JoinHandle<()>,
    _endpoint: quinn::Endpoint,
}

#[derive(Debug)]
struct H3Resp {
    status: StatusCode,
    version: Version,
    headers: HeaderMap,
    body: Vec<u8>,
    error: Option<h3::error::StreamError>,
    elapsed: Duration,
}

impl H3Resp {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

type Stream = h3::client::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>;

impl H3 {
    async fn connect(addr: SocketAddr, cert_der: &[u8]) -> Self {
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from(cert_der.to_vec()))
            .unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        tls.alpn_protocols = vec![b"h3".to_vec()];
        let quic = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
        let mut transport = quinn::TransportConfig::default();
        // Room for unread bodies, so a stalled reader does not stall the server.
        transport
            .stream_receive_window((8u32 << 20).into())
            .receive_window((32u32 << 20).into());
        let mut cfg = quinn::ClientConfig::new(Arc::new(quic));
        cfg.transport_config(Arc::new(transport));
        // A loopback burst must not overflow the default 208 KiB receive
        // buffer while the test runtime is starved: a dropped
        // CONNECTION_CLOSE is only noticed at the idle timeout.
        let socket = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let _ = socket2::SockRef::from(&socket).set_recv_buffer_size(4 << 20);
        let mut endpoint = quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            None,
            socket,
            Arc::new(quinn::TokioRuntime),
        )
        .unwrap();
        endpoint.set_default_client_config(cfg);
        let conn = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
        let (mut driver, send) = h3::client::new(h3_quinn::Connection::new(conn.clone()))
            .await
            .unwrap();
        let driver = tokio::spawn(async move {
            let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
        });
        Self {
            send,
            conn,
            origin: format!("https://localhost:{}", addr.port()),
            _driver: driver,
            _endpoint: endpoint,
        }
    }

    /// Sends a request on a new stream; returns the stream with the
    /// response head read.
    async fn send(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
    ) -> Result<(http::Response<()>, Stream), h3::error::StreamError> {
        let mut b = Request::builder()
            .method(method)
            .uri(format!("{}{}", self.origin, path));
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let mut stream = self.send.clone().send_request(b.body(()).unwrap()).await?;
        stream.finish().await?;
        let resp = stream.recv_response().await?;
        Ok((resp, stream))
    }

    async fn request(&self, method: Method, path: &str, headers: &[(&str, &str)]) -> H3Resp {
        let t0 = Instant::now();
        let (resp, stream) = self
            .send(method, path, headers)
            .await
            .unwrap_or_else(|e| panic!("{path}: {e:?}"));
        read(resp, stream, t0).await
    }

    async fn get(&self, path: &str, headers: &[(&str, &str)]) -> H3Resp {
        self.request(Method::GET, path, headers).await
    }

    /// Waits (up to 5 s) for the connection to close; the application
    /// close the server sent, if that is how it ended.
    async fn closed(&self) -> Result<quinn::ApplicationClose, Option<quinn::ConnectionError>> {
        match tokio::time::timeout(Duration::from_secs(5), self.conn.closed()).await {
            Ok(quinn::ConnectionError::ApplicationClosed(c)) => Ok(c),
            Ok(other) => Err(Some(other)),
            Err(_) => Err(None),
        }
    }
}

/// Reads a whole response body, keeping what arrived before any error.
async fn read(resp: http::Response<()>, mut stream: Stream, t0: Instant) -> H3Resp {
    let mut body = Vec::new();
    let mut error = None;
    loop {
        match stream.recv_data().await {
            Ok(Some(mut chunk)) => {
                while chunk.has_remaining() {
                    let c = chunk.chunk();
                    body.extend_from_slice(c);
                    let n = c.len();
                    chunk.advance(n);
                }
            }
            Ok(None) => break,
            Err(e) => {
                error = Some(e);
                break;
            }
        }
    }
    let (parts, ()) = resp.into_parts();
    H3Resp {
        status: parts.status,
        version: parts.version,
        headers: parts.headers,
        body,
        error,
        elapsed: t0.elapsed(),
    }
}

async fn h3_get(lab: &LabServer, path: &str) -> H3Resp {
    H3::connect(lab.h3_addr().unwrap(), lab.cert_der().unwrap())
        .await
        .get(path, &[])
        .await
}

/// The cap holds (at most +10 %, as on TCP). The lower bound is looser
/// than TCP's −10 %: a debug-build QUIC client (packet crypto and ACKs in
/// userspace) falls behind when the machine is loaded, which is not the
/// throttle's doing.
fn capped(rate: f64, cap: f64) -> bool {
    (cap * 0.85..=cap * 1.10).contains(&rate)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn h3_ranged_gets_are_byte_exact_and_advertised() {
    let lab = h3_lab(&[("clean", "")]).await;
    let h3_addr = lab.h3_addr().unwrap();
    assert_eq!(
        h3_addr.port(),
        lab.https_addr().unwrap().port(),
        "same port number"
    );
    assert_eq!(
        lab.h3_url("clean", "1m.bin"),
        lab.https_url("clean", "1m.bin")
    );

    // Both TCP listeners advertise the QUIC endpoint.
    let alt = format!("h3=\":{}\"", h3_addr.port());
    let plain = get(lab.http_addr(), "/s/clean/empty.bin", &[]).await;
    assert_eq!(plain.header("alt-svc"), Some(alt.as_str()));
    let mut tls = Client::tls(lab.https_addr().unwrap(), lab.cert_der().unwrap(), &[b"h2"]).await;
    let r = tls.get("/_lab/health", &[]).await;
    assert_eq!(r.version, Version::HTTP_2);
    assert_eq!(r.header("alt-svc"), Some(alt.as_str()));

    let f = lab.file("clean", "1m.bin").unwrap();
    let c = H3::connect(h3_addr, lab.cert_der().unwrap()).await;
    let full = c.get("/s/clean/1m.bin", &[]).await;
    assert_eq!(full.status, StatusCode::OK);
    assert_eq!(full.version, Version::HTTP_3);
    assert!(full.error.is_none(), "{:?}", full.error);
    assert_eq!(full.header("content-length"), Some("1048576"));
    assert_eq!(full.header("accept-ranges"), Some("bytes"));
    assert!(full.header("etag").is_some() && full.header("date").is_some());
    assert_eq!(full.header("alt-svc"), None, "not on HTTP/3 itself");
    assert_eq!(full.body.len() as u64, MIB);
    assert_eq!(f.verify(0, &full.body), Ok(()));

    for (spec, start, end) in [
        ("bytes=0-0", 0, 1),
        ("bytes=65530-65545", 65530, 65546), // crosses a 64 KiB block
        ("bytes=1000-", 1000, MIB),
        ("bytes=-500", MIB - 500, MIB),
        ("bytes=1048575-1048575", MIB - 1, MIB),
    ] {
        let p = c.get("/s/clean/1m.bin", &[("range", spec)]).await;
        assert_eq!(p.status, StatusCode::PARTIAL_CONTENT, "{spec}");
        assert_eq!(
            p.header("content-range").unwrap(),
            format!("bytes {start}-{}/{MIB}", end - 1),
            "{spec}"
        );
        assert_eq!(p.body.len() as u64, end - start, "{spec}");
        assert_eq!(f.verify(start, &p.body), Ok(()), "{spec}");
    }
    let bad = c
        .get("/s/clean/1m.bin", &[("range", "bytes=1048576-")])
        .await;
    assert_eq!(bad.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(bad.header("content-range"), Some("bytes */1048576"));

    // Two ranges of a bigger file at once: two streams on one connection.
    let big = lab.file("clean", "10m.bin").unwrap();
    let (a, b) = tokio::join!(
        c.get("/s/clean/10m.bin", &[("range", "bytes=3000000-7999999")]),
        c.get("/s/clean/10m.bin", &[("range", "bytes=9000000-")]),
    );
    assert_eq!(a.body.len(), 5_000_000);
    assert_eq!(big.verify(3_000_000, &a.body), Ok(()));
    assert_eq!(big.verify(9_000_000, &b.body), Ok(()));
    assert_eq!(b.body.len() as u64, 10 * MIB - 9_000_000);

    let head = c.request(Method::HEAD, "/s/clean/1m.bin", &[]).await;
    assert_eq!(head.status, StatusCode::OK);
    assert_eq!(head.header("content-length"), Some("1048576"));
    assert!(head.body.is_empty());

    // Sidecar and control API work over HTTP/3 too.
    let sum = c.get("/s/clean/1m.bin.sha256", &[]).await;
    assert!(
        String::from_utf8(sum.body)
            .unwrap()
            .starts_with(&f.sha256_hex(f.full()))
    );
    let stats = c.get("/_lab/stats/clean", &[]).await;
    let v: serde_json::Value = serde_json::from_slice(&stats.body).unwrap();
    assert_eq!(v["status"]["206"], 7);
    assert_eq!(v["status"]["416"], 1);

    // HTTP/3 is opt-in: nothing advertised by default.
    let off = LabServer::start(LabConfig::with_scenarios(vec![Scenario::clean("clean")]))
        .await
        .unwrap();
    assert!(off.h3_addr().is_none() && off.h3_url("clean", "1m.bin").is_none());
    let r = get(off.http_addr(), "/s/clean/empty.bin", &[]).await;
    assert_eq!(r.header("alt-svc"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn h3_throttles_are_per_quic_connection_and_global() {
    let lab = h3_lab(&[
        (
            "t",
            "[bandwidth]\nper_conn = \"4MB/s\"\n[[files]]\nname = \"f\"\nsize = \"4MB\"",
        ),
        (
            "g",
            "[bandwidth]\nglobal = \"4MB/s\"\n[[files]]\nname = \"f\"\nsize = \"2MB\"",
        ),
    ])
    .await;
    let f = lab.file("t", "f").unwrap();

    let one = h3_get(&lab, "/s/t/f").await;
    assert_eq!(f.verify(0, &one.body), Ok(()));
    assert_eq!(one.body.len(), 4_000_000);
    let rate = one.body.len() as f64 / one.elapsed.as_secs_f64();
    assert!(capped(rate, 4e6), "one QUIC connection: {rate:.0} B/s");

    // Two QUIC connections each get the full per-connection rate.
    let (a, b) = tokio::join!(h3_get(&lab, "/s/t/f"), h3_get(&lab, "/s/t/f"));
    for x in [&a, &b] {
        assert_eq!(x.body.len(), 4_000_000);
        let rate = x.body.len() as f64 / x.elapsed.as_secs_f64();
        assert!(capped(rate, 4e6), "parallel connection: {rate:.0} B/s");
    }

    // Two streams on one QUIC connection share its bucket.
    let c = H3::connect(lab.h3_addr().unwrap(), lab.cert_der().unwrap()).await;
    let t0 = Instant::now();
    let (s1, s2) = tokio::join!(
        c.send(Method::GET, "/s/t/f", &[]),
        c.send(Method::GET, "/s/t/f", &[])
    );
    let ((h1, st1), (h2, st2)) = (s1.unwrap(), s2.unwrap());
    let (x, y) = tokio::join!(read(h1, st1, t0), read(h2, st2, t0));
    let total = (x.body.len() + y.body.len()) as f64;
    assert_eq!(total, 8e6);
    let rate = total / t0.elapsed().as_secs_f64();
    assert!(
        capped(rate, 4e6),
        "two streams, one connection: {rate:.0} B/s"
    );

    // The global cap is shared by every QUIC connection (timed after the
    // handshakes).
    let (addr, cert) = (lab.h3_addr().unwrap(), lab.cert_der().unwrap());
    let (c1, c2) = tokio::join!(H3::connect(addr, cert), H3::connect(addr, cert));
    let t0 = Instant::now();
    let (a, b) = tokio::join!(c1.get("/s/g/f", &[]), c2.get("/s/g/f", &[]));
    let total = (a.body.len() + b.body.len()) as f64;
    assert_eq!(total, 4e6);
    let rate = total / t0.elapsed().as_secs_f64();
    assert!(capped(rate, 4e6), "global: {rate:.0} B/s");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn h3_stats_count_quic_connections_and_streams() {
    let lab = h3_lab(&[(
        "s",
        "[bandwidth]\nper_conn = \"1MB/s\"\n[[files]]\nname = \"f\"\nsize = \"3MB\"",
    )])
    .await;
    let (addr, cert) = (lab.h3_addr().unwrap(), lab.cert_der().unwrap());
    let a = H3::connect(addr, cert).await;
    let b = H3::connect(addr, cert).await;
    // Three slow responses in flight: two streams on `a`, one on `b`.
    let (r1, r2, r3) = tokio::join!(
        a.send(Method::GET, "/s/s/f", &[]),
        a.send(Method::GET, "/s/s/f", &[]),
        b.send(Method::GET, "/s/s/f", &[]),
    );
    let _streams = [r1.unwrap(), r2.unwrap(), r3.unwrap()];
    let st = lab.stats("s").unwrap();
    assert_eq!(st.requests, 3);
    assert_eq!(st.open_connections, 2, "QUIC connections, not streams");
    assert_eq!(st.active_requests, 3);
    let me = &st.clients["127.0.0.1"];
    assert_eq!(
        (me.peak_open_connections, me.peak_active_requests),
        (2, 3),
        "{st:?}"
    );

    a.conn.close(0u32.into(), b"bye");
    assert!(
        eventually(Duration::from_secs(5), || {
            let s = lab.stats("s").unwrap();
            s.open_connections == 1 && s.active_requests == 1
        })
        .await,
        "{:?}",
        lab.stats("s")
    );
    b.conn.close(0u32.into(), b"bye");
    assert!(
        eventually(Duration::from_secs(5), || {
            let s = lab.stats("s").unwrap();
            s.open_connections == 0 && s.active_requests == 0
        })
        .await,
        "{:?}",
        lab.stats("s")
    );
    let st = lab.stats("s").unwrap();
    assert_eq!(st.peak_open_connections, 2);
    assert_eq!(st.peak_active_requests, 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn h3_resets_close_the_quic_connection() {
    let lab = h3_lab(&[
        (
            "r",
            "[resets]\nafter_bytes = \"1MiB\"\nmax = 1\n[[files]]\nname = \"f\"\nsize = \"4MiB\"",
        ),
        (
            "cap",
            "[bandwidth]\nper_conn = \"500kB/s\"\n[[files]]\nname = \"f\"\nsize = \"1MB\"\n[limits]\nmax_conns_per_ip = 1\nrefuse = \"reset\"",
        ),
    ])
    .await;
    let (addr, cert) = (lab.h3_addr().unwrap(), lab.cert_der().unwrap());
    let f = lab.file("r", "f").unwrap();

    let c = H3::connect(addr, cert).await;
    let r = c.get("/s/r/f", &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.error.is_some(), "body must end in an error");
    assert!(r.body.len() as u64 <= MIB, "got {} bytes", r.body.len());
    assert_eq!(f.verify(0, &r.body), Ok(()));
    // The stream is reset (same code) just before the connection closes.
    let err = format!("{:?}", r.error.as_ref().unwrap());
    assert!(err.contains("H3_INTERNAL_ERROR"), "{err}");
    let close = c.closed().await.unwrap_or_else(|e| {
        panic!(
            "no application close: {e:?} after {:?}, {} bytes; server {:?}; client stats {:?}",
            r.elapsed,
            r.body.len(),
            lab.stats("r"),
            c.conn.stats()
        )
    });
    assert_eq!(close.error_code, quinn::VarInt::from_u32(H3_RESET_CODE));
    assert_eq!(&close.reason[..], H3_RESET_REASON);
    assert_eq!(lab.stats("r").unwrap().resets_injected, 1);
    assert!(
        eventually(Duration::from_secs(5), || lab
            .stats("r")
            .unwrap()
            .open_connections
            == 0)
        .await
    );
    // Budget used up: a new QUIC connection downloads the whole file.
    let ok = h3_get(&lab, "/s/r/f").await;
    assert!(ok.error.is_none(), "{:?}", ok.error);
    assert_eq!(ok.body.len() as u64, 4 * MIB);
    assert_eq!(f.verify(0, &ok.body), Ok(()));

    // A connection cap with `refuse = "reset"` closes the newcomer's
    // connection before any response; the first download goes on.
    let first = H3::connect(addr, cert).await;
    let (head, stream) = first.send(Method::GET, "/s/cap/f", &[]).await.unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    let second = H3::connect(addr, cert).await;
    let refused = second.send(Method::GET, "/s/cap/f", &[]).await;
    assert!(
        refused.as_ref().is_err(),
        "got {:?}",
        refused.map(|(head, _)| head.status())
    );
    let close = second.closed().await.expect("application close");
    assert_eq!(close.error_code, quinn::VarInt::from_u32(H3_RESET_CODE));
    let done = read(head, stream, Instant::now()).await;
    assert!(done.error.is_none(), "{:?}", done.error);
    assert_eq!(done.body.len(), 1_000_000);
    assert_eq!(lab.stats("cap").unwrap().refused, 1);
}
