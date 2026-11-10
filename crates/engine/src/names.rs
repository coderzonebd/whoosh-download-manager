//! File names: where they come from, how they are made safe, and how
//! collisions are numbered.
//!
//! Priority: the user's name, then `RemoteMeta::filename`
//! (`Content-Disposition`), then the last segment of the URL path
//! (percent-decoded), then `download`. Every candidate is sanitised here
//! again, even though `vayu-net` already sanitises `Content-Disposition`
//! (defence in depth), and the final path is checked to stay inside the
//! download directory.

use std::path::{Component, Path, PathBuf};

use percent_encoding::percent_decode_str;

/// Suffix of in-progress files: `<final name>.vayu-part`.
pub(crate) const PART_SUFFIX: &str = ".vayu-part";

/// The name used when nothing better is known.
pub(crate) const FALLBACK: &str = "download";

/// Longest name we produce, in bytes (common file-system limit).
const MAX_LEN: usize = 255 - PART_SUFFIX.len();

const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

const DOUBLE_EXTENSIONS: [&str; 7] = [
    ".tar.gz",
    ".tar.bz2",
    ".tar.xz",
    ".tar.zst",
    ".tar.lz",
    ".tar.lzma",
    ".tar.z",
];

/// A safe single path component, or `None` if nothing usable is left.
///
/// Takes the last `/` or `\` separated part (as browsers do with
/// `../../x`), drops control characters, replaces characters Windows
/// forbids, trims spaces and dots at both ends, defuses reserved device
/// names (`CON`, `LPT1`, …) and limits the length while keeping the
/// extension.
pub(crate) fn sanitize(raw: &str) -> Option<String> {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = last
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            c => c,
        })
        .collect();
    let trimmed = s.trim_matches(|c: char| c.is_whitespace() || c == '.');
    if trimmed.is_empty() {
        return None;
    }
    s = trimmed.to_string();
    let stem_upper = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem_upper.as_str()) {
        s.insert(0, '_');
    }
    if s.len() > MAX_LEN {
        s = truncate_keep_extension(&s, MAX_LEN);
    }
    Some(s)
}

fn truncate_keep_extension(s: &str, max: usize) -> String {
    let (stem, ext) = split_extension(s);
    let ext = if ext.len() <= 32 { ext } else { "" };
    let mut budget = max.saturating_sub(ext.len());
    while !stem.is_char_boundary(budget.min(stem.len())) {
        budget -= 1;
    }
    format!("{}{ext}", &stem[..budget.min(stem.len())])
}

/// `(stem, extension-with-dot)`; known double extensions stay together
/// (`a.tar.gz` → `("a", ".tar.gz")`) and a leading dot is not an extension.
pub(crate) fn split_extension(name: &str) -> (&str, &str) {
    let lower = name.to_ascii_lowercase();
    for ext in DOUBLE_EXTENSIONS {
        if lower.len() > ext.len() && lower.ends_with(ext) {
            let at = name.len() - ext.len();
            return (&name[..at], &name[at..]);
        }
    }
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// The last segment of the URL's path, percent-decoded and sanitised.
pub(crate) fn from_url(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map_or(url, |(_, r)| r);
    let path_start = after_scheme.find('/')?;
    let path = &after_scheme[path_start..];
    let path = path.split(['?', '#']).next().unwrap_or("");
    let last = path.rsplit('/').next().unwrap_or("");
    if last.is_empty() {
        return None;
    }
    let decoded = percent_decode_str(last).decode_utf8_lossy();
    sanitize(&decoded)
}

/// A file extension for common MIME types, for names that have none.
fn extension_for_mime(content_type: &str) -> Option<&'static str> {
    let mime = content_type.split(';').next()?.trim().to_ascii_lowercase();
    Some(match mime.as_str() {
        "application/pdf" => ".pdf",
        "application/zip" => ".zip",
        "application/gzip" | "application/x-gzip" => ".gz",
        "application/x-7z-compressed" => ".7z",
        "application/vnd.rar" | "application/x-rar-compressed" => ".rar",
        "application/x-tar" => ".tar",
        "application/json" => ".json",
        "text/plain" => ".txt",
        "text/html" => ".html",
        "text/csv" => ".csv",
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/svg+xml" => ".svg",
        "audio/mpeg" => ".mp3",
        "audio/ogg" => ".ogg",
        "audio/flac" => ".flac",
        "video/mp4" => ".mp4",
        "video/webm" => ".webm",
        "video/x-matroska" => ".mkv",
        "application/x-msdownload" | "application/vnd.microsoft.portable-executable" => ".exe",
        "application/x-iso9660-image" => ".iso",
        "application/vnd.android.package-archive" => ".apk",
        _ => return None,
    })
}

/// The final name by priority (see the module docs). `user` must already
/// be sanitised (the service rejects names that sanitise to nothing).
pub(crate) fn resolve(
    user: Option<&str>,
    remote: Option<&str>,
    url: &str,
    content_type: Option<&str>,
) -> String {
    if let Some(u) = user {
        return u.to_string();
    }
    if let Some(r) = remote.and_then(sanitize) {
        return r;
    }
    if let Some(u) = from_url(url) {
        if split_extension(&u).1.is_empty()
            && let Some(ext) = content_type.and_then(extension_for_mime)
        {
            return format!("{u}{ext}");
        }
        return u;
    }
    match content_type.and_then(extension_for_mime) {
        Some(ext) => format!("{FALLBACK}{ext}"),
        None => FALLBACK.to_string(),
    }
}

/// `name (n).ext` for collision number `n` (n ≥ 1).
pub(crate) fn numbered(name: &str, n: u32) -> String {
    let (stem, ext) = split_extension(name);
    format!("{stem} ({n}){ext}")
}

/// `dir/name`, only if `name` is a single normal path component, so the
/// result can never leave `dir`.
pub(crate) fn inside(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut comps = Path::new(name).components();
    match (comps.next(), comps.next()) {
        (Some(Component::Normal(_)), None) => {
            let p = dir.join(name);
            (p.parent() == Some(dir)).then_some(p)
        }
        _ => None,
    }
}

/// The in-progress path for a final name.
pub(crate) fn part_path(dir: &Path, name: &str) -> Option<PathBuf> {
    inside(dir, &format!("{name}{PART_SUFFIX}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitises() {
        assert_eq!(sanitize("../../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(sanitize("..\\..\\boot.ini").as_deref(), Some("boot.ini"));
        assert_eq!(
            sanitize("a<b>c:d\"e|f?g*h").as_deref(),
            Some("a_b_c_d_e_f_g_h")
        );
        assert_eq!(sanitize("  .hidden. ").as_deref(), Some("hidden"));
        assert_eq!(sanitize("bad\u{0}\u{7}name\n").as_deref(), Some("badname"));
        assert_eq!(sanitize("con.txt").as_deref(), Some("_con.txt"));
        assert_eq!(sanitize("LPT1").as_deref(), Some("_LPT1"));
        assert_eq!(sanitize("..").as_deref(), None);
        assert_eq!(sanitize("dir/").as_deref(), None);
        assert_eq!(sanitize("").as_deref(), None);
        assert_eq!(sanitize("বাংলা ফাইল.pdf").as_deref(), Some("বাংলা ফাইল.pdf"));
        let long = format!("{}.iso", "ক".repeat(200));
        let s = sanitize(&long).unwrap();
        assert!(s.len() <= MAX_LEN && s.ends_with(".iso"), "{s}");
    }

    #[test]
    fn url_names() {
        assert_eq!(
            from_url("https://h/a/b/My%20File%E0%A6%95.zip?x=1#f").as_deref(),
            Some("My Fileক.zip")
        );
        assert_eq!(from_url("https://h/").as_deref(), None);
        assert_eq!(from_url("https://h").as_deref(), None);
        assert_eq!(from_url("https://h/a/%2E%2E").as_deref(), None);
        assert_eq!(
            from_url("https://h/x/..%2F..%2Fevil.sh").as_deref(),
            Some("evil.sh")
        );
    }

    #[test]
    fn resolution_priority() {
        let url = "https://h/files/url-name.bin";
        assert_eq!(
            resolve(Some("mine.iso"), Some("cd.iso"), url, None),
            "mine.iso"
        );
        assert_eq!(resolve(None, Some("cd.iso"), url, None), "cd.iso");
        assert_eq!(resolve(None, Some("../"), url, None), "url-name.bin");
        assert_eq!(resolve(None, None, "https://h/", None), "download");
        assert_eq!(
            resolve(None, None, "https://h/get", Some("application/pdf; q=1")),
            "get.pdf"
        );
        assert_eq!(
            resolve(None, None, "https://h/", Some("application/zip")),
            "download.zip"
        );
    }

    #[test]
    fn numbering() {
        assert_eq!(numbered("a.iso", 1), "a (1).iso");
        assert_eq!(numbered("a.tar.gz", 2), "a (2).tar.gz");
        assert_eq!(numbered("README", 3), "README (3)");
        assert_eq!(numbered(".bashrc", 1), ".bashrc (1)");
    }

    #[test]
    fn paths_stay_inside() {
        let d = Path::new("/dl");
        assert_eq!(inside(d, "a.bin"), Some(PathBuf::from("/dl/a.bin")));
        assert_eq!(inside(d, "../a.bin"), None);
        assert_eq!(inside(d, "x/a.bin"), None);
        assert_eq!(inside(d, "/etc/passwd"), None);
        assert_eq!(inside(d, ".."), None);
        assert_eq!(inside(d, ""), None);
        assert_eq!(
            part_path(d, "a.bin"),
            Some(PathBuf::from("/dl/a.bin.vayu-part"))
        );
    }
}
