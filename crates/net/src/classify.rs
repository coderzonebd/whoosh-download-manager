//! Reading response headers, and turning statuses and transport errors into
//! [`SourceError`]s. Sources classify; the engine decides.

use std::error::Error as StdError;
use std::io;
use std::time::{Duration, SystemTime};

use http::StatusCode;
use http::header::{self, HeaderMap};
use vayu_core::{SourceError, Validators};

/// A parsed `Content-Range` (`bytes a-b/total`, `bytes */total`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContentRange {
    /// Inclusive `first..=last`, with the complete length if stated.
    Bytes {
        first: u64,
        last: u64,
        total: Option<u64>,
    },
    /// `bytes */total` (on 416).
    Unsatisfied { total: u64 },
}

pub(crate) fn content_range(headers: &HeaderMap) -> Option<ContentRange> {
    let v = headers.get(header::CONTENT_RANGE)?.to_str().ok()?.trim();
    let rest = v.strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (range, total) = (range.trim(), total.trim());
    if range == "*" {
        return Some(ContentRange::Unsatisfied {
            total: digits(total)?,
        });
    }
    let total = if total == "*" {
        None
    } else {
        Some(digits(total)?)
    };
    let (a, b) = range.split_once('-')?;
    let (first, last) = (digits(a.trim())?, digits(b.trim())?);
    if last < first || total.is_some_and(|t| last >= t) {
        return None;
    }
    Some(ContentRange::Bytes { first, last, total })
}

fn digits(s: &str) -> Option<u64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

pub(crate) fn content_length(headers: &HeaderMap) -> Option<u64> {
    let mut values = headers.get_all(header::CONTENT_LENGTH).iter();
    let first = digits(values.next()?.to_str().ok()?.trim())?;
    // Conflicting duplicates make the length unknown (RFC 9112 §6.3).
    values
        .all(|v| v.to_str().ok().and_then(|s| digits(s.trim())) == Some(first))
        .then_some(first)
}

fn text(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
    let v = headers.get(name)?.to_str().ok()?.trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// `ETag` and `Last-Modified` of a response, with `size` as given.
pub(crate) fn validators(headers: &HeaderMap, size: Option<u64>) -> Validators {
    Validators {
        etag: text(headers, header::ETAG),
        last_modified: text(headers, header::LAST_MODIFIED),
        size,
    }
}

pub(crate) fn content_type(headers: &HeaderMap) -> Option<String> {
    text(headers, header::CONTENT_TYPE)
}

/// The longest `Retry-After` passed on. The value is remote-controlled; the
/// engine adds it to `Instant::now()`, which panics on overflow.
pub(crate) const MAX_RETRY_AFTER: Duration = Duration::from_secs(3600);

/// `Retry-After` as delta-seconds or an HTTP date (a date in the past is
/// 0), at most [`MAX_RETRY_AFTER`].
pub(crate) fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let v = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    let d = if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
        // Too large for u64: as long as allowed.
        v.parse().map_or(MAX_RETRY_AFTER, Duration::from_secs)
    } else {
        let at = httpdate::parse_http_date(v).ok()?;
        at.duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO)
    };
    Some(d.min(MAX_RETRY_AFTER))
}

/// The error for a final (non-redirect) status the source cannot use.
pub(crate) fn status_error(status: StatusCode, headers: &HeaderMap) -> SourceError {
    let msg = format!("HTTP {status}");
    match status.as_u16() {
        401 | 403 | 410 => SourceError::LinkExpired(msg),
        416 => SourceError::RemoteChanged,
        429 | 503 => SourceError::Throttled {
            message: msg,
            retry_after: retry_after(headers),
        },
        408 | 421 | 425 | 500..=599 => SourceError::Transient(msg),
        _ => SourceError::Fatal(msg),
    }
}

/// Where in a request an error happened; the same failure means different
/// things before and after the response head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Resolving and connecting (TCP, TLS, QUIC handshake).
    Connect,
    /// The connection is up; the response head has not arrived.
    Head,
}

/// What a transport error looks like, independent of the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cause {
    /// Refused, reset, closed by the peer, stream refused, GOAWAY.
    Refused,
    /// A timeout (ours or the transport's).
    Timeout,
    /// The server's certificate is not acceptable.
    Certificate,
    /// Anything else.
    Other,
}

/// Classifies one failure.
pub(crate) fn transport_error(cause: Cause, phase: Phase, what: String) -> SourceError {
    match (cause, phase) {
        (Cause::Certificate, _) => SourceError::Fatal(what),
        // Connections refused or reset before the server answered: the
        // server (or something in front of it) is shedding load.
        (Cause::Refused, Phase::Connect | Phase::Head) => SourceError::Throttled {
            message: what,
            retry_after: None,
        },
        _ => SourceError::Transient(what),
    }
}
// Errors after the response head are always `Transient` and are made
// directly by the body stream.

/// Finds the cause of an error anywhere in its `source()` chain (hyper,
/// h2, rustls, io).
pub(crate) fn cause_of(e: &(dyn StdError + 'static)) -> Cause {
    let mut cur: Option<&(dyn StdError + 'static)> = Some(e);
    while let Some(err) = cur {
        if let Some(io) = err.downcast_ref::<io::Error>() {
            if let Some(inner) = io.get_ref()
                && let Some(tls) = inner.downcast_ref::<rustls::Error>()
            {
                return tls_cause(tls);
            }
            return io_cause(io.kind());
        }
        if let Some(tls) = err.downcast_ref::<rustls::Error>() {
            return tls_cause(tls);
        }
        if let Some(h2) = err.downcast_ref::<h2::Error>() {
            if h2.is_go_away() || h2.is_remote() && h2.reason() == Some(h2::Reason::REFUSED_STREAM)
            {
                return Cause::Refused;
            }
            if h2.is_io() {
                // The io error is the next source.
                cur = err.source();
                continue;
            }
            return Cause::Other;
        }
        if let Some(h) = err.downcast_ref::<hyper::Error>() {
            if h.is_timeout() {
                return Cause::Timeout;
            }
            if h.is_incomplete_message() || h.is_closed() || h.is_canceled() {
                // Look deeper for an io error first; otherwise the peer
                // closed the connection.
                return match err.source().map(cause_of) {
                    Some(c) if c != Cause::Other => c,
                    _ => Cause::Refused,
                };
            }
        }
        cur = err.source();
    }
    Cause::Other
}

pub(crate) fn io_cause(kind: io::ErrorKind) -> Cause {
    use io::ErrorKind::*;
    match kind {
        ConnectionRefused | ConnectionReset | ConnectionAborted | BrokenPipe | UnexpectedEof
        | NotConnected => Cause::Refused,
        TimedOut | WouldBlock => Cause::Timeout,
        _ => Cause::Other,
    }
}

fn tls_cause(e: &rustls::Error) -> Cause {
    match e {
        rustls::Error::InvalidCertificate(_)
        | rustls::Error::NoCertificatesPresented
        | rustls::Error::InvalidCertRevocationList(_) => Cause::Certificate,
        _ => Cause::Other,
    }
}

/// One-line description of an error and its sources.
pub(crate) fn describe(e: &(dyn StdError + 'static)) -> String {
    let mut s = e.to_string();
    let mut cur = e.source();
    while let Some(err) = cur {
        let m = err.to_string();
        if !s.contains(&m) {
            s.push_str(": ");
            s.push_str(&m);
        }
        cur = err.source();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn h(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.append(*k, HeaderValue::from_str(v).unwrap());
        }
        m
    }

    #[test]
    fn content_range_forms() {
        let cr = |v| content_range(&h(&[("content-range", v)]));
        assert_eq!(
            cr("bytes 0-0/100"),
            Some(ContentRange::Bytes {
                first: 0,
                last: 0,
                total: Some(100)
            })
        );
        assert_eq!(
            cr("bytes 10-19/*"),
            Some(ContentRange::Bytes {
                first: 10,
                last: 19,
                total: None
            })
        );
        assert_eq!(
            cr("bytes */0"),
            Some(ContentRange::Unsatisfied { total: 0 })
        );
        assert_eq!(cr("bytes 5-4/10"), None);
        assert_eq!(cr("bytes 5-10/10"), None);
        assert_eq!(cr("items 0-1/2"), None);
        assert_eq!(cr("bytes -1-2/3"), None);
        assert_eq!(cr("bytes 0-1/+3"), None);
    }

    #[test]
    fn content_length_duplicates() {
        assert_eq!(content_length(&h(&[("content-length", "5")])), Some(5));
        assert_eq!(
            content_length(&h(&[("content-length", "5"), ("content-length", "5")])),
            Some(5)
        );
        assert_eq!(
            content_length(&h(&[("content-length", "5"), ("content-length", "6")])),
            None
        );
        assert_eq!(content_length(&h(&[("content-length", "-5")])), None);
    }

    #[test]
    fn retry_after_forms() {
        assert_eq!(
            retry_after(&h(&[("retry-after", "2")])),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            retry_after(&h(&[("retry-after", "Wed, 21 Oct 2015 07:28:00 GMT")])),
            Some(Duration::ZERO)
        );
        let later = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(120));
        let d = retry_after(&h(&[("retry-after", &later)])).unwrap();
        assert!(
            d > Duration::from_secs(100) && d <= Duration::from_secs(120),
            "{d:?}"
        );
        assert_eq!(retry_after(&h(&[("retry-after", "soon")])), None);
        // Remote-controlled: capped, never a value that overflows `now + d`.
        for huge in [
            "18446744073709551615",
            "9223372036854775807",
            "99999999999999999999999",
            "Fri, 31 Dec 9999 23:59:59 GMT",
        ] {
            let d = retry_after(&h(&[("retry-after", huge)])).unwrap();
            assert_eq!(d, MAX_RETRY_AFTER, "{huge}");
            let _ = std::time::Instant::now() + d;
        }
        assert_eq!(retry_after(&HeaderMap::new()), None);
    }

    #[test]
    fn statuses() {
        let none = HeaderMap::new();
        let s = |c: u16| status_error(StatusCode::from_u16(c).unwrap(), &none);
        for c in [401, 403, 410] {
            assert!(matches!(s(c), SourceError::LinkExpired(_)), "{c}");
        }
        for c in [429, 503] {
            assert!(matches!(s(c), SourceError::Throttled { .. }), "{c}");
        }
        for c in [500, 502, 504, 408] {
            assert!(matches!(s(c), SourceError::Transient(_)), "{c}");
        }
        for c in [400, 404, 405, 451] {
            assert!(matches!(s(c), SourceError::Fatal(_)), "{c}");
        }
        assert_eq!(s(416), SourceError::RemoteChanged);
        let with = h(&[("retry-after", "7")]);
        assert_eq!(
            status_error(StatusCode::TOO_MANY_REQUESTS, &with),
            SourceError::Throttled {
                message: "HTTP 429 Too Many Requests".into(),
                retry_after: Some(Duration::from_secs(7))
            }
        );
    }

    #[test]
    fn io_errors() {
        let e = io::Error::from(io::ErrorKind::ConnectionReset);
        assert_eq!(cause_of(&e), Cause::Refused);
        assert!(matches!(
            transport_error(Cause::Refused, Phase::Head, "x".into()),
            SourceError::Throttled { .. }
        ));
        assert!(matches!(
            transport_error(Cause::Timeout, Phase::Connect, "x".into()),
            SourceError::Transient(_)
        ));
        let tls = io::Error::new(
            io::ErrorKind::InvalidData,
            rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer),
        );
        assert_eq!(cause_of(&tls), Cause::Certificate);
    }
}
