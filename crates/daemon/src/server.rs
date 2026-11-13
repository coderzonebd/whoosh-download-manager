//! The listener: HTTP/1.1 on 127.0.0.1, `GET /healthz`, and the WebSocket
//! upgrade on `/rpc` (PROTOCOL.md §1).

use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http::header::{self, HeaderMap, HeaderValue};
use http::{Method, Request, Response, StatusCode};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, Semaphore, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};
use whoosh_proto::RPC_PATH;
use whoosh_proto::service::Service;

use crate::config::DaemonConfig;
use crate::limit::{Budget, Limited};
use crate::runtime::{self, InstanceLock, RuntimeInfo};
use crate::token::Token;
use crate::{ServeError, client, session};

/// Path of the unauthenticated health check.
pub const HEALTHZ_PATH: &str = "/healthz";

pub(crate) struct Shared {
    pub service: Arc<dyn Service>,
    pub token: Token,
    pub config: DaemonConfig,
    /// Sessions, handshakes and HTTP connections close when this turns true.
    pub shutdown: watch::Receiver<bool>,
    /// Set by `daemon.shutdown`; [`RunningDaemon::stop_requested`] waits on it.
    pub stop: watch::Sender<bool>,
    /// Slots for connections that have not completed `session.hello`.
    pub pending_hellos: Arc<Semaphore>,
    pub tracker: Tracker,
}

impl Shared {
    /// A client asked the daemon to stop (`daemon.shutdown`).
    pub fn request_stop(&self) {
        // `send` would drop the value when nobody waits yet.
        self.stop.send_replace(true);
    }
}

/// Counts live connections so shutdown can wait for them.
#[derive(Debug, Default)]
pub(crate) struct Tracker {
    live: AtomicUsize,
    idle: Notify,
}

pub(crate) struct TrackerGuard<'a>(&'a Tracker);

impl Tracker {
    pub fn enter(&self) -> TrackerGuard<'_> {
        self.live.fetch_add(1, Ordering::SeqCst);
        TrackerGuard(self)
    }

    async fn wait_idle(&self) {
        loop {
            let idle = self.idle.notified();
            if self.live.load(Ordering::SeqCst) == 0 {
                return;
            }
            idle.await;
        }
    }
}

impl Drop for TrackerGuard<'_> {
    fn drop(&mut self) {
        if self.0.live.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

/// A daemon serving a [`Service`]. Dropping it without calling
/// [`RunningDaemon::shutdown`] stops the listener but leaves `runtime.json`
/// behind (like a crash); call `shutdown` for a clean exit.
pub struct RunningDaemon {
    addr: SocketAddr,
    data_dir: PathBuf,
    info: RuntimeInfo,
    shared: Arc<Shared>,
    shutdown_tx: watch::Sender<bool>,
    stop_accepting: watch::Sender<bool>,
    accept: Option<JoinHandle<()>>,
    _lock: Option<InstanceLock>,
}

impl std::fmt::Debug for RunningDaemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningDaemon")
            .field("addr", &self.addr)
            .field("data_dir", &self.data_dir)
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl RunningDaemon {
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// `ws://127.0.0.1:<port>/rpc`
    pub fn rpc_url(&self) -> String {
        format!("ws://{}{RPC_PATH}", self.addr)
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn runtime_file(&self) -> PathBuf {
        runtime::runtime_file(&self.data_dir)
    }

    /// What was written to `runtime.json`, token included: handle with care.
    pub fn runtime_info(&self) -> &RuntimeInfo {
        &self.info
    }

    /// Resolves when a client has called `daemon.shutdown`. The binary waits
    /// for this or a signal, then calls [`RunningDaemon::shutdown`].
    pub async fn stop_requested(&self) {
        let mut rx = self.shared.stop.subscribe();
        let _ = rx.wait_for(|stop| *stop).await;
    }

    /// Clean shutdown (PROTOCOL.md `daemon.shutdown`): stop accepting
    /// connections, [`Service::shutdown`] (pause transfers, save state; the
    /// clients still see the resulting events), close every session with
    /// 1001 after what is already queued for it, then remove `runtime.json`.
    pub async fn shutdown(mut self) {
        tracing::info!("shutting down");
        let _ = self.stop_accepting.send(true);
        if let Some(accept) = self.accept.take() {
            let _ = accept.await;
        }
        self.shared.service.shutdown().await;
        let _ = self.shutdown_tx.send(true);
        if tokio::time::timeout(Duration::from_secs(5), self.shared.tracker.wait_idle())
            .await
            .is_err()
        {
            tracing::warn!("some connections did not close in time");
        }
        match runtime::remove_if_ours(&self.data_dir, &self.info) {
            Ok(true) => {}
            Ok(false) => tracing::warn!("runtime.json belongs to another daemon; left in place"),
            Err(e) => tracing::warn!(error = %e, "could not remove runtime.json"),
        }
        tracing::info!("stopped");
    }
}

impl Drop for RunningDaemon {
    fn drop(&mut self) {
        let _ = self.stop_accepting.send(true);
        let _ = self.shutdown_tx.send(true);
    }
}

/// Starts serving `service` as configured. See the crate docs for the order
/// of the single-instance check, the port choice and the runtime file.
pub async fn serve(
    service: Arc<dyn Service>,
    config: DaemonConfig,
) -> Result<RunningDaemon, ServeError> {
    claim(config).await?.serve(service).await
}

/// The data directory, secured and (with `single_instance`) locked for this
/// process, before anything is served. A binary that builds a real
/// [`Service`] (the engine loads and resumes downloads from this directory)
/// claims first and builds the service only once the claim succeeded, so a
/// second daemon never touches the first one's downloads.
#[derive(Debug)]
pub struct Claim {
    config: DaemonConfig,
    lock: Option<runtime::InstanceLock>,
}

/// Secures and locks the data directory (see [`Claim`]). Fails with
/// [`ServeError::AlreadyRunning`] when another daemon owns it.
pub async fn claim(config: DaemonConfig) -> Result<Claim, ServeError> {
    let data_dir = config.data_dir.clone();
    runtime::create_data_dir(&data_dir).map_err(|e| ServeError::DataDir(data_dir.clone(), e))?;
    runtime::secure_dir(&data_dir)
        .map_err(|reason| ServeError::InsecureDataDir(data_dir.clone(), reason))?;

    let lock = if config.single_instance {
        let lock =
            runtime::try_lock(&data_dir).map_err(|e| ServeError::DataDir(data_dir.clone(), e))?;
        let Some(lock) = lock else {
            let existing = runtime::read(&data_dir).ok().flatten();
            return Err(ServeError::AlreadyRunning {
                pid: existing.as_ref().map(|i| i.pid),
                port: existing.as_ref().map(|i| i.port),
            });
        };
        // Belt and braces: a daemon that does not hold our lock (another
        // build, another lock file) but answers hello still wins.
        if let Ok(Some(existing)) = runtime::read(&data_dir) {
            let token = Token::from_string(existing.token.clone());
            if client::probe(existing.port, &token, Duration::from_secs(2))
                .await
                .is_ok()
            {
                return Err(ServeError::AlreadyRunning {
                    pid: Some(existing.pid),
                    port: Some(existing.port),
                });
            }
            tracing::info!(
                stale_pid = existing.pid,
                "replacing a stale runtime.json (that daemon is gone)"
            );
        }
        Some(lock)
    } else {
        None
    };
    Ok(Claim { config, lock })
}

impl Claim {
    pub fn data_dir(&self) -> &std::path::Path {
        &self.config.data_dir
    }

    /// Binds, writes `runtime.json` and starts serving `service`.
    pub async fn serve(self, service: Arc<dyn Service>) -> Result<RunningDaemon, ServeError> {
        serve_claimed(self, service).await
    }
}

async fn serve_claimed(
    claim: Claim,
    service: Arc<dyn Service>,
) -> Result<RunningDaemon, ServeError> {
    let Claim { config, lock } = claim;
    let data_dir = config.data_dir.clone();
    let listener = bind(config.port, config.port_fallback).await?;
    let addr = listener.local_addr().map_err(ServeError::Bind)?;
    if addr.port() != config.port && config.port != 0 {
        tracing::warn!(
            wanted = config.port,
            port = addr.port(),
            "port in use; listening on a free port instead"
        );
    }

    let token = Token::generate().map_err(|e| ServeError::Rng(e.to_string()))?;
    let info = RuntimeInfo {
        port: addr.port(),
        token: token.expose().to_owned(),
        pid: std::process::id(),
        version: config.version.clone(),
    };
    runtime::write_atomic(&data_dir, &info)
        .map_err(|e| ServeError::RuntimeFile(runtime::runtime_file(&data_dir), e))?;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (stop_accepting, accepting_rx) = watch::channel(false);
    let pending_hellos = Arc::new(Semaphore::new(config.max_pending_hellos.max(1)));
    let shared = Arc::new(Shared {
        service,
        token,
        config,
        shutdown: shutdown_rx,
        stop: watch::Sender::new(false),
        pending_hellos,
        tracker: Tracker::default(),
    });
    let accept = tokio::spawn(accept_loop(listener, shared.clone(), accepting_rx));
    tracing::info!(%addr, data_dir = %data_dir.display(), "listening");

    Ok(RunningDaemon {
        addr,
        data_dir,
        info,
        shared,
        shutdown_tx,
        stop_accepting,
        accept: Some(accept),
        _lock: lock,
    })
}

async fn bind(port: u16, fallback: bool) -> Result<TcpListener, ServeError> {
    let loopback = |port| SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    match TcpListener::bind(loopback(port)).await {
        Ok(listener) => Ok(listener),
        Err(e) if fallback && port != 0 && e.kind() == std::io::ErrorKind::AddrInUse => {
            TcpListener::bind(loopback(0))
                .await
                .map_err(ServeError::Bind)
        }
        Err(e) => Err(ServeError::Bind(e)),
    }
}

async fn accept_loop(listener: TcpListener, shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _peer)) => {
                    tokio::spawn(serve_http(stream, shared.clone()));
                }
                Err(e) => {
                    // EMFILE and friends: back off instead of spinning.
                    tracing::warn!(error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
        }
    }
}

async fn serve_http(stream: TcpStream, shared: Arc<Shared>) {
    let _alive = shared.tracker.enter();
    let _ = stream.set_nodelay(true);
    let svc = {
        let shared = shared.clone();
        service_fn(move |req| {
            let shared = shared.clone();
            async move { Ok::<_, Infallible>(route(req, shared)) }
        })
    };
    let conn = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(Duration::from_secs(10))
        .max_buf_size(64 * 1024)
        .serve_connection(TokioIo::new(stream), svc)
        .with_upgrades();
    tokio::pin!(conn);
    let mut shutdown = shared.shutdown.clone();
    tokio::select! {
        _ = conn.as_mut() => {}
        _ = shutdown.changed() => {
            conn.as_mut().graceful_shutdown();
            let _ = tokio::time::timeout(Duration::from_secs(1), conn).await;
        }
    }
}

type Body = Full<Bytes>;

fn plain(status: StatusCode, body: &'static str) -> Response<Body> {
    let mut res = Response::new(Full::new(Bytes::from_static(body.as_bytes())));
    *res.status_mut() = status;
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

fn route(mut req: Request<Incoming>, shared: Arc<Shared>) -> Response<Body> {
    // DNS rebinding defence: a page on evil.example that resolves to
    // 127.0.0.1 sends `Host: evil.example`.
    if !host_allowed(req.headers()) {
        return plain(StatusCode::FORBIDDEN, "forbidden\n");
    }
    match req.uri().path() {
        HEALTHZ_PATH => match *req.method() {
            Method::GET | Method::HEAD => plain(StatusCode::OK, "ok"),
            _ => {
                let mut res = plain(StatusCode::METHOD_NOT_ALLOWED, "method not allowed\n");
                res.headers_mut()
                    .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
                res
            }
        },
        RPC_PATH => {
            if !origin_allowed(req.headers(), &shared.config.allowed_origins) {
                tracing::info!("rejected a WebSocket upgrade from a disallowed Origin");
                return plain(StatusCode::FORBIDDEN, "forbidden\n");
            }
            let key = match websocket_key(&req) {
                Ok(key) => key,
                Err(res) => return *res,
            };
            // Only a few connections may wait for their hello at once, so
            // unauthenticated peers cannot pile up buffers.
            let Ok(slot) = shared.pending_hellos.clone().try_acquire_owned() else {
                tracing::info!("too many connections waiting for session.hello; refusing one");
                let mut res = plain(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "too many connections waiting for session.hello\n",
                );
                res.headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
                return res;
            };
            let on_upgrade = hyper::upgrade::on(&mut req);
            let config = WebSocketConfig::default()
                .max_message_size(Some(shared.config.max_message_bytes))
                .max_frame_size(Some(shared.config.max_message_bytes));
            // Room for the hello plus frame headers and a few control frames.
            let budget = Budget::new(shared.config.max_hello_bytes + 1024);
            tokio::spawn(async move {
                match on_upgrade.await {
                    Ok(upgraded) => {
                        let io = Limited::new(TokioIo::new(upgraded), budget.clone());
                        let ws =
                            WebSocketStream::from_raw_socket(io, Role::Server, Some(config)).await;
                        session::run(ws, shared, budget, slot).await;
                    }
                    Err(e) => tracing::debug!(error = %e, "upgrade failed"),
                }
            });
            let mut res = Response::new(Full::new(Bytes::new()));
            *res.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
            let headers = res.headers_mut();
            headers.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
            headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
            if let Ok(accept) = HeaderValue::from_str(&key) {
                headers.insert(header::SEC_WEBSOCKET_ACCEPT, accept);
            }
            res
        }
        _ => plain(StatusCode::NOT_FOUND, "not found\n"),
    }
}

/// Validates the upgrade request; returns the `Sec-WebSocket-Accept` value.
fn websocket_key(req: &Request<Incoming>) -> Result<String, Box<Response<Body>>> {
    let headers = req.headers();
    let upgrade = req.method() == Method::GET
        && has_token(headers, header::CONNECTION, "upgrade")
        && has_token(headers, header::UPGRADE, "websocket");
    if !upgrade {
        let mut res = plain(StatusCode::UPGRADE_REQUIRED, "WebSocket upgrade required\n");
        res.headers_mut()
            .insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        return Err(Box::new(res));
    }
    if headers
        .get(header::SEC_WEBSOCKET_VERSION)
        .is_none_or(|v| v.as_bytes() != b"13")
    {
        let mut res = plain(
            StatusCode::UPGRADE_REQUIRED,
            "WebSocket version 13 required\n",
        );
        res.headers_mut().insert(
            header::SEC_WEBSOCKET_VERSION,
            HeaderValue::from_static("13"),
        );
        return Err(Box::new(res));
    }
    match headers.get(header::SEC_WEBSOCKET_KEY) {
        Some(key) if !key.is_empty() => Ok(derive_accept_key(key.as_bytes())),
        _ => Err(Box::new(plain(
            StatusCode::BAD_REQUEST,
            "missing Sec-WebSocket-Key\n",
        ))),
    }
}

fn has_token(headers: &HeaderMap, name: header::HeaderName, token: &str) -> bool {
    headers.get_all(name).iter().any(|v| {
        v.to_str()
            .is_ok_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)))
    })
}

/// PROTOCOL.md §1: no `Origin` (native clients) or one of the allow-list.
/// `Origin: null`, several `Origin` headers, or anything else is refused.
pub(crate) fn origin_allowed(headers: &HeaderMap, allowed: &[String]) -> bool {
    let mut origins = headers.get_all(header::ORIGIN).iter();
    match (origins.next(), origins.next()) {
        (None, _) => true,
        (Some(origin), None) => origin
            .to_str()
            .is_ok_and(|o| allowed.iter().any(|a| a.eq_ignore_ascii_case(o))),
        _ => false,
    }
}

/// The `Host` must name the loopback interface (any port).
pub(crate) fn host_allowed(headers: &HeaderMap) -> bool {
    let mut hosts = headers.get_all(header::HOST).iter();
    let (Some(host), None) = (hosts.next(), hosts.next()) else {
        return false;
    };
    let Ok(host) = host.to_str() else {
        return false;
    };
    let name = if host.starts_with('[') {
        host.split_inclusive(']').next().unwrap_or(host)
    } else {
        host.split(':').next().unwrap_or(host)
    };
    ["127.0.0.1", "localhost", "[::1]"]
        .iter()
        .any(|allowed| name.eq_ignore_ascii_case(allowed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn origins() {
        let allowed = crate::config::default_allowed_origins();
        let ok = |pairs: &[(&'static str, &str)]| origin_allowed(&headers(pairs), &allowed);
        assert!(ok(&[]));
        assert!(ok(&[("origin", "tauri://localhost")]));
        assert!(ok(&[("origin", "http://tauri.localhost")]));
        assert!(ok(&[("origin", "https://tauri.localhost")]));
        assert!(!ok(&[("origin", "null")]));
        assert!(!ok(&[("origin", "https://example.com")]));
        assert!(!ok(&[("origin", "http://127.0.0.1:47210")]));
        assert!(!ok(&[("origin", "chrome-extension://abc")]));
        assert!(!ok(&[("origin", "tauri://localhost.evil")]));
        assert!(!ok(&[
            ("origin", "tauri://localhost"),
            ("origin", "https://example.com")
        ]));
    }

    #[test]
    fn hosts() {
        let ok = |h: &str| host_allowed(&headers(&[("host", h)]));
        assert!(ok("127.0.0.1:47210"));
        assert!(ok("127.0.0.1"));
        assert!(ok("localhost:1"));
        assert!(ok("LOCALHOST"));
        assert!(ok("[::1]:47210"));
        assert!(!ok("evil.example:47210"));
        assert!(!ok("127.0.0.1.evil.example"));
        assert!(!ok("localhost.evil.example:80"));
        assert!(!host_allowed(&HeaderMap::new()));
    }
}
