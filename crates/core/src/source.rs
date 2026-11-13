//! Where bytes come from.
//!
//! A [`RangeSource`] is one way to fetch one remote file (HTTP/1.1/2/3 in
//! Phase 1; FTP, mirrors, LAN peers and web seeds later). The engine owns all
//! policy (how many connections, retries, hedging, backoff); a source only
//! performs requests and reports precisely what happened.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use whoosh_proto::RequestContext;

use crate::ByteRange;

/// The application protocol a source ended up using. Recorded in host
/// profiles so the next download from the same host starts on the best one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Http1,
    Http2,
    Http3,
    Other,
}

/// What identifies one *version* of the remote file. Every ranged request
/// is checked against these so bytes of two versions are never mixed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Validators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub size: Option<u64>,
}

impl Validators {
    /// Nothing to compare (the `no-validators` scenario). The engine then
    /// relies on tail verification when resuming.
    pub fn is_empty(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none() && self.size.is_none()
    }

    /// True when `seen` (taken from a later response) describes a different
    /// version: some field present on both sides differs. Fields missing on
    /// either side are not evidence of a change.
    pub fn conflicts_with(&self, seen: &Validators) -> bool {
        fn differ<T: PartialEq>(a: &Option<T>, b: &Option<T>) -> bool {
            matches!((a, b), (Some(a), Some(b)) if a != b)
        }
        differ(&self.etag, &seen.etag)
            || differ(&self.last_modified, &seen.last_modified)
            || differ(&self.size, &seen.size)
    }

    /// The ETag, if it is strong. Only a strong ETag may be sent as
    /// `If-Range` (RFC 9110 §13.1.5); a weak one is still compared.
    pub fn strong_etag(&self) -> Option<&str> {
        self.etag.as_deref().filter(|e| !e.starts_with("W/"))
    }
}

/// What a source learned about the remote file before downloading.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteMeta {
    /// Total size, when the server states it.
    pub size: Option<u64>,
    /// Ranged requests are honoured: a real `206` was seen, not just an
    /// `Accept-Ranges` header (the `lying-ranges` scenario advertises it and
    /// then ignores `Range`).
    pub accepts_ranges: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// File name from `Content-Disposition`, decoded (RFC 5987/6266) and
    /// sanitised: a single path component with no separators, no `..`, no
    /// control characters, and not a reserved Windows device name. `None`
    /// when the header is absent or unusable; the engine then falls back to
    /// the URL path.
    pub filename: Option<String>,
    pub content_type: Option<String>,
    /// URL after redirects. For display, file names and host profiles only:
    /// never build a source from it. When the redirect chain crossed origins
    /// the source dropped cookies and `Authorization` for the new origin,
    /// and a source built from `final_url` with the original
    /// `RequestContext` would send them there. Resume from the original URL.
    pub final_url: Option<String>,
    pub protocol: Option<Protocol>,
}

impl RemoteMeta {
    pub fn validators(&self) -> Validators {
        Validators {
            etag: self.etag.clone(),
            last_modified: self.last_modified.clone(),
            size: self.size,
        }
    }
}

/// Why a source failed. The variant tells the engine what to do next;
/// sources classify, the engine decides.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// Reset, timeout, DNS failure, 500/502/504, or a body that ended early:
    /// retry, possibly on another connection.
    #[error("transient network error: {0}")]
    Transient(String),
    /// 429, 503, or connections refused/reset under load: lower this host's
    /// connection count, and wait `retry_after` when the server gave one.
    #[error("server is limiting us: {message}")]
    Throttled {
        message: String,
        retry_after: Option<Duration>,
    },
    /// 401/403/410 or an expired signature: the link needs refreshing
    /// (`download.refreshUrl`, done automatically by the extension).
    #[error("link expired or forbidden: {0}")]
    LinkExpired(String),
    /// Validators no longer match (or 416 for a range that used to exist):
    /// the remote file changed since the download started.
    #[error("remote file changed")]
    RemoteChanged,
    /// The server answered a ranged request with `200` or with a different
    /// range than asked for. Fall back to one connection from the start.
    #[error("server does not support ranges")]
    RangesUnsupported,
    /// 404, other 4xx, unsupported scheme, invalid URL: retrying will not help.
    #[error("{0}")]
    Fatal(String),
}

impl SourceError {
    /// Stable code for [`whoosh_proto::ErrorInfo::code`].
    pub fn code(&self) -> &'static str {
        match self {
            Self::Transient(_) => "network",
            Self::Throttled { .. } => "throttled",
            Self::LinkExpired(_) => "link_expired",
            Self::RemoteChanged => "remote_changed",
            Self::RangesUnsupported => "ranges_unsupported",
            Self::Fatal(_) => "fatal",
        }
    }
}

/// One stream of bytes for one request. See [`RangeSource::open`] for the
/// guarantees a stream gives.
pub type ByteStream = BoxStream<'static, Result<Bytes, SourceError>>;

/// One request the engine wants made.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenRequest {
    /// `None` means the whole file from the start; used only when ranges are
    /// unsupported or the size is unknown.
    pub range: Option<ByteRange>,
    /// The version being downloaded: from the probe, or from the journal when
    /// resuming. The source must fail with [`SourceError::RemoteChanged`]
    /// rather than deliver bytes of another version.
    pub expect: Validators,
}

/// Anything that can deliver ranges of one remote file.
///
/// Contract (the engine's correctness depends on every point):
///
/// 1. **Concurrency.** The engine calls [`open`](Self::open) many times at
///    once (its learned connection count). Each call is an independent
///    request; how requests map onto connections (one TCP connection per
///    request on HTTP/1.1; multiplexed, but spread over several connections,
///    on HTTP/2 and HTTP/3) is the source's business.
/// 2. **Exact bytes or an error.** A stream yields exactly the requested
///    range, in order. The source checks the status (`206` for a ranged
///    request, else [`SourceError::RangesUnsupported`]), the `Content-Range`
///    (must equal the request, else `RangesUnsupported`), and the length (a
///    short body is [`SourceError::Transient`], a long one is cut off at the
///    end of the range). It never yields a byte from another offset.
/// 3. **Same version.** With a strong ETag in `expect`, send `If-Range`.
///    Always compare the response's validators with `expect`
///    ([`Validators::conflicts_with`]) before yielding the first byte; a
///    conflict is [`SourceError::RemoteChanged`].
/// 4. **Cancellation.** Dropping the stream, or the future from `open`,
///    aborts the request promptly and frees or resets the connection. The
///    engine relies on this for hedged requests.
/// 5. **No hidden retries.** Apart from following redirects and racing
///    addresses while connecting, a source does not retry: the engine needs
///    to see every failure to steer connection counts and hedging. A source
///    may enforce a generous idle timeout (≥ 30 s) as a backstop; stall
///    detection is the engine's job.
/// 6. **Identity encoding.** Request `Accept-Encoding: identity` so ranges
///    and sizes refer to the stored bytes.
pub trait RangeSource: Send + Sync {
    /// A short, stable label for logs and per-source statistics.
    fn label(&self) -> &str;

    /// Learn size, validators, range support and file name with the
    /// cheapest request that is reliable (e.g. `GET` with `Range: bytes=0-0`;
    /// some servers answer `HEAD` differently from `GET`).
    fn probe(&self) -> BoxFuture<'_, Result<RemoteMeta, SourceError>>;

    /// Start one request. Resolves once the response headers have been
    /// checked (points 2 and 3), so the time it takes is time-to-first-byte.
    fn open(&self, req: OpenRequest) -> BoxFuture<'_, Result<ByteStream, SourceError>>;

    /// Probe and start downloading in one request (e.g. `GET` with
    /// `Range: bytes=0-`): what [`RangeSource::probe`] would learn, plus the
    /// body from offset 0. A fresh download starts with this, so its first
    /// bytes arrive one round trip sooner than probe-then-open; on a small
    /// file or a high-latency link that round trip is most of the time.
    ///
    /// The body follows `open`'s contract (exact bytes from offset 0, cancel
    /// on drop, no hidden retries). It runs to the end of the entity: the
    /// engine stops reading where the range this connection still owns ends,
    /// after other connections took over the rest. When the server ignores
    /// the range (`200`), the body is the whole entity and `accepts_ranges`
    /// says whether later ranged requests may still work.
    ///
    /// The default probes, then opens the whole entity: correct, but two
    /// round trips. Network sources should override it.
    fn start(&self) -> BoxFuture<'_, Result<(RemoteMeta, ByteStream), SourceError>> {
        Box::pin(async move {
            let meta = self.probe().await?;
            let stream = self
                .open(OpenRequest {
                    range: None,
                    expect: meta.validators(),
                })
                .await?;
            Ok((meta, stream))
        })
    }
}

/// Builds sources from what the user or the browser handed us.
pub trait SourceFactory: Send + Sync {
    /// A source for `url`, requested as described by `request` (method,
    /// headers, cookie, referer, user agent, body), exactly as the browser
    /// would have. Cheap and synchronous: no network I/O. Unsupported
    /// schemes and invalid URLs are [`SourceError::Fatal`].
    ///
    /// Security: on a redirect to a different origin, the `Cookie` and
    /// `Authorization` headers are dropped (they were captured for the
    /// original origin).
    fn source(
        &self,
        url: &str,
        request: Option<&RequestContext>,
    ) -> Result<Arc<dyn RangeSource>, SourceError>;

    /// Resolve DNS and open (TLS) connections to `url`'s host ahead of a
    /// likely download (`net.preconnect`). Best effort; never fails.
    fn preconnect(&self, url: &str) -> BoxFuture<'_, ()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(etag: Option<&str>, lm: Option<&str>, size: Option<u64>) -> Validators {
        Validators {
            etag: etag.map(Into::into),
            last_modified: lm.map(Into::into),
            size,
        }
    }

    #[test]
    fn missing_fields_are_not_a_conflict() {
        let a = v(Some("\"x\""), None, Some(10));
        assert!(!a.conflicts_with(&v(None, Some("Tue"), None)));
        assert!(!a.conflicts_with(&Validators::default()));
    }

    #[test]
    fn any_differing_field_is_a_conflict() {
        let a = v(Some("\"x\""), Some("Tue"), Some(10));
        assert!(a.conflicts_with(&v(Some("\"y\""), None, None)));
        assert!(a.conflicts_with(&v(None, Some("Wed"), None)));
        assert!(a.conflicts_with(&v(None, None, Some(11))));
        assert!(!a.conflicts_with(&a.clone()));
    }

    #[test]
    fn weak_etag_is_not_used_for_if_range() {
        assert_eq!(v(Some("W/\"x\""), None, None).strong_etag(), None);
        assert_eq!(v(Some("\"x\""), None, None).strong_etag(), Some("\"x\""));
    }
}
