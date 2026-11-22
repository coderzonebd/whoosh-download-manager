// SPDX-License-Identifier: GPL-3.0-or-later
//! The add dialog's format picker, as the web UI's `lib/media.ts`: what
//! `media.probe` found turned into a few plain choices (the best video at
//! a height, or audio only), with an estimate of what they download. The
//! daemon (yt-dlp) makes the final pick. Also the file categories, as the
//! daemon gives them (`wdm_engine::category`).

use wdm_proto::category;
use wdm_proto::media::{AudioConvert, MediaChoice, MediaFormat, MediaFormatKind};

/// A playlist's videos are not opened yet, so the usual heights stand in.
pub const COMMON_HEIGHTS: [u32; 6] = [2160, 1440, 1080, 720, 480, 360];

/// A web page rather than a file, judging by the probe's content type.
pub fn is_web_page(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|t| {
        let t = t.to_ascii_lowercase();
        t.contains("text/html") || t.contains("application/xhtml+xml")
    })
}

/// Heights offered for video, highest first.
pub fn video_heights(formats: &[MediaFormat]) -> Vec<u32> {
    let mut hs: Vec<u32> = formats
        .iter()
        .filter(|f| f.kind != MediaFormatKind::AudioOnly)
        .filter_map(|f| f.height.filter(|h| *h > 0))
        .collect();
    hs.sort_unstable_by(|a, b| b.cmp(a));
    hs.dedup();
    hs
}

pub fn has_audio(formats: &[MediaFormat]) -> bool {
    formats.iter().any(|f| f.kind != MediaFormatKind::VideoOnly)
}

/// The choice for video at `height` (`None`: the best), or audio.
pub fn choice(audio: bool, height: Option<u32>, mp3: bool) -> MediaChoice {
    if audio {
        MediaChoice::Audio {
            convert: if mp3 {
                AudioConvert::Mp3
            } else {
                AudioConvert::Original
            },
        }
    } else {
        MediaChoice::Best { max_height: height }
    }
}

/// Roughly what yt-dlp will pick (`bv*+ba/b`, `ba/b`); formats are worst first.
fn pick<'a>(formats: &'a [MediaFormat], choice: &MediaChoice) -> Vec<&'a MediaFormat> {
    let best_audio = formats
        .iter()
        .rfind(|f| f.kind == MediaFormatKind::AudioOnly);
    match choice {
        MediaChoice::Audio { .. } => best_audio.or(formats.last()).into_iter().collect(),
        MediaChoice::Formats { ids } => formats.iter().filter(|f| ids.contains(&f.id)).collect(),
        MediaChoice::Best { max_height } => {
            let video = formats.iter().rfind(|f| {
                f.kind != MediaFormatKind::AudioOnly
                    && max_height.is_none_or(|m| f.height.unwrap_or(0) <= m)
            });
            match (video, best_audio) {
                (Some(v), Some(a)) if v.kind == MediaFormatKind::VideoOnly => vec![v, a],
                (Some(v), _) => vec![v],
                (None, _) => Vec::new(),
            }
        }
    }
}

/// What a choice downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Estimate {
    pub bytes: Option<u64>,
    /// WDM's engine downloads every part (multi-connection, resumable).
    pub direct: bool,
}

pub fn estimate(formats: &[MediaFormat], choice: &MediaChoice) -> Estimate {
    let picked = pick(formats, choice);
    let known = !picked.is_empty() && picked.iter().all(|f| f.size_bytes.is_some());
    Estimate {
        bytes: known.then(|| picked.iter().filter_map(|f| f.size_bytes).sum()),
        direct: !picked.is_empty() && picked.iter().all(|f| f.direct),
    }
}

/// Size of the best format at a height (for the quality list).
pub fn size_at(formats: &[MediaFormat], height: u32) -> Option<u64> {
    estimate(
        formats,
        &MediaChoice::Best {
            max_height: Some(height),
        },
    )
    .bytes
}

/// The built-in category for a file name (`wdm_engine::category::category_for`).
pub fn category_for(filename: &str) -> &'static str {
    let ext = filename
        .rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map_or("", |(_, ext)| ext)
        .to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "flv" | "wmv" | "m4v" | "mpg" | "mpeg" | "3gp"
        | "ogv" | "ts" | "m2ts" => category::VIDEO,
        "mp3" | "flac" | "wav" | "aac" | "ogg" | "oga" | "m4a" | "opus" | "wma" | "aiff"
        | "alac" | "mid" | "midi" => category::MUSIC,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp"
        | "txt" | "rtf" | "epub" | "mobi" | "csv" | "md" | "djvu" => category::DOCUMENTS,
        "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "lz" | "lzma"
        | "z" | "cab" | "arj" => category::COMPRESSED,
        "exe" | "msi" | "msix" | "appx" | "deb" | "rpm" | "apk" | "appimage" | "dmg" | "pkg"
        | "iso" | "img" | "flatpak" | "snap" | "jar" => category::PROGRAMS,
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "svg" | "bmp" | "tif" | "tiff" | "heic"
        | "heif" | "avif" | "ico" | "raw" => category::IMAGES,
        _ => category::OTHER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn f(
        id: &str,
        kind: &str,
        height: Option<u32>,
        size: Option<u64>,
        direct: bool,
    ) -> MediaFormat {
        serde_json::from_value(json!({
            "id": id, "kind": kind, "ext": "mp4", "height": height, "sizeBytes": size, "direct": direct,
        }))
        .unwrap()
    }

    fn formats() -> Vec<MediaFormat> {
        vec![
            f("a1", "audioOnly", None, Some(1_000), true),
            f("v360", "videoAudio", Some(360), Some(5_000), true),
            f("v720", "videoOnly", Some(720), Some(20_000), true),
            f("v1080", "videoOnly", Some(1080), None, false),
            f("a2", "audioOnly", None, Some(2_000), true),
        ]
    }

    #[test]
    fn heights_and_audio() {
        assert_eq!(video_heights(&formats()), vec![1080, 720, 360]);
        assert!(has_audio(&formats()));
        assert!(!has_audio(&[f("v", "videoOnly", Some(1), None, true)]));
    }

    #[test]
    fn estimates_like_the_ui() {
        let fs = formats();
        // 720p is video only: merged with the best audio.
        assert_eq!(size_at(&fs, 720), Some(22_000));
        assert_eq!(size_at(&fs, 360), Some(5_000));
        // The best is 1080p, whose size is unknown and which yt-dlp fetches.
        let best = estimate(&fs, &choice(false, None, false));
        assert_eq!(
            best,
            Estimate {
                bytes: None,
                direct: false
            }
        );
        assert_eq!(
            estimate(&fs, &choice(true, None, true)),
            Estimate {
                bytes: Some(2_000),
                direct: true
            }
        );
        assert_eq!(
            estimate(&[], &choice(false, None, false)),
            Estimate {
                bytes: None,
                direct: false
            }
        );
    }

    #[test]
    fn web_pages_and_categories() {
        assert!(is_web_page(Some("text/html; charset=utf-8")));
        assert!(!is_web_page(Some("application/zip")) && !is_web_page(None));
        assert_eq!(category_for("Ubuntu.ISO"), category::PROGRAMS);
        assert_eq!(category_for("a.tar.gz"), category::COMPRESSED);
        assert_eq!(category_for("clip.mp4"), category::VIDEO);
        assert_eq!(category_for(".bashrc"), category::OTHER);
        assert_eq!(category_for("README"), category::OTHER);
    }
}
