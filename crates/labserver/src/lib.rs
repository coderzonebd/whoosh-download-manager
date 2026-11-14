//! `whoosh-labserver`: a deterministic, fault-injecting HTTP test server.
//!
//! The engine's Phase 1 tests and the benchmark harness run against it. It
//! serves virtual files whose bytes are a pure function of `(seed, offset)`
//! ([`VirtualFile`]), over HTTP/1.1 and HTTP/2 (h2c prior knowledge on the
//! plain port; ALPN `h2`/`http/1.1` on the TLS port with a self-signed
//! certificate) and, opt-in, HTTP/3 over QUIC ([`LabConfig::h3_port`],
//! advertised with `Alt-Svc`). It misbehaves on demand per [`Scenario`]:
//! latency, per-connection and global bandwidth caps, per-IP concurrency
//! caps, connection resets, stalls, redirect chains, expiring signed URLs,
//! cookie/referer gates, missing ranges/lengths/validators, changing files
//! and odd `Content-Disposition` headers.
//!
//! ```no_run
//! # async fn demo() -> Result<(), whoosh_labserver::LabError> {
//! use whoosh_labserver::{ByteRange, LabConfig, LabServer};
//!
//! let lab = LabServer::start(LabConfig::default()).await?;
//! let url = lab.url("flaky", "100m.bin"); // http://127.0.0.1:<port>/s/flaky/100m.bin
//! // ... download it with the engine ...
//! let expected = lab.file("flaky", "100m.bin").unwrap();
//! let _first_kib = expected.bytes(ByteRange::new(0, 1024).unwrap());
//! let stats = lab.stats("flaky").unwrap();
//! println!("{url}: {} resets, peak {} connections", stats.resets_injected, stats.max_client_peak_connections());
//! # Ok(()) }
//! ```

use http::HeaderValue;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

mod body;
mod bucket;
mod conn;
pub mod content;
mod control;
mod handler;
mod quic;
pub mod scenario;
mod server;
pub mod sign;
mod state;
pub mod stats;
mod tls;
pub mod units;

pub use content::VirtualFile;
/// Application error code and reason of an injected reset on HTTP/3 (the
/// QUIC connection is closed with them; see the README).
pub use quic::{RESET_CODE as H3_RESET_CODE, RESET_REASON as H3_RESET_REASON};
pub use scenario::{FileSpec, Scenario, ScenarioError};
pub use stats::{ClientStats, ScenarioStats};
pub use whoosh_core::ByteRange;

use state::Shared;

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Errors starting the server.
#[derive(Debug, thiserror::Error)]
pub enum LabError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Scenario(#[from] ScenarioError),
    #[error("TLS setup: {0}")]
    Tls(String),
    #[error("{0}")]
    Config(String),
}

/// How to start a [`LabServer`].
#[derive(Debug, Clone)]
pub struct LabConfig {
    /// Address to bind all listeners to. Default `127.0.0.1`.
    pub bind: IpAddr,
    /// Plain HTTP/1.1 + h2c port; 0 (default) picks a free port.
    pub http_port: u16,
    /// HTTPS port; `None` disables TLS. Default `Some(0)` (free port).
    pub tls_port: Option<u16>,
    /// HTTP/3 (QUIC) UDP port; `None` (default) disables HTTP/3. `Some(0)`
    /// uses the same number as the TLS port (any free port if TLS is off).
    /// When on, both TCP listeners send `Alt-Svc: h3=":<port>"`.
    pub h3_port: Option<u16>,
    /// Scenarios to serve. Default: the built-in set ([`scenario::builtin`]).
    pub scenarios: Vec<Scenario>,
    /// Memory for cached content blocks. Default 128 MiB.
    pub cache_bytes: usize,
    /// Names in the self-signed certificate. Default `localhost`,
    /// `127.0.0.1`, `::1` (plus the bind address).
    pub cert_names: Vec<String>,
}

impl Default for LabConfig {
    fn default() -> Self {
        Self {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            http_port: 0,
            tls_port: Some(0),
            h3_port: None,
            scenarios: scenario::builtin(),
            cache_bytes: 128 << 20,
            cert_names: vec!["localhost".into(), "127.0.0.1".into(), "::1".into()],
        }
    }
}

impl LabConfig {
    /// Default settings serving only `scenarios`.
    pub fn with_scenarios(scenarios: Vec<Scenario>) -> Self {
        Self {
            scenarios,
            ..Self::default()
        }
    }

    /// Adds (or replaces, by name) a scenario.
    pub fn scenario(mut self, sc: Scenario) -> Self {
        self.scenarios.retain(|s| s.name != sc.name);
        self.scenarios.push(sc);
        self
    }
}

/// A running server. Dropping it stops the listeners and closes every
/// connection.
#[derive(Debug)]
pub struct LabServer {
    http: SocketAddr,
    https: Option<SocketAddr>,
    h3: Option<SocketAddr>,
    shared: Arc<Shared>,
    cert_der: Option<Vec<u8>>,
    shutdown: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl LabServer {
    /// Binds the listeners and starts serving on the current tokio runtime.
    pub async fn start(config: LabConfig) -> Result<Self, LabError> {
        let tls = match config.tls_port.or(config.h3_port) {
            Some(_) => {
                let mut names = config.cert_names.clone();
                let bind = config.bind.to_string();
                if !config.bind.is_unspecified() && !names.contains(&bind) {
                    names.push(bind);
                }
                Some(tls::self_signed(&names)?)
            }
            None => None,
        };
        let shared = Arc::new(Shared::new(
            config.scenarios,
            config.cache_bytes,
            tls.as_ref().map(|t| t.cert_pem.clone()),
        )?);
        let (tx, rx) = watch::channel(false);
        let mut tasks = Vec::new();

        let listener = TcpListener::bind((config.bind, config.http_port)).await?;
        let http = listener.local_addr()?;
        let (tls_listener, endpoint) = match &tls {
            Some(t) => bind_tls_and_h3(config.bind, config.tls_port, config.h3_port, t).await?,
            None => (None, None),
        };
        let h3 = endpoint
            .as_ref()
            .map(quinn::Endpoint::local_addr)
            .transpose()?;
        let alt_svc = h3.and_then(|a| HeaderValue::from_str(&format!("h3=\":{}\"", a.port())).ok());

        tasks.push(tokio::spawn(server::accept_loop(
            listener,
            None,
            alt_svc.clone(),
            shared.clone(),
            rx.clone(),
        )));
        let mut https = None;
        if let (Some(listener), Some(t)) = (tls_listener, &tls) {
            https = Some(listener.local_addr()?);
            tasks.push(tokio::spawn(server::accept_loop(
                listener,
                Some(t.acceptor.clone()),
                alt_svc,
                shared.clone(),
                rx.clone(),
            )));
        }
        if let Some(ep) = endpoint {
            tasks.push(tokio::spawn(quic::accept_loop(ep, shared.clone(), rx)));
        }
        Ok(Self {
            http,
            https,
            h3,
            shared,
            cert_der: tls.map(|t| t.cert_der),
            shutdown: tx,
            tasks,
        })
    }

    /// The plain (HTTP/1.1 + h2c) listener.
    pub fn http_addr(&self) -> SocketAddr {
        self.http
    }

    /// The TLS listener, if enabled.
    pub fn https_addr(&self) -> Option<SocketAddr> {
        self.https
    }

    /// The HTTP/3 (QUIC, UDP) endpoint, if enabled.
    pub fn h3_addr(&self) -> Option<SocketAddr> {
        self.h3
    }

    /// `http://<addr>/s/<scenario>/<file>`.
    pub fn url(&self, scenario: &str, file: &str) -> String {
        format!("http://{}/s/{scenario}/{file}", self.http)
    }

    /// `https://localhost:<port>/s/<scenario>/<file>`, if TLS is enabled.
    pub fn https_url(&self, scenario: &str, file: &str) -> Option<String> {
        self.https
            .map(|a| format!("https://localhost:{}/s/{scenario}/{file}", a.port()))
    }

    /// `https://localhost:<udp port>/s/<scenario>/<file>`, if HTTP/3 is
    /// enabled. The same as [`LabServer::https_url`] when the ports match.
    pub fn h3_url(&self, scenario: &str, file: &str) -> Option<String> {
        self.h3
            .map(|a| format!("https://localhost:{}/s/{scenario}/{file}", a.port()))
    }

    /// The self-signed certificate (DER) clients must trust for HTTPS and
    /// HTTP/3.
    pub fn cert_der(&self) -> Option<&[u8]> {
        self.cert_der.as_deref()
    }

    /// The same certificate as PEM.
    pub fn cert_pem(&self) -> Option<&str> {
        self.shared.cert_pem.as_deref()
    }

    /// Names of the scenarios being served.
    pub fn scenario_names(&self) -> Vec<String> {
        self.shared
            .scenarios
            .iter()
            .map(|rt| rt.sc.name.clone())
            .collect()
    }

    /// The scenario as loaded.
    pub fn scenario(&self, name: &str) -> Option<&Scenario> {
        self.shared.scenario(name).map(|rt| &rt.sc)
    }

    /// Expected content of version 0 of a file.
    pub fn file(&self, scenario: &str, file: &str) -> Option<VirtualFile> {
        Some(self.shared.scenario(scenario)?.file(file)?.base)
    }

    /// The version a request for this file would get right now (always 0
    /// unless the scenario has `[changing]`).
    pub fn current_version(&self, scenario: &str, file: &str) -> Option<u64> {
        let rt = self.shared.scenario(scenario)?;
        Some(rt.current_version(rt.file(file)?))
    }

    /// The strong `ETag` the server sends for `version` of a file.
    pub fn etag(&self, scenario: &str, file: &str, version: u64) -> Option<String> {
        Some(state::etag(self.file(scenario, file)?.version(version)))
    }

    /// Snapshot of a scenario's counters.
    pub fn stats(&self, scenario: &str) -> Option<ScenarioStats> {
        Some(self.shared.scenario(scenario)?.stats.snapshot())
    }

    /// Resets counters, fault budgets and changing-file state of one
    /// scenario, or of all when `None`. Peaks restart from what is open now.
    pub fn reset(&self, scenario: Option<&str>) {
        for rt in &self.shared.scenarios {
            if scenario.is_none_or(|s| s == rt.sc.name) {
                rt.reset();
            }
        }
    }

    /// A freshly signed path (`/s/<sc>/<file>?expires=…&sig=…`), valid for
    /// `ttl` or the scenario's `signed_url.ttl_secs`.
    pub fn mint_path(&self, scenario: &str, file: &str, ttl: Option<Duration>) -> Option<String> {
        let rt = self.shared.scenario(scenario)?;
        rt.file(file)?;
        Some(rt.mint(file, ttl).0)
    }

    /// [`LabServer::mint_path`] as a full `http://` URL.
    pub fn mint_url(&self, scenario: &str, file: &str, ttl: Option<Duration>) -> Option<String> {
        Some(format!(
            "http://{}{}",
            self.http,
            self.mint_path(scenario, file, ttl)?
        ))
    }

    /// Stops accepting, closes all connections and waits for the listeners.
    pub async fn shutdown(mut self) {
        let _ = self.shutdown.send(true);
        for t in self.tasks.drain(..) {
            let _ = t.await;
        }
    }
}

/// Binds the TLS listener and the HTTP/3 endpoint, as configured. With
/// `h3_port` 0 the endpoint takes the TLS port's number; if the OS picked
/// that port and the UDP port of the same number cannot be had, both are
/// rebound.
///
/// "Cannot be had" is either taken (`AddrInUse`) or, on Windows, refused
/// with `WSAEACCES` (`PermissionDenied`): Windows reserves blocks of ports
/// per protocol (Hyper-V, WinNAT and Docker do, and the CI runners have
/// such blocks; `netsh int ipv4 show excludedportrange protocol=udp` lists
/// them). A TCP port the OS handed out can fall inside a UDP reservation,
/// and binding UDP there fails with 10013 rather than "in use".
async fn bind_tls_and_h3(
    bind: IpAddr,
    tls_port: Option<u16>,
    h3_port: Option<u16>,
    tls: &tls::Tls,
) -> Result<(Option<TcpListener>, Option<quinn::Endpoint>), LabError> {
    let mut attempts = 0;
    loop {
        let listener = match tls_port {
            Some(port) => Some(TcpListener::bind((bind, port)).await?),
            None => None,
        };
        let Some(h3_port) = h3_port else {
            return Ok((listener, None));
        };
        let udp_port = match (h3_port, &listener) {
            (0, Some(l)) => l.local_addr()?.port(),
            (p, _) => p,
        };
        match quic::endpoint(SocketAddr::new(bind, udp_port), tls.quic.clone()) {
            Ok(ep) => return Ok((listener, Some(ep))),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
                ) && h3_port == 0
                    && tls_port == Some(0)
                    && attempts < 20 =>
            {
                attempts += 1;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

impl Drop for LabServer {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}
