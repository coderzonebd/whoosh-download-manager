//! File names from `Content-Disposition` (RFC 6266, RFC 8187/5987), and
//! the sanitising rules of [`whoosh_core::RemoteMeta::filename`].
//!
//! Decoding follows what browsers do, which is also what the benchmark
//! grades (`crates/bench/src/names.rs`):
//!
//! 1. `filename*` (`charset'lang'%XX…`, UTF-8 or ISO-8859-1) wins over
//!    `filename` when it decodes.
//! 2. A plain `filename` with raw non-ASCII bytes is UTF-8 when the bytes
//!    are valid UTF-8, else ISO-8859-1 (Latin-1).
//! 3. Quoted strings unescape `\"` and `\\` only; other backslashes are kept
//!    (old servers send Windows paths) and then count as path separators.

use percent_encoding::percent_decode;

/// The file name a `Content-Disposition` header value asks for, decoded and
/// [sanitised](sanitize_filename). `None` when there is no usable name.
pub fn filename_from_content_disposition(raw: &[u8]) -> Option<String> {
    let text = decode_bytes(raw);
    let params = parse_params(&text);
    let star = params
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("filename*"))
        .filter_map(|(_, v)| decode_ext_value(v))
        .find_map(|v| sanitize_filename(&v));
    star.or_else(|| {
        params
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("filename"))
            .find_map(|(_, v)| sanitize_filename(v))
    })
}

/// Reduces a suggested name to one safe path component:
///
/// - only the last component of a path (`/` and `\` both separate), so
///   `../../etc/passwd` becomes `passwd`;
/// - no control characters (C0, DEL, C1) and no bidirectional-text
///   controls (which can disguise an extension, e.g. `exe.txt`);
/// - no leading dots (no hidden files, no `..`), no leading or trailing
///   whitespace, no trailing dots;
/// - on Windows only, `<>:"|?*` become `_`;
/// - a reserved Windows device name (`CON`, `nul.txt`, `COM1`, …) gets a
///   `_` prefix on every platform, so a download can move between systems;
/// - at most 255 bytes of UTF-8, keeping a short extension.
///
/// Returns `None` when nothing usable is left.
pub fn sanitize_filename(name: &str) -> Option<String> {
    let last = name
        .split(['/', '\\'])
        .rev()
        .find(|c| !c.trim().is_empty())?;
    let mut s: String = last
        .chars()
        .filter(|&c| !c.is_control() && !is_bidi_control(c))
        .map(|c| {
            if cfg!(windows) && is_windows_reserved_char(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    s = s
        .trim_start_matches(|c: char| c == '.' || c.is_whitespace())
        .trim_end_matches(|c: char| c == '.' || c.is_whitespace())
        .to_string();
    if s.is_empty() {
        return None;
    }
    if is_windows_device_name(&s) {
        s.insert(0, '_');
    }
    Some(truncate_name(s, 255))
}

/// Header bytes as text: UTF-8 when valid, else ISO-8859-1.
fn decode_bytes(raw: &[u8]) -> String {
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) => raw.iter().map(|&b| char::from(b)).collect(),
    }
}

/// `type; name=value; name="quoted"` → `[(name, value)]`, values unquoted.
/// The disposition type itself is skipped.
fn parse_params(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut parts = split_unquoted(s, ';').into_iter();
    let _disposition_type = parts.next();
    for part in parts {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        let k = k.trim();
        let v = v.trim();
        if k.is_empty() {
            continue;
        }
        out.push((k.to_string(), unquote(v)));
    }
    out
}

/// Splits on `sep` outside double quotes (honouring `\"` inside them).
fn split_unquoted(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut quoted, mut escaped) = (0, false, false);
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
        } else if quoted && c == '\\' {
            escaped = true;
        } else if c == '"' {
            quoted = !quoted;
        } else if c == sep && !quoted {
            out.push(&s[start..i]);
            start = i + c.len_utf8();
        }
    }
    out.push(&s[start..]);
    out
}

/// Removes surrounding quotes; inside them `\"` → `"` and `\\` → `\`.
fn unquote(v: &str) -> String {
    let Some(inner) = v.strip_prefix('"') else {
        return v.to_string();
    };
    let inner = inner.strip_suffix('"').unwrap_or(inner);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && matches!(chars.peek(), Some('"' | '\\')) {
            out.extend(chars.next());
        } else {
            out.push(c);
        }
    }
    out
}

/// RFC 8187 `ext-value`: `charset'[language]'pct-encoded`.
fn decode_ext_value(v: &str) -> Option<String> {
    let mut it = v.splitn(3, '\'');
    let charset = it.next()?.trim();
    let _language = it.next()?;
    let encoded = it.next()?;
    let bytes: Vec<u8> = percent_decode(encoded.as_bytes()).collect();
    if charset.eq_ignore_ascii_case("iso-8859-1") || charset.eq_ignore_ascii_case("latin1") {
        Some(bytes.iter().map(|&b| char::from(b)).collect())
    } else {
        // UTF-8 is the only charset RFC 8187 requires; try it for unknown
        // labels too rather than guess.
        String::from_utf8(bytes).ok()
    }
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}

fn is_windows_reserved_char(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')
}

/// `CON`, `PRN`, `AUX`, `NUL`, `COM0-9`, `LPT0-9` (also with superscript
/// digits), alone or followed by an extension, in any case.
fn is_windows_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    let upper = stem.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let mut chars = upper.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    let rest: Vec<char> = chars.collect();
    (prefix == "COM" || prefix == "LPT")
        && rest.len() == 1
        && matches!(rest[0], '0'..='9' | '¹' | '²' | '³')
}

/// Cuts `s` to at most `max` bytes at a character boundary, keeping an
/// extension of up to 16 bytes.
fn truncate_name(s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let ext = s
        .rfind('.')
        .map(|i| &s[i..])
        .filter(|e| e.len() <= 16)
        .unwrap_or("");
    let budget = max - ext.len();
    let mut cut = budget.min(s.len() - ext.len());
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{}", &s[..cut], ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cd(raw: &[u8]) -> Option<String> {
        filename_from_content_disposition(raw)
    }

    #[test]
    fn star_wins_over_plain() {
        let h = "attachment; filename=\"bangla-report.pdf\"; filename*=UTF-8''%E0%A6%AC%E0%A6%BE%E0%A6%82%E0%A6%B2%E0%A6%BE%20%E0%A7%A8.pdf";
        assert_eq!(cd(h.as_bytes()).as_deref(), Some("বাংলা ২.pdf"));
    }

    #[test]
    fn star_only_and_latin1_charset() {
        assert_eq!(
            cd(b"attachment; filename*=utf-8''%E0%A6%AA.zip").as_deref(),
            Some("প.zip")
        );
        assert_eq!(
            cd(b"attachment; filename*=ISO-8859-1'en'r%E9sum%E9.pdf").as_deref(),
            Some("résumé.pdf")
        );
        // Quoted ext-value (invalid but seen in the wild).
        assert_eq!(
            cd(b"attachment; filename*=\"UTF-8''a%20b.txt\"").as_deref(),
            Some("a b.txt")
        );
    }

    #[test]
    fn bad_star_falls_back_to_plain() {
        assert_eq!(
            cd(b"attachment; filename*=UTF-8''%FF%FE; filename=\"ok.bin\"").as_deref(),
            Some("ok.bin")
        );
    }

    #[test]
    fn quoted_string_escapes() {
        let h = br#"attachment; filename="annual \"final\" report.pdf""#;
        let want = if cfg!(windows) {
            "annual _final_ report.pdf"
        } else {
            "annual \"final\" report.pdf"
        };
        assert_eq!(cd(h).as_deref(), Some(want));
        // A Windows path: backslashes are separators, not escapes.
        assert_eq!(
            cd(br#"attachment; filename="C:\temp\x.txt""#).as_deref(),
            Some("x.txt")
        );
        // `;` inside quotes does not split.
        assert_eq!(
            cd(br#"inline; filename="a;b.txt""#).as_deref(),
            Some("a;b.txt")
        );
    }

    #[test]
    fn raw_bytes_utf8_then_latin1() {
        assert_eq!(
            cd(b"attachment; filename=\"r\xe9sum\xe9.pdf\"").as_deref(),
            Some("résumé.pdf")
        );
        let raw = b"attachment; filename=\"\xe0\xa6\xac\xe0\xa6\xbe\xe0\xa6\x82\xe0\xa6\xb2\xe0\xa6\xbe.txt\"";
        assert_eq!(cd(raw).as_deref(), Some("বাংলা.txt"));
    }

    #[test]
    fn token_values_and_case() {
        assert_eq!(
            cd(b"ATTACHMENT; FILENAME=data.csv").as_deref(),
            Some("data.csv")
        );
        assert_eq!(
            cd(b"attachment; filename = my file.zip ;").as_deref(),
            Some("my file.zip")
        );
        assert_eq!(cd(b"attachment"), None);
        assert_eq!(cd(b"inline; size=3"), None);
        assert_eq!(cd(b""), None);
    }

    #[test]
    fn traversal_and_hidden() {
        assert_eq!(
            cd(b"attachment; filename=\"../../etc/passwd\"").as_deref(),
            Some("passwd")
        );
        assert_eq!(sanitize_filename("..").as_deref(), None);
        assert_eq!(sanitize_filename("../").as_deref(), None);
        assert_eq!(sanitize_filename("/").as_deref(), None);
        assert_eq!(sanitize_filename(".bashrc").as_deref(), Some("bashrc"));
        assert_eq!(sanitize_filename("dir/").as_deref(), Some("dir"));
        assert_eq!(sanitize_filename("  a.txt. . ").as_deref(), Some("a.txt"));
    }

    #[test]
    fn controls_and_bidi_removed() {
        assert_eq!(
            sanitize_filename("a\u{0}b\nc\u{7f}.txt").as_deref(),
            Some("abc.txt")
        );
        assert_eq!(
            sanitize_filename("photo\u{202e}gpj.exe").as_deref(),
            Some("photogpj.exe")
        );
        assert_eq!(sanitize_filename("\u{85}x").as_deref(), Some("x"));
    }

    #[test]
    fn windows_device_names() {
        assert_eq!(sanitize_filename("CON").as_deref(), Some("_CON"));
        assert_eq!(sanitize_filename("nul.txt").as_deref(), Some("_nul.txt"));
        assert_eq!(
            sanitize_filename("com1.tar.gz").as_deref(),
            Some("_com1.tar.gz")
        );
        assert_eq!(sanitize_filename("LPT9").as_deref(), Some("_LPT9"));
        assert_eq!(
            sanitize_filename("console.log").as_deref(),
            Some("console.log")
        );
        assert_eq!(sanitize_filename("com10").as_deref(), Some("com10"));
    }

    #[test]
    fn long_names_keep_extension() {
        let long = format!("{}.pdf", "ব".repeat(200)); // 600 bytes + 4
        let s = sanitize_filename(&long).unwrap();
        assert!(s.len() <= 255, "{}", s.len());
        assert!(s.ends_with(".pdf"));
        assert!(s.starts_with('ব'));
    }
}
