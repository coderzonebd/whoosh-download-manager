//! Turning a [`RequestContext`] into the requests a source sends.

use base64::Engine as _;
use bytes::Bytes;
use http::Method;
use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use url::Url;
use vayu_core::SourceError;
use vayu_proto::RequestContext;

/// Headers a caller may not set: hop-by-hop (RFC 9110 §7.6.1), framing and
/// routing (`Host`, `Content-Length`), and everything the source itself
/// controls (`Range`, `If-Range`, `Accept-Encoding`) or that would turn a
/// download into a cache revalidation (`If-None-Match`, …).
const DROPPED: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
    "content-length",
    "expect",
    "range",
    "if-range",
    "accept-encoding",
    "if-match",
    "if-none-match",
    "if-modified-since",
    "if-unmodified-since",
];

/// Headers that only make sense with a request body; dropped when a
/// redirect turns the request into a `GET`.
const BODY_HEADERS: &[HeaderName] = &[
    header::CONTENT_TYPE,
    header::CONTENT_ENCODING,
    header::CONTENT_LANGUAGE,
    header::CONTENT_LOCATION,
];

/// Method, headers and body of the browser's request, cleaned up. Every
/// request a source makes (probe, opens, redirect hops) starts from this.
#[derive(Debug, Clone)]
pub(crate) struct Template {
    pub method: Method,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Template {
    /// Builds the template for `url`. Fails (`Fatal`) on an invalid method
    /// or body; invalid header names or values are skipped.
    pub fn new(
        url: &Url,
        ctx: Option<&RequestContext>,
        default_ua: &str,
    ) -> Result<Self, SourceError> {
        let mut headers = HeaderMap::new();
        let mut method = Method::GET;
        let mut body = Bytes::new();
        if let Some(ctx) = ctx {
            if let Some(m) = ctx.method.as_deref().filter(|m| !m.trim().is_empty()) {
                method = Method::from_bytes(m.trim().to_ascii_uppercase().as_bytes())
                    .map_err(|_| SourceError::Fatal(format!("invalid method {m:?}")))?;
            }
            if let Some(b) = ctx.body_base64.as_deref().filter(|b| !b.is_empty()) {
                let raw = base64::engine::general_purpose::STANDARD
                    .decode(b.trim())
                    .map_err(|e| SourceError::Fatal(format!("invalid bodyBase64: {e}")))?;
                body = Bytes::from(raw);
            }
            // Names listed in `Connection` are hop-by-hop too.
            let listed: Vec<String> = ctx
                .headers
                .iter()
                .filter(|h| h.name.eq_ignore_ascii_case("connection"))
                .flat_map(|h| h.value.split(','))
                .map(|n| n.trim().to_ascii_lowercase())
                .collect();
            for h in &ctx.headers {
                let lower = h.name.trim().to_ascii_lowercase();
                if DROPPED.contains(&lower.as_str()) || listed.contains(&lower) {
                    continue;
                }
                let explicit = match lower.as_str() {
                    "cookie" => ctx.cookie.is_some(),
                    "referer" => ctx.referer.is_some(),
                    "user-agent" => ctx.user_agent.is_some(),
                    _ => false,
                };
                if explicit {
                    continue;
                }
                match (
                    HeaderName::from_bytes(lower.as_bytes()),
                    HeaderValue::from_str(h.value.trim()),
                ) {
                    (Ok(n), Ok(v)) => {
                        headers.append(n, v);
                    }
                    _ => tracing::debug!(name = %h.name, "skipping invalid request header"),
                }
            }
            for (name, value) in [
                (header::COOKIE, &ctx.cookie),
                (header::REFERER, &ctx.referer),
                (header::USER_AGENT, &ctx.user_agent),
            ] {
                if let Some(v) = value.as_deref().and_then(|v| HeaderValue::from_str(v).ok()) {
                    headers.insert(name, v);
                }
            }
        }
        if !headers.contains_key(header::USER_AGENT)
            && let Ok(v) = HeaderValue::from_str(default_ua)
        {
            headers.insert(header::USER_AGENT, v);
        }
        if !headers.contains_key(header::ACCEPT) {
            headers.insert(header::ACCEPT, HeaderValue::from_static("*/*"));
        }
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
        // Credentials in the URL become Basic auth, as browsers do.
        if !url.username().is_empty() && !headers.contains_key(header::AUTHORIZATION) {
            let user = percent_encoding::percent_decode_str(url.username()).decode_utf8_lossy();
            let pass = url
                .password()
                .map(|p| percent_encoding::percent_decode_str(p).decode_utf8_lossy())
                .unwrap_or_default();
            let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
            if let Ok(v) = HeaderValue::from_str(&format!("Basic {token}")) {
                headers.insert(header::AUTHORIZATION, v);
            }
        }
        Ok(Self {
            method,
            headers,
            body,
        })
    }

    /// Applies a redirect to `to` with `status` (RFC 9110 §15.4): drops
    /// credentials on a change of origin, and turns the request into a
    /// body-less `GET` on 303, and on 301/302 after a `POST` (as browsers
    /// do). Returns whether credentials were dropped.
    pub fn redirect(&mut self, from: &Url, to: &Url, status: u16) -> bool {
        let to_get = match status {
            303 => self.method != Method::HEAD,
            301 | 302 => self.method == Method::POST,
            _ => false,
        };
        if to_get {
            self.method = Method::GET;
            self.body = Bytes::new();
            for h in BODY_HEADERS {
                self.headers.remove(h);
            }
        }
        let cross = from.origin() != to.origin();
        if cross {
            self.headers.remove(header::COOKIE);
            self.headers.remove(header::AUTHORIZATION);
        }
        cross
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vayu_proto::Header;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn ctx(headers: &[(&str, &str)]) -> RequestContext {
        RequestContext {
            headers: headers
                .iter()
                .map(|(n, v)| Header {
                    name: n.to_string(),
                    value: v.to_string(),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn defaults_without_context() {
        let t = Template::new(&url("http://a/"), None, "ua/1").unwrap();
        assert_eq!(t.method, Method::GET);
        assert_eq!(t.headers[header::USER_AGENT], "ua/1");
        assert_eq!(t.headers[header::ACCEPT_ENCODING], "identity");
        assert_eq!(t.headers[header::ACCEPT], "*/*");
        assert!(t.body.is_empty());
    }

    #[test]
    fn drops_hop_by_hop_and_controlled_headers() {
        let c = ctx(&[
            ("Connection", "keep-alive, X-Hop"),
            ("X-Hop", "1"),
            ("Keep-Alive", "timeout=5"),
            ("Transfer-Encoding", "chunked"),
            ("TE", "trailers"),
            ("Upgrade", "h2c"),
            ("Host", "evil.example"),
            ("Content-Length", "99"),
            ("Range", "bytes=5-"),
            ("If-Range", "\"x\""),
            ("If-None-Match", "\"x\""),
            ("If-Modified-Since", "Tue, 01 Jan 2026 00:00:00 GMT"),
            ("Accept-Encoding", "gzip, br"),
            ("Proxy-Authorization", "Basic xyz"),
            ("X-Keep", "yes"),
            ("Accept", "application/zip"),
            ("bad name", "x"),
            ("X-Bad-Value", "a\nb"),
        ]);
        let t = Template::new(&url("http://a/"), Some(&c), "ua").unwrap();
        let names: Vec<&str> = t.headers.keys().map(|k| k.as_str()).collect();
        for gone in [
            "connection",
            "x-hop",
            "keep-alive",
            "transfer-encoding",
            "te",
            "upgrade",
            "host",
            "content-length",
            "range",
            "if-range",
            "if-none-match",
            "if-modified-since",
            "proxy-authorization",
            "x-bad-value",
        ] {
            assert!(!names.contains(&gone), "{gone} kept: {names:?}");
        }
        assert_eq!(t.headers["x-keep"], "yes");
        assert_eq!(t.headers[header::ACCEPT], "application/zip");
        assert_eq!(t.headers[header::ACCEPT_ENCODING], "identity");
    }

    #[test]
    fn explicit_fields_win_over_headers() {
        let mut c = ctx(&[("Cookie", "a=1"), ("User-Agent", "old"), ("Referer", "x")]);
        c.cookie = Some("b=2".into());
        c.user_agent = Some("Browser/1".into());
        c.referer = Some("https://page/".into());
        let t = Template::new(&url("http://a/"), Some(&c), "ua").unwrap();
        assert_eq!(t.headers.get_all(header::COOKIE).iter().count(), 1);
        assert_eq!(t.headers[header::COOKIE], "b=2");
        assert_eq!(t.headers[header::USER_AGENT], "Browser/1");
        assert_eq!(t.headers[header::REFERER], "https://page/");
        // Without explicit fields, the header list is used.
        let t = Template::new(&url("http://a/"), Some(&ctx(&[("cookie", "a=1")])), "ua").unwrap();
        assert_eq!(t.headers[header::COOKIE], "a=1");
    }

    #[test]
    fn method_and_body() {
        let c = RequestContext {
            method: Some("post".into()),
            body_base64: Some("aGVsbG8=".into()),
            ..Default::default()
        };
        let t = Template::new(&url("http://a/"), Some(&c), "ua").unwrap();
        assert_eq!(t.method, Method::POST);
        assert_eq!(&t.body[..], b"hello");
        let bad = RequestContext {
            body_base64: Some("!!".into()),
            ..Default::default()
        };
        assert!(matches!(
            Template::new(&url("http://a/"), Some(&bad), "ua"),
            Err(SourceError::Fatal(_))
        ));
        let bad = RequestContext {
            method: Some("GE T".into()),
            ..Default::default()
        };
        assert!(matches!(
            Template::new(&url("http://a/"), Some(&bad), "ua"),
            Err(SourceError::Fatal(_))
        ));
    }

    #[test]
    fn url_credentials_become_basic_auth() {
        let t = Template::new(&url("http://us%20er:p%40ss@a/"), None, "ua").unwrap();
        // base64("us er:p@ss")
        assert_eq!(t.headers[header::AUTHORIZATION], "Basic dXMgZXI6cEBzcw==");
    }

    #[test]
    fn redirects_drop_credentials_across_origins_only() {
        let mut c = ctx(&[("Authorization", "Bearer t"), ("X-Other", "1")]);
        c.cookie = Some("s=1".into());
        let base = Template::new(&url("https://a.example/f"), Some(&c), "ua").unwrap();

        let mut t = base.clone();
        assert!(!t.redirect(
            &url("https://a.example/f"),
            &url("https://a.example/g"),
            302
        ));
        assert!(t.headers.contains_key(header::COOKIE));
        assert!(t.headers.contains_key(header::AUTHORIZATION));

        for to in [
            "https://b.example/f",
            "http://a.example/f",
            "https://a.example:8443/f",
        ] {
            let mut t = base.clone();
            assert!(
                t.redirect(&url("https://a.example/f"), &url(to), 307),
                "{to}"
            );
            assert!(!t.headers.contains_key(header::COOKIE), "{to}");
            assert!(!t.headers.contains_key(header::AUTHORIZATION), "{to}");
            assert_eq!(t.headers["x-other"], "1");
        }
    }

    #[test]
    fn redirect_methods() {
        let c = RequestContext {
            method: Some("POST".into()),
            body_base64: Some("eA==".into()),
            headers: vec![Header {
                name: "Content-Type".into(),
                value: "application/x-www-form-urlencoded".into(),
            }],
            ..Default::default()
        };
        let u = url("http://a/");
        let base = Template::new(&u, Some(&c), "ua").unwrap();
        for (status, keeps) in [
            (301, false),
            (302, false),
            (303, false),
            (307, true),
            (308, true),
        ] {
            let mut t = base.clone();
            t.redirect(&u, &u, status);
            assert_eq!(t.method == Method::POST, keeps, "{status}");
            assert_eq!(!t.body.is_empty(), keeps, "{status}");
            assert_eq!(
                t.headers.contains_key(header::CONTENT_TYPE),
                keeps,
                "{status}"
            );
        }
    }
}
