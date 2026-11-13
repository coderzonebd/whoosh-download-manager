//! `whoosh-net`: the engine's network layer.
//!
//! [`HttpSourceFactory`] implements [`whoosh_core::SourceFactory`] for `http`
//! and `https` URLs; the [`HttpSource`]s it makes implement
//! [`whoosh_core::RangeSource`] over HTTP/1.1, HTTP/2 and HTTP/3.
//!
//! ```no_run
//! # async fn demo() -> Result<(), whoosh_core::SourceError> {
//! use futures::StreamExt;
//! use whoosh_core::{ByteRange, OpenRequest, SourceFactory};
//! use whoosh_net::{HttpSourceFactory, NetConfig};
//!
//! let factory = HttpSourceFactory::new(NetConfig::default());
//! let source = factory.source("https://example.com/big.iso", None)?;
//! let meta = source.probe().await?;
//! let mut stream = source
//!     .open(OpenRequest {
//!         range: ByteRange::new(0, 1 << 20),
//!         expect: meta.validators(),
//!     })
//!     .await?;
//! while let Some(chunk) = stream.next().await {
//!     let _bytes = chunk?;
//! }
//! # Ok(()) }
//! ```
//!
//! # How the contract is met
//!
//! 1. **Concurrency.** Every `open` is its own request. HTTP/1.1 uses one
//!    connection per request in flight, kept alive and reused. HTTP/2 and
//!    HTTP/3 put at most [`NetConfig::max_streams_per_connection`] requests
//!    on one connection (default 1) and spread them least-loaded first.
//! 2. **Exact bytes.** A ranged request needs `206` with a `Content-Range`
//!    equal to the request (else `RangesUnsupported`); a short body is
//!    `Transient`, a long one is cut off at the end of the range.
//! 3. **Same version.** A strong ETag in `expect` is sent as `If-Range`;
//!    the response's ETag, Last-Modified and size are compared with
//!    `expect` before the first byte (`RemoteChanged`). A `200` to a
//!    ranged request is `RemoteChanged` when its validators differ (the
//!    `If-Range` did not match), else `RangesUnsupported`.
//! 4. **Cancellation.** Dropping the stream or the `open` future closes an
//!    HTTP/1.1 connection, resets an HTTP/2 stream, and stops an HTTP/3
//!    stream (`STOP_SENDING`).
//! 5. **No hidden retries.** Only redirects are followed, and addresses
//!    raced while connecting. A request whose pooled connection turned out
//!    to be closed before it was sent goes out once on a new connection;
//!    a cached redirect target that answers 401/403/404/410 is dropped and
//!    the chain followed again from the original URL.
//! 6. **Identity encoding.** `Accept-Encoding: identity` on every request.
//!
//! Errors: 401/403/410 → `LinkExpired`; 429/503 (with `Retry-After`) and
//! connections refused or reset before the response head → `Throttled`;
//! 408/5xx, timeouts, DNS failures and errors mid-body → `Transient`; 416
//! → `RemoteChanged`; other 4xx, bad URLs, bad certificates → `Fatal`.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use url::Url;
use whoosh_core::{RangeSource, SourceError, SourceFactory};
use whoosh_proto::RequestContext;

mod altsvc;
mod body;
mod classify;
mod client;
mod config;
mod connect;
pub mod filename;
mod h3guard;
mod origin;
mod pool;
mod request;
mod source;
mod tls;

pub use config::{DEFAULT_USER_AGENT, NetConfig};
pub use filename::{filename_from_content_disposition, sanitize_filename};
pub use pool::PoolStats;
pub use source::HttpSource;

use client::Client;
use origin::Origin;
use request::Template;

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Makes [`HttpSource`]s. All sources from one factory share its
/// connection pool, DNS cache, TLS session cache and `Alt-Svc` cache, so
/// make one factory per process.
#[derive(Debug)]
pub struct HttpSourceFactory {
    client: Arc<Client>,
}

impl HttpSourceFactory {
    /// A factory with `config`. Cheap: no I/O happens until the first
    /// request (the OS root store is loaded on the first `https` one).
    pub fn new(config: NetConfig) -> Arc<Self> {
        Arc::new(Self {
            client: Arc::new(Client::new(config)),
        })
    }

    /// Like [`SourceFactory::source`], returning the concrete type.
    pub fn http_source(
        &self,
        url: &str,
        request: Option<&RequestContext>,
    ) -> Result<Arc<HttpSource>, SourceError> {
        let mut url = Url::parse(url.trim())
            .map_err(|e| SourceError::Fatal(format!("invalid URL {url:?}: {e}")))?;
        Origin::parse_url(&url)?;
        let template = Template::new(&url, request, &self.client.cfg.user_agent)?;
        // Credentials became an Authorization header; never send them in
        // the request line. The fragment never goes on the wire either.
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_fragment(None);
        Ok(Arc::new(HttpSource::new(
            self.client.clone(),
            url,
            template,
        )))
    }

    /// Records that `url`'s origin serves HTTP/3 on UDP `port` of the same
    /// host, for `ttl`, as an `Alt-Svc: h3=":<port>"` response would. For
    /// host profiles: a host known to speak HTTP/3 is reached over QUIC
    /// from the first request. Ignored for `http://` URLs.
    pub fn remember_http3(&self, url: &str, port: u16, ttl: Duration) {
        if let Some(origin) = Url::parse(url.trim())
            .ok()
            .and_then(|u| Origin::parse_url(&u).ok())
        {
            self.client
                .alt_svc
                .remember(&origin, altsvc::H3Alt { host: None, port }, ttl);
        }
    }

    /// What the connection pool holds right now (all origins): open
    /// connections per protocol and requests in flight on multiplexed
    /// ones. For diagnostics and tests.
    pub fn pool_stats(&self) -> PoolStats {
        self.client.pool.stats()
    }

    /// The configuration in use.
    pub fn config(&self) -> &NetConfig {
        &self.client.cfg
    }
}

impl SourceFactory for HttpSourceFactory {
    fn source(
        &self,
        url: &str,
        request: Option<&RequestContext>,
    ) -> Result<Arc<dyn RangeSource>, SourceError> {
        self.http_source(url, request)
            .map(|s| s as Arc<dyn RangeSource>)
    }

    /// Resolves the host and opens up to
    /// [`NetConfig::preconnect_connections`] connections (TLS included,
    /// HTTP/3 when `Alt-Svc` for the origin is known), parked in the pool
    /// for [`NetConfig::pool_idle_timeout`]. Never fails.
    fn preconnect(&self, url: &str) -> BoxFuture<'_, ()> {
        let origin = Url::parse(url.trim())
            .ok()
            .and_then(|u| Origin::parse_url(&u).ok());
        async move {
            let Some(origin) = origin else { return };
            let want = self.client.cfg.preconnect_connections;
            let have = self.client.pool.warm(&origin);
            let opens = (have..want).map(|_| self.client.preconnect(&origin));
            for r in futures::future::join_all(opens).await {
                if let Err(e) = r {
                    tracing::debug!(%origin, error = %e, "preconnect failed");
                }
            }
        }
        .boxed()
    }
}
