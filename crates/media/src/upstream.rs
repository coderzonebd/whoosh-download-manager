//! Where yt-dlp and ffmpeg come from, and how each download is checked.
//!
//! | Tool | Platforms | Source | Checksum |
//! |---|---|---|---|
//! | yt-dlp | all | `github.com/yt-dlp/yt-dlp` releases (the project's own standalone builds) | `SHA2-256SUMS` of the same release |
//! | ffmpeg | Windows, Linux | `github.com/BtbN/FFmpeg-Builds` (linked from ffmpeg.org's download page), the **LGPL** static build of the newest release branch (`nX.Y`), else master | `checksums.sha256` of the same release |
//! | ffmpeg | macOS | `ffmpeg.martin-riedl.de` release build (no LGPL macOS build exists upstream; this one is GPL, run as a separate program and never linked) | the `.sha256` file published next to it |
//!
//! Every file is checked against the upstream checksum before it is
//! unpacked or made executable; a mismatch deletes it.

use crate::fetch::Fetcher;

/// Base URLs, replaceable in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstreams {
    /// `https://github.com/yt-dlp/yt-dlp/releases`
    pub yt_dlp: String,
    /// `https://github.com/BtbN/FFmpeg-Builds/releases`
    pub ffmpeg: String,
    /// `https://ffmpeg.martin-riedl.de/redirect/latest/macos`
    pub ffmpeg_macos: String,
}

impl Default for Upstreams {
    fn default() -> Self {
        Self {
            yt_dlp: "https://github.com/yt-dlp/yt-dlp/releases".into(),
            ffmpeg: "https://github.com/BtbN/FFmpeg-Builds/releases".into(),
            ffmpeg_macos: "https://ffmpeg.martin-riedl.de/redirect/latest/macos".into(),
        }
    }
}

/// How the downloaded file holds the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Archive {
    /// The file is the program.
    Raw,
    /// `.zip` or `.tar.xz`, unpacked with the system's `tar`.
    Packed,
}

/// One file to fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub url: String,
    /// Lower-case hex SHA-256 published by upstream.
    pub sha256: String,
    pub version: String,
    pub archive: Archive,
}

pub const YT_DLP_UPSTREAM: &str = "github.com/yt-dlp/yt-dlp";
pub const YT_DLP_LICENSE: &str = "Unlicense";

pub fn ffmpeg_upstream() -> &'static str {
    if cfg!(target_os = "macos") {
        "ffmpeg.martin-riedl.de"
    } else {
        "github.com/BtbN/FFmpeg-Builds"
    }
}

pub fn ffmpeg_license() -> &'static str {
    if cfg!(target_os = "macos") {
        "GPL-3.0-or-later"
    } else {
        "LGPL-2.1-or-later"
    }
}

/// yt-dlp's standalone build for this platform.
pub fn yt_dlp_asset_name() -> Option<&'static str> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let musl = cfg!(target_env = "musl");
    Some(match (os, arch) {
        ("linux", "x86_64") if musl => "yt-dlp_musllinux",
        ("linux", "aarch64") if musl => "yt-dlp_musllinux_aarch64",
        ("linux", "x86_64") => "yt-dlp_linux",
        ("linux", "aarch64") => "yt-dlp_linux_aarch64",
        ("macos", _) => "yt-dlp_macos",
        ("windows", "x86_64") => "yt-dlp.exe",
        ("windows", "aarch64") => "yt-dlp_arm64.exe",
        _ => return None,
    })
}

/// BtbN's platform name (Windows and Linux).
fn btbn_platform() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux64",
        ("linux", "aarch64") => "linuxarm64",
        ("windows", "x86_64") => "win64",
        ("windows", "aarch64") => "winarm64",
        _ => return None,
    })
}

fn macos_arch() -> &'static str {
    if std::env::consts::ARCH == "aarch64" {
        "arm64"
    } else {
        "amd64"
    }
}

/// `(sha256, file name)` pairs of a `sha256sum`-style list.
pub fn parse_sums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let hash = it.next()?.to_ascii_lowercase();
            let name = it.next()?.trim_start_matches('*').to_owned();
            (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
                .then_some((hash, name))
        })
        .collect()
}

fn sum_for<'a>(sums: &'a [(String, String)], name: &str) -> Option<&'a str> {
    sums.iter()
        .find(|(_, n)| n == name)
        .map(|(h, _)| h.as_str())
}

/// The last path segment of a release URL `…/tag/<tag>`, if it looks like
/// a version (digits and dots).
fn tag_from_url(url: &str) -> Option<String> {
    let (_, tag) = url.trim_end_matches('/').rsplit_once("/tag/")?;
    (!tag.is_empty() && tag.len() <= 32 && tag.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
        .then(|| tag.to_owned())
}

/// The newest yt-dlp release for this platform.
pub async fn latest_yt_dlp(fetcher: &dyn Fetcher, up: &Upstreams) -> Result<Asset, String> {
    let name = yt_dlp_asset_name().ok_or("yt-dlp publishes no build for this platform")?;
    let latest = fetcher.resolve(&format!("{}/latest", up.yt_dlp)).await?;
    let tag = tag_from_url(&latest)
        .ok_or_else(|| format!("cannot tell yt-dlp's latest version from {latest}"))?;
    let base = format!("{}/download/{tag}", up.yt_dlp);
    let sums = parse_sums(&fetcher.text(&format!("{base}/SHA2-256SUMS")).await?);
    let sha256 = sum_for(&sums, name)
        .ok_or_else(|| format!("yt-dlp {tag}: {name} is not in SHA2-256SUMS"))?;
    Ok(Asset {
        url: format!("{base}/{name}"),
        sha256: sha256.to_owned(),
        version: tag,
        archive: Archive::Raw,
    })
}

/// From BtbN's list, the static LGPL build of the newest release branch
/// for `platform`, else the master build. Returns `(file, version)`.
pub fn pick_btbn(sums: &[(String, String)], platform: &str) -> Option<(String, String)> {
    let ext = if platform.starts_with("win") {
        "zip"
    } else {
        "tar.xz"
    };
    // ffmpeg-n9.0-latest-linux64-lgpl-9.0.tar.xz
    let mut best: Option<((u32, u32), String)> = None;
    for (_, name) in sums {
        let Some(rest) = name.strip_prefix("ffmpeg-n") else {
            continue;
        };
        let Some((series, rest)) = rest.split_once("-latest-") else {
            continue;
        };
        if rest != format!("{platform}-lgpl-{series}.{ext}") {
            continue;
        }
        let mut parts = series.split('.').map(|p| p.parse::<u32>());
        let (Some(Ok(major)), Some(Ok(minor)), None) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if best.as_ref().is_none_or(|(v, _)| (major, minor) > *v) {
            best = Some(((major, minor), name.clone()));
        }
    }
    if let Some(((major, minor), name)) = best {
        return Some((name, format!("{major}.{minor}")));
    }
    let master = format!("ffmpeg-master-latest-{platform}-lgpl.{ext}");
    sums.iter()
        .any(|(_, n)| *n == master)
        .then(|| (master, "master".to_owned()))
}

/// The newest ffmpeg build for this platform.
pub async fn latest_ffmpeg(fetcher: &dyn Fetcher, up: &Upstreams) -> Result<Asset, String> {
    if cfg!(target_os = "macos") {
        let start = format!("{}/{}/release/ffmpeg.zip", up.ffmpeg_macos, macos_arch());
        let url = fetcher.resolve(&start).await?;
        // …/download/macos/arm64/1789931890_9.0.2/ffmpeg.zip
        let version = url
            .rsplit('/')
            .nth(1)
            .and_then(|seg| seg.split_once('_'))
            .map(|(_, v)| v.to_owned())
            .filter(|v| !v.is_empty() && v.len() <= 32)
            .ok_or_else(|| format!("cannot tell ffmpeg's version from {url}"))?;
        let sums = parse_sums(&fetcher.text(&format!("{url}.sha256")).await?);
        let sha256 = sum_for(&sums, "ffmpeg.zip")
            .ok_or_else(|| format!("no checksum for {url}"))?
            .to_owned();
        return Ok(Asset {
            url,
            sha256,
            version,
            archive: Archive::Packed,
        });
    }
    let platform = btbn_platform().ok_or("no ffmpeg build is fetched for this platform")?;
    let base = format!("{}/download/latest", up.ffmpeg);
    let sums = parse_sums(&fetcher.text(&format!("{base}/checksums.sha256")).await?);
    let (name, version) = pick_btbn(&sums, platform)
        .ok_or_else(|| format!("no LGPL ffmpeg build for {platform} in checksums.sha256"))?;
    let sha256 = sum_for(&sums, &name).unwrap_or_default().to_owned();
    Ok(Asset {
        url: format!("{base}/{name}"),
        sha256,
        version,
        archive: Archive::Packed,
    })
}

/// Whether `latest` is newer than `current`. yt-dlp versions are dates
/// (`2026.10.26`, nightlies add a fourth number); ffmpeg's are
/// `major.minor[.patch]`, possibly behind an `n` and with a suffix
/// (`n9.0.2-35-g…`, `6.1.1-3ubuntu5`). Unparseable versions never offer an
/// update unless they differ from a parseable latest.
pub fn is_newer(latest: &str, current: &str) -> bool {
    fn nums(v: &str) -> Vec<u64> {
        v.trim_start_matches(['n', 'N', 'v'])
            .split(|c: char| !c.is_ascii_digit() && c != '.')
            .next()
            .unwrap_or_default()
            .split('.')
            .map_while(|p| p.parse().ok())
            .collect()
    }
    let (l, c) = (nums(latest), nums(current));
    if l.is_empty() {
        return false;
    }
    if c.is_empty() {
        return true;
    }
    // Compare only as many numbers as the latest names: "9.0" is not
    // newer than "9.0.2".
    let c = &c[..c.len().min(l.len())];
    l.as_slice() > c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_parse_both_styles() {
        let s = "e51e69e89489f199530f13d32a4342da3ddb2cf34842f364603a066db05afbf3  yt-dlp\n\
                 F5389D3698ECDE4EF06F4E6B1CE0294FE9AC1396228C8990B8D3416D6D177A7D *yt-dlp.exe\n\
                 junk\n";
        let v = parse_sums(s);
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].1, "yt-dlp.exe");
        assert!(v[1].0.starts_with("f5389d"));
    }

    #[test]
    fn btbn_prefers_the_newest_release_branch() {
        let names = [
            "ffmpeg-master-latest-linux64-lgpl.tar.xz",
            "ffmpeg-n8.1-latest-linux64-lgpl-8.1.tar.xz",
            "ffmpeg-n9.0-latest-linux64-lgpl-9.0.tar.xz",
            "ffmpeg-n9.0-latest-linux64-lgpl-shared-9.0.tar.xz",
            "ffmpeg-n9.0-latest-linux64-gpl-9.0.tar.xz",
            "ffmpeg-n10.0-latest-win64-lgpl-10.0.zip",
        ];
        let sums: Vec<_> = names
            .iter()
            .map(|n| ("0".repeat(64), (*n).to_owned()))
            .collect();
        assert_eq!(
            pick_btbn(&sums, "linux64"),
            Some((
                "ffmpeg-n9.0-latest-linux64-lgpl-9.0.tar.xz".into(),
                "9.0".into()
            ))
        );
        assert_eq!(
            pick_btbn(&sums, "win64").map(|p| p.1),
            Some("10.0".to_owned())
        );
        let only_master = vec![(
            "0".repeat(64),
            "ffmpeg-master-latest-winarm64-lgpl.zip".to_owned(),
        )];
        assert_eq!(
            pick_btbn(&only_master, "winarm64").map(|p| p.1),
            Some("master".into())
        );
        assert_eq!(pick_btbn(&only_master, "linux64"), None);
    }

    #[test]
    fn tags_come_from_the_release_url() {
        assert_eq!(
            tag_from_url("https://github.com/yt-dlp/yt-dlp/releases/tag/2026.10.26"),
            Some("2026.10.26".into())
        );
        assert_eq!(
            tag_from_url("https://github.com/yt-dlp/yt-dlp/releases"),
            None
        );
        assert_eq!(tag_from_url("https://x/tag/../../evil"), None);
    }

    #[test]
    fn versions_compare_by_numbers() {
        assert!(is_newer("2026.10.26", "2026.03.01"));
        assert!(!is_newer("2026.10.26", "2026.10.26"));
        assert!(!is_newer("2026.10.26", "2026.10.26.123456"));
        assert!(is_newer("9.0", "6.1.1-3ubuntu5"));
        assert!(!is_newer("9.0", "n9.0.2-35-gf1acb0a266"));
        assert!(is_newer("9.0.3", "9.0.2"));
        assert!(!is_newer("master", "9.0"));
        assert!(is_newer("9.0", "N-128889-g88c46fce27"));
    }
}
