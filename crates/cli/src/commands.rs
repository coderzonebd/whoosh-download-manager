//! Every command except `get` and `watch`.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};
use vayu_proto::{
    AddParams, AddResult, Checksum, DownloadDetail, DownloadSummary, Header, IdParams, IdsParams,
    ListParams, PreconnectParams, RemoveParams, RequestContext, Settings, method,
};

use crate::daemon::{self, Connected};
use crate::error::{CliError, Result, exit};
use crate::runtime::{self, Env, Os};
use crate::{DaemonCmd, DownloadOpts, Global, SettingsCmd, format};

/// What every command needs: where the daemon is and how to print.
#[derive(Debug)]
pub struct Ctx {
    pub data_dir: PathBuf,
    pub json: bool,
    pub autostart: bool,
}

impl Ctx {
    pub fn new(global: &Global) -> Result<Self> {
        let data_dir = match &global.data_dir {
            Some(dir) => absolute(dir)?,
            None => runtime::data_dir(Os::current(), &Env::from_process())
                .map_err(CliError::DaemonUnavailable)?,
        };
        Ok(Self {
            data_dir,
            json: global.json,
            autostart: !global.no_start,
        })
    }

    /// Connects to the daemon, starting it if needed and allowed.
    pub async fn connect(&self) -> Result<Connected> {
        daemon::ensure(&self.data_dir, self.autostart).await
    }

    pub fn print_json(&self, v: &impl Serialize) {
        match serde_json::to_string_pretty(v) {
            Ok(text) => println!("{text}"),
            Err(e) => eprintln!("vayu: cannot encode the result: {e}"),
        }
    }
}

fn absolute(p: &Path) -> Result<PathBuf> {
    if p.is_absolute() {
        return Ok(p.to_owned());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(p))
        .map_err(|e| CliError::Other(format!("cannot resolve {}: {e}", p.display())))
}

// ------------------------------------------------------------------ add

/// Builds `download.add` params from the shared download options.
pub fn add_params(url: String, opts: &DownloadOpts, start: bool) -> Result<AddParams> {
    if opts.queue.is_some() {
        return Err(CliError::Unsupported(
            "--queue is not supported yet: protocol v1 has no queue field in download.add".into(),
        ));
    }
    let mut request = RequestContext {
        cookie: opts.cookie.clone(),
        referer: opts.referer.clone(),
        user_agent: opts.user_agent.clone(),
        ..RequestContext::default()
    };
    for raw in &opts.headers {
        let Some((name, value)) = raw.split_once(':') else {
            return Err(CliError::Usage(format!(
                "-H {raw:?}: expected 'Name: value'"
            )));
        };
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err(CliError::Usage(format!("-H {raw:?}: bad header name")));
        }
        // The protocol has dedicated fields for these three.
        let slot = match name.to_ascii_lowercase().as_str() {
            "cookie" => Some(&mut request.cookie),
            "referer" => Some(&mut request.referer),
            "user-agent" => Some(&mut request.user_agent),
            _ => None,
        };
        match slot {
            Some(slot) => {
                if slot.is_none() {
                    *slot = Some(value.to_owned());
                }
            }
            None => request.headers.push(Header {
                name: name.to_owned(),
                value: value.to_owned(),
            }),
        }
    }
    let checksum = opts.checksum.as_deref().map(parse_checksum).transpose()?;
    let directory = opts
        .dir
        .as_deref()
        .map(|d| absolute(d).map(|d| d.to_string_lossy().into_owned()))
        .transpose()?;
    Ok(AddParams {
        url,
        request: (request != RequestContext::default()).then_some(request),
        filename: opts.name.clone(),
        directory,
        category: opts.category.clone(),
        start,
        mirrors: opts.mirrors.clone(),
        checksum,
    })
}

fn parse_checksum(raw: &str) -> Result<Checksum> {
    let usage = || {
        CliError::Usage(format!(
            "--checksum {raw:?}: expected ALG:HEX with ALG one of md5, sha1, sha256, sha512"
        ))
    };
    let (alg, hex) = raw.split_once(':').ok_or_else(usage)?;
    let alg = alg.trim().to_ascii_lowercase();
    let hex = hex.trim().to_ascii_lowercase();
    let len = match alg.as_str() {
        "md5" => 32,
        "sha1" => 40,
        "sha256" => 64,
        "sha512" => 128,
        _ => return Err(usage()),
    };
    if hex.len() != len || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(CliError::Usage(format!(
            "--checksum: a {alg} checksum is {len} hex digits"
        )));
    }
    Ok(Checksum {
        algorithm: alg,
        value: hex,
    })
}

pub async fn add(ctx: &Ctx, urls: Vec<String>, opts: &DownloadOpts, paused: bool) -> Result<u8> {
    if urls.len() > 1 && opts.name.is_some() {
        return Err(CliError::Usage(
            "-n/--name only makes sense with a single URL".into(),
        ));
    }
    let params: Vec<AddParams> = urls
        .into_iter()
        .map(|url| add_params(url, opts, !paused))
        .collect::<Result<_>>()?;
    let Connected { client, .. } = ctx.connect().await?;
    let mut results = Vec::new();
    let mut first_error: Option<CliError> = None;
    for p in params {
        let url = p.url.clone();
        match client.call::<_, AddResult>(method::DOWNLOAD_ADD, &p).await {
            Ok(added) => {
                if !ctx.json {
                    println!("{}", added.id);
                }
                results.push(json!({ "url": url, "id": added.id }));
            }
            Err(e) => {
                if !ctx.json {
                    eprintln!("vayu: {url}: {e}");
                }
                results.push(json!({ "url": url, "error": e.to_json()["error"] }));
                first_error.get_or_insert(e);
            }
        }
    }
    if ctx.json {
        ctx.print_json(&results);
    }
    Ok(first_error.map_or(exit::OK, |e| e.exit_code()))
}

// ------------------------------------------------------------------ list / info

pub async fn list(ctx: &Ctx, params: ListParams) -> Result<u8> {
    let Connected { client, .. } = ctx.connect().await?;
    let rows: Vec<DownloadSummary> = client.call(method::DOWNLOAD_LIST, &params).await?;
    if ctx.json {
        ctx.print_json(&rows);
    } else if rows.is_empty() {
        eprintln!("no downloads");
    } else {
        let width = if std::io::stdout().is_terminal() {
            format::terminal_width()
        } else {
            usize::MAX / 4
        };
        print!("{}", format::table(&rows, width));
    }
    Ok(exit::OK)
}

pub async fn info(ctx: &Ctx, id: String) -> Result<u8> {
    let Connected { client, .. } = ctx.connect().await?;
    let d: DownloadDetail = client.call(method::DOWNLOAD_GET, IdParams { id }).await?;
    if ctx.json {
        ctx.print_json(&d);
        return Ok(exit::OK);
    }
    let s = &d.summary;
    let now = now_ms();
    let ago = |t: i64| format!("{} ago", format::duration(((now - t).max(0) / 1000) as u64));
    let mut lines: Vec<(&str, String)> = vec![
        ("id", s.id.clone()),
        ("url", s.url.clone()),
        ("file", format::path_of(s)),
        ("state", format::state(s.state).to_owned()),
        (
            "progress",
            match s.total_bytes {
                Some(t) => format!(
                    "{} ({} of {})",
                    format::percent(s.downloaded_bytes, Some(t)),
                    format::bytes(s.downloaded_bytes),
                    format::bytes(t)
                ),
                None => format!("{} (size unknown)", format::bytes(s.downloaded_bytes)),
            },
        ),
    ];
    if s.state == vayu_proto::DownloadState::Downloading {
        let eta = s
            .eta_secs
            .map(|e| format!(", ETA {}", format::duration(e)))
            .unwrap_or_default();
        lines.push((
            "speed",
            format!(
                "{}{eta}, {} connections",
                format::speed(s.speed_bps),
                s.connections
            ),
        ));
    }
    lines.push(("category", s.category.clone().unwrap_or_else(|| "-".into())));
    lines.push(("queue", s.queue.clone().unwrap_or_else(|| "main".into())));
    lines.push(("resumable", if s.resumable { "yes" } else { "no" }.into()));
    lines.push(("added", ago(s.added_at)));
    if let Some(t) = s.completed_at {
        lines.push(("completed", ago(t)));
    }
    if let Some(e) = &s.error {
        lines.push(("error", format!("{}: {}", e.code, e.message)));
    }
    for (k, v) in lines {
        println!("{k:<10} {v}");
    }
    if !d.segments.is_empty() {
        println!("segments:");
        println!(
            "  {:>14}  {:>14}  {:>10}  {:>12}  SOURCE",
            "START", "END", "DONE", "SPEED"
        );
        for g in &d.segments {
            println!(
                "  {:>14}  {:>14}  {:>10}  {:>12}  {}",
                g.start,
                g.end,
                format::bytes(g.downloaded_bytes),
                format::speed(g.speed_bps),
                g.source
            );
        }
    }
    Ok(exit::OK)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ------------------------------------------------------------------ pause / resume / rm

pub async fn ids_call(ctx: &Ctx, method: &str, ids: Vec<String>) -> Result<u8> {
    let Connected { client, .. } = ctx.connect().await?;
    client
        .call_value(
            method,
            serde_json::to_value(IdsParams { ids: ids.clone() }).unwrap_or_default(),
        )
        .await?;
    if ctx.json {
        ctx.print_json(&json!({ "ok": true, "ids": ids }));
    }
    Ok(exit::OK)
}

pub async fn remove(ctx: &Ctx, ids: Vec<String>, delete_files: bool) -> Result<u8> {
    let Connected { client, .. } = ctx.connect().await?;
    let params = RemoveParams {
        ids: ids.clone(),
        delete_files,
    };
    client
        .call_value(
            method::DOWNLOAD_REMOVE,
            serde_json::to_value(params).unwrap_or_default(),
        )
        .await?;
    if ctx.json {
        ctx.print_json(&json!({ "ok": true, "ids": ids }));
    }
    Ok(exit::OK)
}

pub async fn preconnect(ctx: &Ctx, url: String) -> Result<u8> {
    let Connected { client, .. } = ctx.connect().await?;
    client
        .call_value(
            method::NET_PRECONNECT,
            serde_json::to_value(PreconnectParams { url }).unwrap_or_default(),
        )
        .await?;
    if ctx.json {
        ctx.print_json(&json!({ "ok": true }));
    }
    Ok(exit::OK)
}

// ------------------------------------------------------------------ settings

pub async fn settings(ctx: &Ctx, action: Option<SettingsCmd>) -> Result<u8> {
    let pairs = match action {
        None | Some(SettingsCmd::Get) => Vec::new(),
        Some(SettingsCmd::Set { pairs }) => pairs,
    };
    // Validate before connecting, so typos fail fast with exit code 2.
    let edits: Vec<(Key, String)> = pairs.iter().map(|p| parse_pair(p)).collect::<Result<_>>()?;
    let Connected { client, .. } = ctx.connect().await?;
    let mut settings: Settings = client.call(method::SETTINGS_GET, Value::Null).await?;
    if !edits.is_empty() {
        for (key, value) in edits {
            apply(&mut settings, key, &value)?;
        }
        settings = client.call(method::SETTINGS_SET, &settings).await?;
    }
    if ctx.json {
        ctx.print_json(&settings);
    } else {
        let speed = settings
            .speed_limit_bps
            .map(|b| format!("{b} ({})", format::speed(b)))
            .unwrap_or_else(|| "unlimited".into());
        for (k, v) in [
            ("defaultDirectory", settings.default_directory.clone()),
            (
                "maxActiveDownloads",
                settings.max_active_downloads.to_string(),
            ),
            (
                "maxConnectionsPerDownload",
                settings.max_connections_per_download.to_string(),
            ),
            ("speedLimitBps", speed),
            ("language", settings.language.clone()),
            ("theme", settings.theme.clone()),
        ] {
            println!("{k:<26} {v}");
        }
    }
    Ok(exit::OK)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    DefaultDirectory,
    MaxActiveDownloads,
    MaxConnectionsPerDownload,
    SpeedLimitBps,
    Language,
    Theme,
}

fn parse_pair(pair: &str) -> Result<(Key, String)> {
    let (key, value) = pair
        .split_once('=')
        .ok_or_else(|| CliError::Usage(format!("{pair:?}: expected key=value")))?;
    // camelCase, snake_case and kebab-case all work.
    let norm: String = key
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .collect::<String>()
        .to_ascii_lowercase();
    let key = match norm.as_str() {
        "defaultdirectory" | "directory" | "dir" => Key::DefaultDirectory,
        "maxactivedownloads" | "maxactive" => Key::MaxActiveDownloads,
        "maxconnectionsperdownload" | "maxconnections" | "connections" => {
            Key::MaxConnectionsPerDownload
        }
        "speedlimitbps" | "speedlimit" => Key::SpeedLimitBps,
        "language" | "lang" => Key::Language,
        "theme" => Key::Theme,
        _ => {
            return Err(CliError::Usage(format!(
                "unknown setting {key:?} (defaultDirectory, maxActiveDownloads, \
                 maxConnectionsPerDownload, speedLimitBps, language, theme)"
            )));
        }
    };
    Ok((key, value.trim().to_owned()))
}

fn apply(s: &mut Settings, key: Key, value: &str) -> Result<()> {
    let number = |v: &str| {
        v.parse::<u32>()
            .map_err(|_| CliError::Usage(format!("{key:?}: {v:?} is not a whole number")))
    };
    match key {
        Key::DefaultDirectory => {
            s.default_directory = absolute(Path::new(value))?.to_string_lossy().into_owned();
        }
        Key::MaxActiveDownloads => s.max_active_downloads = number(value)?,
        Key::MaxConnectionsPerDownload => s.max_connections_per_download = number(value)?,
        Key::SpeedLimitBps => {
            s.speed_limit_bps = match value.to_ascii_lowercase().as_str() {
                "" | "none" | "unlimited" | "off" | "0" => None,
                v => Some(parse_size(v).ok_or_else(|| {
                    CliError::Usage(format!(
                        "speedLimitBps: {value:?} is not a byte rate (e.g. 1048576, 10MiB, none)"
                    ))
                })?),
            }
        }
        Key::Language => s.language = value.to_owned(),
        Key::Theme => s.theme = value.to_owned(),
    }
    Ok(())
}

/// `10MiB`, `512k`, `1.5G` or plain bytes (binary multiples).
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (n, unit) = s.split_at(split);
    let n: f64 = n.parse().ok()?;
    let mult: u64 = match unit.trim().to_ascii_lowercase().trim_end_matches("/s") {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        _ => return None,
    };
    let v = n * mult as f64;
    (v.is_finite() && v >= 1.0 && v < u64::MAX as f64).then_some(v as u64)
}

// ------------------------------------------------------------------ daemon

pub async fn daemon(ctx: &Ctx, action: DaemonCmd) -> Result<u8> {
    match action {
        DaemonCmd::Status => match daemon::connect_running(&ctx.data_dir).await? {
            Some(Connected { client, info }) => {
                let hello = client.hello.clone();
                client.close().await;
                if ctx.json {
                    ctx.print_json(&json!({
                        "running": true,
                        "pid": info.pid,
                        "port": info.port,
                        "version": hello.daemon_version,
                        "protocol": hello.protocol,
                        "capabilities": hello.capabilities,
                        "dataDir": ctx.data_dir,
                    }));
                } else {
                    println!(
                        "vayu-daemon {} is running (pid {}, port {})",
                        hello.daemon_version,
                        info.pid.map_or("?".into(), |p| p.to_string()),
                        info.port
                    );
                    println!("data dir:     {}", ctx.data_dir.display());
                    println!("capabilities: {}", hello.capabilities.join(", "));
                }
                Ok(exit::OK)
            }
            None => {
                if ctx.json {
                    ctx.print_json(&json!({ "running": false, "dataDir": ctx.data_dir }));
                } else {
                    println!("vayu-daemon is not running");
                }
                Ok(exit::DAEMON_UNAVAILABLE)
            }
        },
        DaemonCmd::Stop => match daemon::connect_running(&ctx.data_dir).await? {
            Some(connected) => {
                let pid = connected.info.pid;
                daemon::stop(connected, &ctx.data_dir).await?;
                if ctx.json {
                    ctx.print_json(&json!({ "stopped": true, "pid": pid }));
                } else {
                    println!("vayu-daemon stopped");
                }
                Ok(exit::OK)
            }
            None => {
                if ctx.json {
                    ctx.print_json(&json!({ "stopped": false, "running": false }));
                } else {
                    println!("vayu-daemon is not running");
                }
                Ok(exit::OK)
            }
        },
        DaemonCmd::Start { foreground, args } => {
            if let Some(Connected { client, info }) = daemon::connect_running(&ctx.data_dir).await?
            {
                client.close().await;
                if ctx.json {
                    ctx.print_json(&json!({ "started": false, "running": true, "pid": info.pid, "port": info.port }));
                } else {
                    println!(
                        "vayu-daemon is already running (pid {}, port {})",
                        info.pid.map_or("?".into(), |p| p.to_string()),
                        info.port
                    );
                }
                return Ok(exit::OK);
            }
            if foreground {
                return run_foreground(&ctx.data_dir, &args);
            }
            let child = daemon::spawn_detached(&ctx.data_dir, &args)?;
            let Connected { client, info } = daemon::wait_until_up(&ctx.data_dir, child).await?;
            client.close().await;
            if ctx.json {
                ctx.print_json(&json!({ "started": true, "running": true, "pid": info.pid, "port": info.port }));
            } else {
                println!(
                    "vayu-daemon started (pid {}, port {})",
                    info.pid.map_or("?".into(), |p| p.to_string()),
                    info.port
                );
            }
            Ok(exit::OK)
        }
    }
}

/// Runs the daemon in this terminal: replaces this process on Unix, waits
/// for it elsewhere.
fn run_foreground(data_dir: &Path, args: &[String]) -> Result<u8> {
    let exe = daemon::daemon_exe();
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("--data-dir").arg(data_dir).args(args);
    let fail = |e: std::io::Error| {
        CliError::DaemonUnavailable(format!("cannot run {}: {e}", exe.display()))
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(fail(cmd.exec()))
    }
    #[cfg(not(unix))]
    {
        let status = cmd.status().map_err(fail)?;
        Ok(status
            .code()
            .and_then(|c| u8::try_from(c).ok())
            .unwrap_or(exit::OTHER))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_params_from_options() {
        let opts = DownloadOpts {
            dir: Some("rel/dir".into()),
            headers: vec![
                "X-Token: abc".into(),
                "Cookie: from-header".into(),
                "referer:https://r/".into(),
            ],
            cookie: Some("from-flag".into()),
            checksum: Some("SHA256:".to_owned() + &"AB".repeat(32)),
            ..DownloadOpts::default()
        };
        let p = add_params("https://e/x".into(), &opts, false).unwrap();
        assert!(!p.start);
        assert!(Path::new(p.directory.as_deref().unwrap()).is_absolute());
        let r = p.request.unwrap();
        assert_eq!(r.cookie.as_deref(), Some("from-flag"), "the flag wins");
        assert_eq!(r.referer.as_deref(), Some("https://r/"));
        assert_eq!(
            r.headers,
            [Header {
                name: "X-Token".into(),
                value: "abc".into()
            }]
        );
        let c = p.checksum.unwrap();
        assert_eq!(c.algorithm, "sha256");
        assert_eq!(c.value, "ab".repeat(32));

        let bare = add_params("https://e/x".into(), &DownloadOpts::default(), true).unwrap();
        assert_eq!(bare.request, None);

        let usage = |o: DownloadOpts| add_params("u".into(), &o, true).unwrap_err().exit_code();
        assert_eq!(
            usage(DownloadOpts {
                headers: vec!["nocolon".into()],
                ..Default::default()
            }),
            2
        );
        assert_eq!(
            usage(DownloadOpts {
                checksum: Some("sha256:12".into()),
                ..Default::default()
            }),
            2
        );
        assert_eq!(
            usage(DownloadOpts {
                checksum: Some("crc32:12345678".into()),
                ..Default::default()
            }),
            2
        );
        assert_eq!(
            usage(DownloadOpts {
                queue: Some("night".into()),
                ..Default::default()
            }),
            10
        );
    }

    #[test]
    fn settings_edits() {
        let mut s = Settings {
            default_directory: "/d".into(),
            max_active_downloads: 3,
            max_connections_per_download: 8,
            speed_limit_bps: None,
            language: "en".into(),
            theme: "system".into(),
        };
        for pair in [
            "max_active_downloads=5",
            "maxConnectionsPerDownload=16",
            "speed-limit=10MiB",
            "theme=dark",
        ] {
            let (k, v) = parse_pair(pair).unwrap();
            apply(&mut s, k, &v).unwrap();
        }
        assert_eq!(s.max_active_downloads, 5);
        assert_eq!(s.max_connections_per_download, 16);
        assert_eq!(s.speed_limit_bps, Some(10 << 20));
        assert_eq!(s.theme, "dark");
        let (k, v) = parse_pair("speedLimitBps=none").unwrap();
        apply(&mut s, k, &v).unwrap();
        assert_eq!(s.speed_limit_bps, None);
        assert!(parse_pair("colour=red").is_err());
        assert!(parse_pair("theme").is_err());
        let (k, v) = parse_pair("maxActiveDownloads=lots").unwrap();
        assert!(apply(&mut s, k, &v).is_err());
        assert_eq!(parse_size("1.5k"), Some(1536));
        assert_eq!(parse_size("2MiB/s"), Some(2 << 20));
        assert_eq!(parse_size("x"), None);
    }
}
