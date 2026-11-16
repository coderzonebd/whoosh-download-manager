//! yt-dlp, ffmpeg and QuickJS shipped inside the desktop installers, so video and
//! audio sites work right after installation.
//!
//! **At build time** (`wdm-media-bundle`, run by the release workflow)
//! [`fetch`] takes the newest builds for the release target from the same
//! upstreams, with the same checksum check, as the first-use download
//! ([`crate::upstream`]): every file is checked against the checksum the
//! project publishes before it is unpacked. For macOS it joins the arm64
//! and x86_64 ffmpeg into one universal program with `lipo` (yt-dlp's macOS
//! build is universal already). It writes the programs and a
//! [`MANIFEST`] that records the versions, the upstream files and their
//! checksums, and the SHA-256 of every file as shipped. For Linux each
//! program goes into an uncompressed `<name>.tar` (the AppImage bundler
//! rewrites every ELF program in the image, a tar it leaves alone), and the
//! manifest records the checksums of both the tar and the program in it
//! (plus, in
//! `LICENSES/`, the licence that comes inside BtbN's ffmpeg archive).
//! Signing changes the
//! files, so after signing the workflow runs [`rehash`]; after bundling,
//! [`verify`] checks the copy inside the app.
//!
//! **At run time** the desktop app passes the bundle's directory to the
//! daemon (`--media-tools-dir`). [`crate::Tools`] copies the programs into
//! `<data dir>/media/bin` (unpacking the Linux tars with the system's `tar`
//! first), checking each against the manifest, so they can
//! be updated there like a downloaded copy. They are never run from the
//! bundle. Without a bundle (the command line, distribution packages), or
//! when a file does not match, the tools are downloaded on first use as
//! before.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wdm_proto::UnixMillis;

use crate::fetch::Fetcher;
use crate::process::{self, Tool};
use crate::tools::{self, Kind};
use crate::upstream::{self, Archive, Asset, Platform, Upstreams};
use crate::util::{now_ms, write_atomic};

/// The manifest's file name in the bundle directory.
pub const MANIFEST: &str = "manifest.json";

/// `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// The Rust target triple the tools are for, e.g.
    /// `universal-apple-darwin`.
    pub target: String,
    pub fetched_at: UnixMillis,
    /// By tool name (`yt-dlp`, `ffmpeg`).
    pub tools: BTreeMap<String, BundledTool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundledTool {
    pub version: String,
    pub upstream: String,
    pub license: String,
    /// The upstream files this came from, with the checksums upstream
    /// publishes (two for a universal macOS ffmpeg).
    pub sources: Vec<Source>,
    /// File name → SHA-256 (lower-case hex) of the file as shipped. The
    /// tool's program is one of them; ffmpeg may bring ffprobe.
    pub files: BTreeMap<String, String>,
    /// Files shipped inside an uncompressed `<name>.tar` (Linux): file name
    /// → SHA-256 of the tar. The AppImage bundler (linuxdeploy) rewrites
    /// every ELF program it finds in the image; a tar it leaves alone.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tars: BTreeMap<String, String>,
}

/// The tar a file is shipped in.
pub fn tar_name(name: &str) -> String {
    format!("{name}.tar")
}

/// Whether `dir` has `name` of `tool`, as a file or in its tar.
pub fn shipped(dir: &Path, tool: &BundledTool, name: &str) -> bool {
    if tool.tars.contains_key(name) {
        dir.join(tar_name(name)).is_file()
    } else {
        dir.join(name).is_file()
    }
}

/// The path of `name` from `dir`: the file itself, or, for a file shipped
/// in a tar, the tar's checksum checked and the file unpacked into a new
/// directory below `scratch`. The file's own checksum is the caller's to
/// check.
pub async fn extract(
    dir: &Path,
    tool: &BundledTool,
    name: &str,
    scratch: &Path,
) -> Result<PathBuf, String> {
    if !valid_file_name(name) {
        return Err(format!("bad file name {name:?}"));
    }
    let Some(tar_sha) = tool.tars.get(name) else {
        return Ok(dir.join(name));
    };
    let tar = dir.join(tar_name(name));
    let got = sha256_file(&tar).await?;
    if !got.eq_ignore_ascii_case(tar_sha) {
        return Err(format!(
            "checksum mismatch for {}: the manifest says {tar_sha}, the file is {got}",
            tar.display()
        ));
    }
    let out = scratch.join(format!("x-{name}-{}", crate::util::random_hex(4)));
    tokio::fs::create_dir_all(&out)
        .await
        .map_err(|e| format!("{}: {e}", out.display()))?;
    tools::unpack(&tar, &out).await?;
    let file = out.join(name);
    let meta = tokio::fs::symlink_metadata(&file)
        .await
        .map_err(|e| format!("{name} is not in {}: {e}", tar.display()))?;
    if !meta.is_file() {
        return Err(format!("{name} in {} is not a regular file", tar.display()));
    }
    Ok(file)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub url: String,
    pub sha256: String,
}

/// A file name the manifest may use: a plain name, no path.
pub fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// The manifest in `dir`, if there is a readable one.
pub async fn read(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join(MANIFEST);
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() > 1 << 20 {
        return Err(format!("{}: larger than 1 MiB", path.display()));
    }
    let m: Manifest =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    for tool in m.tools.values() {
        if let Some(bad) = tool
            .files
            .keys()
            .chain(tool.tars.keys())
            .find(|n| !valid_file_name(n))
        {
            return Err(format!("{}: bad file name {bad:?}", path.display()));
        }
    }
    Ok(m)
}

async fn write(dir: &Path, m: &Manifest) -> Result<(), String> {
    let mut json = serde_json::to_vec_pretty(m).map_err(|e| e.to_string())?;
    json.push(b'\n');
    write_atomic(&dir.join(MANIFEST), &json)
        .await
        .map_err(|e| format!("{MANIFEST}: {e}"))
}

/// SHA-256 of the file at `path`, as lower-case hex.
pub async fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .await
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}

/// Copies `from` to `to` (created or truncated; no extended attributes
/// such as macOS quarantine come along) and checks the copy against
/// `sha256`. A mismatch deletes `to`.
pub async fn copy_verified(from: &Path, to: &Path, sha256: &str) -> Result<(), String> {
    let copy = async {
        let mut src = tokio::fs::File::open(from)
            .await
            .map_err(|e| format!("{}: {e}", from.display()))?;
        let mut dst = tokio::fs::File::create(to)
            .await
            .map_err(|e| format!("{}: {e}", to.display()))?;
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = src
                .read(&mut buf)
                .await
                .map_err(|e| format!("{}: {e}", from.display()))?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
            dst.write_all(&buf[..n])
                .await
                .map_err(|e| format!("{}: {e}", to.display()))?;
        }
        dst.sync_all()
            .await
            .map_err(|e| format!("{}: {e}", to.display()))?;
        Ok::<_, String>(hex::encode(hash.finalize()))
    };
    let result = match copy.await {
        Ok(got) if !sha256.is_empty() && got.eq_ignore_ascii_case(sha256) => Ok(()),
        Ok(got) => Err(format!(
            "checksum mismatch for {}: the manifest says {}, the file is {got}",
            from.display(),
            if sha256.is_empty() { "nothing" } else { sha256 }
        )),
        Err(e) => Err(e),
    };
    if result.is_err() {
        let _ = tokio::fs::remove_file(to).await;
    }
    result
}

/// The platforms whose builds make up `target`'s tools: one, or both macOS
/// architectures for `universal-apple-darwin`.
pub fn platforms_for(target: &str) -> Option<Vec<Platform>> {
    if target == "universal-apple-darwin" {
        return Some(vec![
            Platform::from_triple("aarch64-apple-darwin")?,
            Platform::from_triple("x86_64-apple-darwin")?,
        ]);
    }
    Platform::from_triple(target).map(|p| vec![p])
}

/// Build time: fetches the newest yt-dlp and ffmpeg for `target` into
/// `out` (created; existing files of the same names are replaced) and
/// writes the manifest. `log` gets one line per step.
pub async fn fetch(
    fetcher: &dyn Fetcher,
    up: &Upstreams,
    target: &str,
    out: &Path,
    log: &(dyn Fn(&str) + Send + Sync),
) -> Result<Manifest, String> {
    let platforms = platforms_for(target)
        .ok_or_else(|| format!("no yt-dlp and ffmpeg builds are known for {target}"))?;
    let first = platforms[0];
    tokio::fs::create_dir_all(out)
        .await
        .map_err(|e| format!("{}: {e}", out.display()))?;
    let work = out.join(format!(".work-{}", crate::util::random_hex(6)));
    tokio::fs::create_dir_all(&work)
        .await
        .map_err(|e| format!("{}: {e}", work.display()))?;
    let result = fetch_into(fetcher, up, target, &platforms, first, out, &work, log).await;
    let _ = tokio::fs::remove_dir_all(&work).await;
    result
}

#[allow(clippy::too_many_arguments)]
async fn fetch_into(
    fetcher: &dyn Fetcher,
    up: &Upstreams,
    target: &str,
    platforms: &[Platform],
    first: Platform,
    out: &Path,
    work: &Path,
    log: &(dyn Fn(&str) + Send + Sync),
) -> Result<Manifest, String> {
    let exe = first.exe_suffix();
    let mut manifest = Manifest {
        target: target.to_owned(),
        fetched_at: now_ms(),
        tools: BTreeMap::new(),
    };

    // yt-dlp: one standalone program (universal on macOS).
    let asset = upstream::latest_yt_dlp_for(fetcher, up, first).await?;
    log(&format!("yt-dlp {}: {}", asset.version, asset.url));
    let dir = work.join("yt-dlp");
    let download = fetch_asset(fetcher, &asset, &dir).await?;
    let name = format!("{}{exe}", Kind::YtDlp.name());
    place(&download, &out.join(&name)).await?;
    manifest.tools.insert(
        Kind::YtDlp.name().to_owned(),
        BundledTool {
            version: asset.version.clone(),
            upstream: upstream::YT_DLP_UPSTREAM.to_owned(),
            license: upstream::YT_DLP_LICENSE.to_owned(),
            sources: vec![Source {
                url: asset.url,
                sha256: asset.sha256,
            }],
            files: BTreeMap::from([(name, String::new())]),
            tars: BTreeMap::new(),
        },
    );

    // QuickJS (yt-dlp's JavaScript runtime for YouTube when the computer has
    // none): one static program per architecture, joined for a universal
    // target.
    let qjs = format!("{}{exe}", Kind::QuickJs.program());
    let mut parts = Vec::new();
    let mut sources = Vec::new();
    let mut qjs_version: Option<String> = None;
    for (i, p) in platforms.iter().enumerate() {
        let asset = upstream::latest_quickjs_for(fetcher, up, *p).await?;
        log(&format!(
            "QuickJS {} ({} {}): {}",
            asset.version, p.os, p.arch, asset.url
        ));
        if qjs_version.as_ref().is_some_and(|v| *v != asset.version) {
            return Err("QuickJS versions differ between architectures; try again later".into());
        }
        qjs_version = Some(asset.version.clone());
        let download = fetch_asset(fetcher, &asset, &work.join(format!("quickjs-{i}"))).await?;
        parts.push(download);
        sources.push(Source {
            url: asset.url,
            sha256: asset.sha256,
        });
    }
    let to = out.join(&qjs);
    match parts.as_slice() {
        [one] => place(one, &to).await?,
        many => lipo(&many.iter().collect::<Vec<_>>(), &to).await?,
    }
    manifest.tools.insert(
        Kind::QuickJs.name().to_owned(),
        BundledTool {
            version: qjs_version.unwrap_or_default(),
            upstream: upstream::QUICKJS_UPSTREAM.to_owned(),
            license: upstream::QUICKJS_LICENSE.to_owned(),
            sources,
            files: BTreeMap::from([(qjs, String::new())]),
            tars: BTreeMap::new(),
        },
    );

    // ffmpeg (and ffprobe where the archive has it): one build per
    // architecture, joined for a universal target.
    let names = [
        format!("{}{exe}", Kind::Ffmpeg.name()),
        format!("ffprobe{exe}"),
    ];
    let mut builds: Vec<(Asset, BTreeMap<String, PathBuf>)> = Vec::new();
    for (i, p) in platforms.iter().enumerate() {
        let asset = upstream::latest_ffmpeg_for(fetcher, up, *p).await?;
        log(&format!(
            "ffmpeg {} ({} {}): {}",
            asset.version, p.os, p.arch, asset.url
        ));
        let dir = work.join(format!("ffmpeg-{i}"));
        let download = fetch_asset(fetcher, &asset, &dir).await?;
        let unpacked = dir.join("unpacked");
        tokio::fs::create_dir_all(&unpacked)
            .await
            .map_err(|e| e.to_string())?;
        tools::unpack(&download, &unpacked).await?;
        let mut found = BTreeMap::new();
        for n in &names {
            if let Some(f) = tools::find_file(&unpacked, n) {
                found.insert(n.clone(), f);
            }
        }
        if !found.contains_key(&names[0]) {
            return Err(format!("{} is not in {}", names[0], asset.url));
        }
        // BtbN's archives carry the licence of that build.
        if i == 0
            && let Some(license) = tools::find_file(&unpacked, "LICENSE.txt")
        {
            let dir = out.join("LICENSES");
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|e| format!("{}: {e}", dir.display()))?;
            tokio::fs::copy(&license, dir.join("ffmpeg-build-LICENSE.txt"))
                .await
                .map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        builds.push((asset, found));
    }
    let version = builds[0].0.version.clone();
    if let Some((other, _)) = builds.iter().find(|(a, _)| a.version != version) {
        return Err(format!(
            "ffmpeg versions differ between architectures: {version} and {}; try again later",
            other.version
        ));
    }
    let mut files = BTreeMap::new();
    for n in &names {
        let parts: Vec<&PathBuf> = builds.iter().filter_map(|(_, f)| f.get(n)).collect();
        if parts.len() != builds.len() {
            // ffprobe only where every architecture has it.
            continue;
        }
        let to = out.join(n);
        if let [one] = parts.as_slice() {
            place(one, &to).await?;
        } else {
            lipo(&parts, &to).await?;
        }
        files.insert(n.clone(), String::new());
    }
    manifest.tools.insert(
        Kind::Ffmpeg.name().to_owned(),
        BundledTool {
            version,
            upstream: upstream::ffmpeg_upstream_for(first).to_owned(),
            license: upstream::ffmpeg_license_for(first).to_owned(),
            sources: builds
                .into_iter()
                .map(|(a, _)| Source {
                    url: a.url,
                    sha256: a.sha256,
                })
                .collect(),
            files,
            tars: BTreeMap::new(),
        },
    );
    hash_files(out, &mut manifest).await?;
    if first.os == "linux" {
        pack(out, &mut manifest).await?;
    }
    write(out, &manifest).await?;
    Ok(manifest)
}

/// Puts every file into an uncompressed `<name>.tar` next to it (and
/// removes the file), recording the tars' checksums.
async fn pack(dir: &Path, m: &mut Manifest) -> Result<(), String> {
    let tar = tools::system_tar().ok_or("no `tar` program to pack the tools with")?;
    for tool in m.tools.values_mut() {
        for name in tool.files.keys() {
            let packed = dir.join(tar_name(name));
            let _ = tokio::fs::remove_file(&packed).await;
            let out = process::run(
                &Tool::new(&tar),
                vec![
                    OsString::from("-cf"),
                    packed.as_os_str().to_owned(),
                    OsString::from("-C"),
                    dir.as_os_str().to_owned(),
                    OsString::from(name),
                ],
                Duration::from_secs(300),
                64 * 1024,
            )
            .await
            .map_err(|e| format!("tar: {e}"))?;
            if !out.success {
                return Err(format!("tar: {}", out.error_line()));
            }
            tool.tars.insert(name.clone(), sha256_file(&packed).await?);
            tokio::fs::remove_file(dir.join(name))
                .await
                .map_err(|e| format!("{}: {e}", dir.join(name).display()))?;
        }
    }
    Ok(())
}

/// A scratch directory for unpacking tars, removed by the caller.
async fn scratch_dir() -> Result<PathBuf, String> {
    let d = std::env::temp_dir().join(format!("wdm-media-bundle-{}", crate::util::random_hex(6)));
    tokio::fs::create_dir_all(&d)
        .await
        .map_err(|e| format!("{}: {e}", d.display()))?;
    Ok(d)
}

/// Downloads `asset` into `dir` and checks it against upstream's checksum.
async fn fetch_asset(fetcher: &dyn Fetcher, asset: &Asset, dir: &Path) -> Result<PathBuf, String> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let name = match asset.archive {
        Archive::Raw => "download",
        Archive::Packed if asset.url.ends_with(".zip") => "download.zip",
        Archive::Packed => "download.tar.xz",
    };
    let dest = dir.join(name);
    tools::download_verified(fetcher, asset, &dest, &|_, _| {}).await?;
    Ok(dest)
}

async fn place(from: &Path, to: &Path) -> Result<(), String> {
    let _ = tokio::fs::remove_file(to).await;
    tokio::fs::copy(from, to)
        .await
        .map_err(|e| format!("{}: {e}", to.display()))?;
    tools::make_executable(to).await
}

async fn lipo(parts: &[&PathBuf], to: &Path) -> Result<(), String> {
    let _ = tokio::fs::remove_file(to).await;
    let mut args = vec![OsString::from("-create"), OsString::from("-output")];
    args.push(to.as_os_str().to_owned());
    args.extend(parts.iter().map(|p| p.as_os_str().to_owned()));
    let out = process::run(
        &Tool::new("lipo"),
        args,
        Duration::from_secs(300),
        64 * 1024,
    )
    .await
    .map_err(|e| format!("lipo: {e}"))?;
    if !out.success {
        return Err(format!("lipo: {}", out.error_line()));
    }
    tools::make_executable(to).await
}

async fn hash_files(dir: &Path, m: &mut Manifest) -> Result<(), String> {
    let scratch = scratch_dir().await?;
    let result = async {
        for tool in m.tools.values_mut() {
            // Re-taking the tars' checksums first, so `extract` accepts them.
            for (name, sha) in tool.tars.iter_mut() {
                *sha = sha256_file(&dir.join(tar_name(name))).await?;
            }
            let names: Vec<String> = tool.files.keys().cloned().collect();
            for name in names {
                let file = extract(dir, tool, &name, &scratch).await?;
                let sha = sha256_file(&file).await?;
                tool.files.insert(name, sha);
            }
        }
        Ok(())
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&scratch).await;
    result
}

/// After signing changed the files: records their checksums again.
pub async fn rehash(dir: &Path) -> Result<Manifest, String> {
    let mut m = read(dir).await?;
    hash_files(dir, &mut m).await?;
    write(dir, &m).await?;
    Ok(m)
}

/// Checks every file in `dir` against its manifest (the copy inside a
/// built app: the bundler must not have changed them).
/// Files shipped in a tar are checked twice: the tar, and the file in it.
pub async fn verify(dir: &Path) -> Result<Manifest, String> {
    let m = read(dir).await?;
    let scratch = scratch_dir().await?;
    let result = async {
        for tool in m.tools.values() {
            for (name, sha) in &tool.files {
                let file = extract(dir, tool, name, &scratch).await?;
                let got = sha256_file(&file).await?;
                if !got.eq_ignore_ascii_case(sha) {
                    return Err(format!(
                        "{name} in {}: the manifest says {sha}, the file is {got}",
                        dir.display()
                    ));
                }
            }
        }
        Ok(())
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&scratch).await;
    result.map(|()| m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_plain() {
        assert!(valid_file_name("ffmpeg.exe"));
        assert!(valid_file_name("yt-dlp"));
        assert!(!valid_file_name("../ffmpeg"));
        assert!(!valid_file_name("bin/ffmpeg"));
        assert!(!valid_file_name("C:ffmpeg"));
        assert!(!valid_file_name(".hidden"));
        assert!(!valid_file_name(""));
    }

    #[test]
    fn universal_macos_is_both_architectures() {
        let p = platforms_for("universal-apple-darwin").unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].os, p[0].arch), ("macos", "aarch64"));
        assert_eq!((p[1].os, p[1].arch), ("macos", "x86_64"));
        assert_eq!(platforms_for("aarch64-pc-windows-msvc").unwrap().len(), 1);
        assert!(platforms_for("wasm32-unknown-unknown").is_none());
    }

    #[tokio::test]
    async fn copies_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("a");
        tokio::fs::write(&from, b"program").await.unwrap();
        let sha = crate::fetch::sha256_hex(b"program");
        let to = dir.path().join("b");
        copy_verified(&from, &to, &sha).await.unwrap();
        assert_eq!(tokio::fs::read(&to).await.unwrap(), b"program");
        let bad = dir.path().join("c");
        let e = copy_verified(&from, &bad, &"0".repeat(64))
            .await
            .unwrap_err();
        assert!(e.contains("checksum mismatch"), "{e}");
        assert!(!bad.exists(), "a mismatching copy is deleted");
        assert!(copy_verified(&from, &bad, "").await.is_err());
    }
}
