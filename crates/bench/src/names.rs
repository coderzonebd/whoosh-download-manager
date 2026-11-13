//! Grading the file name a client chose, for the `Content-Disposition`
//! scenario (`bangla-filename`).
//!
//! The expectations follow RFC 6266: `filename*` (RFC 8187/5987, UTF-8)
//! wins over `filename`; a raw non-ASCII `filename` is decoded the way
//! browsers do (UTF-8 if valid, else ISO-8859-1); a name with directory
//! parts must be reduced to its last component and stay inside the
//! download directory.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// How a client named the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NameCheck {
    /// Exactly the name the header asks for.
    Expected,
    /// The expected name with characters Windows forbids (`<>:"/\|?*`)
    /// replaced or dropped, or a traversal reduced to one safe component
    /// other than the basename: acceptable.
    Sanitized,
    /// The ASCII `filename=` fallback although a `filename*` was sent.
    AsciiFallback,
    /// The header's raw bytes used as the name, undecoded (not valid UTF-8).
    RawBytes,
    /// Ignored the header and used the URL's last segment.
    UrlName,
    /// Some other name (mangled encoding, replaced characters, …).
    Altered,
    /// Wrote outside the download directory (path traversal).
    Escaped,
}

impl NameCheck {
    /// Expected or an acceptable sanitisation.
    pub fn is_ok(self) -> bool {
        matches!(self, NameCheck::Expected | NameCheck::Sanitized)
    }

    pub fn label(self) -> &'static str {
        match self {
            NameCheck::Expected => "expected",
            NameCheck::Sanitized => "sanitised",
            NameCheck::AsciiFallback => "ASCII fallback",
            NameCheck::RawBytes => "undecoded bytes",
            NameCheck::UrlName => "URL name",
            NameCheck::Altered => "altered",
            NameCheck::Escaped => "ESCAPED DIR",
        }
    }
}

/// The name a correct client saves `file` of `scenario` under, and the
/// ASCII fallback if there is one. `None` when the scenario does not test
/// names (any name is fine).
pub fn expected(scenario: &str, file: &str) -> Option<(&'static str, Option<&'static str>)> {
    if scenario != "bangla-filename" {
        return None;
    }
    Some(match file {
        "bangla.bin" => ("বাংলা প্রতিবেদন ২০২৬.pdf", Some("bangla-report.pdf")),
        "bangla-star-only.bin" => ("পরীক্ষা.zip", None),
        "plain.bin" => ("annual \"final\" report.pdf", None),
        "latin1.bin" => ("résumé.pdf", None),
        "raw-utf8.bin" => ("বাংলা.txt", None),
        "traversal.bin" => ("passwd", None),
        _ => return None,
    })
}

/// How the raw header bytes look as a name after [`escape_name`], for the
/// file whose header is not valid UTF-8.
fn raw_form(file: &str) -> Option<&'static str> {
    (file == "latin1.bin").then_some("r\\xe9sum\\xe9.pdf")
}

/// `name` with every character Windows forbids in file names replaced by
/// `_`, and with them removed.
fn windows_safe(name: &str) -> [String; 2] {
    let bad = |c: char| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*');
    [
        name.chars().map(|c| if bad(c) { '_' } else { c }).collect(),
        name.chars().filter(|c| !bad(*c)).collect(),
    ]
}

/// A file name as text: valid UTF-8 as is, other bytes as `\xNN`.
pub fn escape_name(name: &std::ffi::OsStr) -> String {
    #[cfg(unix)]
    {
        use std::fmt::Write as _;
        use std::os::unix::ffi::OsStrExt;
        let mut out = String::new();
        for chunk in name.as_bytes().utf8_chunks() {
            out.push_str(chunk.valid());
            for b in chunk.invalid() {
                let _ = write!(out, "\\x{b:02x}");
            }
        }
        out
    }
    #[cfg(not(unix))]
    {
        name.to_string_lossy().into_owned()
    }
}

/// Grades the name a client saved `file` under (as [`escape_name`] shows
/// it). `escaped` is true when the client wrote outside its download
/// directory.
pub fn grade(scenario: &str, file: &str, saved: &str, escaped: bool) -> Option<NameCheck> {
    if escaped {
        return Some(NameCheck::Escaped);
    }
    let (want, fallback) = expected(scenario, file)?;
    let base = Path::new(saved)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(if base == want {
        NameCheck::Expected
    } else if Some(base.as_str()) == fallback {
        NameCheck::AsciiFallback
    } else if base == file {
        NameCheck::UrlName
    } else if file == "traversal.bin" && !base.is_empty() && base != ".." {
        // Contained (not escaped) single component: a safe sanitisation.
        NameCheck::Sanitized
    } else if windows_safe(want).contains(&base) {
        NameCheck::Sanitized
    } else if Some(base.as_str()) == raw_form(file) {
        NameCheck::RawBytes
    } else {
        NameCheck::Altered
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grades() {
        let g = |f, s| grade("bangla-filename", f, s, false).unwrap();
        assert_eq!(
            g("bangla.bin", "বাংলা প্রতিবেদন ২০২৬.pdf"),
            NameCheck::Expected
        );
        assert_eq!(
            g("bangla.bin", "bangla-report.pdf"),
            NameCheck::AsciiFallback
        );
        assert_eq!(g("bangla.bin", "bangla.bin"), NameCheck::UrlName);
        assert_eq!(g("latin1.bin", "résumé.pdf"), NameCheck::Expected);
        assert_eq!(g("latin1.bin", "r\\xe9sum\\xe9.pdf"), NameCheck::RawBytes);
        assert_eq!(
            g("latin1.bin", "r\u{fffd}sum\u{fffd}.pdf"),
            NameCheck::Altered
        );
        assert_eq!(
            g("plain.bin", "annual \"final\" report.pdf"),
            NameCheck::Expected
        );
        assert_eq!(
            g("plain.bin", "annual _final_ report.pdf"),
            NameCheck::Sanitized
        );
        assert_eq!(
            g("plain.bin", "annual final report.pdf"),
            NameCheck::Sanitized
        );
        assert_eq!(
            g("plain.bin", "annual 'final' report.pdf"),
            NameCheck::Altered
        );
        assert_eq!(g("traversal.bin", "passwd"), NameCheck::Expected);
        assert_eq!(g("traversal.bin", ".._.._etc_passwd"), NameCheck::Sanitized);
        assert_eq!(g("traversal.bin", "traversal.bin"), NameCheck::UrlName);
        assert_eq!(
            grade("bangla-filename", "traversal.bin", "passwd", true),
            Some(NameCheck::Escaped)
        );
        assert_eq!(grade("clean", "1m.bin", "1m.bin", false), None);
        assert!(NameCheck::Expected.is_ok());
        assert!(NameCheck::Sanitized.is_ok());
        assert!(!NameCheck::AsciiFallback.is_ok());
        assert!(!NameCheck::RawBytes.is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn escapes_invalid_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::OsStr::from_bytes(b"r\xe9sum\xe9.pdf");
        assert_eq!(escape_name(raw), "r\\xe9sum\\xe9.pdf");
        assert_eq!(escape_name(std::ffi::OsStr::new("বাংলা.txt")), "বাংলা.txt");
    }
}
