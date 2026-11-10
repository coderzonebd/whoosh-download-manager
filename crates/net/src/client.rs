//! One HTTP exchange on the best available connection: pooled or new,
//! HTTP/3 (when `Alt-Svc` says so, racing TCP), HTTP/2 or HTTP/1.1.

use std::future::Future;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use http::header::{self, HeaderMap, HeaderValue};
use http::{Method, Request, StatusCode, Version};
use http_body_util::Full;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use tokio::io::{AsyncRead, AsyncWrite};
use url::Url;
use vayu_core::SourceError;

use crate::altsvc::{AltSvcCache, H3Alt};
use crate::body::{Lease, RespBody};
use crate::classify::{Cause, Phase, cause_of, describe, transport_error};
use crate::connect::{self, AttemptError, Resolver};
use crate::h3guard::{GuardedConn, MAX_FIELD_SECTION, Violation};
use crate::origin::Origin;
use crate::pool::{H1Conn, H2Sender, H3Sender, Mux, MuxLease, Pool};
use crate::tls::Tls;
use crate::{NetConfig, lock};

/// HTTP/2 flow-control windows: ~1 Gbit/s at 100 ms RTT per connection;
/// also the most a slow reader leaves buffered in memory.
const H2_STREAM_WINDOW: u32 = 16 << 20;
const H2_CONN_WINDOW: u32 = 32 << 20;
/// Lets HTTP/2 servers send DATA frames bigger than the 16 KiB default:
/// fewer frames, less per-frame work on both ends.
const H2_MAX_FRAME: u32 = 256 << 10;
/// HTTP/1.1 read buffer ceiling (hyper's adaptive buffer grows to it).
const H1_MAX_BUF: usize = 1 << 20;
/// UDP socket buffers for QUIC; the default 208 KiB overflows on bursts.
const UDP_BUF: usize = 4 << 20;
/// How long a pooled HTTP/1.1 connection may take to become ready.
const READY_WAIT: Duration = Duration::from_secs(2);

/// A response with its head read.
pub(crate) struct Response {
    pub status: StatusCode,
    pub version: Version,
    pub headers: HeaderMap,
    pub body: RespBody,
}

enum Acquired {
    H1(H1Conn),
    H2(MuxLease<H2Sender>),
    H3(MuxLease<H3Sender>),
}

enum NewConn {
    H1(H1Conn),
    H2(Arc<Mux<H2Sender>>),
    H3(Arc<Mux<H3Sender>>),
}

/// Shared state behind every source made by one factory.
pub(crate) struct Client {
    pub cfg: NetConfig,
    pub dns: Resolver,
    tls: Tls,
    pub alt_svc: AltSvcCache,
    pub pool: Arc<Pool>,
    quic_v4: Mutex<Option<quinn::Endpoint>>,
    quic_v6: Mutex<Option<quinn::Endpoint>>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("cfg", &self.cfg)
            .field("pool", &self.pool)
            .finish_non_exhaustive()
    }
}

/// The request as it goes on the wire.
pub(crate) struct Outgoing {
    pub method: Method,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Client {
    pub fn new(cfg: NetConfig) -> Self {
        Self {
            dns: Resolver::new(cfg.connect_timeout),
            tls: Tls::new(&cfg),
            alt_svc: AltSvcCache::default(),
            pool: Pool::new(cfg.max_streams_per_connection, cfg.pool_idle_timeout),
            quic_v4: Mutex::new(None),
            quic_v6: Mutex::new(None),
            cfg,
        }
    }

    /// Sends `req` to `url` and waits for the response head (at most
    /// `idle_timeout`). Does not follow redirects. A request that never
    /// left on a pooled connection that turned out to be dead is sent once
    /// more on a new connection; nothing else is retried.
    pub async fn send(
        &self,
        url: &Url,
        origin: &Origin,
        req: &Outgoing,
    ) -> Result<Response, SourceError> {
        let mut retried = false;
        loop {
            // The re-send goes on a new connection, not another pooled one
            // that may be just as stale.
            let acquired = self.acquire(origin, retried).await?;
            let res = match acquired {
                Acquired::H1(c) => self.send_h1(url, origin, req, c).await,
                Acquired::H2(l) => self.send_h2(url, req, l).await,
                Acquired::H3(l) => self.send_h3(url, req, l).await,
            };
            match res {
                Ok(r) => {
                    if r.version != Version::HTTP_3 {
                        self.alt_svc.observe(origin, &r.headers);
                    }
                    return Ok(r);
                }
                Err(Failure::Unsent) if !retried => retried = true,
                Err(Failure::Unsent) => {
                    return Err(SourceError::Transient(
                        "connection closed before the request was sent".into(),
                    ));
                }
                Err(Failure::Error(e)) => return Err(e),
            }
        }
    }

    async fn acquire(&self, origin: &Origin, fresh: bool) -> Result<Acquired, SourceError> {
        if origin.is_https()
            && self.cfg.http3
            && let Some(alt) = self.alt_svc.get(origin)
        {
            if !fresh && let Some(l) = self.pool.take_h3(origin) {
                return Ok(Acquired::H3(l));
            }
            return self
                .connect_h3_or_tcp(origin, &alt)
                .await
                .map(|c| self.register(origin, c));
        }
        if !fresh {
            if let Some(l) = self.pool.take_h2(origin) {
                return Ok(Acquired::H2(l));
            }
            if let Some(c) = self.pool.take_h1(origin) {
                return Ok(Acquired::H1(c));
            }
        }
        self.connect_tcp(origin)
            .await
            .map(|c| self.register(origin, c))
    }

    /// Puts a new connection in the pool with the caller's request as its
    /// first user.
    fn register(&self, origin: &Origin, c: NewConn) -> Acquired {
        match c {
            NewConn::H1(c) => Acquired::H1(c),
            NewConn::H2(m) => Acquired::H2(
                self.pool
                    .add_h2(origin.clone(), m, true)
                    .expect("lease requested"),
            ),
            NewConn::H3(m) => Acquired::H3(
                self.pool
                    .add_h3(origin.clone(), m, true)
                    .expect("lease requested"),
            ),
        }
    }

    /// Opens one connection to `origin` and parks it in the pool.
    pub async fn preconnect(&self, origin: &Origin) -> Result<(), SourceError> {
        let c = match self.alt_svc.get(origin) {
            Some(alt) if origin.is_https() && self.cfg.http3 => {
                self.connect_h3_or_tcp(origin, &alt).await?
            }
            _ => self.connect_tcp(origin).await?,
        };
        match c {
            NewConn::H1(c) => self.pool.put_h1(origin.clone(), c),
            NewConn::H2(m) => drop(self.pool.add_h2(origin.clone(), m, false)),
            NewConn::H3(m) => drop(self.pool.add_h3(origin.clone(), m, false)),
        }
        Ok(())
    }

    // ------------------------------------------------------------ TCP

    async fn connect_tcp(&self, origin: &Origin) -> Result<NewConn, SourceError> {
        let ips = self.dns.resolve(origin.host()).await?;
        let addrs: Vec<SocketAddr> = ips
            .iter()
            .map(|ip| SocketAddr::new(*ip, origin.port()))
            .collect();
        let what = format!("connecting to {origin}");
        let (tcp, addr) = connect::tcp(
            &addrs,
            self.cfg.happy_eyeballs_delay,
            self.cfg.connect_timeout,
            &what,
        )
        .await?;
        self.dns.connected(origin.host(), addr.ip());
        if !origin.is_https() {
            return if self.cfg.h2c_prior_knowledge {
                self.handshake_h2(tcp, origin).await
            } else {
                self.handshake_h1(tcp, origin).await
            };
        }
        let connector = self.tls.tcp()?;
        let name =
            rustls::pki_types::ServerName::try_from(origin.host().to_string()).map_err(|e| {
                SourceError::Fatal(format!("invalid TLS server name {}: {e}", origin.host()))
            })?;
        let tls = match tokio::time::timeout(self.cfg.connect_timeout, connector.connect(name, tcp))
            .await
        {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                return Err(transport_error(
                    cause_of(&e),
                    Phase::Connect,
                    format!("TLS handshake with {origin}: {}", describe(&e)),
                ));
            }
            Err(_) => {
                return Err(SourceError::Transient(format!(
                    "TLS handshake with {origin} timed out"
                )));
            }
        };
        if tls.get_ref().1.alpn_protocol() == Some(b"h2") {
            self.handshake_h2(tls, origin).await
        } else {
            self.handshake_h1(tls, origin).await
        }
    }

    async fn handshake_h1<T>(&self, io: T, origin: &Origin) -> Result<NewConn, SourceError>
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (sender, conn) = hyper::client::conn::http1::Builder::new()
            .max_buf_size(H1_MAX_BUF)
            .handshake(TokioIo::new(io))
            .await
            .map_err(|e| handshake_error(origin, &e))?;
        let task = tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::trace!(error = %e, "http/1.1 connection ended");
            }
        });
        Ok(NewConn::H1(H1Conn::new(sender, task.abort_handle())))
    }

    async fn handshake_h2<T>(&self, io: T, origin: &Origin) -> Result<NewConn, SourceError>
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (sender, conn) = hyper::client::conn::http2::Builder::new(TokioExecutor::new())
            .timer(TokioTimer::new())
            .initial_stream_window_size(H2_STREAM_WINDOW)
            .initial_connection_window_size(H2_CONN_WINDOW)
            .max_frame_size(H2_MAX_FRAME)
            .handshake(TokioIo::new(io))
            .await
            .map_err(|e| handshake_error(origin, &e))?;
        let closed = Arc::new(AtomicBool::new(false));
        let flag = closed.clone();
        let task = tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::trace!(error = %e, "http/2 connection ended");
            }
            flag.store(true, Ordering::Release);
        });
        Ok(NewConn::H2(Mux::new(
            sender,
            closed,
            task.abort_handle(),
            None,
            None,
        )))
    }

    async fn send_h1(
        &self,
        url: &Url,
        origin: &Origin,
        req: &Outgoing,
        mut c: H1Conn,
    ) -> Result<Response, Failure> {
        match tokio::time::timeout(READY_WAIT, c.sender.ready()).await {
            Ok(Ok(())) => {}
            // A dead pooled connection: nothing was sent.
            _ if c.reused => return Err(Failure::Unsent),
            Ok(Err(e)) => return Err(Failure::Error(head_error(&e))),
            Err(_) => {
                return Err(Failure::Error(SourceError::Transient(
                    "connection not ready".into(),
                )));
            }
        }
        let path = &url[url::Position::BeforePath..url::Position::AfterQuery];
        let mut r = Request::builder()
            .method(req.method.clone())
            .uri(if path.is_empty() { "/" } else { path })
            .version(Version::HTTP_11)
            .body(Full::new(req.body.clone()))
            .map_err(|e| Failure::Error(SourceError::Fatal(format!("building request: {e}"))))?;
        *r.headers_mut() = req.headers.clone();
        let host = HeaderValue::from_str(&origin.authority())
            .map_err(|e| Failure::Error(SourceError::Fatal(format!("invalid host: {e}"))))?;
        r.headers_mut().insert(header::HOST, host);
        let reused = c.reused;
        let sent = tokio::time::timeout(self.cfg.idle_timeout, c.sender.try_send_request(r)).await;
        match sent {
            Err(_) => Err(Failure::Error(self.head_timeout())),
            Ok(Ok(resp)) => {
                let (parts, body) = resp.into_parts();
                Ok(Response {
                    status: parts.status,
                    version: parts.version,
                    headers: parts.headers,
                    body: RespBody::hyper(
                        body,
                        Lease::H1 {
                            conn: Some(c),
                            origin: origin.clone(),
                            pool: Arc::downgrade(&self.pool),
                            complete: false,
                        },
                    ),
                })
            }
            Ok(Err(mut e)) => {
                if reused && e.take_message().is_some() {
                    Err(Failure::Unsent)
                } else {
                    Err(Failure::Error(head_error(e.error())))
                }
            }
        }
    }

    async fn send_h2(
        &self,
        url: &Url,
        req: &Outgoing,
        lease: MuxLease<H2Sender>,
    ) -> Result<Response, Failure> {
        let mut sender = lease.conn.sender.clone();
        if sender.ready().await.is_err() {
            lease.retire();
            return Err(Failure::Unsent);
        }
        let mut r = Request::builder()
            .method(req.method.clone())
            .uri(request_uri(url))
            .version(Version::HTTP_2)
            .body(Full::new(req.body.clone()))
            .map_err(|e| Failure::Error(SourceError::Fatal(format!("building request: {e}"))))?;
        *r.headers_mut() = req.headers.clone();
        match tokio::time::timeout(self.cfg.idle_timeout, sender.try_send_request(r)).await {
            Err(_) => Err(Failure::Error(self.head_timeout())),
            Ok(Ok(resp)) => {
                let (parts, body) = resp.into_parts();
                Ok(Response {
                    status: parts.status,
                    version: parts.version,
                    headers: parts.headers,
                    body: RespBody::hyper(body, Lease::H2(lease)),
                })
            }
            Ok(Err(mut e)) => {
                lease.retire();
                if e.take_message().is_some() {
                    Err(Failure::Unsent)
                } else {
                    Err(Failure::Error(head_error(e.error())))
                }
            }
        }
    }

    fn head_timeout(&self) -> SourceError {
        SourceError::Transient(format!("no response within {:?}", self.cfg.idle_timeout))
    }

    // ------------------------------------------------------------ QUIC

    fn endpoint(&self, v6: bool) -> std::io::Result<quinn::Endpoint> {
        let slot = if v6 { &self.quic_v6 } else { &self.quic_v4 };
        let mut g = lock(slot);
        // One UDP socket per address family, shared by all QUIC
        // connections (as quinn intends); a socket per connection measured
        // no better on loopback.
        if let Some(e) = &*g {
            return Ok(e.clone());
        }
        let bind: SocketAddr = if v6 {
            (Ipv6Addr::UNSPECIFIED, 0).into()
        } else {
            (Ipv4Addr::UNSPECIFIED, 0).into()
        };
        let socket = std::net::UdpSocket::bind(bind)?;
        let s = socket2::SockRef::from(&socket);
        let _ = s.set_recv_buffer_size(UDP_BUF);
        let _ = s.set_send_buffer_size(UDP_BUF);
        let ep = quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            None,
            socket,
            Arc::new(quinn::TokioRuntime),
        )?;
        *g = Some(ep.clone());
        Ok(ep)
    }

    async fn connect_h3(
        &self,
        origin: &Origin,
        alt: &H3Alt,
    ) -> Result<Arc<Mux<H3Sender>>, SourceError> {
        let host = alt.host.as_deref().unwrap_or(origin.host());
        let ips = self.dns.resolve(host).await?;
        let addrs: Vec<SocketAddr> = ips
            .iter()
            .map(|ip| SocketAddr::new(*ip, alt.port))
            .collect();
        let cfg = self.tls.quic()?;
        let server_name = origin.host().to_string();
        let what = format!("QUIC to {origin}");
        let (conn, addr) = connect::race(
            &addrs,
            self.cfg.happy_eyeballs_delay,
            self.cfg.connect_timeout,
            &what,
            |a| {
                let cfg = cfg.clone();
                let name = server_name.clone();
                async move {
                    let ep = self
                        .endpoint(a.is_ipv6())
                        .map_err(|e| AttemptError::io(&e))?;
                    let connecting = ep.connect_with(cfg, a, &name).map_err(|e| AttemptError {
                        cause: Cause::Other,
                        message: e.to_string(),
                    })?;
                    connecting.await.map_err(|e| AttemptError {
                        cause: quic_cause(&e),
                        message: e.to_string(),
                    })
                }
            },
        )
        .await?;
        self.dns.connected(host, addr.ip());
        let violation: Violation = Arc::new(OnceLock::new());
        let h3conn = GuardedConn::new(conn.clone(), violation.clone());
        let mut builder = h3::client::builder();
        builder.max_field_section_size(MAX_FIELD_SECTION);
        let (mut driver, sender) =
            match tokio::time::timeout(self.cfg.connect_timeout, builder.build(h3conn)).await {
                Ok(Ok(x)) => x,
                Ok(Err(e)) => {
                    return Err(SourceError::Transient(format!(
                        "HTTP/3 setup with {origin}: {e}"
                    )));
                }
                Err(_) => {
                    return Err(SourceError::Transient(format!(
                        "HTTP/3 setup with {origin} timed out"
                    )));
                }
            };
        let closed = Arc::new(AtomicBool::new(false));
        let flag = closed.clone();
        let task = tokio::spawn(async move {
            let e = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
            tracing::trace!(error = %e, "http/3 connection ended");
            flag.store(true, Ordering::Release);
        });
        Ok(Mux::new(
            sender,
            closed,
            task.abort_handle(),
            Some(conn),
            Some(violation),
        ))
    }

    /// HTTP/3 first; TCP joins the race after `h3_fallback_delay`, or at
    /// once if QUIC fails. If TCP wins or QUIC fails, HTTP/3 is not tried
    /// for this origin for a while.
    async fn connect_h3_or_tcp(
        &self,
        origin: &Origin,
        alt: &H3Alt,
    ) -> Result<NewConn, SourceError> {
        // Both futures are lazy: TCP does nothing until first polled.
        let mut quic = Box::pin(self.connect_h3(origin, alt));
        let mut tcp = Box::pin(self.connect_tcp(origin));
        let mut delay = Box::pin(tokio::time::sleep(self.cfg.h3_fallback_delay));
        let (mut quic_live, mut tcp_started, mut tcp_live) = (true, false, true);
        let mut tcp_err: Option<SourceError> = None;
        std::future::poll_fn(|cx| {
            if quic_live && let Poll::Ready(r) = quic.as_mut().poll(cx) {
                quic_live = false;
                match r {
                    Ok(m) => return Poll::Ready(Ok(NewConn::H3(m))),
                    Err(e) => {
                        tracing::debug!(%origin, error = %e, "HTTP/3 failed; using TCP");
                        self.alt_svc.mark_broken(origin);
                        if let Some(te) = tcp_err.take() {
                            return Poll::Ready(Err(te));
                        }
                    }
                }
            }
            if !tcp_started && (!quic_live || delay.as_mut().poll(cx).is_ready()) {
                tcp_started = true;
            }
            if tcp_started
                && tcp_live
                && let Poll::Ready(r) = tcp.as_mut().poll(cx)
            {
                tcp_live = false;
                match r {
                    Ok(c) => {
                        if quic_live {
                            tracing::debug!(%origin, "TCP won the race against HTTP/3");
                            self.alt_svc.mark_broken(origin);
                        }
                        return Poll::Ready(Ok(c));
                    }
                    Err(e) if !quic_live => return Poll::Ready(Err(e)),
                    // QUIC may still make it; keep waiting for it.
                    Err(e) => tcp_err = Some(e),
                }
            }
            Poll::Pending
        })
        .await
    }

    async fn send_h3(
        &self,
        url: &Url,
        req: &Outgoing,
        lease: MuxLease<H3Sender>,
    ) -> Result<Response, Failure> {
        let mut sender = lease.conn.sender.clone();
        let mut b = http::Request::builder()
            .method(req.method.clone())
            .uri(request_uri(url));
        if let Some(h) = b.headers_mut() {
            *h = req.headers.clone();
        }
        let r = b
            .body(())
            .map_err(|e| Failure::Error(SourceError::Fatal(format!("building request: {e}"))))?;
        let exchange = async {
            let mut stream = sender.send_request(r).await.map_err(|e| (true, e))?;
            if !req.body.is_empty() {
                stream
                    .send_data(req.body.clone())
                    .await
                    .map_err(|e| (false, e))?;
            }
            stream.finish().await.map_err(|e| (false, e))?;
            let resp = stream.recv_response().await.map_err(|e| (false, e))?;
            Ok::<_, (bool, h3::error::StreamError)>((resp, stream))
        };
        match tokio::time::timeout(self.cfg.idle_timeout, exchange).await {
            Err(_) => Err(Failure::Error(self.head_timeout())),
            Ok(Ok((resp, stream))) => {
                let (parts, ()) = resp.into_parts();
                Ok(Response {
                    status: parts.status,
                    version: Version::HTTP_3,
                    headers: parts.headers,
                    body: RespBody::h3(stream, Lease::H3(lease)),
                })
            }
            Ok(Err((_, _))) if lease.conn.violation().is_some() => {
                lease.retire();
                Err(Failure::Error(SourceError::Fatal(format!(
                    "HTTP/3 server exceeded a limit: {}",
                    lease.conn.violation().unwrap_or_default()
                ))))
            }
            // The stream could not even be opened: nothing was sent.
            Ok(Err((true, _))) => {
                lease.retire();
                Err(Failure::Unsent)
            }
            Ok(Err((false, e))) => {
                lease.retire();
                // h3's error variants are not matchable outside the crate.
                let text = e.to_string();
                let cause = if text.contains("Timeout") || text.contains("timed out") {
                    Cause::Timeout
                } else if text.contains("Header too big") {
                    Cause::Other
                } else {
                    Cause::Refused
                };
                Err(Failure::Error(transport_error(
                    cause,
                    Phase::Head,
                    format!("HTTP/3: {e}"),
                )))
            }
        }
    }
}

/// Why one attempt to send failed.
enum Failure {
    /// The request never left (a dead pooled connection): safe to send on
    /// a new connection.
    Unsent,
    Error(SourceError),
}

fn request_uri(url: &Url) -> &str {
    // Absolute form without the fragment (`:scheme`, `:authority`, `:path`).
    &url[..url::Position::AfterQuery]
}

fn handshake_error(origin: &Origin, e: &hyper::Error) -> SourceError {
    transport_error(
        cause_of(e),
        Phase::Connect,
        format!("HTTP handshake with {origin}: {}", describe(e)),
    )
}

fn head_error(e: &hyper::Error) -> SourceError {
    transport_error(cause_of(e), Phase::Head, describe(e))
}

fn quic_cause(e: &quinn::ConnectionError) -> Cause {
    use quinn::ConnectionError::*;
    match e {
        TimedOut => Cause::Timeout,
        ConnectionClosed(_) | ApplicationClosed(_) | Reset | LocallyClosed => Cause::Refused,
        _ => Cause::Other,
    }
}
