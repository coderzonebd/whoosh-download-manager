//! Mark of the web: telling the operating system that a file came from the
//! internet, so its protections apply when the user opens it (MASTER-PLAN
//! §5), as browsers do.
//!
//! - **Windows:** a `Zone.Identifier` alternate data stream holding
//!   `ZoneId=3` (the Internet zone) and, when known, `ReferrerUrl` and
//!   `HostUrl`. SmartScreen, Office's Protected View and the "this file came
//!   from another computer" prompt read it. Needs NTFS or ReFS; FAT and
//!   exFAT have no streams.
//! - **macOS:** the `com.apple.quarantine` extended attribute, which
//!   Gatekeeper checks on first open: `0081;<hex Unix time>;Whoosh;`.
//! - **Linux:** no OS protection reads a mark, but file managers show the
//!   freedesktop.org attributes `user.xdg.origin.url` and
//!   `user.xdg.referrer.url`, so they are written where the filesystem
//!   allows user attributes.
//!
//! The marks move with a rename on the same filesystem, so the engine marks
//! the in-progress file before its final move ([`whoosh_core::Storage::
//! mark_origin`]) and the finished file is never seen without one.
//!
//! **What is recorded of the URLs:** scheme, host, port and path. User
//! names, passwords, the query and the fragment are dropped: download
//! links often carry tokens there, and the engine clears such secrets once a
//! download completes, so they must not live on in the file's metadata.
//! Only `http`, `https`, `ftp` and `ftps` URLs are recorded.

use std::io;
use std::path::Path;

/// Where a file came from, as recorded in its mark.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Origin {
    /// The download's URL (after redirects), shortened (see the module docs).
    pub host_url: Option<String>,
    /// The page that linked to it, shortened the same way.
    pub referrer_url: Option<String>,
}

impl Origin {
    pub fn new(url: &str, referrer: Option<&str>) -> Self {
        Self {
            host_url: shorten(url),
            referrer_url: referrer.and_then(shorten),
        }
    }
}

/// Marks `path` as downloaded from the internet (see the module docs).
/// Fails where the filesystem cannot hold the mark; callers treat that as
/// a warning.
pub fn mark_of_the_web(path: &Path, origin: &Origin) -> io::Result<()> {
    imp::mark(path, origin)
}

/// The `Zone.Identifier` stream's contents, CRLF-terminated like the ones
/// Windows and browsers write.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn zone_identifier(origin: &Origin) -> String {
    let mut s = String::from("[ZoneTransfer]\r\nZoneId=3\r\n");
    if let Some(r) = &origin.referrer_url {
        s.push_str(&format!("ReferrerUrl={r}\r\n"));
    }
    if let Some(h) = &origin.host_url {
        s.push_str(&format!("HostUrl={h}\r\n"));
    }
    s
}

/// The `com.apple.quarantine` value: flags (`0081`: downloaded, not yet
/// approved by the user), the time in hex, the agent's name, and an empty
/// event id (no entry in the QuarantineEvents database).
#[cfg_attr(not(target_vendor = "apple"), allow(dead_code))]
pub(crate) fn quarantine_value(unix_secs: u64) -> String {
    format!("0081;{unix_secs:08x};Whoosh;")
}

/// `scheme://host[:port]/path`, without user info, query or fragment; `None`
/// for any other scheme or something that is not a URL.
pub(crate) fn shorten(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https" | "ftp" | "ftps") {
        return None;
    }
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, "/"),
    };
    let host = authority.rsplit('@').next().unwrap_or_default();
    if host.is_empty() || host.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    if path.chars().any(char::is_control) {
        return None;
    }
    Some(format!("{scheme}://{host}{path}"))
}

#[cfg(windows)]
mod imp {
    use std::ffi::OsString;
    use std::io;
    use std::path::Path;

    use super::Origin;

    pub(super) fn mark(path: &Path, origin: &Origin) -> io::Result<()> {
        let mut stream = OsString::from(path.as_os_str());
        stream.push(":Zone.Identifier");
        std::fs::write(stream, super::zone_identifier(origin))
    }
}

#[cfg(target_vendor = "apple")]
mod imp {
    use std::io;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::Origin;

    pub(super) fn mark(path: &Path, _origin: &Origin) -> io::Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let value = super::quarantine_value(now);
        rustix::fs::lsetxattr(
            path,
            "com.apple.quarantine",
            value.as_bytes(),
            rustix::fs::XattrFlags::empty(),
        )
        .map_err(io::Error::from)
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod imp {
    use std::io;
    use std::path::Path;

    use super::Origin;

    pub(super) fn mark(path: &Path, origin: &Origin) -> io::Result<()> {
        let attrs = [
            ("user.xdg.origin.url", &origin.host_url),
            ("user.xdg.referrer.url", &origin.referrer_url),
        ];
        for (name, value) in attrs {
            if let Some(v) = value {
                rustix::fs::lsetxattr(path, name, v.as_bytes(), rustix::fs::XattrFlags::empty())
                    .map_err(io::Error::from)?;
            }
        }
        Ok(())
    }
}

#[cfg(not(any(
    windows,
    target_vendor = "apple",
    target_os = "linux",
    target_os = "android"
)))]
mod imp {
    use std::io;
    use std::path::Path;

    use super::Origin;

    pub(super) fn mark(_path: &Path, _origin: &Origin) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_keep_where_and_drop_secrets() {
        assert_eq!(
            shorten("https://user:pw@cdn.example.com:8443/a/b.zip?token=s3cret#x").as_deref(),
            Some("https://cdn.example.com:8443/a/b.zip")
        );
        assert_eq!(
            shorten("HTTP://example.com").as_deref(),
            Some("http://example.com/")
        );
        assert_eq!(
            shorten("ftp://example.com/pub/f.iso").as_deref(),
            Some("ftp://example.com/pub/f.iso")
        );
        for bad in [
            "data:text/plain,hi",
            "blob:https://x/1",
            "file:///etc/passwd",
            "not a url",
            "https://",
            "https://exa mple.com/",
            "https://example.com/a\r\nZoneId=0",
        ] {
            assert_eq!(shorten(bad), None, "{bad}");
        }
    }

    #[test]
    fn zone_identifier_has_the_internet_zone_and_known_urls() {
        let o = Origin::new(
            "https://cdn.example.com/f.exe?sig=1",
            Some("https://example.com/download"),
        );
        assert_eq!(
            zone_identifier(&o),
            "[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://example.com/download\r\n\
             HostUrl=https://cdn.example.com/f.exe\r\n"
        );
        assert_eq!(
            zone_identifier(&Origin::new("magnet:?xt=1", None)),
            "[ZoneTransfer]\r\nZoneId=3\r\n"
        );
    }

    #[test]
    fn quarantine_value_has_flags_time_and_agent() {
        assert_eq!(quarantine_value(0x6000_0000), "0081;60000000;Whoosh;");
    }

    /// The mark lands on the file and survives a rename, which is how the
    /// finished file gets it.
    #[test]
    fn the_mark_moves_with_a_rename() {
        let d = tempfile::tempdir().unwrap();
        let part = d.path().join("f.bin.whoosh-part");
        std::fs::write(&part, b"data").unwrap();
        let o = Origin::new(
            "https://cdn.example.com/f.bin?t=1",
            Some("https://example.com/"),
        );
        let marked = mark_of_the_web(&part, &o);
        let done = d.path().join("f.bin");
        std::fs::rename(&part, &done).unwrap();
        assert_eq!(std::fs::read(&done).unwrap(), b"data");
        match read_mark(&done) {
            Some(mark) => {
                marked.unwrap();
                assert_eq!(mark, expected(&o));
            }
            // A filesystem without user attributes (tmpfs before Linux 6.6).
            None => assert!(cfg!(target_os = "linux") && marked.is_err(), "{marked:?}"),
        }
    }

    #[cfg(windows)]
    fn read_mark(p: &Path) -> Option<String> {
        let mut s = std::ffi::OsString::from(p.as_os_str());
        s.push(":Zone.Identifier");
        Some(std::fs::read_to_string(s).unwrap())
    }

    #[cfg(windows)]
    fn expected(o: &Origin) -> String {
        zone_identifier(o)
    }

    #[cfg(target_vendor = "apple")]
    fn read_mark(p: &Path) -> Option<String> {
        let mut buf = vec![0u8; 256];
        let n = rustix::fs::lgetxattr(p, "com.apple.quarantine", &mut buf).unwrap();
        let value = String::from_utf8(buf[..n].to_vec()).unwrap();
        // The time varies: compare the rest.
        let mut fields: Vec<&str> = value.split(';').collect();
        assert!(u64::from_str_radix(fields[1], 16).is_ok(), "{value}");
        fields[1] = "*";
        Some(fields.join(";"))
    }

    #[cfg(target_vendor = "apple")]
    fn expected(_: &Origin) -> String {
        "0081;*;Whoosh;".into()
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn read_mark(p: &Path) -> Option<String> {
        let mut buf = vec![0u8; 256];
        let n = rustix::fs::lgetxattr(p, "user.xdg.origin.url", &mut buf).ok()?;
        Some(String::from_utf8(buf[..n].to_vec()).unwrap())
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn expected(o: &Origin) -> String {
        o.host_url.clone().unwrap()
    }
}
