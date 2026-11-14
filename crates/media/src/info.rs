//! Reading yt-dlp's info JSON.

use serde_json::Value;
use whoosh_proto::{
    AudioConvert, Header, MediaChoice, MediaFormat, MediaFormatKind, MediaProbeResult,
    MediaSubtitle, RequestContext,
};

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn u64_of(v: &Value, key: &str) -> Option<u64> {
    let x = v.get(key)?;
    x.as_u64().or_else(|| {
        x.as_f64()
            .filter(|f| f.is_finite() && *f >= 0.0)
            .map(|f| f.round() as u64)
    })
}

fn u32_of(v: &Value, key: &str) -> Option<u32> {
    u64_of(v, key).and_then(|n| u32::try_from(n).ok())
}

/// A codec field: absent, `null` and `"none"` all mean "no such stream".
fn codec(v: &Value, key: &str) -> Option<String> {
    str_of(v, key).filter(|c| c != "none")
}

fn has_stream(v: &Value, key: &str) -> Option<bool> {
    match v.get(key).and_then(Value::as_str) {
        Some("none") => Some(false),
        Some(_) => Some(true),
        None => None,
    }
}

/// What a format carries. yt-dlp leaves codecs unknown (`null`) for some
/// sites; such a format is taken as video with audio.
pub fn kind_of(f: &Value) -> MediaFormatKind {
    match (has_stream(f, "vcodec"), has_stream(f, "acodec")) {
        (Some(false), _) => MediaFormatKind::AudioOnly,
        (_, Some(false)) => MediaFormatKind::VideoOnly,
        _ if str_of(f, "resolution").as_deref() == Some("audio only") => MediaFormatKind::AudioOnly,
        _ => MediaFormatKind::VideoAudio,
    }
}

/// Our engine can download it: one plain HTTP(S) URL, not fragments.
pub fn is_direct(f: &Value) -> bool {
    let protocol_ok = matches!(
        f.get("protocol").and_then(Value::as_str),
        Some("http" | "https") | None
    );
    let url_ok =
        str_of(f, "url").is_some_and(|u| u.starts_with("https://") || u.starts_with("http://"));
    protocol_ok && url_ok && f.get("fragments").is_none_or(Value::is_null)
}

/// Format ids are passed to yt-dlp's `-f`: allow only plain ones.
pub fn valid_format_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// Subtitle languages go into `--sub-langs`.
pub fn valid_lang(lang: &str) -> bool {
    !lang.is_empty()
        && lang.len() <= 32
        && lang
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn format_of(f: &Value) -> Option<MediaFormat> {
    let id = str_of(f, "format_id").filter(|id| valid_format_id(id))?;
    // Storyboards and other images are not media.
    if str_of(f, "ext").as_deref() == Some("mhtml") {
        return None;
    }
    let (size, approximate) = match u64_of(f, "filesize") {
        Some(s) => (Some(s), false),
        None => (u64_of(f, "filesize_approx"), true),
    };
    Some(MediaFormat {
        kind: kind_of(f),
        ext: str_of(f, "ext").unwrap_or_else(|| "bin".into()),
        width: u32_of(f, "width"),
        height: u32_of(f, "height"),
        fps: u32_of(f, "fps"),
        vcodec: codec(f, "vcodec"),
        acodec: codec(f, "acodec"),
        abr_kbps: u32_of(f, "abr"),
        size_bytes: size,
        size_approximate: approximate && size.is_some(),
        note: str_of(f, "format_note"),
        direct: is_direct(f),
        id,
    })
}

/// `media.probe`'s answer from `yt-dlp -J`.
pub fn probe_result(info: &Value, page_url: &str) -> Result<MediaProbeResult, String> {
    if info.get("_type").and_then(Value::as_str) == Some("playlist") {
        return Err("this page is a playlist; open a single video".into());
    }
    let mut formats: Vec<MediaFormat> = match info.get("formats").and_then(Value::as_array) {
        Some(list) => list.iter().filter_map(format_of).collect(),
        None => format_of(info).into_iter().collect(),
    };
    formats.dedup_by(|a, b| a.id == b.id);
    if formats.is_empty() {
        return Err("no downloadable formats on this page".into());
    }
    let mut subtitles = Vec::new();
    for (key, automatic) in [("subtitles", false), ("automatic_captions", true)] {
        if let Some(map) = info.get(key).and_then(Value::as_object) {
            for (lang, tracks) in map {
                if !valid_lang(lang) || lang == "live_chat" {
                    continue;
                }
                if subtitles.iter().any(|s: &MediaSubtitle| s.lang == *lang) {
                    continue;
                }
                let name = tracks
                    .as_array()
                    .and_then(|t| t.first())
                    .and_then(|t| str_of(t, "name"));
                subtitles.push(MediaSubtitle {
                    lang: lang.clone(),
                    name,
                    automatic,
                });
            }
        }
    }
    Ok(MediaProbeResult {
        url: str_of(info, "webpage_url").unwrap_or_else(|| page_url.to_owned()),
        title: title_of(info),
        extractor: str_of(info, "extractor_key")
            .or_else(|| str_of(info, "extractor"))
            .unwrap_or_else(|| "generic".into())
            .to_ascii_lowercase(),
        thumbnail: str_of(info, "thumbnail")
            .filter(|t| t.starts_with("https://") || t.starts_with("http://")),
        duration_secs: u64_of(info, "duration"),
        uploader: str_of(info, "uploader").or_else(|| str_of(info, "channel")),
        live: info
            .get("is_live")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        formats,
        subtitles,
    })
}

pub fn title_of(info: &Value) -> String {
    str_of(info, "title")
        .or_else(|| str_of(info, "id"))
        .unwrap_or_else(|| "video".into())
        .chars()
        .take(300)
        .collect()
}

/// yt-dlp's `-f` for a choice. Built only from validated pieces.
pub fn selector(choice: &MediaChoice) -> Result<String, String> {
    Ok(match choice {
        MediaChoice::Best { max_height: None } => "bv*+ba/b".into(),
        MediaChoice::Best {
            max_height: Some(h),
        } => format!("bv*[height<={h}]+ba/b[height<={h}]/bv*+ba/b"),
        MediaChoice::Audio { .. } => "ba/b".into(),
        MediaChoice::Formats { ids } => {
            if ids.is_empty() || ids.len() > 2 {
                return Err("choose one format, or one video and one audio format".into());
            }
            if let Some(bad) = ids.iter().find(|id| !valid_format_id(id)) {
                return Err(format!("not a format id: {bad:?}"));
            }
            ids.join("+")
        }
    })
}

/// One file to fetch, from the resolved info JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub format_id: String,
    pub kind: MediaFormatKind,
    pub ext: String,
    pub url: String,
    pub direct: bool,
    pub request: RequestContext,
    pub size: Option<u64>,
    pub height: Option<u32>,
    pub acodec: Option<String>,
}

/// The formats yt-dlp picked (`requested_formats` when it merges).
pub fn parts_of(resolved: &Value) -> Result<Vec<Part>, String> {
    let list: Vec<&Value> = match resolved.get("requested_formats").and_then(Value::as_array) {
        Some(list) if !list.is_empty() => list.iter().collect(),
        _ => vec![resolved],
    };
    let parts: Vec<Part> = list
        .into_iter()
        .map(|f| {
            let format_id = str_of(f, "format_id")
                .filter(|id| valid_format_id(id))
                .ok_or("yt-dlp picked a format without a usable id")?;
            Ok(Part {
                kind: kind_of(f),
                ext: str_of(f, "ext")
                    .filter(|e| e.len() <= 8 && e.bytes().all(|b| b.is_ascii_alphanumeric()))
                    .unwrap_or_else(|| "bin".into()),
                url: str_of(f, "url").unwrap_or_default(),
                direct: is_direct(f),
                request: request_of(f),
                size: u64_of(f, "filesize").or_else(|| u64_of(f, "filesize_approx")),
                height: u32_of(f, "height"),
                acodec: codec(f, "acodec"),
                format_id,
            })
        })
        .collect::<Result<_, String>>()?;
    if parts.is_empty() {
        return Err("yt-dlp picked no format".into());
    }
    Ok(parts)
}

/// The headers yt-dlp would send for this format (`http_headers`), and the
/// cookies it holds for the format's own URL (`cookies`): never the page's.
pub fn request_of(f: &Value) -> RequestContext {
    let mut req = RequestContext::default();
    if let Some(h) = f.get("http_headers").and_then(Value::as_object) {
        for (name, value) in h {
            let Some(value) = value.as_str() else {
                continue;
            };
            if name.is_empty() || name.contains(['\r', '\n', ':']) || value.contains(['\r', '\n']) {
                continue;
            }
            match name.to_ascii_lowercase().as_str() {
                "user-agent" => req.user_agent = Some(value.to_owned()),
                "referer" => req.referer = Some(value.to_owned()),
                // The engine sets these itself.
                "cookie" | "range" | "accept-encoding" | "host" | "content-length" => {}
                _ => req.headers.push(Header {
                    name: name.clone(),
                    value: value.to_owned(),
                }),
            }
        }
    }
    if let Some(c) = f.get("cookies").and_then(Value::as_str) {
        let pairs = cookie_pairs(c);
        if !pairs.is_empty() {
            req.cookie = Some(pairs);
        }
    }
    req
}

/// `name=value` pairs from yt-dlp's `cookies` field, which is written like
/// `Set-Cookie` attributes (`a=1; Domain=.x; Path=/; Secure; b=2; …`).
fn cookie_pairs(s: &str) -> String {
    const ATTRS: [&str; 7] = [
        "domain", "path", "expires", "max-age", "secure", "httponly", "samesite",
    ];
    s.split(';')
        .map(str::trim)
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            let k = k.trim();
            (!k.is_empty()
                && !ATTRS.contains(&k.to_ascii_lowercase().as_str())
                && !v.contains(['\r', '\n']))
            .then(|| format!("{k}={}", v.trim()))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// What ffmpeg (or nothing) must do with the downloaded parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    /// One part, already the final file: rename it.
    Rename { ext: String },
    /// Video and audio into one container.
    Merge { ext: String },
    /// Audio out of the one part: copied (`codec_copy`) or encoded to MP3.
    Audio { ext: String, mp3: bool },
}

impl Finish {
    pub fn ext(&self) -> &str {
        match self {
            Finish::Rename { ext } | Finish::Merge { ext } | Finish::Audio { ext, .. } => ext,
        }
    }

    pub fn needs_ffmpeg(&self) -> bool {
        !matches!(self, Finish::Rename { .. })
    }
}

pub fn finish_for(parts: &[Part], choice: &MediaChoice) -> Finish {
    let audio_only = matches!(choice, MediaChoice::Audio { .. });
    let mp3 = matches!(
        choice,
        MediaChoice::Audio {
            convert: AudioConvert::Mp3
        }
    );
    if parts.len() >= 2 {
        let exts: Vec<&str> = parts.iter().map(|p| p.ext.as_str()).collect();
        let ext = if exts.iter().all(|e| matches!(*e, "mp4" | "m4a" | "mov")) {
            "mp4"
        } else if exts.iter().all(|e| *e == "webm") {
            "webm"
        } else {
            "mkv"
        };
        return Finish::Merge { ext: ext.into() };
    }
    let p = &parts[0];
    if mp3 {
        return Finish::Audio {
            ext: "mp3".into(),
            mp3: true,
        };
    }
    if audio_only && p.kind != MediaFormatKind::AudioOnly {
        // Only a combined format was there: keep its audio stream.
        let ext = match p.acodec.as_deref().unwrap_or_default() {
            c if c.starts_with("mp4a") => "m4a",
            c if c.starts_with("opus") => "opus",
            c if c.starts_with("vorbis") => "ogg",
            c if c.starts_with("mp3") => "mp3",
            _ => "mka",
        };
        return Finish::Audio {
            ext: ext.into(),
            mp3: false,
        };
    }
    Finish::Rename { ext: p.ext.clone() }
}

/// "1080p mp4", "audio m4a".
pub fn label(parts: &[Part], finish: &Finish) -> String {
    let height = parts.iter().filter_map(|p| p.height).max();
    let audio = matches!(finish, Finish::Audio { .. })
        || parts.iter().all(|p| p.kind == MediaFormatKind::AudioOnly);
    match (audio, height) {
        (false, Some(h)) => format!("{h}p {}", finish.ext()),
        (true, _) => format!("audio {}", finish.ext()),
        (false, None) => finish.ext().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "id": "abc", "title": "A / B: test", "webpage_url": "https://video.example/watch?v=abc",
            "extractor_key": "Youtube", "duration": 212.4, "thumbnail": "https://i.example/t.jpg",
            "formats": [
                { "format_id": "sb0", "ext": "mhtml", "protocol": "mhtml", "vcodec": "none", "acodec": "none" },
                { "format_id": "140", "ext": "m4a", "protocol": "https", "url": "https://cdn.example/a",
                  "vcodec": "none", "acodec": "mp4a.40.2", "abr": 129.5, "filesize": 3_000_000,
                  "http_headers": { "User-Agent": "UA", "Accept": "*/*", "Cookie": "no" },
                  "cookies": "SID=1; Domain=.example; Path=/; Secure; HSID=2; Expires=1" },
                { "format_id": "137", "ext": "mp4", "protocol": "https", "url": "https://cdn.example/v",
                  "vcodec": "avc1.640028", "acodec": "none", "height": 1080, "width": 1920, "fps": 30,
                  "filesize_approx": 90_000_000 },
                { "format_id": "hls-720", "ext": "mp4", "protocol": "m3u8_native", "url": "https://cdn.example/m.m3u8",
                  "vcodec": "avc1", "acodec": "mp4a", "height": 720 },
                { "format_id": "bad id;rm", "ext": "mp4", "url": "https://x" }
            ],
            "subtitles": { "en": [{ "ext": "vtt", "name": "English" }], "live_chat": [] },
            "automatic_captions": { "bn": [{ "ext": "vtt" }], "en": [{ "ext": "vtt" }] }
        })
    }

    #[test]
    fn probe_reads_formats_and_subtitles() {
        let r = probe_result(&sample(), "https://video.example/x").unwrap();
        assert_eq!(r.title, "A / B: test");
        assert_eq!(r.extractor, "youtube");
        assert_eq!(r.duration_secs, Some(212));
        let ids: Vec<_> = r.formats.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["140", "137", "hls-720"]);
        assert_eq!(r.formats[0].kind, MediaFormatKind::AudioOnly);
        assert_eq!(r.formats[0].abr_kbps, Some(130));
        assert_eq!(r.formats[1].kind, MediaFormatKind::VideoOnly);
        assert!(r.formats[1].size_approximate);
        assert!(r.formats[1].direct);
        assert_eq!(r.formats[2].kind, MediaFormatKind::VideoAudio);
        assert!(!r.formats[2].direct, "HLS goes through yt-dlp");
        let langs: Vec<_> = r
            .subtitles
            .iter()
            .map(|s| (s.lang.as_str(), s.automatic))
            .collect();
        assert_eq!(langs, [("en", false), ("bn", true)]);
    }

    #[test]
    fn playlists_are_refused() {
        assert!(probe_result(&json!({ "_type": "playlist", "entries": [] }), "u").is_err());
    }

    #[test]
    fn selectors_never_carry_raw_input() {
        assert_eq!(
            selector(&MediaChoice::Best { max_height: None }).unwrap(),
            "bv*+ba/b"
        );
        assert_eq!(
            selector(&MediaChoice::Best {
                max_height: Some(720)
            })
            .unwrap(),
            "bv*[height<=720]+ba/b[height<=720]/bv*+ba/b"
        );
        assert_eq!(
            selector(&MediaChoice::Formats {
                ids: vec!["137".into(), "140".into()]
            })
            .unwrap(),
            "137+140"
        );
        assert!(
            selector(&MediaChoice::Formats {
                ids: vec!["1/best".into()]
            })
            .is_err()
        );
        assert!(selector(&MediaChoice::Formats { ids: vec![] }).is_err());
        assert!(
            selector(&MediaChoice::Formats {
                ids: vec!["a".into(), "b".into(), "c".into()]
            })
            .is_err()
        );
    }

    #[test]
    fn parts_carry_format_headers_and_cookies_only() {
        let info = sample();
        let resolved = json!({ "requested_formats": [info["formats"][2], info["formats"][1]] });
        let parts = parts_of(&resolved).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].format_id, "137");
        let a = &parts[1].request;
        assert_eq!(a.user_agent.as_deref(), Some("UA"));
        assert_eq!(a.cookie.as_deref(), Some("SID=1; HSID=2"));
        assert_eq!(
            a.headers,
            vec![Header {
                name: "Accept".into(),
                value: "*/*".into()
            }]
        );
        assert_eq!(
            finish_for(&parts, &MediaChoice::Best { max_height: None }),
            Finish::Merge { ext: "mp4".into() }
        );
        assert_eq!(
            label(&parts, &Finish::Merge { ext: "mp4".into() }),
            "1080p mp4"
        );
    }

    #[test]
    fn single_formats_finish_by_rename_or_audio_extraction() {
        let info = sample();
        let one = parts_of(&info["formats"][1]).unwrap();
        assert_eq!(
            finish_for(
                &one,
                &MediaChoice::Audio {
                    convert: AudioConvert::Original
                }
            ),
            Finish::Rename { ext: "m4a".into() }
        );
        assert_eq!(
            finish_for(
                &one,
                &MediaChoice::Audio {
                    convert: AudioConvert::Mp3
                }
            ),
            Finish::Audio {
                ext: "mp3".into(),
                mp3: true
            }
        );
        let combined = parts_of(&info["formats"][3]).unwrap();
        assert!(!combined[0].direct);
        assert_eq!(
            finish_for(
                &combined,
                &MediaChoice::Audio {
                    convert: AudioConvert::Original
                }
            ),
            Finish::Audio {
                ext: "m4a".into(),
                mp3: false
            }
        );
        assert_eq!(
            label(&one, &Finish::Rename { ext: "m4a".into() }),
            "audio m4a"
        );
    }
}
