//! Where yt-dlp and ffmpeg come from, and how each download is checked.
//!
//! | Tool | Platforms | Source | Checksum |
//! |---|---|---|---|
//! | yt-dlp | all | `github.com/yt-dlp/yt-dlp` releases (the project's own standalone builds) | `SHA2-256SUMS` of the same release |
//! | ffmpeg | Windows, Linux | `github.com/BtbN/FFmpeg-Builds` (linked from ffmpeg.org's download page), the **LGPL** static build of the newest release branch (`nX.Y`), else master | `checksums.sha256` of the same release |
//! | ffmpeg | macOS | `ffmpeg.martin-riedl.de` release build (no LGPL macOS build exists upstream; this one is GPL, run as a separate program and never linked) | the `.sha256` file published next to it |
//! | QuickJS (`qjs`) | all | `github.com/quickjs-ng/quickjs` releases (the project's own static builds; Windows on ARM runs the x86_64 one) | the asset's SHA-256 digest from GitHub's release API |
//! | Deno | Windows, macOS, Linux (glibc) | `github.com/denoland/deno` releases (the project's own builds, a zip; the version from `dl.deno.land/release-latest.txt`, so no GitHub API request that shared addresses run out of) | the `.sha256sum` file the release publishes next to it |
//!
//! Every file is checked against the upstream checksum before it is
//! unpacked or made executable; a mismatch deletes it.
//!
//! The same functions, for a release target instead of this machine
//! (`*_for`, [`Platform`]), fetch the copies the desktop installers ship
//! ([`crate::bundle`]).

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
    /// `https://api.github.com/repos/quickjs-ng/quickjs/releases/latest`
    pub quickjs: String,
    /// `https://dl.deno.land/release-latest.txt` (the newest version, e.g. `v2.9.8`)
    pub deno: String,
    /// `https://github.com/denoland/deno/releases/download` (`/<tag>/<asset>`)
    pub deno_releases: String,
}

impl Default for Upstreams {
    fn default() -> Self {
        Self {
            yt_dlp: "https://github.com/yt-dlp/yt-dlp/releases".into(),
            ffmpeg: "https://github.com/BtbN/FFmpeg-Builds/releases".into(),
            ffmpeg_macos: "https://ffmpeg.martin-riedl.de/redirect/latest/macos".into(),
            quickjs: "https://api.github.com/repos/quickjs-ng/quickjs/releases/latest".into(),
            deno: "https://dl.deno.land/release-latest.txt".into(),
            deno_releases: "https://github.com/denoland/deno/releases/download".into(),
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
pub const QUICKJS_UPSTREAM: &str = "github.com/quickjs-ng/quickjs";
pub const QUICKJS_LICENSE: &str = "MIT";
pub const DENO_UPSTREAM: &str = "github.com/denoland/deno";
pub const DENO_LICENSE: &str = "MIT";

/// An operating system and processor the tools are fetched for: this
/// machine at run time, any release target when bundling them into an
/// installer (`wdm-media-bundle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Platform {
    /// `std::env::consts::OS` values: `linux`, `macos`, `windows`.
    pub os: &'static str,
    /// `std::env::consts::ARCH` values: `x86_64`, `aarch64`.
    pub arch: &'static str,
    pub musl: bool,
}

impl Platform {
    /// This machine.
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            musl: cfg!(target_env = "musl"),
        }
    }

    /// From a Rust target triple, e.g. `aarch64-pc-windows-msvc`.
    pub fn from_triple(triple: &str) -> Option<Self> {
        let arch = match triple.split('-').next()? {
            "x86_64" => "x86_64",
            "aarch64" => "aarch64",
            _ => return None,
        };
        let os = if triple.contains("-linux-") {
            "linux"
        } else if triple.contains("-apple-darwin") {
            "macos"
        } else if triple.contains("-windows-") {
            "windows"
        } else {
            return None;
        };
        Some(Self {
            os,
            arch,
            musl: triple.ends_with("-musl"),
        })
    }

    pub fn exe_suffix(self) -> &'static str {
        if self.os == "windows" { ".exe" } else { "" }
    }
}

pub fn ffmpeg_upstream() -> &'static str {
    ffmpeg_upstream_for(Platform::current())
}

pub fn ffmpeg_upstream_for(p: Platform) -> &'static str {
    if p.os == "macos" {
        "ffmpeg.martin-riedl.de"
    } else {
        "github.com/BtbN/FFmpeg-Builds"
    }
}

pub fn ffmpeg_license() -> &'static str {
    ffmpeg_license_for(Platform::current())
}

pub fn ffmpeg_license_for(p: Platform) -> &'static str {
    if p.os == "macos" {
        "GPL-3.0-or-later"
    } else {
        "LGPL-2.1-or-later"
    }
}

/// yt-dlp's standalone build for this platform.
pub fn yt_dlp_asset_name() -> Option<&'static str> {
    yt_dlp_asset_name_for(Platform::current())
}

pub fn yt_dlp_asset_name_for(p: Platform) -> Option<&'static str> {
    Some(match (p.os, p.arch) {
        ("linux", "x86_64") if p.musl => "yt-dlp_musllinux",
        ("linux", "aarch64") if p.musl => "yt-dlp_musllinux_aarch64",
        ("linux", "x86_64") => "yt-dlp_linux",
        ("linux", "aarch64") => "yt-dlp_linux_aarch64",
        ("macos", _) => "yt-dlp_macos",
        ("windows", "x86_64") => "yt-dlp.exe",
        ("windows", "aarch64") => "yt-dlp_arm64.exe",
        _ => return None,
    })
}

/// BtbN's platform name (Windows and Linux).
fn btbn_platform(p: Platform) -> Option<&'static str> {
    Some(match (p.os, p.arch) {
        ("linux", "x86_64") => "linux64",
        ("linux", "aarch64") => "linuxarm64",
        ("windows", "x86_64") => "win64",
        ("windows", "aarch64") => "winarm64",
        _ => return None,
    })
}

fn macos_arch(p: Platform) -> &'static str {
    if p.arch == "aarch64" {
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
    latest_yt_dlp_for(fetcher, up, Platform::current()).await
}

/// The newest yt-dlp release for `p`.
pub async fn latest_yt_dlp_for(
    fetcher: &dyn Fetcher,
    up: &Upstreams,
    p: Platform,
) -> Result<Asset, String> {
    let name = yt_dlp_asset_name_for(p).ok_or("yt-dlp publishes no build for this platform")?;
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
    latest_ffmpeg_for(fetcher, up, Platform::current()).await
}

/// The newest ffmpeg build for `p`.
pub async fn latest_ffmpeg_for(
    fetcher: &dyn Fetcher,
    up: &Upstreams,
    p: Platform,
) -> Result<Asset, String> {
    if p.os == "macos" {
        let start = format!("{}/{}/release/ffmpeg.zip", up.ffmpeg_macos, macos_arch(p));
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
    let platform = btbn_platform(p).ok_or("no ffmpeg build is fetched for this platform")?;
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

/// QuickJS-ng's static build for `p`. Windows on ARM runs the x86_64 one.
pub fn quickjs_asset_name_for(p: Platform) -> Option<&'static str> {
    Some(match (p.os, p.arch) {
        ("linux", "x86_64") => "qjs-linux-x86_64",
        ("linux", "aarch64") => "qjs-linux-aarch64",
        ("macos", "x86_64") => "qjs-darwin-x86_64",
        ("macos", "aarch64") => "qjs-darwin-arm64",
        ("windows", "x86_64" | "aarch64") => "qjs-windows-x86_64.exe",
        _ => return None,
    })
}

/// From GitHub's "latest release" JSON: the version (tag without `v`), and
/// the URL and SHA-256 of asset `name`.
pub fn pick_github_asset(json: &str, name: &str) -> Result<(String, String, String), String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("unreadable release information: {e}"))?;
    let tag = v["tag_name"].as_str().unwrap_or_default();
    let version = tag.strip_prefix('v').unwrap_or(tag);
    if version.is_empty()
        || version.len() > 32
        || !version.bytes().all(|b| b.is_ascii_digit() || b == b'.')
    {
        return Err(format!("cannot tell the version from the tag {tag:?}"));
    }
    let asset = v["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"].as_str() == Some(name)))
        .ok_or_else(|| format!("{name} is not in release {tag}"))?;
    let url = asset["browser_download_url"]
        .as_str()
        .filter(|u| u.starts_with("https://github.com/"))
        .ok_or_else(|| format!("{name}: no download address"))?;
    let sha = asset["digest"]
        .as_str()
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| format!("{name}: GitHub lists no SHA-256 for it"))?;
    Ok((version.to_owned(), url.to_owned(), sha.to_ascii_lowercase()))
}

/// The newest QuickJS for this platform.
pub async fn latest_quickjs(fetcher: &dyn Fetcher, up: &Upstreams) -> Result<Asset, String> {
    latest_quickjs_for(fetcher, up, Platform::current()).await
}

/// The newest QuickJS for `p`.
pub async fn latest_quickjs_for(
    fetcher: &dyn Fetcher,
    up: &Upstreams,
    p: Platform,
) -> Result<Asset, String> {
    let name = quickjs_asset_name_for(p).ok_or("QuickJS publishes no build for this platform")?;
    let (version, url, sha256) = pick_github_asset(&fetcher.text(&up.quickjs).await?, name)?;
    Ok(Asset {
        url,
        sha256,
        version,
        archive: Archive::Raw,
    })
}

/// Deno's build for `p`: a zip holding the `deno` program. None for musl
/// Linux (Deno links glibc).
pub fn deno_asset_name_for(p: Platform) -> Option<&'static str> {
    Some(match (p.os, p.arch, p.musl) {
        ("linux", "x86_64", false) => "deno-x86_64-unknown-linux-gnu.zip",
        ("linux", "aarch64", false) => "deno-aarch64-unknown-linux-gnu.zip",
        ("macos", "x86_64", _) => "deno-x86_64-apple-darwin.zip",
        ("macos", "aarch64", _) => "deno-aarch64-apple-darwin.zip",
        ("windows", "x86_64", _) => "deno-x86_64-pc-windows-msvc.zip",
        ("windows", "aarch64", _) => "deno-aarch64-pc-windows-msvc.zip",
        _ => return None,
    })
}

/// The checksum in a Deno `.sha256sum` file: `<hash>  <name>` (made with `shasum`), or, for the
/// Windows builds, PowerShell's `Get-FileHash` listing (`Hash : <HASH>` and `Path : …\<name>`).
pub fn deno_sum(text: &str, name: &str) -> Option<String> {
    if let Some(h) = sum_for(&parse_sums(text), name) {
        return Some(h.to_owned());
    }
    let mut hash = None;
    let mut path_ok = false;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Hash" if value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) => {
                hash = Some(value.to_ascii_lowercase());
            }
            // `C:\a\deno\…\deno-x86_64-pc-windows-msvc.zip` (the drive's colon is in value).
            "Path" => path_ok = line.trim_end().ends_with(name),
            _ => {}
        }
    }
    hash.filter(|_| path_ok)
}

/// The newest Deno for this platform.
pub async fn latest_deno(fetcher: &dyn Fetcher, up: &Upstreams) -> Result<Asset, String> {
    latest_deno_for(fetcher, up, Platform::current()).await
}

/// The newest Deno for `p`.
pub async fn latest_deno_for(
    fetcher: &dyn Fetcher,
    up: &Upstreams,
    p: Platform,
) -> Result<Asset, String> {
    let name = deno_asset_name_for(p).ok_or("Deno publishes no build for this platform")?;
    let tag = fetcher.text(&up.deno).await?.trim().to_owned();
    let version = tag.strip_prefix('v').unwrap_or(&tag).to_owned();
    if version.is_empty()
        || version.len() > 32
        || !version.bytes().all(|b| b.is_ascii_digit() || b == b'.')
    {
        return Err(format!("cannot tell Deno's version from {tag:?}"));
    }
    let url = format!("{}/v{version}/{name}", up.deno_releases);
    let sha256 = deno_sum(&fetcher.text(&format!("{url}.sha256sum")).await?, name)
        .ok_or_else(|| format!("{name}.sha256sum lists no checksum for it"))?;
    Ok(Asset {
        url,
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
    fn platforms_come_from_target_triples() {
        let p = |t| Platform::from_triple(t).unwrap();
        assert_eq!(
            yt_dlp_asset_name_for(p("x86_64-pc-windows-msvc")),
            Some("yt-dlp.exe")
        );
        assert_eq!(
            yt_dlp_asset_name_for(p("aarch64-pc-windows-msvc")),
            Some("yt-dlp_arm64.exe")
        );
        assert_eq!(
            yt_dlp_asset_name_for(p("aarch64-unknown-linux-gnu")),
            Some("yt-dlp_linux_aarch64")
        );
        assert_eq!(
            yt_dlp_asset_name_for(p("x86_64-unknown-linux-musl")),
            Some("yt-dlp_musllinux")
        );
        assert_eq!(
            yt_dlp_asset_name_for(p("x86_64-apple-darwin")),
            Some("yt-dlp_macos")
        );
        assert_eq!(
            btbn_platform(p("aarch64-pc-windows-msvc")),
            Some("winarm64")
        );
        assert_eq!(
            btbn_platform(p("x86_64-unknown-linux-gnu")),
            Some("linux64")
        );
        assert_eq!(macos_arch(p("aarch64-apple-darwin")), "arm64");
        assert_eq!(
            ffmpeg_license_for(p("aarch64-apple-darwin")),
            "GPL-3.0-or-later"
        );
        assert_eq!(
            ffmpeg_license_for(p("x86_64-pc-windows-msvc")),
            "LGPL-2.1-or-later"
        );
        assert_eq!(p("x86_64-pc-windows-msvc").exe_suffix(), ".exe");
        assert_eq!(Platform::from_triple("universal-apple-darwin"), None);
        assert_eq!(Platform::from_triple("riscv64gc-unknown-linux-gnu"), None);
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

    #[test]
    fn quickjs_comes_from_githubs_release_digest() {
        let json = r#"{"tag_name":"v0.18.0","assets":[
            {"name":"qjs-linux-x86_64","browser_download_url":"https://github.com/quickjs-ng/quickjs/releases/download/v0.18.0/qjs-linux-x86_64",
             "digest":"sha256:85DED23B9FABE8C1644FC40A8D2BBDD71157AFFCBD807359C618CC717EE24D7D"},
            {"name":"qjs-darwin-arm64","browser_download_url":"https://github.com/x","digest":null}]}"#;
        let (v, url, sha) = pick_github_asset(json, "qjs-linux-x86_64").unwrap();
        assert_eq!(v, "0.18.0");
        assert!(url.ends_with("/v0.18.0/qjs-linux-x86_64"));
        assert_eq!(
            sha,
            "85ded23b9fabe8c1644fc40a8d2bbdd71157affcbd807359c618cc717ee24d7d"
        );
        // No digest: never installed unchecked.
        assert!(pick_github_asset(json, "qjs-darwin-arm64").is_err());
        assert!(pick_github_asset(json, "qjs-nope").is_err());
        assert!(
            pick_github_asset(r#"{"tag_name":"latest","assets":[]}"#, "qjs-linux-x86_64").is_err()
        );
    }

    #[test]
    fn quickjs_builds_per_platform() {
        let p = |t| Platform::from_triple(t).unwrap();
        assert_eq!(
            quickjs_asset_name_for(p("x86_64-unknown-linux-gnu")),
            Some("qjs-linux-x86_64")
        );
        assert_eq!(
            quickjs_asset_name_for(p("aarch64-unknown-linux-gnu")),
            Some("qjs-linux-aarch64")
        );
        assert_eq!(
            quickjs_asset_name_for(p("aarch64-apple-darwin")),
            Some("qjs-darwin-arm64")
        );
        assert_eq!(
            quickjs_asset_name_for(p("aarch64-pc-windows-msvc")),
            Some("qjs-windows-x86_64.exe")
        );
    }

    #[test]
    fn reads_both_kinds_of_deno_checksum_file() {
        let name = "deno-x86_64-pc-windows-msvc.zip";
        let windows = "\r\nAlgorithm : SHA256\r\nHash      : CBA84B26E0DF5AE7F74FC66B86A95B866872A86292EBD84FA1E959927EDD93BF\r\nPath      : C:\\a\\deno\\deno\\target\\release\\deno-x86_64-pc-windows-msvc.zip\r\n\r\n";
        assert_eq!(
            deno_sum(windows, name).as_deref(),
            Some("cba84b26e0df5ae7f74fc66b86a95b866872a86292ebd84fa1e959927edd93bf")
        );
        // Another file's listing is not this one's checksum.
        assert_eq!(deno_sum(windows, "deno-aarch64-pc-windows-msvc.zip"), None);
        let unix = "e890975bc2549807bbcc0b584ffafda72cc7c93349dd99c80483dd784f6c4609  deno-x86_64-unknown-linux-gnu.zip\n";
        assert_eq!(
            deno_sum(unix, "deno-x86_64-unknown-linux-gnu.zip").as_deref(),
            Some("e890975bc2549807bbcc0b584ffafda72cc7c93349dd99c80483dd784f6c4609")
        );
    }

    #[test]
    fn deno_builds_per_platform() {
        let p = |t| Platform::from_triple(t).unwrap();
        assert_eq!(
            deno_asset_name_for(p("aarch64-pc-windows-msvc")),
            Some("deno-aarch64-pc-windows-msvc.zip")
        );
        assert_eq!(
            deno_asset_name_for(p("x86_64-unknown-linux-gnu")),
            Some("deno-x86_64-unknown-linux-gnu.zip")
        );
        assert_eq!(
            deno_asset_name_for(p("aarch64-apple-darwin")),
            Some("deno-aarch64-apple-darwin.zip")
        );
        assert_eq!(deno_asset_name_for(p("x86_64-unknown-linux-musl")), None);
    }
}
