//! `whoosh-bench fetch`: downloads pinned competitor builds from each
//! project's official GitHub releases, verifies them, and unpacks them
//! under the cache directory (`~/.cache/whoosh-bench/tools/`). Never run
//! automatically; binaries are never committed.
//!
//! Verification, all of which must pass:
//! 1. The asset's SHA-256 equals the hash pinned in [`PINS`] (taken from
//!    the publisher's checksum file when the pin was made). A mismatch means
//!    the release changed after pinning: always refused.
//! 2. The hash the release publishes — the project's own checksum file when
//!    it ships one, otherwise the SHA-256 digest GitHub shows for the asset
//!    on the release page — must exist and match. If neither is published,
//!    the asset is refused unless `--allow-unverified`.
//! 3. The download URL must be a `github.com/<repo>/releases/download/`
//!    URL of the pinned repository.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How an asset is packed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Archive {
    /// The asset is the executable itself.
    Raw,
    TarGz,
    Zip,
}

/// One pinned release asset.
#[derive(Debug, Clone, Copy)]
pub struct Pin {
    /// Tool name, as adapters use it (`aria2-next`, `mdm`, …).
    pub tool: &'static str,
    pub version: &'static str,
    pub repo: &'static str,
    pub tag: &'static str,
    /// `linux-x86_64`, `windows-x86_64`, …
    pub platform: &'static str,
    pub asset: &'static str,
    /// SHA-256 at pinning time.
    pub sha256: &'static str,
    /// The project's checksum file in the same release, if it has one.
    pub checksum_asset: Option<&'static str>,
    pub archive: Archive,
    /// Executables to expose: (file name inside the asset, name to install
    /// it as). Files are found anywhere in the unpacked tree.
    pub binaries: &'static [(&'static str, &'static str)],
}

/// Pinned competitor builds (2026-11-10).
pub const PINS: &[Pin] = &[
    Pin {
        tool: "aria2-next",
        version: "3.1.0",
        repo: "AnInsomniacy/aria2-next",
        tag: "v3.1.0",
        platform: "linux-x86_64",
        asset: "aria2-next-3.1.0-linux-x86_64",
        sha256: "5a264fc39312cc1abc6d12cfd2d96464d27f0349fd7cb5e0cbf1ee5bd686e890",
        checksum_asset: Some("aria2-next-3.1.0-checksums.sha256"),
        archive: Archive::Raw,
        binaries: &[("aria2-next-3.1.0-linux-x86_64", "aria2-next")],
    },
    Pin {
        tool: "aria2-next",
        version: "3.1.0",
        repo: "AnInsomniacy/aria2-next",
        tag: "v3.1.0",
        platform: "windows-x86_64",
        asset: "aria2-next-3.1.0-windows-x86_64.exe",
        sha256: "12bbd48dfd2c86f55419bf92b196dd3aedd6983116b5ce10c85d27a5a76c2baa",
        checksum_asset: Some("aria2-next-3.1.0-checksums.sha256"),
        archive: Archive::Raw,
        binaries: &[("aria2-next-3.1.0-windows-x86_64.exe", "aria2-next.exe")],
    },
    Pin {
        tool: "mdm",
        version: "0.5.2",
        repo: "anefzaoui/misterdownloadmanager",
        tag: "v0.5.2",
        platform: "linux-x86_64",
        asset: "MisterDownloadManager-0.5.2-linux-x86_64.tar.gz",
        sha256: "3327ebd6b5565186c3e1286bbfdc85ffc95fa681cc2a57a51b0bc07ff42a009f",
        checksum_asset: Some("SHA256SUMS.txt"),
        archive: Archive::TarGz,
        binaries: &[("misterdownloadmanager-cli", "misterdownloadmanager-cli")],
    },
    Pin {
        tool: "mdm",
        version: "0.5.2",
        repo: "anefzaoui/misterdownloadmanager",
        tag: "v0.5.2",
        platform: "windows-x86_64",
        asset: "MisterDownloadManager-0.5.2-portable.zip",
        sha256: "0907a6c6aed5c4967e89549e883147d2b33b073e6f8b0d914dff84d32f3b7cf7",
        checksum_asset: Some("SHA256SUMS.txt"),
        archive: Archive::Zip,
        binaries: &[(
            "misterdownloadmanager-cli.exe",
            "misterdownloadmanager-cli.exe",
        )],
    },
    Pin {
        tool: "surge",
        version: "0.12.4",
        repo: "SurgeDM/Surge",
        tag: "v0.12.4",
        platform: "linux-x86_64",
        asset: "Surge_0.12.4_linux_amd64.tar.gz",
        sha256: "2f7b4c4bc0bcac5996e6d223e5be676c041125832212fa3d92f7fe120d6fcd69",
        checksum_asset: Some("Surge_0.12.4_checksums.txt"),
        archive: Archive::TarGz,
        binaries: &[("surge", "surge")],
    },
    Pin {
        tool: "surge",
        version: "0.12.4",
        repo: "SurgeDM/Surge",
        tag: "v0.12.4",
        platform: "windows-x86_64",
        asset: "Surge_0.12.4_windows_amd64.zip",
        sha256: "fcdd43dc053aebf039c96389b32d8024287aa37b23821c657a22434dca17ea34",
        checksum_asset: Some("Surge_0.12.4_checksums.txt"),
        archive: Archive::Zip,
        binaries: &[("surge.exe", "surge.exe")],
    },
    // Gopeed publishes no checksum file; GitHub's asset digest is the
    // published hash. Its only headless build is the web server.
    Pin {
        tool: "gopeed",
        version: "1.9.3",
        repo: "GopeedLab/gopeed",
        tag: "v1.9.3",
        platform: "linux-x86_64",
        asset: "gopeed-web-v1.9.3-linux-amd64.zip",
        sha256: "52444c86ffb5152c89d1c3a3b074285c4ef6dd3dabc9d4ae097cc703e0fa3e15",
        checksum_asset: None,
        archive: Archive::Zip,
        binaries: &[("gopeed", "gopeed-web")],
    },
    Pin {
        tool: "gopeed",
        version: "1.9.3",
        repo: "GopeedLab/gopeed",
        tag: "v1.9.3",
        platform: "windows-x86_64",
        asset: "gopeed-web-v1.9.3-windows-amd64.zip",
        sha256: "ee3d41fa19c366b820a2483619328156579108ac03c53ea35cdb33327320f756",
        checksum_asset: None,
        archive: Archive::Zip,
        binaries: &[("gopeed.exe", "gopeed-web.exe")],
    },
];

/// `linux-x86_64`-style name of this platform.
pub fn platform() -> String {
    let os = std::env::consts::OS;
    format!("{os}-{}", std::env::consts::ARCH)
}

/// `$XDG_CACHE_HOME/whoosh-bench` (`~/.cache/whoosh-bench`), or
/// `%LOCALAPPDATA%\whoosh-bench` on Windows.
pub fn cache_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("WHOOSH_BENCH_CACHE") {
        return PathBuf::from(d);
    }
    if cfg!(windows)
        && let Some(d) = std::env::var_os("LOCALAPPDATA")
    {
        return PathBuf::from(d).join("whoosh-bench");
    }
    if let Some(d) = std::env::var_os("XDG_CACHE_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(d).join("whoosh-bench");
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".cache").join("whoosh-bench")
}

/// What `fetch` records next to an unpacked tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Installed {
    pub tool: String,
    pub version: String,
    pub repo: String,
    pub tag: String,
    pub asset: String,
    pub sha256: String,
    /// `checksum-file:<name>`, `github-digest`, or `unverified`.
    pub verified_by: String,
    /// Installed name -> absolute path.
    pub binaries: BTreeMap<String, PathBuf>,
}

/// Every tool `fetch` has installed under `cache`.
pub fn installed(cache: &Path) -> Vec<Installed> {
    let Ok(rd) = std::fs::read_dir(cache.join("tools")) else {
        return Vec::new();
    };
    let mut v: Vec<Installed> = rd
        .flatten()
        .filter_map(|e| std::fs::read(e.path().join("tool.json")).ok())
        .filter_map(|b| serde_json::from_slice(&b).ok())
        .collect();
    v.sort_by(|a, b| (&a.tool, &a.version).cmp(&(&b.tool, &b.version)));
    v
}

/// Finds an installed executable named `name` (any tool, newest pinned
/// version last wins).
pub fn installed_binary(cache: &Path, name: &str) -> Option<(PathBuf, Installed)> {
    installed(cache).into_iter().rev().find_map(|t| {
        let p = t
            .binaries
            .get(name)
            .or_else(|| t.binaries.get(&format!("{name}.exe")))?;
        p.is_file().then(|| (p.clone(), t.clone()))
    })
}

// ------------------------------------------------------------- GitHub --

#[derive(Debug, Deserialize)]
struct Release {
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .user_agent("whoosh-bench (benchmark harness; fetch)")
        .build()
        .into()
}

fn get_release(agent: &ureq::Agent, pin: &Pin) -> Result<Release> {
    let url = format!(
        "https://api.github.com/repos/{}/releases/tags/{}",
        pin.repo, pin.tag
    );
    let mut req = agent
        .get(&url)
        .header("Accept", "application/vnd.github+json");
    let token = std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty());
    if let Some(t) = &token {
        req = req.header("Authorization", &format!("Bearer {t}"));
    }
    let mut r = req.call().with_context(|| format!("GET {url}"))?;
    let body = r.body_mut().read_to_string()?;
    serde_json::from_str(&body).with_context(|| format!("parse release {}", pin.tag))
}

/// The download URL must be an official release asset of `repo`.
/// GitHub owner and repository names are case-insensitive.
pub fn is_official_asset_url(repo: &str, tag: &str, url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://github.com/") else {
        return false;
    };
    let Some(rest) = rest
        .get(..repo.len())
        .filter(|r| r.eq_ignore_ascii_case(repo))
        .map(|_| &rest[repo.len()..])
    else {
        return false;
    };
    let tail = format!("/releases/download/{tag}/");
    rest.strip_prefix(tail.as_str())
        .is_some_and(|name| !name.is_empty() && !name.contains('/'))
}

/// Finds `asset`'s hash in a `sha256sum`-style checksum file.
pub fn hash_from_checksum_file(text: &str, asset: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        let hash = it.next()?;
        let name = it.next()?.trim_start_matches('*');
        (name == asset && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

/// Downloads `url` to `dest`, returning the SHA-256 (hex).
fn download(agent: &ureq::Agent, url: &str, dest: &Path) -> Result<String> {
    let mut r = agent
        .get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    if r.status() != 200 {
        bail!("GET {url}: HTTP {}", r.status());
    }
    let mut reader = r.body_mut().with_config().limit(2 << 30).reader();
    let mut f = std::fs::File::create(dest)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        f.write_all(&buf[..n])?;
    }
    f.sync_all()?;
    Ok(hex::encode(h.finalize()))
}

fn download_text(agent: &ureq::Agent, url: &str) -> Result<String> {
    let mut r = agent
        .get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    if r.status() != 200 {
        bail!("GET {url}: HTTP {}", r.status());
    }
    Ok(r.body_mut().read_to_string()?)
}

// ------------------------------------------------------------- unpack --

fn unpack(archive: Archive, file: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    match archive {
        Archive::Raw => {}
        Archive::TarGz => {
            let gz = flate2::read::GzDecoder::new(std::fs::File::open(file)?);
            let mut t = tar::Archive::new(gz);
            t.set_preserve_permissions(true);
            // `unpack` refuses entries that would land outside `dest`.
            t.unpack(dest).context("unpack tar.gz")?;
        }
        Archive::Zip => {
            let mut z = zip::ZipArchive::new(std::fs::File::open(file)?)?;
            // `extract` uses each entry's enclosed name (no traversal).
            z.extract(dest).context("unpack zip")?;
        }
    }
    Ok(())
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let p = e.path();
            let ft = e.file_type().ok()?;
            if ft.is_dir() {
                stack.push(p);
            } else if e.file_name() == name {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(unix)]
fn make_executable(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(p)?.permissions();
    perm.set_mode(perm.mode() | 0o755);
    std::fs::set_permissions(p, perm)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_: &Path) -> Result<()> {
    Ok(())
}

// --------------------------------------------------------------- main --

/// Options of `whoosh-bench fetch`.
#[derive(Debug, Clone)]
pub struct FetchOptions {
    /// Tools to fetch; empty = every tool pinned for this platform.
    pub tools: Vec<String>,
    pub allow_unverified: bool,
    pub cache: PathBuf,
    pub platform: String,
}

/// Fetches, verifies and installs the selected pins. Returns what was
/// installed; stops at the first verification failure.
pub fn fetch(opts: &FetchOptions) -> Result<Vec<Installed>> {
    let pins: Vec<&Pin> = PINS
        .iter()
        .filter(|p| p.platform == opts.platform)
        .filter(|p| opts.tools.is_empty() || opts.tools.iter().any(|t| t == p.tool))
        .collect();
    if pins.is_empty() {
        bail!(
            "no pinned builds for {:?} on {} (pinned: {})",
            opts.tools,
            opts.platform,
            PINS.iter()
                .map(|p| format!("{} ({})", p.tool, p.platform))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let agent = agent();
    let mut done = Vec::new();
    for pin in pins {
        done.push(fetch_one(&agent, pin, opts)?);
    }
    Ok(done)
}

fn fetch_one(agent: &ureq::Agent, pin: &Pin, opts: &FetchOptions) -> Result<Installed> {
    eprintln!(
        "fetch: {} {} ({}) from github.com/{}",
        pin.tool, pin.version, pin.asset, pin.repo
    );
    let release = get_release(agent, pin)?;
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == pin.asset)
        .with_context(|| format!("release {} has no asset {}", pin.tag, pin.asset))?;
    if !is_official_asset_url(pin.repo, pin.tag, &asset.browser_download_url) {
        bail!(
            "refusing {}: not an official release URL",
            asset.browser_download_url
        );
    }

    // The hash the release publishes.
    let (published, verified_by) = if let Some(sums) = pin.checksum_asset {
        let s = release
            .assets
            .iter()
            .find(|a| a.name == sums)
            .with_context(|| format!("release {} lacks its checksum file {sums}", pin.tag))?;
        if !is_official_asset_url(pin.repo, pin.tag, &s.browser_download_url) {
            bail!(
                "refusing checksum file {}: not an official release URL",
                s.browser_download_url
            );
        }
        let text = download_text(agent, &s.browser_download_url)?;
        let h = hash_from_checksum_file(&text, pin.asset)
            .with_context(|| format!("{sums} does not list {}", pin.asset))?;
        (Some(h), format!("checksum-file:{sums}"))
    } else if let Some(d) = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
    {
        (Some(d.to_ascii_lowercase()), "github-digest".to_owned())
    } else {
        (None, "unverified".to_owned())
    };
    if published.is_none() && !opts.allow_unverified {
        bail!(
            "{} publishes no checksum for {}; refusing (pass --allow-unverified to accept it)",
            pin.repo,
            pin.asset
        );
    }

    let dest = opts
        .cache
        .join("tools")
        .join(format!("{}-{}", pin.tool, pin.version));
    let downloads = opts.cache.join("downloads");
    std::fs::create_dir_all(&downloads)?;
    let file = downloads.join(pin.asset);
    let actual = download(agent, &asset.browser_download_url, &file)?;
    let fail = |why: String| -> Result<Installed> {
        let _ = std::fs::remove_file(&file);
        bail!("{}: {why}; deleted the download", pin.asset)
    };
    if actual != pin.sha256 {
        return fail(format!(
            "SHA-256 {actual} differs from the pinned {} (release changed since pinning?)",
            pin.sha256
        ));
    }
    if let Some(p) = &published
        && &actual != p
    {
        return fail(format!("SHA-256 {actual} differs from the published {p}"));
    }
    eprintln!("fetch:   sha256 {actual} OK (pinned; published via {verified_by})");

    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    let unpacked = dest.join("files");
    unpack(pin.archive, &file, &unpacked)?;
    let mut binaries = BTreeMap::new();
    for (inside, installed_as) in pin.binaries {
        let src = if pin.archive == Archive::Raw {
            file.clone()
        } else {
            find_file(&unpacked, inside)
                .with_context(|| format!("{} contains no {inside}", pin.asset))?
        };
        let bin_dir = dest.join("bin");
        std::fs::create_dir_all(&bin_dir)?;
        let target = bin_dir.join(installed_as);
        std::fs::copy(&src, &target)?;
        make_executable(&target)?;
        // Keep the unpacked tree: some tools load files next to the binary.
        // Prefer running the binary in place for those.
        let run_path = if pin.archive == Archive::Raw {
            target.clone()
        } else {
            make_executable(&src)?;
            std::fs::canonicalize(&src)?
        };
        binaries.insert((*installed_as).to_owned(), run_path);
    }
    if pin.archive == Archive::Raw {
        let _ = std::fs::remove_dir_all(&unpacked);
    }
    let inst = Installed {
        tool: pin.tool.to_owned(),
        version: pin.version.to_owned(),
        repo: pin.repo.to_owned(),
        tag: pin.tag.to_owned(),
        asset: pin.asset.to_owned(),
        sha256: actual,
        verified_by,
        binaries,
    };
    std::fs::write(dest.join("tool.json"), serde_json::to_vec_pretty(&inst)?)?;
    eprintln!("fetch:   installed in {}", dest.display());
    Ok(inst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_are_well_formed() {
        for p in PINS {
            assert_eq!(p.sha256.len(), 64, "{}", p.asset);
            assert!(
                p.sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            );
            assert!(p.tag.contains(p.version), "{} tag/version", p.asset);
            assert!(p.asset.contains(p.version), "{} names its version", p.asset);
            assert!(!p.binaries.is_empty());
        }
    }

    #[test]
    fn checksum_file_parsing() {
        let text = "\
5a264fc39312cc1abc6d12cfd2d96464d27f0349fd7cb5e0cbf1ee5bd686e890  aria2-next-3.1.0-linux-x86_64
12BBD48DFD2C86F55419BF92B196DD3AEDD6983116B5CE10C85D27A5A76C2BAA *aria2-next-3.1.0-windows-x86_64.exe
nothex  other
";
        assert_eq!(
            hash_from_checksum_file(text, "aria2-next-3.1.0-linux-x86_64").unwrap(),
            "5a264fc39312cc1abc6d12cfd2d96464d27f0349fd7cb5e0cbf1ee5bd686e890"
        );
        assert_eq!(
            hash_from_checksum_file(text, "aria2-next-3.1.0-windows-x86_64.exe").unwrap(),
            "12bbd48dfd2c86f55419bf92b196dd3aedd6983116b5ce10c85d27a5a76c2baa"
        );
        assert!(hash_from_checksum_file(text, "other").is_none());
        assert!(hash_from_checksum_file(text, "aria2-next").is_none());
    }

    #[test]
    fn official_urls_only() {
        let ok = "https://github.com/SurgeDM/Surge/releases/download/v0.12.4/Surge_0.12.4_linux_amd64.tar.gz";
        assert!(
            is_official_asset_url("SurgeDM/surge", "v0.12.4", ok),
            "case-insensitive"
        );
        assert!(is_official_asset_url("SurgeDM/Surge", "v0.12.4", ok));
        assert!(!is_official_asset_url("SurgeDM/Surg", "v0.12.4", ok));
        assert!(!is_official_asset_url("SurgeDM/surge", "v0.12.3", ok));
        assert!(!is_official_asset_url("evil/surge", "v0.12.4", ok));
        assert!(!is_official_asset_url(
            "SurgeDM/surge",
            "v0.12.4",
            "https://github.com/SurgeDM/surge/releases/download/v0.12.4/../x"
        ));
        assert!(!is_official_asset_url(
            "SurgeDM/surge",
            "v0.12.4",
            "http://github.com/SurgeDM/surge/releases/download/v0.12.4/a"
        ));
    }

    #[test]
    fn unpack_raw_and_find() {
        let dir = std::env::temp_dir().join(format!("whoosh-bench-fetch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("a/b/tool"), b"x").unwrap();
        assert_eq!(find_file(&dir, "tool").unwrap(), dir.join("a/b/tool"));
        assert!(find_file(&dir, "missing").is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
