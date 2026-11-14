//! File names for media downloads, by the same rules as every download
//! (`whoosh_net::sanitize_filename`).

use std::path::{Path, PathBuf};

/// Room left for `.f<format>.<ext>.whoosh-part` and ` (12)`.
const MAX_STEM_BYTES: usize = 180;

/// A file name stem from a title or the user's name: path separators
/// become dashes (a title "AC/DC" must not lose "AC"), then the usual
/// sanitizing, then shortened on a character boundary.
pub fn stem(name: &str) -> Option<String> {
    let flat: String = name
        .chars()
        .map(|c| if c == '/' || c == '\\' { '-' } else { c })
        .collect();
    let mut s = whoosh_net::sanitize_filename(&flat)?;
    if s.len() > MAX_STEM_BYTES {
        let mut cut = MAX_STEM_BYTES;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s = s
            .trim_end_matches(|c: char| c == '.' || c.is_whitespace())
            .to_owned();
    }
    (!s.is_empty()).then_some(s)
}

/// `<dir>/<stem>.<ext>`, or `<stem> (n).<ext>` when taken.
pub fn unique(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    (1..10_000)
        .map(|n| dir.join(format!("{stem} ({n}).{ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_keep_their_words() {
        assert_eq!(
            stem("AC/DC - Thunderstruck").as_deref(),
            Some("AC-DC - Thunderstruck")
        );
        assert_eq!(stem("..hidden").as_deref(), Some("hidden"));
        assert_eq!(stem("  "), None);
        assert_eq!(stem("a\u{202e}b\nc").as_deref(), Some("abc"));
        let long = "অ".repeat(200);
        let s = stem(&long).unwrap();
        assert!(s.len() <= MAX_STEM_BYTES && s.chars().all(|c| c == 'অ'));
    }

    #[test]
    fn unique_numbers_taken_names() {
        let dir = tempfile::tempdir().unwrap();
        let p = unique(dir.path(), "a", "mp4");
        assert_eq!(p, dir.path().join("a.mp4"));
        std::fs::write(&p, b"x").unwrap();
        assert_eq!(unique(dir.path(), "a", "mp4"), dir.path().join("a (1).mp4"));
    }
}
