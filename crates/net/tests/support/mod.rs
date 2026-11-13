//! Shared helpers for the integration tests: labserver per transport, a
//! raw HTTP/1.1 fixture server that records requests, and stream readers.

#![allow(dead_code)]

use std::collections::HashMap;
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use whoosh_core::{
    ByteRange, ByteStream, OpenRequest, Protocol, RangeSource, SourceError, SourceFactory,
    Validators,
};
use whoosh_labserver::{LabConfig, LabServer, Scenario};
use whoosh_net::{HttpSourceFactory, NetConfig};
use whoosh_proto::RequestContext;

pub const MIB: u64 = 1 << 20;

/// Which transport a labserver test runs over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum T {
    /// `http://`, HTTP/1.1.
    H1,
    /// `https://`, HTTP/1.1 only (`http2: false`), like an HTTP/1.1-only
    /// HTTPS server.
    H1s,
    /// `https://`, ALPN h2.
    H2,
    /// `https://` with HTTP/3 advertised; requests go over QUIC once the
    /// `Alt-Svc` is known.
    H3,
}

impl T {
    pub fn protocol(self) -> Protocol {
        match self {
            T::H1 | T::H1s => Protocol::Http1,
            T::H2 => Protocol::Http2,
            T::H3 => Protocol::Http3,
        }
    }
}

pub fn r(a: u64, b: u64) -> ByteRange {
    ByteRange::new(a, b).unwrap()
}

pub fn open_req(range: Option<ByteRange>, expect: Validators) -> OpenRequest {
    OpenRequest { range, expect }
}

/// Test settings: trust only the lab certificate, give QUIC plenty of
/// time before TCP joins the race (debug builds on a loaded machine).
pub fn test_config(lab: &LabServer) -> NetConfig {
    NetConfig {
        extra_root_certs: lab.cert_der().map(|c| vec![c.to_vec()]).unwrap_or_default(),
        platform_roots: false,
        h3_fallback_delay: Duration::from_secs(3),
        ..NetConfig::default()
    }
}

fn make_factory(
    lab: &LabServer,
    t: T,
    tweak: impl FnOnce(&mut NetConfig),
) -> Arc<HttpSourceFactory> {
    let mut cfg = test_config(lab);
    if t == T::H1s {
        cfg.http2 = false;
    }
    tweak(&mut cfg);
    let factory = HttpSourceFactory::new(cfg);
    if t == T::H3 {
        // As a host profile would: QUIC from the first request on. (Learning
        // it from `Alt-Svc` has its own test.)
        let port = lab.h3_addr().unwrap().port();
        let origin = format!("https://localhost:{}/", lab.https_addr().unwrap().port());
        factory.remember_http3(&origin, port, Duration::from_secs(3600));
    }
    factory
}

/// A labserver plus a factory talking to it over one transport.
pub struct Lab {
    pub lab: LabServer,
    pub factory: Arc<HttpSourceFactory>,
    pub t: T,
}

impl Lab {
    /// The built-in scenarios.
    pub async fn new(t: T) -> Self {
        Self::with(t, None, |_| {}).await
    }

    /// Only `scenarios` (when given), and a tweaked config.
    pub async fn with(
        t: T,
        scenarios: Option<Vec<Scenario>>,
        tweak: impl FnOnce(&mut NetConfig),
    ) -> Self {
        let mut lc = match scenarios {
            Some(s) => LabConfig::with_scenarios(s),
            None => LabConfig::default(),
        };
        if t == T::H3 {
            lc.h3_port = Some(0);
        }
        let lab = LabServer::start(lc).await.unwrap();
        let factory = make_factory(&lab, t, tweak);
        Self { lab, factory, t }
    }

    /// Another factory for the same server and transport.
    pub fn factory_with(&self, tweak: impl FnOnce(&mut NetConfig)) -> Arc<HttpSourceFactory> {
        make_factory(&self.lab, self.t, tweak)
    }

    pub fn origin_url(&self, path: &str) -> String {
        match self.t {
            T::H1 => format!("http://{}{path}", self.lab.http_addr()),
            T::H1s | T::H2 | T::H3 => format!(
                "https://localhost:{}{path}",
                self.lab.https_addr().unwrap().port()
            ),
        }
    }

    pub fn url(&self, sc: &str, file: &str) -> String {
        self.origin_url(&format!("/s/{sc}/{file}"))
    }

    pub fn source(&self, sc: &str, file: &str) -> Arc<dyn RangeSource> {
        self.factory.source(&self.url(sc, file), None).unwrap()
    }

    pub fn source_with(&self, sc: &str, file: &str, ctx: &RequestContext) -> Arc<dyn RangeSource> {
        self.factory.source(&self.url(sc, file), Some(ctx)).unwrap()
    }

    /// Asserts `bytes` are `file`'s bytes at `offset` (version 0).
    pub fn verify(&self, sc: &str, file: &str, offset: u64, bytes: &[u8]) {
        let f = self.lab.file(sc, file).unwrap();
        if let Err(at) = f.verify(offset, bytes) {
            panic!(
                "{sc}/{file}: wrong byte at {at} (read {} bytes from {offset})",
                bytes.len()
            );
        }
    }
}

/// Reads a stream to the end: the bytes, and the error that ended it.
pub async fn read_all(mut s: ByteStream) -> (Vec<u8>, Option<SourceError>) {
    let mut out = Vec::new();
    while let Some(c) = s.next().await {
        match c {
            Ok(b) => out.extend_from_slice(&b),
            Err(e) => return (out, Some(e)),
        }
    }
    (out, None)
}

/// Opens and reads a whole range, panicking on any error.
pub async fn fetch(src: &dyn RangeSource, range: Option<ByteRange>, expect: Validators) -> Vec<u8> {
    let s = src
        .open(open_req(range, expect))
        .await
        .unwrap_or_else(|e| panic!("open {range:?}: {e:?}"));
    let (b, e) = read_all(s).await;
    assert!(e.is_none(), "reading {range:?}: {e:?}");
    b
}

pub async fn open_err(
    src: &dyn RangeSource,
    range: Option<ByteRange>,
    expect: Validators,
) -> SourceError {
    match src.open(open_req(range, expect)).await {
        Ok(s) => {
            let (b, e) = read_all(s).await;
            panic!("expected an error, got {} bytes and {e:?}", b.len())
        }
        Err(e) => e,
    }
}

/// Polls `f` until it returns true or `within` passes.
pub async fn eventually<F, Fut>(within: Duration, mut f: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let t0 = Instant::now();
    loop {
        if f().await {
            return true;
        }
        if t0.elapsed() > within {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ------------------------------------------------------------------ fixture

/// One request as the fixture server saw it.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Which accepted connection (0-based) carried it.
    pub conn: usize,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn count(&self, name: &str) -> usize {
        self.headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(name))
            .count()
    }
}

/// What the fixture answers.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Send `Content-Length: body.len()` (default) or this value.
    pub length: Option<Option<u64>>,
    /// Close the connection after the reply.
    pub close: bool,
    /// Close it without saying so (no `Connection: close`), like a server
    /// whose keep-alive timeout fires.
    pub silent_close: bool,
}

impl Reply {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
            length: None,
            close: false,
            silent_close: false,
        }
    }

    pub fn silent_close(mut self) -> Self {
        self.silent_close = true;
        self
    }

    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }

    pub fn body(mut self, b: impl Into<Vec<u8>>) -> Self {
        self.body = b.into();
        self
    }

    /// Announce `n` bytes whatever the body is (`None`: no header, close).
    pub fn length(mut self, n: Option<u64>) -> Self {
        self.length = Some(n);
        self
    }

    pub fn close(mut self) -> Self {
        self.close = true;
        self
    }
}

type Handler = Arc<dyn Fn(&Seen) -> Reply + Send + Sync>;

/// A raw HTTP/1.1 server: records every request and answers with
/// `handler`. Keeps connections alive unless a reply says otherwise.
pub struct Fixture {
    pub addr: SocketAddr,
    pub seen: Arc<Mutex<Vec<Seen>>>,
    pub accepts: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fixture {
    pub async fn start(handler: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let accepts = Arc::new(AtomicUsize::new(0));
        let handler: Handler = Arc::new(handler);
        let (s2, a2) = (seen.clone(), accepts.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let n = a2.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(serve(sock, n, s2.clone(), handler.clone()));
            }
        });
        Self {
            addr,
            seen,
            accepts,
            task,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    pub fn last(&self) -> Seen {
        self.requests().pop().expect("no request seen")
    }
}

async fn serve(mut sock: TcpStream, conn: usize, seen: Arc<Mutex<Vec<Seen>>>, handler: Handler) {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i;
            }
            let mut chunk = [0u8; 4096];
            match sock.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        buf.drain(..head_end + 4);
        let mut lines = head.split("\r\n");
        let mut rl = lines.next().unwrap_or_default().split(' ');
        let method = rl.next().unwrap_or_default().to_string();
        let target = rl.next().unwrap_or_default().to_string();
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        let len: usize = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(0);
        while buf.len() < len {
            let mut chunk = [0u8; 4096];
            match sock.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        let body: Vec<u8> = buf.drain(..len).collect();
        let req = Seen {
            method,
            target,
            headers,
            body,
            conn,
        };
        seen.lock().unwrap().push(req.clone());
        let reply = handler(&req);
        let mut out = format!("HTTP/1.1 {} X\r\n", reply.status);
        for (k, v) in &reply.headers {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
        let mut close = reply.close;
        match reply.length {
            None => out.push_str(&format!("Content-Length: {}\r\n", reply.body.len())),
            Some(Some(n)) => out.push_str(&format!("Content-Length: {n}\r\n")),
            Some(None) => close = true,
        }
        if close {
            out.push_str("Connection: close\r\n");
        }
        out.push_str("\r\n");
        let mut bytes = out.into_bytes();
        if req.method != "HEAD" {
            bytes.extend_from_slice(&reply.body);
        }
        if sock.write_all(&bytes).await.is_err() {
            return;
        }
        if close || reply.silent_close {
            let _ = sock.shutdown().await;
            return;
        }
    }
}

/// A factory for fixture tests (plain HTTP).
pub fn plain_factory(tweak: impl FnOnce(&mut NetConfig)) -> Arc<HttpSourceFactory> {
    let mut cfg = NetConfig {
        platform_roots: false,
        ..NetConfig::default()
    };
    tweak(&mut cfg);
    HttpSourceFactory::new(cfg)
}

/// Header map helper for fixture handlers.
pub fn query(target: &str) -> HashMap<String, String> {
    target
        .split_once('?')
        .map(|(_, q)| {
            q.split('&')
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Sleeps until a labserver signed path (`…?expires=<unix secs>&sig=…`) is
/// past its expiry by the server's own rule (`now > expires`, in whole
/// seconds), whatever the load: no fixed sleep.
pub async fn sleep_past_expiry(path: &str) {
    let expires: u64 = query(path)["expires"].parse().unwrap();
    let past = std::time::UNIX_EPOCH + Duration::from_secs(expires + 1);
    if let Ok(wait) = past.duration_since(std::time::SystemTime::now()) {
        tokio::time::sleep(wait + Duration::from_millis(50)).await;
    }
}

/// The client's own view of `t`'s connections: (open connections, requests
/// in flight on them), from the factory's pool.
pub fn client_conns(f: &HttpSourceFactory, t: T) -> (usize, usize) {
    let st = f.pool_stats();
    match t {
        T::H1 | T::H1s => (st.http1_busy + st.http1_idle, st.http1_busy),
        T::H2 => (st.http2, st.streams),
        T::H3 => (st.http3, st.streams),
    }
}
