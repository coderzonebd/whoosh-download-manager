//! Request routing and the `/s/<scenario>/<file>` handler.

use bytes::Bytes;
use http::header::{self, HeaderMap, HeaderValue};
use http::{Method, Request, Response, StatusCode, Version};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use std::convert::Infallible;
use std::io;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use vayu_core::ByteRange;

use crate::body::{BodyPlan, stream_body};
use crate::conn::ConnCtl;
use crate::scenario::{RangeMode, Refuse};
use crate::sign::{self, SigCheck};
use crate::state::{ActiveGuard, ScenarioRt, Shared, etag, last_modified};

pub(crate) type Body = BoxBody<Bytes, io::Error>;

/// What the connection serving a request knows about it.
#[derive(Debug, Clone)]
pub(crate) struct ReqCtx {
    pub shared: Arc<Shared>,
    pub conn: Arc<ConnCtl>,
    pub tls: bool,
}

pub(crate) fn empty(status: StatusCode) -> Response<Body> {
    let mut r = Response::new(Empty::new().map_err(|e| match e {}).boxed());
    *r.status_mut() = status;
    r
}

pub(crate) fn text(status: StatusCode, body: impl Into<Bytes>) -> Response<Body> {
    let body: Bytes = body.into();
    let mut r = Response::new(Full::new(body).map_err(|e| match e {}).boxed());
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    r
}

pub(crate) fn json(status: StatusCode, value: &impl serde::Serialize) -> Response<Body> {
    let mut body = serde_json::to_vec_pretty(value).unwrap_or_default();
    body.push(b'\n');
    let mut r = text(status, body);
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    r
}

fn not_found() -> Response<Body> {
    text(StatusCode::NOT_FOUND, "not found\n")
}

/// The service function for every connection.
pub(crate) async fn handle(
    req: Request<Incoming>,
    ctx: ReqCtx,
) -> Result<Response<Body>, Infallible> {
    let path = req.uri().path().to_owned();
    let resp = if let Some(rest) = path.strip_prefix("/s/") {
        serve_scenario(req, rest, &ctx).await
    } else if let Some(rest) = path.strip_prefix("/_lab/") {
        crate::control::handle(req, rest, &ctx).await
    } else if path == "/" {
        crate::control::index(&ctx)
    } else {
        not_found()
    };
    Ok(resp)
}

async fn serve_scenario(req: Request<Incoming>, rest: &str, ctx: &ReqCtx) -> Response<Body> {
    let Some((name, tail)) = rest.split_once('/') else {
        return not_found();
    };
    let Some(rt) = ctx.shared.scenario(name).cloned() else {
        return not_found();
    };
    let ip = ctx.conn.peer;
    rt.stats.count_request(ip);
    if ctx.conn.touch(rt.idx) {
        rt.stats.conn_opened(ip);
    }
    let resp = serve_file(req, tail, &rt, ctx).await;
    rt.stats.count_status(resp.status().as_u16());
    resp
}

/// Splits `_hop/<k>/<file>` into `(k, file)`; anything else is hop 0.
fn parse_hop(tail: &str) -> Option<(u32, &str)> {
    match tail.strip_prefix("_hop/") {
        Some(r) => {
            let (k, f) = r.split_once('/')?;
            Some((k.parse().ok()?, f))
        }
        None => Some((0, tail)),
    }
}

async fn serve_file(
    req: Request<Incoming>,
    tail: &str,
    rt: &Arc<ScenarioRt>,
    ctx: &ReqCtx,
) -> Response<Body> {
    let sc = &rt.sc;
    let Some((hop, file_name)) = parse_hop(tail) else {
        return not_found();
    };
    let is_head = req.method() == Method::HEAD;
    if req.method() != Method::GET && !is_head {
        let mut r = text(StatusCode::METHOD_NOT_ALLOWED, "GET or HEAD only\n");
        r.headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        return r;
    }

    // `<file>.sha256` sidecar: never faulted, gated or delayed.
    if hop == 0
        && let Some(base) = file_name.strip_suffix(".sha256")
    {
        let Some(f) = rt.file(base) else {
            return not_found();
        };
        let vf = f.base.version(rt.current_version(f));
        let shared = ctx.shared.clone();
        let digest = tokio::task::spawn_blocking(move || shared.file_digest(vf))
            .await
            .unwrap_or_default();
        let r = text(StatusCode::OK, format!("{digest}  {base}\n"));
        return if is_head { strip_body(r) } else { r };
    }
    let Some(file) = rt.file(file_name).cloned() else {
        return not_found();
    };

    let ttfb = Duration::from_millis(sc.latency.ttfb_ms);
    let early = gatekeep(&req, hop, file_name, rt, ctx);
    let guard = match early {
        Some(_) => None,
        None => match ActiveGuard::acquire(rt, ctx.conn.peer) {
            Some(g) => Some(g),
            None => {
                tokio::time::sleep(ttfb).await;
                return refuse(&req, rt, ctx).await;
            }
        },
    };
    if !ttfb.is_zero() {
        tokio::time::sleep(ttfb).await;
    }
    if let Some(r) = early {
        return r;
    }

    // Validators of the version this response will carry.
    let version = rt.current_version(&file);
    let vf = file.base.version(version);
    let size = vf.size;
    let tag = sc.validators.etag.then(|| etag(vf));
    let mtime = sc.validators.last_modified.then(|| last_modified(version));

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        file.spec
            .content_type
            .as_deref()
            .and_then(|c| HeaderValue::from_str(c).ok())
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    if sc.ranges.mode != RangeMode::Ignore {
        headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    }
    if let Some(t) = &tag {
        headers.insert(header::ETAG, hv(t));
    }
    if let Some(m) = mtime {
        headers.insert(header::LAST_MODIFIED, hv(&httpdate::fmt_http_date(m)));
    }
    if let Some(d) = &file.disposition {
        headers.insert(header::CONTENT_DISPOSITION, d.clone());
    }
    headers.insert("x-lab-version", hv(&version.to_string()));

    if let Some(status) = preconditions(req.headers(), tag.as_deref(), mtime) {
        let mut r = empty(status);
        r.headers_mut().extend(headers);
        if status == StatusCode::PRECONDITION_FAILED {
            r.headers_mut().remove(header::CONTENT_TYPE);
        }
        return r;
    }

    // Range selection.
    let range_header = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok());
    let outcome = match range_header {
        Some(h)
            if sc.ranges.mode == RangeMode::Normal
                && if_range_matches(req.headers(), tag.as_deref(), mtime) =>
        {
            parse_range(h, size)
        }
        _ => RangeOutcome::Full,
    };
    let (status, range) = match outcome {
        RangeOutcome::Full => (
            StatusCode::OK,
            ByteRange {
                start: 0,
                end: size,
            },
        ),
        RangeOutcome::Partial(r) => {
            headers.insert(
                header::CONTENT_RANGE,
                hv(&format!("bytes {}-{}/{}", r.start, r.end - 1, size)),
            );
            (StatusCode::PARTIAL_CONTENT, r)
        }
        RangeOutcome::Unsatisfiable => {
            let mut r = empty(StatusCode::RANGE_NOT_SATISFIABLE);
            r.headers_mut().extend(headers);
            r.headers_mut()
                .insert(header::CONTENT_RANGE, hv(&format!("bytes */{size}")));
            r.headers_mut()
                .insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
            return r;
        }
    };
    if !sc.ranges.chunked {
        headers.insert(header::CONTENT_LENGTH, hv(&range.len().to_string()));
    }

    let body = if is_head || range.is_empty() {
        drop(guard);
        empty_body()
    } else {
        stream_body(BodyPlan {
            shared: ctx.shared.clone(),
            rt: rt.clone(),
            file,
            seed: vf.seed,
            start: range.start,
            end: range.end,
            conn: ctx.conn.clone(),
            guard,
        })
    };
    let mut r = Response::new(body);
    *r.status_mut() = status;
    *r.headers_mut() = headers;
    r
}

fn empty_body() -> Body {
    Empty::new().map_err(|e| match e {}).boxed()
}

fn strip_body(r: Response<Body>) -> Response<Body> {
    let (parts, _) = r.into_parts();
    Response::from_parts(parts, empty_body())
}

fn hv(s: &str) -> HeaderValue {
    HeaderValue::from_str(s).unwrap_or(HeaderValue::from_static("invalid"))
}

/// Redirects, signatures and access gates. `Some` is the response to send
/// instead of the file.
fn gatekeep(
    req: &Request<Incoming>,
    hop: u32,
    file: &str,
    rt: &ScenarioRt,
    ctx: &ReqCtx,
) -> Option<Response<Body>> {
    let sc = &rt.sc;
    let hops = sc.redirects.map_or(0, |r| r.count);
    if hop > hops {
        return Some(not_found());
    }
    if let Some(redir) = sc.redirects
        && hop < redir.count
    {
        let mut loc = format!("/s/{}/_hop/{}/{}", sc.name, hop + 1, file);
        if let Some(q) = req.uri().query() {
            loc.push('?');
            loc.push_str(q);
        }
        if redir.absolute {
            loc = format!("{}{}", origin(req, ctx), loc);
        }
        let status = StatusCode::from_u16(redir.status).unwrap_or(StatusCode::FOUND);
        let mut r = text(status, format!("redirecting to {loc}\n"));
        r.headers_mut().insert(header::LOCATION, hv(&loc));
        return Some(r);
    }
    if let Some(su) = &sc.signed_url {
        let check = sign::check(
            &rt.secret,
            &rt.path(file),
            req.uri().query(),
            sign::now_unix(),
        );
        if check != SigCheck::Valid {
            let status = StatusCode::from_u16(su.status).unwrap_or(StatusCode::FORBIDDEN);
            let why = match check {
                SigCheck::Missing => "missing signature",
                SigCheck::Invalid => "bad signature",
                SigCheck::Expired => "link expired",
                SigCheck::Valid => "",
            };
            return Some(text(status, format!("{why}\n")));
        }
    }
    let acc = &sc.access;
    let status = StatusCode::from_u16(acc.status).unwrap_or(StatusCode::FORBIDDEN);
    if let Some(want) = &acc.cookie {
        let has = req
            .headers()
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .any(|pair| pair.trim() == want);
        if !has {
            return Some(text(status, "cookie required\n"));
        }
    }
    if let Some(prefix) = &acc.referer {
        let ok = req
            .headers()
            .get(header::REFERER)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|r| r.starts_with(prefix.as_str()));
        if !ok {
            return Some(text(status, "referer required\n"));
        }
    }
    None
}

/// `scheme://authority` the client used.
pub(crate) fn origin<B>(req: &Request<B>, ctx: &ReqCtx) -> String {
    let scheme = if ctx.tls { "https" } else { "http" };
    let host = req
        .uri()
        .authority()
        .map(|a| a.as_str().to_owned())
        .or_else(|| {
            req.headers()
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "localhost".into());
    format!("{scheme}://{host}")
}

async fn refuse(req: &Request<Incoming>, rt: &ScenarioRt, ctx: &ReqCtx) -> Response<Body> {
    let limits = rt.sc.limits.unwrap_or(crate::scenario::LimitConfig {
        max_conns_per_ip: 1,
        refuse: Refuse::ServiceUnavailable,
        retry_after_secs: 1,
    });
    let status = match limits.refuse {
        Refuse::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        Refuse::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
        Refuse::Reset => {
            ctx.conn.kill();
            // The connection task drops the connection (RST) before any
            // response is written.
            return std::future::pending().await;
        }
    };
    let mut r = text(status, "too many connections from your address\n");
    r.headers_mut().insert(
        header::RETRY_AFTER,
        hv(&limits.retry_after_secs.to_string()),
    );
    if matches!(req.version(), Version::HTTP_10 | Version::HTTP_11) {
        r.headers_mut()
            .insert(header::CONNECTION, HeaderValue::from_static("close"));
    }
    r
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RangeOutcome {
    Full,
    Partial(ByteRange),
    Unsatisfiable,
}

/// Interprets a `Range` header for a file of `size` bytes. Syntax errors,
/// other units and multi-range requests are ignored (whole file), as
/// RFC 9110 allows.
pub(crate) fn parse_range(header: &str, size: u64) -> RangeOutcome {
    let Some((unit, spec)) = header.trim().split_once('=') else {
        return RangeOutcome::Full;
    };
    if !unit.trim().eq_ignore_ascii_case("bytes") || spec.contains(',') {
        return RangeOutcome::Full;
    }
    let Some((a, b)) = spec.trim().split_once('-') else {
        return RangeOutcome::Full;
    };
    let (a, b) = (a.trim(), b.trim());
    let num = |s: &str| -> Option<u64> {
        if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
            None
        } else {
            s.parse().ok()
        }
    };
    if a.is_empty() {
        // Suffix range: the last `n` bytes.
        let Some(n) = num(b) else {
            return RangeOutcome::Full;
        };
        if n == 0 || size == 0 {
            return RangeOutcome::Unsatisfiable;
        }
        return RangeOutcome::Partial(ByteRange {
            start: size - n.min(size),
            end: size,
        });
    }
    let Some(first) = num(a) else {
        return RangeOutcome::Full;
    };
    let last = if b.is_empty() {
        None
    } else {
        match num(b) {
            Some(l) if l >= first => Some(l),
            _ => return RangeOutcome::Full,
        }
    };
    if first >= size {
        return RangeOutcome::Unsatisfiable;
    }
    let end = last.map_or(size, |l| l.saturating_add(1).min(size));
    RangeOutcome::Partial(ByteRange { start: first, end })
}

fn etag_list_matches(list: &str, tag: Option<&str>, weak: bool) -> bool {
    let norm = |t: &str| -> String {
        let t = t.trim();
        if weak {
            t.strip_prefix("W/").unwrap_or(t).to_owned()
        } else {
            t.to_owned()
        }
    };
    let Some(tag) = tag else {
        return list.trim() == "*";
    };
    list.trim() == "*"
        || list.split(',').any(|t| {
            let t = t.trim();
            (weak || !t.starts_with("W/")) && norm(t) == norm(tag)
        })
}

fn header_date(h: &HeaderMap, name: header::HeaderName) -> Option<SystemTime> {
    httpdate::parse_http_date(h.get(name)?.to_str().ok()?).ok()
}

/// RFC 9110 §13.2.2 order: If-Match, If-Unmodified-Since, If-None-Match,
/// If-Modified-Since. `Some` is the status to answer with.
fn preconditions(
    h: &HeaderMap,
    tag: Option<&str>,
    mtime: Option<SystemTime>,
) -> Option<StatusCode> {
    let date = |name| header_date(h, name).zip(mtime);
    match h.get(header::IF_MATCH).and_then(|v| v.to_str().ok()) {
        Some(v) if !etag_list_matches(v, tag, false) => {
            return Some(StatusCode::PRECONDITION_FAILED);
        }
        None if date(header::IF_UNMODIFIED_SINCE).is_some_and(|(since, m)| m > since) => {
            return Some(StatusCode::PRECONDITION_FAILED);
        }
        _ => {}
    }
    match h.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) {
        Some(v) if etag_list_matches(v, tag, true) => Some(StatusCode::NOT_MODIFIED),
        None if date(header::IF_MODIFIED_SINCE).is_some_and(|(since, m)| m <= since) => {
            Some(StatusCode::NOT_MODIFIED)
        }
        _ => None,
    }
}

/// `If-Range`: true when absent or when it names the current version.
fn if_range_matches(h: &HeaderMap, tag: Option<&str>, mtime: Option<SystemTime>) -> bool {
    let Some(v) = h.get(header::IF_RANGE).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let v = v.trim();
    if v.starts_with('"') {
        tag == Some(v)
    } else if v.starts_with("W/") {
        false
    } else {
        match (httpdate::parse_http_date(v), mtime) {
            (Ok(d), Some(m)) => d == m,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use RangeOutcome::*;

    fn p(s: u64, e: u64) -> RangeOutcome {
        Partial(ByteRange { start: s, end: e })
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-0", 10), p(0, 1));
        assert_eq!(parse_range("bytes=2-5", 10), p(2, 6));
        assert_eq!(parse_range("bytes=2-", 10), p(2, 10));
        assert_eq!(parse_range("bytes=5-100", 10), p(5, 10));
        assert_eq!(parse_range("bytes=-3", 10), p(7, 10));
        assert_eq!(parse_range("bytes=-30", 10), p(0, 10));
        assert_eq!(parse_range(" Bytes = 1 - 2 ", 10), p(1, 3));
        assert_eq!(parse_range("bytes=10-", 10), Unsatisfiable);
        assert_eq!(parse_range("bytes=10-20", 10), Unsatisfiable);
        assert_eq!(parse_range("bytes=-0", 10), Unsatisfiable);
        assert_eq!(parse_range("bytes=0-", 0), Unsatisfiable);
        assert_eq!(parse_range("bytes=-5", 0), Unsatisfiable);
        // Ignored: whole file.
        assert_eq!(parse_range("bytes=5-2", 10), Full);
        assert_eq!(parse_range("bytes=0-1,4-5", 10), Full);
        assert_eq!(parse_range("items=0-1", 10), Full);
        assert_eq!(parse_range("bytes=x-1", 10), Full);
        assert_eq!(parse_range("bytes=+1-2", 10), Full);
        assert_eq!(parse_range("bytes", 10), Full);
    }

    #[test]
    fn conditionals() {
        let tag = Some("\"abc\"");
        let mt = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let m = Some(mt);
        let mut h = HeaderMap::new();
        assert_eq!(preconditions(&h, tag, m), None);
        h.insert(header::IF_MATCH, hv("\"zzz\", \"abc\""));
        assert_eq!(preconditions(&h, tag, m), None);
        h.insert(header::IF_MATCH, hv("W/\"abc\""));
        assert_eq!(
            preconditions(&h, tag, m),
            Some(StatusCode::PRECONDITION_FAILED)
        );
        h.insert(header::IF_MATCH, hv("*"));
        assert_eq!(preconditions(&h, None, m), None);
        h.clear();
        h.insert(header::IF_NONE_MATCH, hv("W/\"abc\""));
        assert_eq!(preconditions(&h, tag, m), Some(StatusCode::NOT_MODIFIED));
        h.clear();
        h.insert(header::IF_MODIFIED_SINCE, hv(&httpdate::fmt_http_date(mt)));
        assert_eq!(preconditions(&h, tag, m), Some(StatusCode::NOT_MODIFIED));

        let mut h = HeaderMap::new();
        assert!(if_range_matches(&h, tag, m));
        h.insert(header::IF_RANGE, hv("\"abc\""));
        assert!(if_range_matches(&h, tag, m));
        assert!(!if_range_matches(&h, None, m));
        h.insert(header::IF_RANGE, hv(&httpdate::fmt_http_date(mt)));
        assert!(if_range_matches(&h, tag, m));
        assert!(!if_range_matches(&h, tag, None));
    }
}
