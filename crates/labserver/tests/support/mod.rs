//! A deliberately simple HTTP client for the integration tests: one
//! connection per `Client`, so tests control exactly how many TCP
//! connections exist, and errors keep their io::Error cause.

#![allow(dead_code)]

use bytes::Bytes;
use http::{HeaderMap, Method, Request, StatusCode, Version};
use http_body_util::{BodyExt, Empty};
use hyper::body::Incoming;
use hyper::client::conn::{http1, http2};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::{CertificateDer, ServerName};
use std::error::Error as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use whoosh_labserver::{LabConfig, LabServer, Scenario};

/// Starts a server with only these `(name, toml)` scenarios.
pub async fn lab(scenarios: &[(&str, &str)]) -> LabServer {
    let scs = scenarios
        .iter()
        .map(|(n, t)| Scenario::from_toml(n, t).unwrap())
        .collect();
    LabServer::start(LabConfig::with_scenarios(scs))
        .await
        .unwrap()
}

#[derive(Debug)]
pub struct Resp {
    pub status: StatusCode,
    pub version: Version,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    pub error: Option<hyper::Error>,
    /// Time from sending the request to the response head.
    pub ttfb: Duration,
    /// Time from sending the request to the end (or error) of the body.
    pub elapsed: Duration,
    /// Longest pause between two body frames.
    pub max_gap: Duration,
}

impl Resp {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

/// True if `e` (or a cause) is ECONNRESET.
pub fn is_reset(e: &(dyn std::error::Error + 'static)) -> bool {
    let mut cur = Some(e);
    while let Some(err) = cur {
        if err
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::ConnectionReset)
        {
            return true;
        }
        cur = err.source();
    }
    false
}

enum Conn {
    H1(http1::SendRequest<Empty<Bytes>>),
    H2(http2::SendRequest<Empty<Bytes>>),
}

pub struct Client {
    conn: Conn,
    origin: String,
}

impl Client {
    pub async fn h1(addr: SocketAddr) -> Self {
        let tcp = TcpStream::connect(addr).await.unwrap();
        let (send, conn) = http1::handshake(TokioIo::new(tcp)).await.unwrap();
        tokio::spawn(conn);
        Self {
            conn: Conn::H1(send),
            origin: format!("http://{addr}"),
        }
    }

    /// HTTP/2 over cleartext with prior knowledge.
    pub async fn h2c(addr: SocketAddr) -> Self {
        let tcp = TcpStream::connect(addr).await.unwrap();
        let (send, conn) = Self::h2_builder()
            .handshake(TokioIo::new(tcp))
            .await
            .unwrap();
        tokio::spawn(conn);
        Self {
            conn: Conn::H2(send),
            origin: format!("http://{addr}"),
        }
    }

    fn h2_builder() -> http2::Builder<TokioExecutor> {
        let mut b = http2::Builder::new(TokioExecutor::new());
        b.initial_stream_window_size(8 << 20)
            .initial_connection_window_size(16 << 20);
        b
    }

    /// TLS, trusting `cert_der`, offering `alpn`; uses HTTP/2 if negotiated.
    pub async fn tls(addr: SocketAddr, cert_der: &[u8], alpn: &[&[u8]]) -> Self {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(cert_der.to_vec())).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
        let tcp = TcpStream::connect(addr).await.unwrap();
        let tls = TlsConnector::from(Arc::new(cfg))
            .connect(ServerName::try_from("localhost").unwrap(), tcp)
            .await
            .unwrap();
        let h2 = tls.get_ref().1.alpn_protocol() == Some(b"h2");
        let io = TokioIo::new(tls);
        let conn = if h2 {
            let (send, conn) = Self::h2_builder().handshake(io).await.unwrap();
            tokio::spawn(conn);
            Conn::H2(send)
        } else {
            let (send, conn) = http1::handshake(io).await.unwrap();
            tokio::spawn(conn);
            Conn::H1(send)
        };
        Self {
            conn,
            origin: format!("https://localhost:{}", addr.port()),
        }
    }

    /// Sends a request and returns the response head with the body unread.
    pub async fn send(
        &mut self,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
    ) -> Result<http::Response<Incoming>, hyper::Error> {
        let mut b = Request::builder().method(method);
        b = match self.conn {
            // HTTP/1.1: origin-form plus Host.
            Conn::H1(_) => b.uri(path).header(
                "host",
                self.origin.split("://").nth(1).unwrap_or("localhost"),
            ),
            // HTTP/2: absolute form for :scheme and :authority.
            Conn::H2(_) => b.uri(format!("{}{}", self.origin, path)),
        };
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let req = b.body(Empty::new()).unwrap();
        match &mut self.conn {
            Conn::H1(s) => {
                s.ready().await?;
                s.send_request(req).await
            }
            Conn::H2(s) => {
                s.ready().await?;
                s.send_request(req).await
            }
        }
    }

    pub async fn request(&mut self, method: Method, path: &str, headers: &[(&str, &str)]) -> Resp {
        let t0 = Instant::now();
        let resp = self
            .send(method, path, headers)
            .await
            .unwrap_or_else(|e| panic!("{path}: {e:?}"));
        read(resp, t0).await
    }

    pub async fn get(&mut self, path: &str, headers: &[(&str, &str)]) -> Resp {
        self.request(Method::GET, path, headers).await
    }

    pub async fn head(&mut self, path: &str) -> Resp {
        self.request(Method::HEAD, path, &[]).await
    }
}

/// Reads a whole response body, keeping what arrived before any error.
pub async fn read(resp: http::Response<Incoming>, t0: Instant) -> Resp {
    let ttfb = t0.elapsed();
    let (parts, mut body) = resp.into_parts();
    let mut data = Vec::new();
    let mut error = None;
    let mut last = Instant::now();
    let mut max_gap = Duration::ZERO;
    while let Some(frame) = body.frame().await {
        match frame {
            Ok(f) => {
                if let Some(d) = f.data_ref() {
                    let now = Instant::now();
                    max_gap = max_gap.max(now - last);
                    last = now;
                    data.extend_from_slice(d);
                }
            }
            Err(e) => {
                error = Some(e);
                break;
            }
        }
    }
    Resp {
        status: parts.status,
        version: parts.version,
        headers: parts.headers,
        body: data,
        error,
        ttfb,
        elapsed: t0.elapsed(),
        max_gap,
    }
}

/// One GET on a fresh HTTP/1.1 connection.
pub async fn get(addr: SocketAddr, path: &str, headers: &[(&str, &str)]) -> Resp {
    Client::h1(addr).await.get(path, headers).await
}

/// Polls `f` until it is true or `timeout` passes.
pub async fn eventually(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    f()
}

/// Percent-decodes `s` (for RFC 5987 values).
pub fn pct_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// Sanity check that `e` came from the error path (used in assertions).
pub fn describe(e: &hyper::Error) -> String {
    let mut s = format!("{e}");
    let mut cur = e.source();
    while let Some(c) = cur {
        s.push_str(&format!(" <- {c}"));
        cur = c.source();
    }
    s
}
