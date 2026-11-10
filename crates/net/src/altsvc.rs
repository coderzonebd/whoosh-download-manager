//! `Alt-Svc` (RFC 7838): which `https` origins offer HTTP/3, cached per
//! origin until `ma` (default 24 h) expires.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use http::header::{self, HeaderMap};

use crate::lock;
use crate::origin::Origin;

/// RFC 7838 §3.1: freshness when `ma` is absent.
const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 3600);
/// How long HTTP/3 is not tried again for an origin after it failed or
/// lost the race against TCP.
pub(crate) const BROKEN_FOR: Duration = Duration::from_secs(300);

/// An HTTP/3 endpoint for an origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct H3Alt {
    /// Alternative host; `None` means the origin's own host.
    pub host: Option<String>,
    pub port: u16,
}

#[derive(Debug)]
struct Entry {
    alt: H3Alt,
    expires: Instant,
}

#[derive(Debug, Default)]
pub(crate) struct AltSvcCache {
    entries: Mutex<HashMap<Origin, Entry>>,
    broken: Mutex<HashMap<Origin, Instant>>,
}

impl AltSvcCache {
    /// Learns from a response that came over TCP from `origin`.
    pub fn observe(&self, origin: &Origin, headers: &HeaderMap) {
        if !origin.is_https() {
            return; // RFC 9114 §3.1.2: only for https origins.
        }
        let Some(v) = headers.get(header::ALT_SVC) else {
            return;
        };
        let Ok(v) = v.to_str() else { return };
        match parse(v) {
            Parsed::Clear => {
                lock(&self.entries).remove(origin);
            }
            Parsed::H3 { alt, max_age } => {
                let expires = Instant::now() + max_age;
                lock(&self.entries).insert(origin.clone(), Entry { alt, expires });
            }
            Parsed::Nothing => {}
        }
    }

    /// Records an HTTP/3 endpoint for `origin` (from a host profile).
    pub fn remember(&self, origin: &Origin, alt: H3Alt, ttl: Duration) {
        if origin.is_https() && !ttl.is_zero() {
            let expires = Instant::now() + ttl;
            lock(&self.entries).insert(origin.clone(), Entry { alt, expires });
            lock(&self.broken).remove(origin);
        }
    }

    /// The HTTP/3 endpoint to try for `origin`, unless unknown, expired or
    /// recently broken.
    pub fn get(&self, origin: &Origin) -> Option<H3Alt> {
        let now = Instant::now();
        {
            let mut broken = lock(&self.broken);
            match broken.get(origin) {
                Some(&until) if until > now => return None,
                Some(_) => {
                    broken.remove(origin);
                }
                None => {}
            }
        }
        let mut entries = lock(&self.entries);
        match entries.get(origin) {
            Some(e) if e.expires > now => Some(e.alt.clone()),
            Some(_) => {
                entries.remove(origin);
                None
            }
            None => None,
        }
    }

    /// Stops using HTTP/3 for `origin` for a while.
    pub fn mark_broken(&self, origin: &Origin) {
        lock(&self.broken).insert(origin.clone(), Instant::now() + BROKEN_FOR);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    Clear,
    H3 { alt: H3Alt, max_age: Duration },
    Nothing,
}

/// The first `h3` alternative in an `Alt-Svc` value.
fn parse(v: &str) -> Parsed {
    if v.trim().eq_ignore_ascii_case("clear") {
        return Parsed::Clear;
    }
    for alt in split_unquoted(v, ',') {
        let mut parts = split_unquoted(alt, ';').into_iter();
        let Some((proto, authority)) = parts.next().and_then(|p| p.split_once('=')) else {
            continue;
        };
        if proto.trim() != "h3" {
            continue;
        }
        let authority = authority.trim().trim_matches('"');
        let Some((host, port)) = authority.rsplit_once(':') else {
            continue;
        };
        let Ok(port) = port.parse::<u16>() else {
            continue;
        };
        if port == 0 {
            continue;
        }
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let mut max_age = DEFAULT_MAX_AGE;
        for p in parts {
            if let Some((k, val)) = p.split_once('=')
                && k.trim().eq_ignore_ascii_case("ma")
                && let Ok(secs) = val.trim().trim_matches('"').parse::<u64>()
            {
                max_age = Duration::from_secs(secs);
            }
        }
        if max_age.is_zero() {
            continue;
        }
        return Parsed::H3 {
            alt: H3Alt {
                host: (!host.is_empty()).then(|| host.to_ascii_lowercase()),
                port,
            },
            max_age,
        };
    }
    Parsed::Nothing
}

fn split_unquoted(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut quoted) = (0, false);
    for (i, c) in s.char_indices() {
        if c == '"' {
            quoted = !quoted;
        } else if c == sep && !quoted {
            out.push(&s[start..i]);
            start = i + 1;
        }
    }
    out.push(&s[start..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn h3(host: Option<&str>, port: u16, secs: u64) -> Parsed {
        Parsed::H3 {
            alt: H3Alt {
                host: host.map(Into::into),
                port,
            },
            max_age: Duration::from_secs(secs),
        }
    }

    #[test]
    fn parses_common_forms() {
        assert_eq!(parse("h3=\":443\""), h3(None, 443, 86400));
        assert_eq!(
            parse("h3-29=\":443\"; ma=60, h3=\":8443\"; ma=3600; persist=1"),
            h3(None, 8443, 3600)
        );
        assert_eq!(
            parse("h3=\"Alt.Example:443\";ma=10"),
            h3(Some("alt.example"), 443, 10)
        );
        assert_eq!(parse("h3=\"[::1]:443\""), h3(Some("::1"), 443, 86400));
        assert_eq!(parse("clear"), Parsed::Clear);
        assert_eq!(parse("h2=\":443\""), Parsed::Nothing);
        assert_eq!(parse("h3=\":0\""), Parsed::Nothing);
        assert_eq!(parse("h3=\":443\"; ma=0"), Parsed::Nothing);
        assert_eq!(parse("garbage"), Parsed::Nothing);
    }

    #[test]
    fn cache_honours_scheme_expiry_clear_and_broken() {
        let c = AltSvcCache::default();
        let https = Origin::parse_url(&url::Url::parse("https://a.test/x").unwrap()).unwrap();
        let http = Origin::parse_url(&url::Url::parse("http://a.test/x").unwrap()).unwrap();
        let mut hm = HeaderMap::new();
        hm.insert(
            header::ALT_SVC,
            HeaderValue::from_static("h3=\":443\"; ma=1"),
        );
        c.observe(&http, &hm);
        assert_eq!(c.get(&http), None, "plain http never uses Alt-Svc");
        c.observe(&https, &hm);
        assert_eq!(c.get(&https).map(|a| a.port), Some(443));
        c.mark_broken(&https);
        assert_eq!(c.get(&https), None);
        lock(&c.broken).clear();
        assert!(c.get(&https).is_some());
        hm.insert(header::ALT_SVC, HeaderValue::from_static("clear"));
        c.observe(&https, &hm);
        assert_eq!(c.get(&https), None);
        // Expiry.
        hm.insert(
            header::ALT_SVC,
            HeaderValue::from_static("h3=\":443\"; ma=1"),
        );
        c.observe(&https, &hm);
        lock(&c.entries).get_mut(&https).unwrap().expires = Instant::now();
        assert_eq!(c.get(&https), None);
    }
}
