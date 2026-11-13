//! [`HttpSource`]: one remote file over `http`/`https`.

use std::sync::{Arc, Mutex};

use futures::FutureExt;
use futures::future::BoxFuture;
use http::header::{self, HeaderName, HeaderValue};
use http::{StatusCode, Version};
use url::Url;
use whoosh_core::{
    ByteRange, ByteStream, OpenRequest, Protocol, RangeSource, RemoteMeta, SourceError, Validators,
};

use crate::body::{self, CheckedBody, DRAIN_LIMIT, DRAIN_WAIT, INLINE_WAIT};
use crate::classify::{self, ContentRange};
use crate::client::{Client, Outgoing, Response};
use crate::filename::filename_from_content_disposition;
use crate::lock;
use crate::origin::Origin;
use crate::request::Template;

/// Where requests go, and with what: the original URL, or the end of a
/// redirect chain seen before (with the request as the redirects changed
/// it, e.g. without cookies after a change of origin).
#[derive(Debug, Clone)]
struct Target {
    url: Url,
    template: Template,
}

/// A remote file reachable over HTTP/1.1, HTTP/2 or HTTP/3.
///
/// Made by [`HttpSourceFactory`](crate::HttpSourceFactory). Implements the
/// whole [`RangeSource`] contract; see the crate docs for the details.
pub struct HttpSource {
    client: Arc<Client>,
    start: Target,
    /// The last redirect chain's end, reused by later requests so a chain
    /// is followed once per download rather than once per range. Cleared
    /// (and the chain followed again) when that URL answers 401/403/404/410.
    resolved: Mutex<Option<Target>>,
    label: String,
}

impl std::fmt::Debug for HttpSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpSource")
            .field("url", &self.start.url.as_str())
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// The result of following redirects.
struct Fetched {
    resp: Response,
    url: Url,
}

impl HttpSource {
    pub(crate) fn new(client: Arc<Client>, url: Url, template: Template) -> Self {
        let label = Origin::parse_url(&url)
            .map(|o| o.authority())
            .unwrap_or_else(|_| url.to_string());
        Self {
            client,
            start: Target { url, template },
            resolved: Mutex::new(None),
            label,
        }
    }

    /// The URL this source was made for.
    pub fn url(&self) -> &str {
        self.start.url.as_str()
    }

    /// Sends the request (plus `extra` headers), following redirects.
    async fn fetch(&self, extra: &[(HeaderName, HeaderValue)]) -> Result<Fetched, SourceError> {
        let cached = lock(&self.resolved).clone();
        if let Some(t) = cached {
            let f = self.follow(t, extra).await?;
            if !matches!(f.resp.status.as_u16(), 401 | 403 | 404 | 410) {
                return Ok(f);
            }
            // The redirect target may have expired (signed CDN URLs do):
            // forget it and ask the original URL again. This is part of
            // following redirects, not a retry of the same request.
            tracing::debug!(url = %f.url, status = %f.resp.status, "redirect target failed; following from the start");
            f.resp.body.drain(DRAIN_LIMIT, INLINE_WAIT).await;
            *lock(&self.resolved) = None;
        }
        self.follow(self.start.clone(), extra).await
    }

    async fn follow(
        &self,
        mut t: Target,
        extra: &[(HeaderName, HeaderValue)],
    ) -> Result<Fetched, SourceError> {
        let max = self.client.cfg.max_redirects;
        let mut hops = 0;
        loop {
            let origin = Origin::parse_url(&t.url)?;
            let mut headers = t.template.headers.clone();
            for (k, v) in extra {
                headers.insert(k.clone(), v.clone());
            }
            let out = Outgoing {
                method: t.template.method.clone(),
                headers,
                body: t.template.body.clone(),
            };
            let resp = self.client.send(&t.url, &origin, &out).await?;
            let status = resp.status;
            let location = resp
                .headers
                .get(header::LOCATION)
                .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned());
            let Some(location) =
                location.filter(|_| status.is_redirection() && status != StatusCode::NOT_MODIFIED)
            else {
                if hops > 0 && status.is_success() {
                    *lock(&self.resolved) = Some(t.clone());
                }
                return Ok(Fetched { resp, url: t.url });
            };
            if hops >= max {
                resp.body.discard(DRAIN_LIMIT, DRAIN_WAIT);
                return Err(SourceError::Fatal(format!("more than {max} redirects")));
            }
            let next = t
                .url
                .join(location.trim())
                .map_err(|e| SourceError::Fatal(format!("bad redirect to {location:?}: {e}")))?;
            if !matches!(next.scheme(), "http" | "https") {
                resp.body.discard(DRAIN_LIMIT, DRAIN_WAIT);
                return Err(SourceError::Fatal(format!(
                    "redirect to unsupported URL {next}"
                )));
            }
            // Read the (small) redirect body so the connection can carry
            // the next hop.
            resp.body.drain(DRAIN_LIMIT, INLINE_WAIT).await;
            t.template.redirect(&t.url, &next, status.as_u16());
            tracing::trace!(from = %t.url, to = %next, %status, "redirect");
            t.url = next;
            hops += 1;
        }
    }

    async fn do_probe(&self) -> Result<RemoteMeta, SourceError> {
        let range = [(header::RANGE, HeaderValue::from_static("bytes=0-0"))];
        let Fetched { resp, url } = self.fetch(&range).await?;
        let h = &resp.headers;
        let (size, accepts_ranges) = match resp.status.as_u16() {
            206 => match classify::content_range(h) {
                Some(ContentRange::Bytes {
                    first: 0,
                    last: 0,
                    total,
                }) => (total, true),
                Some(ContentRange::Bytes { total, .. }) => (total, false),
                _ => (None, false),
            },
            200 => (classify::content_length(h), false),
            // `bytes=0-0` of an empty file.
            416 => match classify::content_range(h) {
                Some(ContentRange::Unsatisfied { total }) => (Some(total), false),
                _ => return Err(SourceError::Fatal("HTTP 416 to bytes=0-0".into())),
            },
            _ => {
                let e = classify::status_error(resp.status, h);
                resp.body.drain(DRAIN_LIMIT, INLINE_WAIT).await;
                return Err(e);
            }
        };
        let meta = remote_meta(&resp, &url, size, accepts_ranges);
        // A 206 has one byte left to read, so the connection is ready for
        // the first `open`; a 200 is the whole file: close that connection.
        let limit = if resp.status == StatusCode::OK
            && classify::content_length(&resp.headers).is_none_or(|n| n > DRAIN_LIMIT)
        {
            0
        } else {
            DRAIN_LIMIT
        };
        resp.body.drain(limit, INLINE_WAIT).await;
        Ok(meta)
    }

    async fn do_open(&self, req: OpenRequest) -> Result<ByteStream, SourceError> {
        let mut extra = Vec::with_capacity(2);
        if let Some(r) = req.range {
            let Some(value) = r.to_http_header() else {
                return Ok(body::empty());
            };
            extra.push((
                header::RANGE,
                HeaderValue::from_str(&value).expect("digits and punctuation"),
            ));
            if let Some(tag) = req.expect.strong_etag()
                && let Ok(v) = HeaderValue::from_str(tag)
            {
                extra.push((header::IF_RANGE, v));
            }
        }
        let Fetched { resp, .. } = self.fetch(&extra).await?;
        let (expected, seen) = match check_response(&resp, req.range, &req.expect) {
            Ok(x) => x,
            Err(e) => {
                // An error page is read (so the connection is kept); a
                // whole file sent instead of a range is not.
                let limit = if resp.status.is_success() {
                    0
                } else {
                    DRAIN_LIMIT
                };
                resp.body.drain(limit, INLINE_WAIT).await;
                return Err(e);
            }
        };
        if req.expect.conflicts_with(&seen) {
            // Bytes of another version: never read, connection closed.
            return Err(SourceError::RemoteChanged);
        }
        Ok(Box::pin(CheckedBody::new(
            resp.body,
            expected,
            self.client.cfg.idle_timeout,
        )))
    }
}

impl HttpSource {
    /// `probe` and the body from offset 0 in one request: `Range: bytes=0-`.
    async fn do_start(&self) -> Result<(RemoteMeta, ByteStream), SourceError> {
        let range = [(header::RANGE, HeaderValue::from_static("bytes=0-"))];
        let Fetched { resp, url } = self.fetch(&range).await?;
        let h = &resp.headers;
        // (size, accepts_ranges, body bytes to deliver)
        let (size, accepts_ranges, expected) = match resp.status.as_u16() {
            206 => match classify::content_range(h) {
                // The body runs to the end of the entity. A server that sends
                // only a prefix (`0-K/N`, K+1 < N) gets a short body:
                // `Transient` after K+1 correct bytes, never a clean end.
                Some(ContentRange::Bytes {
                    first: 0,
                    last,
                    total,
                }) if last.checked_add(1).is_some() => (total, true, total.or(last.checked_add(1))),
                // A range that does not start at 0 (or none at all): do what
                // probe-then-open would: the whole entity, unranged.
                other => {
                    let total = match other {
                        Some(ContentRange::Bytes { total, .. }) => total,
                        _ => None,
                    };
                    let meta = remote_meta(&resp, &url, total, false);
                    resp.body.drain(0, INLINE_WAIT).await;
                    let stream = self
                        .do_open(OpenRequest {
                            range: None,
                            expect: meta.validators(),
                        })
                        .await?;
                    return Ok((meta, stream));
                }
            },
            // The range was ignored: the whole entity. As in `probe`, only a
            // real 206 counts as range support (`lying-ranges` advertises
            // `Accept-Ranges` and ignores `Range`).
            200 => {
                let len = classify::content_length(h);
                (len, false, len)
            }
            // `bytes=0-` of an empty file.
            416 => match classify::content_range(h) {
                Some(ContentRange::Unsatisfied { total: 0 }) => {
                    let meta = remote_meta(&resp, &url, Some(0), false);
                    resp.body.drain(DRAIN_LIMIT, INLINE_WAIT).await;
                    return Ok((meta, body::empty()));
                }
                _ => return Err(SourceError::Fatal("HTTP 416 to bytes=0-".into())),
            },
            _ => {
                let e = classify::status_error(resp.status, h);
                resp.body.drain(DRAIN_LIMIT, INLINE_WAIT).await;
                return Err(e);
            }
        };
        let meta = remote_meta(&resp, &url, size, accepts_ranges);
        let stream = Box::pin(CheckedBody::new(
            resp.body,
            expected,
            self.client.cfg.idle_timeout,
        ));
        Ok((meta, stream))
    }
}

/// What a probe-like response says about the remote file.
fn remote_meta(resp: &Response, url: &Url, size: Option<u64>, accepts_ranges: bool) -> RemoteMeta {
    let h = &resp.headers;
    let v = classify::validators(h, size);
    RemoteMeta {
        size,
        accepts_ranges,
        etag: v.etag,
        last_modified: v.last_modified,
        filename: h
            .get(header::CONTENT_DISPOSITION)
            .and_then(|v| filename_from_content_disposition(v.as_bytes())),
        content_type: classify::content_type(h),
        final_url: Some(url.to_string()),
        protocol: Some(protocol(resp.version)),
    }
}

/// Checks status and `Content-Range` against the request (contract point
/// 2). Returns how many body bytes to deliver and the validators seen.
fn check_response(
    resp: &Response,
    range: Option<ByteRange>,
    expect: &Validators,
) -> Result<(Option<u64>, Validators), SourceError> {
    let h = &resp.headers;
    match (range, resp.status.as_u16()) {
        (Some(r), 206) => match classify::content_range(h) {
            Some(ContentRange::Bytes { first, last, total }) => {
                let seen = classify::validators(h, total);
                // Another version (e.g. a shorter file whose server clamped
                // the range) beats a mismatched range.
                if expect.conflicts_with(&seen) {
                    Err(SourceError::RemoteChanged)
                } else if first == r.start && last.checked_add(1) == Some(r.end) {
                    Ok((Some(r.len()), seen))
                } else {
                    Err(SourceError::RangesUnsupported)
                }
            }
            _ => Err(SourceError::RangesUnsupported),
        },
        (Some(_), 200) => {
            // Either the server ignores ranges, or `If-Range` did not match
            // because the file changed: the validators tell which.
            let seen = classify::validators(h, classify::content_length(h));
            if expect.conflicts_with(&seen) {
                Err(SourceError::RemoteChanged)
            } else {
                Err(SourceError::RangesUnsupported)
            }
        }
        (None, 200) => {
            let len = classify::content_length(h);
            Ok((len.or(expect.size), classify::validators(h, len)))
        }
        (None, 206) => match classify::content_range(h) {
            // Only acceptable if it is the whole file.
            Some(ContentRange::Bytes {
                first: 0,
                last,
                total: Some(t),
            }) if last.checked_add(1) == Some(t) => Ok((Some(t), classify::validators(h, Some(t)))),
            _ => Err(SourceError::RangesUnsupported),
        },
        (_, _) => Err(classify::status_error(resp.status, h)),
    }
}

fn protocol(v: Version) -> Protocol {
    match v {
        Version::HTTP_2 => Protocol::Http2,
        Version::HTTP_3 => Protocol::Http3,
        Version::HTTP_11 | Version::HTTP_10 => Protocol::Http1,
        _ => Protocol::Other,
    }
}

impl RangeSource for HttpSource {
    fn label(&self) -> &str {
        &self.label
    }

    fn probe(&self) -> BoxFuture<'_, Result<RemoteMeta, SourceError>> {
        self.do_probe().boxed()
    }

    fn open(&self, req: OpenRequest) -> BoxFuture<'_, Result<ByteStream, SourceError>> {
        self.do_open(req).boxed()
    }

    /// One `GET` with `Range: bytes=0-`: the probe's metadata and the body
    /// from offset 0, one round trip sooner than probe-then-open.
    fn start(&self) -> BoxFuture<'_, Result<(RemoteMeta, ByteStream), SourceError>> {
        self.do_start().boxed()
    }
}
