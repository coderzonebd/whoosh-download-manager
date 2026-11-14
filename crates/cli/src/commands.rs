//! Every command except `get` and `watch`.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};
use whoosh_proto::{
    AddParams, AddResult, Checksum, DownloadDetail, DownloadSummary, Durability, Header, IdParams,
    IdsParams, ListParams, PreconnectParams, RemoveParams, RequestContext, Settings, method,
};

use crate::daemon::{self, Connected, Found};
use crate::error::{CliError, Result, exit};
use crate::runtime;
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
            None => runtime::default_data_dir()
                .map_err(|e| CliError::DaemonUnavailable(e.to_string()))?,
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
            Err(e) => eprintln!("whoosh: cannot encode the result: {e}"),
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

/// The request context from the download options: the flags, then the
/// files (each read once, so `-` can be stdin even for several URLs). The
/// dedicated flags (`--cookie`, `--cookie-file`, `--referer`,
/// `--user-agent`) win over the same header given with `-H`.
pub fn request_context(opts: &DownloadOpts) -> Result<Option<RequestContext>> {
    let stdin = Path::new("-");
    if opts.cookie_file.as_deref() == Some(stdin) && opts.headers_file.as_deref() == Some(stdin) {
        return Err(CliError::Usage(
            "--cookie-file and --headers-file cannot both read stdin".into(),
        ));
    }
    let mut request = RequestContext {
        cookie: opts.cookie.clone(),
        referer: opts.referer.clone(),
        user_agent: opts.user_agent.clone(),
        ..RequestContext::default()
    };
    if let Some(path) = &opts.cookie_file {
        let text = read_source(path, "--cookie-file")?;
        let value = text.trim();
        if value.is_empty() || value.contains(['\r', '\n', '\0']) {
            return Err(CliError::Usage(format!(
                "--cookie-file {}: expected one Cookie header value on one line",
                path.display()
            )));
        }
        request.cookie = Some(value.to_owned());
    }
    for raw in &opts.headers {
        add_header(&mut request, "-H", raw)?;
    }
    if let Some(path) = &opts.headers_file {
        let text = read_source(path, "--headers-file")?;
        for line in text.lines().map(str::trim) {
            if !line.is_empty() && !line.starts_with('#') {
                add_header(&mut request, "--headers-file", line)?;
            }
        }
    }
    Ok((request != RequestContext::default()).then_some(request))
}

/// The URLs of `whoosh add`: the arguments, then `-i FILE`'s lines, with
/// blank entries and `#` comments skipped (a list pasted with empty lines
/// between links must not add empty downloads).
pub fn url_list(
    args: Vec<String>,
    input_file: Option<&Path>,
    opts: &DownloadOpts,
) -> Result<Vec<String>> {
    let stdin = Path::new("-");
    if input_file == Some(stdin)
        && (opts.cookie_file.as_deref() == Some(stdin)
            || opts.headers_file.as_deref() == Some(stdin))
    {
        return Err(CliError::Usage(
            "-i/--input-file cannot read stdin together with --cookie-file or --headers-file"
                .into(),
        ));
    }
    let mut urls: Vec<String> = args
        .into_iter()
        .map(|u| u.trim().to_owned())
        .filter(|u| !u.is_empty())
        .collect();
    if let Some(path) = input_file {
        let text = read_source(path, "-i/--input-file")?;
        urls.extend(
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_owned),
        );
    }
    if urls.is_empty() {
        return Err(CliError::Usage("no URLs to add".into()));
    }
    Ok(urls)
}

/// A file's contents, or stdin's for `-`.
fn read_source(path: &Path, flag: &str) -> Result<String> {
    let read = if path == Path::new("-") {
        std::io::read_to_string(std::io::stdin())
    } else {
        std::fs::read_to_string(path)
    };
    read.map_err(|e| CliError::Usage(format!("{flag} {}: {e}", path.display())))
}

fn add_header(request: &mut RequestContext, flag: &str, raw: &str) -> Result<()> {
    // Never echo a header value: it may be a secret.
    let shown = raw.split(':').next().unwrap_or_default();
    let Some((name, value)) = raw.split_once(':') else {
        return Err(CliError::Usage(format!(
            "{flag} {shown:?}: expected 'Name: value'"
        )));
    };
    let (name, value) = (name.trim(), value.trim());
    let token_char = |c: char| c.is_ascii_graphic() && !"()<>@,;:\\\"/[]?={}".contains(c);
    if name.is_empty() || !name.chars().all(token_char) {
        return Err(CliError::Usage(format!(
            "{flag} {shown:?}: bad header name"
        )));
    }
    if value.contains(['\r', '\n', '\0']) {
        return Err(CliError::Usage(format!(
            "{flag} {name}: the value contains a line break"
        )));
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
    Ok(())
}

/// Builds `download.add` params from the shared download options and the
/// request context from [`request_context`].
pub fn add_params(
    url: String,
    opts: &DownloadOpts,
    request: Option<RequestContext>,
    start: bool,
) -> Result<AddParams> {
    let checksum = opts.checksum.as_deref().map(parse_checksum).transpose()?;
    let directory = opts
        .dir
        .as_deref()
        .map(|d| absolute(d).map(|d| d.to_string_lossy().into_owned()))
        .transpose()?;
    Ok(AddParams {
        url,
        request,
        filename: opts.name.clone(),
        directory,
        category: opts.category.clone(),
        start,
        mirrors: opts.mirrors.clone(),
        checksum,
        connections: opts.connections,
        queue: opts.queue.clone(),
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
    let request = request_context(opts)?;
    let params: Vec<AddParams> = urls
        .into_iter()
        .map(|url| add_params(url, opts, request.clone(), !paused))
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
                    eprintln!("whoosh: {url}: {e}");
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
    if s.state == whoosh_proto::DownloadState::Downloading {
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
            (
                "durability",
                match settings.durability {
                    Durability::PowerSafe => "powerSafe".to_owned(),
                    Durability::ProcessSafe => "processSafe".to_owned(),
                },
            ),
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
    Durability,
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
        "durability" => Key::Durability,
        _ => {
            return Err(CliError::Usage(format!(
                "unknown setting {key:?} (defaultDirectory, maxActiveDownloads, \
                 maxConnectionsPerDownload, speedLimitBps, language, theme, durability)"
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
        Key::Durability => {
            s.durability = match value.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
                "powersafe" | "safe" => Durability::PowerSafe,
                "processsafe" | "fast" => Durability::ProcessSafe,
                _ => {
                    return Err(CliError::Usage(format!(
                        "durability: {value:?} is not powerSafe or processSafe"
                    )));
                }
            }
        }
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
            Found::Running(connected) => {
                let Connected { client, info } = *connected;
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
                        "whoosh-daemon {} is running (pid {}, port {})",
                        hello.daemon_version, info.pid, info.port
                    );
                    println!("data dir:     {}", ctx.data_dir.display());
                    println!("capabilities: {}", hello.capabilities.join(", "));
                }
                Ok(exit::OK)
            }
            Found::NotRunning(why) => {
                if ctx.json {
                    ctx.print_json(
                        &json!({ "running": false, "reason": why, "dataDir": ctx.data_dir }),
                    );
                } else {
                    println!("whoosh-daemon is not running ({why})");
                }
                Ok(exit::DAEMON_UNAVAILABLE)
            }
        },
        DaemonCmd::Stop => match daemon::connect_running(&ctx.data_dir).await? {
            Found::Running(connected) => {
                let pid = connected.info.pid;
                daemon::stop(*connected, &ctx.data_dir).await?;
                if ctx.json {
                    ctx.print_json(&json!({ "stopped": true, "pid": pid }));
                } else {
                    println!("whoosh-daemon stopped");
                }
                Ok(exit::OK)
            }
            // Nothing is signalled or sent: a stale runtime file's pid or
            // port may belong to something else by now.
            Found::NotRunning(why) => {
                if ctx.json {
                    ctx.print_json(&json!({ "stopped": false, "running": false, "reason": why }));
                } else {
                    println!("whoosh-daemon is not running ({why})");
                }
                Ok(exit::OK)
            }
        },
        DaemonCmd::Start { foreground, args } => {
            if let Found::Running(connected) = daemon::connect_running(&ctx.data_dir).await? {
                let Connected { client, info } = *connected;
                client.close().await;
                if ctx.json {
                    ctx.print_json(&json!({ "started": false, "running": true, "pid": info.pid, "port": info.port }));
                } else {
                    println!(
                        "whoosh-daemon is already running (pid {}, port {})",
                        info.pid, info.port
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
                    "whoosh-daemon started (pid {}, port {})",
                    info.pid, info.port
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
            connections: Some(4),
            queue: Some("night".into()),
            ..DownloadOpts::default()
        };
        let request = request_context(&opts).unwrap();
        let p = add_params("https://e/x".into(), &opts, request, false).unwrap();
        assert!(!p.start);
        assert_eq!(
            (p.connections, p.queue.as_deref()),
            (Some(4), Some("night"))
        );
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

        assert_eq!(request_context(&DownloadOpts::default()).unwrap(), None);

        let usage = |o: DownloadOpts| {
            request_context(&o)
                .and_then(|r| add_params("u".into(), &o, r, true))
                .unwrap_err()
        };
        for (opts, needle) in [
            (
                DownloadOpts {
                    headers: vec!["nocolon".into()],
                    ..Default::default()
                },
                "Name: value",
            ),
            (
                DownloadOpts {
                    headers: vec!["Bad Name: x".into()],
                    ..Default::default()
                },
                "bad header",
            ),
            (
                DownloadOpts {
                    headers: vec!["X: a\nInjected: b".into()],
                    ..Default::default()
                },
                "line break",
            ),
            (
                DownloadOpts {
                    checksum: Some("sha256:12".into()),
                    ..Default::default()
                },
                "64 hex",
            ),
            (
                DownloadOpts {
                    checksum: Some("crc32:12345678".into()),
                    ..Default::default()
                },
                "ALG",
            ),
            (
                DownloadOpts {
                    cookie_file: Some("-".into()),
                    headers_file: Some("-".into()),
                    ..Default::default()
                },
                "stdin",
            ),
            (
                DownloadOpts {
                    cookie_file: Some("/nonexistent/c".into()),
                    ..Default::default()
                },
                "--cookie-file",
            ),
        ] {
            let e = usage(opts);
            assert_eq!(e.exit_code(), 2, "{e}");
            assert!(e.to_string().contains(needle), "{e}");
        }
        // A secret header value is never echoed in an error.
        let e = usage(DownloadOpts {
            headers: vec!["Bad Name: s3cret".into()],
            ..Default::default()
        });
        assert!(!e.to_string().contains("s3cret"), "{e}");
    }

    #[test]
    fn cookie_and_header_files() {
        let dir = std::env::temp_dir().join(format!("whoosh-cli-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cookie = dir.join("cookie");
        std::fs::write(&cookie, "session=abc; theme=dark\n").unwrap();
        let headers = dir.join("headers");
        std::fs::write(
            &headers,
            "# comment\n\nAuthorization: Bearer t0k\nUser-Agent: from-file\nX-A: 1\n",
        )
        .unwrap();
        let opts = DownloadOpts {
            cookie_file: Some(cookie.clone()),
            headers_file: Some(headers.clone()),
            user_agent: Some("from-flag".into()),
            ..DownloadOpts::default()
        };
        let r = request_context(&opts).unwrap().unwrap();
        assert_eq!(r.cookie.as_deref(), Some("session=abc; theme=dark"));
        assert_eq!(r.user_agent.as_deref(), Some("from-flag"));
        assert_eq!(r.headers.len(), 2);
        assert_eq!(r.headers[0].value, "Bearer t0k");
        std::fs::write(&cookie, "a=1\nb=2\n").unwrap();
        let opts = DownloadOpts {
            cookie_file: Some(cookie),
            ..DownloadOpts::default()
        };
        assert_eq!(request_context(&opts).unwrap_err().exit_code(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
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
            durability: Default::default(),
            speed_schedule: Vec::new(),
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

    #[test]
    fn url_lists_skip_blank_lines_and_comments() {
        let dir = std::env::temp_dir().join(format!("whoosh-cli-urls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("links.txt");
        // Pasted with empty lines between links, a comment and CRLF endings.
        std::fs::write(&file, "https://a/1\r\n\r\n  https://a/2  \n# later\n\n").unwrap();
        let opts = DownloadOpts::default();
        let urls = url_list(vec!["".into(), " https://x/0 ".into()], Some(&file), &opts).unwrap();
        assert_eq!(urls, ["https://x/0", "https://a/1", "https://a/2"]);
        assert!(url_list(vec!["  ".into()], None, &opts).is_err());
        let both_stdin = DownloadOpts {
            headers_file: Some("-".into()),
            ..DownloadOpts::default()
        };
        assert!(url_list(vec![], Some(Path::new("-")), &both_stdin).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
