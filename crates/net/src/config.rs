use std::time::Duration;

/// `User-Agent` sent when the [`RequestContext`](vayu_proto::RequestContext)
/// has none: an honest product token, like `curl/8.x` or `Wget/1.x`. When
/// a download comes from the browser extension, the context carries the
/// browser's own `User-Agent` and that is sent instead, so sites that
/// serve different files per browser behave as they did in the browser.
pub const DEFAULT_USER_AGENT: &str = concat!("vayu/", env!("CARGO_PKG_VERSION"));

/// Settings for [`HttpSourceFactory`](crate::HttpSourceFactory).
///
/// Construct with `NetConfig { field: value, ..Default::default() }`.
#[derive(Debug, Clone)]
pub struct NetConfig {
    /// Default `User-Agent`. Default [`DEFAULT_USER_AGENT`].
    pub user_agent: String,
    /// Limit for one connection attempt (one address: TCP connect, then the
    /// TLS handshake; or the QUIC handshake). Default 10 s.
    pub connect_timeout: Duration,
    /// Happy Eyeballs (RFC 8305) "Connection Attempt Delay": how long one
    /// address gets before the next one is tried in parallel. Default
    /// 250 ms (the RFC's recommendation).
    pub happy_eyeballs_delay: Duration,
    /// Backstop for a silent server: no response head, or no body bytes,
    /// for this long fails the request with `Transient`. Stall detection is
    /// the engine's job (contract point 5); keep this ≥ 30 s. Default 60 s.
    pub idle_timeout: Duration,
    /// How long an unused connection (kept alive after a request, or opened
    /// by `preconnect`) stays in the pool. Default 30 s.
    pub pool_idle_timeout: Duration,
    /// Redirects followed per request. Default 10.
    pub max_redirects: u32,
    /// Offer HTTP/2 in TLS ALPN. Default true.
    pub http2: bool,
    /// Use HTTP/3 for `https` origins that advertise `h3` in `Alt-Svc`.
    /// Default true.
    pub http3: bool,
    /// Speak HTTP/2 without TLS (h2c, prior knowledge) to `http://` URLs.
    /// Real servers rarely support it; for tests and benchmarks. Default
    /// false.
    pub h2c_prior_knowledge: bool,
    /// Concurrent requests on one HTTP/2 or HTTP/3 connection before
    /// another connection is opened. One connection shares one congestion
    /// window (and one per-connection server cap), so the default, 1, gives
    /// each concurrent `open` its own connection and makes the engine's
    /// learned connection count mean connections. Raise it to multiplex.
    pub max_streams_per_connection: usize,
    /// When HTTP/3 is tried, how long QUIC gets before a TCP connection is
    /// raced against it. Default 300 ms.
    pub h3_fallback_delay: Duration,
    /// Connections `preconnect` opens. Default 1.
    pub preconnect_connections: usize,
    /// Extra trusted root certificates (DER), on top of the OS store; for
    /// tests (`lab.cert_der()`) and private CAs.
    pub extra_root_certs: Vec<Vec<u8>>,
    /// Trust the operating system's root store (via
    /// `rustls-platform-verifier`). Default true. Tests set false to trust
    /// only `extra_root_certs`.
    pub platform_roots: bool,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            user_agent: DEFAULT_USER_AGENT.to_string(),
            connect_timeout: Duration::from_secs(10),
            happy_eyeballs_delay: Duration::from_millis(250),
            idle_timeout: Duration::from_secs(60),
            pool_idle_timeout: Duration::from_secs(30),
            max_redirects: 10,
            http2: true,
            http3: true,
            h2c_prior_knowledge: false,
            max_streams_per_connection: 1,
            h3_fallback_delay: Duration::from_millis(300),
            preconnect_connections: 1,
            extra_root_certs: Vec::new(),
            platform_roots: true,
        }
    }
}
