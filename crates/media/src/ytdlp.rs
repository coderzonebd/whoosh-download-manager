//! yt-dlp's command lines. Every run starts with [`base_args`]: no config
//! files (a user's config could add `--exec` or browser cookies), no
//! playlists, our own cache directory. The URL always comes after `--`, so
//! it can never be read as an option.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use whoosh_proto::RequestContext;

use crate::process::{self, Tool};
use crate::util::{random_hex, with_suffix};

/// Directories yt-dlp may write to.
#[derive(Debug, Clone)]
pub struct Dirs {
    pub cache: PathBuf,
    /// Short-lived files (cookie files).
    pub tmp: PathBuf,
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_JSON: usize = 64 << 20;
pub const PROGRESS_MARK: &str = "WHOOSHP";
pub const FILE_MARK: &str = "WHOOSHF";

fn os(s: impl Into<OsString>) -> OsString {
    s.into()
}

pub fn base_args(dirs: &Dirs) -> Vec<OsString> {
    vec![
        os("--ignore-config"),
        os("--no-playlist"),
        os("--no-color"),
        os("--cache-dir"),
        dirs.cache.as_os_str().to_owned(),
    ]
}

/// yt-dlp's output templates treat `%` specially; a directory or name
/// containing one must not change the template.
pub fn template_escape(s: &str) -> String {
    s.replace('%', "%%")
}

/// `<dir>/<stem>.%(ext)s`, escaped.
pub fn output_template(dir: &Path, stem: &str) -> OsString {
    let dir = template_escape(&dir.to_string_lossy());
    let mut p = PathBuf::from(dir);
    p.push(format!("{}.%(ext)s", template_escape(stem)));
    p.into_os_string()
}

/// A cookie file holding only the cookie the extension handed over for
/// this page's host, deleted when dropped.
#[derive(Debug)]
pub struct CookieFile(PathBuf);

impl CookieFile {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for CookieFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Netscape cookie file for `url`'s host from a `Cookie` header value.
pub fn netscape_cookies(url: &str, cookie: &str) -> Option<String> {
    let host = url::Url::parse(url).ok()?.host_str()?.to_owned();
    if host.contains(['\t', '\n', '\r']) {
        return None;
    }
    let is_ip = host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[');
    let (domain, subdomains) = if is_ip {
        (host.clone(), "FALSE")
    } else {
        (format!(".{host}"), "TRUE")
    };
    let expires = crate::util::now_ms() / 1000 + 24 * 3600;
    let mut out = String::from("# Netscape HTTP Cookie File\n");
    for pair in cookie.split(';') {
        let Some((name, value)) = pair.trim().split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty()
            || name.contains(['\t', '\n', '\r'])
            || value.contains(['\t', '\n', '\r'])
        {
            continue;
        }
        out.push_str(&format!(
            "{domain}\t{subdomains}\t/\tFALSE\t{expires}\t{name}\t{}\n",
            value.trim()
        ));
    }
    Some(out)
}

/// `--user-agent`, `--referer` and `--cookies` from what the browser sent.
/// The cookie is only ever the one the extension captured for this URL.
pub async fn request_args(
    dirs: &Dirs,
    url: &str,
    request: Option<&RequestContext>,
) -> std::io::Result<(Vec<OsString>, Option<CookieFile>)> {
    let mut args = Vec::new();
    let Some(req) = request else {
        return Ok((args, None));
    };
    let clean = |s: &str| !s.is_empty() && !s.contains(['\r', '\n']);
    if let Some(ua) = req.user_agent.as_deref().filter(|s| clean(s)) {
        args.extend([os("--user-agent"), os(ua)]);
    }
    if let Some(r) = req
        .referer
        .as_deref()
        .filter(|s| clean(s) && (s.starts_with("https://") || s.starts_with("http://")))
    {
        args.extend([os("--referer"), os(r)]);
    }
    let mut file = None;
    if let Some(text) = req.cookie.as_deref().and_then(|c| netscape_cookies(url, c)) {
        tokio::fs::create_dir_all(&dirs.tmp).await?;
        let path = dirs.tmp.join(format!("cookies-{}.txt", random_hex(8)));
        write_private(&path, text.as_bytes()).await?;
        args.extend([os("--cookies"), path.as_os_str().to_owned()]);
        file = Some(CookieFile(path));
    }
    Ok((args, file))
}

async fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let path = path.to_owned();
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o.open(&path)?;
        f.write_all(&bytes)
    })
    .await
    .map_err(std::io::Error::other)?
}

fn parse_json(out: &process::Output) -> Result<Value, String> {
    if !out.success {
        return Err(out.error_line());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // -J prints one JSON document; take the last non-empty line in case
    // anything else slipped onto stdout.
    let line = text
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| "yt-dlp printed no information".to_owned())?;
    serde_json::from_str(line).map_err(|e| format!("yt-dlp printed unreadable JSON: {e}"))
}

/// `yt-dlp -J <url>`: everything about the page.
pub async fn probe(
    tool: &Tool,
    dirs: &Dirs,
    url: &str,
    request: Option<&RequestContext>,
) -> Result<Value, String> {
    let (req_args, _cookies) = request_args(dirs, url, request)
        .await
        .map_err(|e| e.to_string())?;
    let mut args = base_args(dirs);
    args.push(os("-J"));
    args.extend(req_args);
    args.extend([os("--"), os(url)]);
    let out = process::run(tool, args, PROBE_TIMEOUT, MAX_JSON)
        .await
        .map_err(|e| format!("yt-dlp: {e}"))?;
    parse_json(&out)
}

/// Picks formats with `selector` from a saved info JSON (no network for
/// most sites).
pub async fn resolve(
    tool: &Tool,
    dirs: &Dirs,
    info: &Path,
    selector: &str,
) -> Result<Value, String> {
    let mut args = base_args(dirs);
    args.extend([
        os("--load-info-json"),
        info.as_os_str().to_owned(),
        os("-f"),
        os(selector),
        os("-J"),
    ]);
    let out = process::run(tool, args, PROBE_TIMEOUT, MAX_JSON)
        .await
        .map_err(|e| format!("yt-dlp: {e}"))?;
    parse_json(&out)
}

/// Saves the subtitles in `langs` as `<dir>/<stem>.<lang>.<ext>`.
pub async fn subtitles(
    tool: &Tool,
    dirs: &Dirs,
    info: &Path,
    langs: &[String],
    dir: &Path,
    stem: &str,
) -> Result<(), String> {
    let mut args = base_args(dirs);
    args.extend([
        os("--load-info-json"),
        info.as_os_str().to_owned(),
        os("--skip-download"),
        os("--write-subs"),
        os("--write-auto-subs"),
        os("--sub-langs"),
        os(langs.join(",")),
        os("-o"),
        output_template(dir, stem),
    ]);
    let out = process::run(tool, args, PROBE_TIMEOUT, 1 << 20)
        .await
        .map_err(|e| format!("yt-dlp: {e}"))?;
    if out.success {
        Ok(())
    } else {
        Err(out.error_line())
    }
}

/// What yt-dlp reports while it downloads by itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FallbackProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
    pub speed: u64,
}

/// How a yt-dlp download ended.
#[derive(Debug, PartialEq, Eq)]
pub enum FallbackEnd {
    Done(PathBuf),
    /// Stopped by us (pause, shutdown, removal).
    Cancelled,
    Failed(String),
}

/// Everything a yt-dlp download needs.
#[derive(Debug)]
pub struct FallbackRun<'a> {
    pub info: &'a Path,
    pub selector: &'a str,
    pub dir: &'a Path,
    pub stem: &'a str,
    pub ffmpeg: Option<&'a Path>,
    pub subtitles: &'a [String],
    pub mp3: bool,
    /// The audio stream of a combined format, kept as it is.
    pub extract_audio: bool,
    pub limit_bps: Option<u64>,
}

pub fn fallback_args(dirs: &Dirs, run: &FallbackRun<'_>) -> Vec<OsString> {
    let mut args = base_args(dirs);
    args.extend([
        os("--load-info-json"),
        run.info.as_os_str().to_owned(),
        os("-f"),
        os(run.selector),
        os("-o"),
        output_template(run.dir, run.stem),
        os("--newline"),
        os("--progress"),
        os("--progress-template"),
        os(format!(
            "download:{PROGRESS_MARK} %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s %(progress.speed)s"
        )),
        os("--print"),
        os(format!("after_move:{FILE_MARK} %(filepath)s")),
    ]);
    if let Some(ffmpeg) = run.ffmpeg {
        args.extend([os("--ffmpeg-location"), ffmpeg.as_os_str().to_owned()]);
    }
    if !run.subtitles.is_empty() {
        args.extend([
            os("--write-subs"),
            os("--write-auto-subs"),
            os("--sub-langs"),
            os(run.subtitles.join(",")),
        ]);
    }
    if run.mp3 {
        args.extend([os("-x"), os("--audio-format"), os("mp3")]);
    } else if run.extract_audio {
        args.push(os("-x"));
    }
    if let Some(bps) = run.limit_bps.filter(|b| *b > 0) {
        args.extend([os("--limit-rate"), os(bps.to_string())]);
    }
    args
}

/// Parses `WHOOSHP <downloaded> <total> <estimate> <speed>` (yt-dlp prints
/// `NA` for unknown fields and floats for some).
pub fn parse_progress(line: &str) -> Option<(u64, Option<u64>, u64)> {
    let rest = line.trim().strip_prefix(PROGRESS_MARK)?;
    let mut f = rest.split_whitespace().map(|v| {
        v.parse::<f64>()
            .ok()
            .filter(|x| x.is_finite() && *x >= 0.0)
            .map(|x| x as u64)
    });
    let downloaded = f.next()??;
    let total = f.next().flatten();
    let estimate = f.next().flatten();
    let speed = f.next().flatten().unwrap_or(0);
    Some((downloaded, total.or(estimate), speed))
}

/// Runs a yt-dlp download until it ends or `cancel` fires (dropped or
/// sent), calling `progress` for every progress line.
pub async fn download(
    tool: &Tool,
    dirs: &Dirs,
    run: &FallbackRun<'_>,
    request: Option<(&str, &RequestContext)>,
    cancel: tokio::sync::oneshot::Receiver<()>,
    mut progress: impl FnMut(FallbackProgress) + Send,
) -> FallbackEnd {
    let (req_args, _cookies) = match request {
        Some((url, req)) => match request_args(dirs, url, Some(req)).await {
            Ok(r) => r,
            Err(e) => return FallbackEnd::Failed(e.to_string()),
        },
        None => (Vec::new(), None),
    };
    let mut args = fallback_args(dirs, run);
    args.extend(req_args);
    let mut cmd = tool.command(args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return FallbackEnd::Failed(format!("yt-dlp: {e}")),
    };
    let mut out = BufReader::new(child.stdout.take().expect("piped")).lines();
    let mut err = BufReader::new(child.stderr.take().expect("piped")).lines();
    let mut cancel = cancel;
    let mut last_error = String::new();
    let mut file: Option<PathBuf> = None;
    // yt-dlp downloads the formats of a merge one after the other; each
    // starts again from zero.
    let (mut base, mut current) = (0u64, FallbackProgress::default());
    let (mut out_open, mut err_open) = (true, true);
    let mut handle = |line: &str, last_error: &mut String, file: &mut Option<PathBuf>| {
        let line = line.trim_end();
        if let Some((d, t, s)) = parse_progress(line) {
            if d + (1 << 20) < current.downloaded {
                base += current.total.unwrap_or(current.downloaded);
            }
            current = FallbackProgress {
                downloaded: d,
                total: t,
                speed: s,
            };
            progress(FallbackProgress {
                downloaded: base + d,
                total: t.map(|t| base + t),
                speed: s,
            });
        } else if let Some(p) = line.strip_prefix(FILE_MARK) {
            let p = p.trim();
            if !p.is_empty() {
                *file = Some(PathBuf::from(p));
            }
        } else if line.starts_with("ERROR:") {
            *last_error = process::error_line(line);
        }
    };
    while out_open || err_open {
        tokio::select! {
            _ = &mut cancel => {
                let _ = child.kill().await;
                return FallbackEnd::Cancelled;
            }
            line = out.next_line(), if out_open => match line {
                Ok(Some(l)) => handle(&l, &mut last_error, &mut file),
                _ => out_open = false,
            },
            line = err.next_line(), if err_open => match line {
                Ok(Some(l)) => handle(&l, &mut last_error, &mut file),
                _ => err_open = false,
            },
        }
    }
    let status = tokio::select! {
        _ = &mut cancel => {
            let _ = child.kill().await;
            return FallbackEnd::Cancelled;
        }
        s = child.wait() => s,
    };
    match status {
        Ok(s) if s.success() => match file {
            Some(f) => FallbackEnd::Done(f),
            None => FallbackEnd::Failed("yt-dlp did not say where it saved the file".into()),
        },
        Ok(_) => FallbackEnd::Failed(if last_error.is_empty() {
            "yt-dlp failed".into()
        } else {
            last_error
        }),
        Err(e) => FallbackEnd::Failed(format!("yt-dlp: {e}")),
    }
}

/// Partial files yt-dlp leaves for `stem` in `dir` (`.part`, `.ytdl`,
/// fragments), for cleaning up a removed download.
pub fn partial_files(dir: &Path, stem: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let prefix = format!("{stem}.");
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();
            name.starts_with(&prefix)
                && (name.ends_with(".part")
                    || name.ends_with(".ytdl")
                    || name.contains(".part-Frag")
                    || name.ends_with(".temp"))
        })
        .collect()
}

/// Subtitle files written for `stem` (`<stem>.<lang>.<ext>`).
pub fn subtitle_files(dir: &Path, stem: &str, langs: &[String]) -> Vec<PathBuf> {
    const EXTS: [&str; 9] = [
        "vtt", "srt", "ass", "ssa", "ttml", "srv1", "srv2", "srv3", "json3",
    ];
    let mut out = Vec::new();
    for lang in langs {
        for ext in EXTS {
            let p = dir.join(format!("{stem}.{lang}.{ext}"));
            if p.is_file() {
                out.push(p);
            }
        }
    }
    out
}

/// Path of a temporary output next to `final_path`.
pub fn temp_output(final_path: &Path) -> PathBuf {
    with_suffix(final_path, ".whoosh-tmp")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines_parse() {
        assert_eq!(
            parse_progress("WHOOSHP 100 1000 NA 50.5"),
            Some((100, Some(1000), 50))
        );
        assert_eq!(
            parse_progress("WHOOSHP 100 NA 2000.0 NA"),
            Some((100, Some(2000), 0))
        );
        assert_eq!(parse_progress("WHOOSHP NA NA NA NA"), None);
        assert_eq!(parse_progress("[download] 5%"), None);
    }

    #[test]
    fn templates_escape_percent() {
        let t = output_template(Path::new("/tmp/100%"), "a %(title)s");
        let t = t.to_string_lossy();
        assert!(t.ends_with("a %%(title)s.%(ext)s"), "{t}");
        assert!(t.contains("100%%"), "{t}");
    }

    #[test]
    fn cookie_files_are_scoped_to_the_host() {
        let c = netscape_cookies("https://www.example.com/watch?v=1", "a=1; b = 2;bad").unwrap();
        let lines: Vec<_> = c.lines().skip(1).collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with(".www.example.com\tTRUE\t/\tFALSE\t"));
        assert!(lines[0].ends_with("\ta\t1"));
        assert!(lines[1].ends_with("\tb\t2"));
        let ip = netscape_cookies("http://127.0.0.1:8080/x", "a=1").unwrap();
        assert!(ip.lines().nth(1).unwrap().starts_with("127.0.0.1\tFALSE"));
        assert!(netscape_cookies("not a url", "a=1").is_none());
    }

    #[test]
    fn fallback_args_put_options_before_nothing_unsafe() {
        let dirs = Dirs {
            cache: "/c".into(),
            tmp: "/t".into(),
        };
        let run = FallbackRun {
            info: Path::new("/j/i.json"),
            selector: "hls-720",
            dir: Path::new("/d"),
            stem: "x",
            ffmpeg: None,
            subtitles: &["en".into(), "bn".into()],
            mp3: false,
            extract_audio: false,
            limit_bps: Some(1000),
        };
        let a: Vec<String> = fallback_args(&dirs, &run)
            .into_iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&a[..2], ["--ignore-config", "--no-playlist"]);
        assert!(a.windows(2).any(|w| w == ["--sub-langs", "en,bn"]));
        assert!(a.windows(2).any(|w| w == ["--limit-rate", "1000"]));
        assert!(!a.iter().any(|s| s.contains("cookies-from-browser")));
    }
}
