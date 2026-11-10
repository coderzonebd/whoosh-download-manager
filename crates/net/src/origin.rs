use std::fmt;
use std::net::IpAddr;

use url::Url;
use vayu_core::SourceError;

/// Scheme, host and port: the unit of connection pooling and `Alt-Svc`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Origin {
    https: bool,
    /// Lowercase domain or IP literal, without brackets.
    host: String,
    port: u16,
}

impl Origin {
    /// The origin of an `http`/`https` URL; other schemes are `Fatal`.
    pub fn parse_url(url: &Url) -> Result<Self, SourceError> {
        let https = match url.scheme() {
            "https" => true,
            "http" => false,
            s => return Err(SourceError::Fatal(format!("unsupported scheme {s:?}"))),
        };
        let host = match url.host() {
            Some(url::Host::Domain(d)) if !d.is_empty() => d.to_ascii_lowercase(),
            Some(url::Host::Ipv4(a)) => a.to_string(),
            Some(url::Host::Ipv6(a)) => a.to_string(),
            _ => return Err(SourceError::Fatal(format!("no host in {url}"))),
        };
        let port = url
            .port_or_known_default()
            .ok_or_else(|| SourceError::Fatal(format!("no port for {url}")))?;
        Ok(Self { https, host, port })
    }

    pub fn is_https(&self) -> bool {
        self.https
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn ip(&self) -> Option<IpAddr> {
        self.host.parse().ok()
    }

    /// `Host` header value: the port only when not the scheme's default.
    pub fn authority(&self) -> String {
        let host = match self.ip() {
            Some(IpAddr::V6(a)) => format!("[{a}]"),
            _ => self.host.clone(),
        };
        let default = if self.https { 443 } else { 80 };
        if self.port == default {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = if self.https { "https" } else { "http" };
        write!(f, "{scheme}://{}", self.authority())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(s: &str) -> Result<Origin, SourceError> {
        Origin::parse_url(&Url::parse(s).unwrap())
    }

    #[test]
    fn origins() {
        let a = o("https://Example.COM/x?y").unwrap();
        assert_eq!(
            (a.is_https(), a.host(), a.port()),
            (true, "example.com", 443)
        );
        assert_eq!(a.authority(), "example.com");
        assert_eq!(
            o("http://example.com:80/").unwrap().authority(),
            "example.com"
        );
        assert_eq!(
            o("http://example.com:8080/").unwrap().authority(),
            "example.com:8080"
        );
        let v6 = o("http://[::1]:81/").unwrap();
        assert_eq!(v6.host(), "::1");
        assert_eq!(v6.authority(), "[::1]:81");
        assert_eq!(v6.to_string(), "http://[::1]:81");
        assert!(matches!(
            o("ftp://example.com/"),
            Err(SourceError::Fatal(_))
        ));
        assert!(matches!(
            o("file:///etc/passwd"),
            Err(SourceError::Fatal(_))
        ));
        assert_ne!(o("http://a/").unwrap(), o("https://a/").unwrap());
    }
}
