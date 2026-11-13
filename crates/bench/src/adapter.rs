//! Client adapters: one per download manager (or per settings profile).
//!
//! An adapter turns a [`Job`] — URL, output directory, connection setting,
//! extra headers — into an [`Invocation`] (a command line, plus an API
//! script for clients that only have a local API), and knows which files in
//! the output directory are its own bookkeeping rather than the download.
//!
//! Every run gets an empty output directory and a private state directory
//! that becomes `HOME`, `XDG_*_HOME` (and `APPDATA` / `LOCALAPPDATA` on
//! Windows) for the client, so no run sees another run's config, host
//! profiles, resume journals or HSTS caches: every measurement is a cold
//! start, and the user's own config files (`~/.curlrc`, `~/.wgetrc`, …) do
//! not leak in.

use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// How many connections a client may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Conn {
    /// Whatever the client (or the adapter's profile) uses by default.
    #[default]
    Default,
    /// Ask for exactly this many connections where the client allows it.
    Fixed(u32),
}

impl fmt::Display for Conn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Conn::Default => f.write_str("default"),
            Conn::Fixed(n) => write!(f, "{n}"),
        }
    }
}

/// One download to perform.
#[derive(Debug, Clone)]
pub struct Job<'a> {
    pub url: &'a str,
    /// Empty directory the file must end up in.
    pub out_dir: &'a Path,
    /// Private, empty directory for the client's config and state.
    pub state_dir: &'a Path,
    /// Extra request headers (`Cookie`, `Referer`) the scenario needs.
    pub headers: &'a [(String, String)],
    pub conn: Conn,
    /// A free local TCP port, for clients that run a local API server.
    pub port: u16,
}

/// How the harness tells that a download is complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Completion {
    /// The process exits as soon as the file is done (curl, wget, aria2).
    Exit,
    /// The client downloads into a partial/control file and renames it at
    /// the end, but its process lingers afterwards (Surge's
    /// `--exit-when-done` polls every 2 s; MDM's CLI reports on a 0.5 s
    /// tick). Done = the download directory holds a file of the expected
    /// size and none of the client's control files; the harness then stops
    /// the process.
    FinalFile,
    /// The client's API reports the task done.
    Api,
}

/// How the harness drives a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Run a command; the download is done when it exits.
    Cli,
    /// Start a local server, submit the download over its API, poll until
    /// done, then stop the server.
    Api,
    /// GUI-only: the harness serves the file and a person starts the
    /// download; measured from the server's side.
    Manual,
}

/// A process to start, and how to tell when its download is done.
#[derive(Debug, Clone)]
pub struct Invocation {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    pub cwd: PathBuf,
    pub api: Option<ApiScript>,
}

/// Download submission over a client's local HTTP API.
#[derive(Debug, Clone, PartialEq)]
pub enum ApiScript {
    /// Gopeed's REST API (`gopeed-web`): `POST /api/v1/tasks`, then poll
    /// `GET /api/v1/tasks/<id>` until `status` is `done` or `error`.
    Gopeed {
        base: String,
        body: serde_json::Value,
    },
}

/// A download manager (or one settings profile of it) the harness can run.
pub trait Adapter: Send + Sync + fmt::Debug {
    /// Stable id used on the command line and in the data (`aria2c-tuned`).
    fn id(&self) -> &str;
    /// One line for the report.
    fn description(&self) -> &str;
    /// The tool whose binary this adapter runs (several adapters can share
    /// one, e.g. `aria2c` and `aria2c-tuned`).
    fn tool(&self) -> &str;
    fn mode(&self) -> Mode {
        Mode::Cli
    }
    /// Executable names to look for, in order.
    fn executable_names(&self) -> Vec<&str> {
        vec![self.tool()]
    }
    /// Arguments that print the version on stdout.
    fn version_args(&self) -> &[&str] {
        &["--version"]
    }
    /// Whether the client can send the extra headers some scenarios need.
    fn supports_headers(&self) -> bool {
        true
    }
    /// Builds the invocation for `job` using the resolved `program`.
    fn invocation(&self, program: &Path, job: &Job) -> Invocation;
    /// Files the client leaves next to the download for its own bookkeeping
    /// (control files, journals); ignored when looking for the output.
    fn is_control_file(&self, _name: &str) -> bool {
        false
    }
    /// How completion is detected (see [`Completion`]).
    fn completion(&self) -> Completion {
        match self.mode() {
            Mode::Api => Completion::Api,
            _ => Completion::Exit,
        }
    }
    /// The settings that matter for reproducing this adapter's numbers.
    fn settings(&self) -> String {
        String::new()
    }
    /// A long-running helper this client needs, the way a download manager
    /// keeps its daemon resident: started before the measured window and
    /// stopped after it, neither timed. Its CPU and memory during the run
    /// are added to the client's (Linux). `None` for most clients.
    fn resident(&self, _program: &Path, _job: &Job) -> Option<Resident> {
        None
    }
}

/// A resident helper process (see [`Adapter::resident`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Resident {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    pub cwd: PathBuf,
    /// Ready once this file holds JSON whose `pid` is the helper's pid.
    pub ready_file: PathBuf,
    /// Where its stdout and stderr go.
    pub log: PathBuf,
}

/// Environment that confines a client's config and state to `state_dir`.
pub fn isolated_env(state_dir: &Path) -> Vec<(OsString, OsString)> {
    let sub = |s: &str| state_dir.join(s).into_os_string();
    let mut env = vec![
        ("HOME".into(), state_dir.as_os_str().to_owned()),
        ("XDG_CONFIG_HOME".into(), sub("config")),
        ("XDG_DATA_HOME".into(), sub("data")),
        ("XDG_STATE_HOME".into(), sub("state")),
        ("XDG_CACHE_HOME".into(), sub("cache")),
        ("XDG_RUNTIME_DIR".into(), sub("runtime")),
        ("TMPDIR".into(), sub("tmp")),
    ];
    if cfg!(windows) {
        env.push(("USERPROFILE".into(), state_dir.as_os_str().to_owned()));
        env.push(("APPDATA".into(), sub("AppData/Roaming")));
        env.push(("LOCALAPPDATA".into(), sub("AppData/Local")));
        env.push(("TEMP".into(), sub("tmp")));
        env.push(("TMP".into(), sub("tmp")));
    }
    env
}

/// Directories [`isolated_env`] points at; created before the run.
pub fn isolated_dirs(state_dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = ["config", "data", "state", "cache", "runtime", "tmp"]
        .iter()
        .map(|s| state_dir.join(s))
        .collect();
    if cfg!(windows) {
        v.push(state_dir.join("AppData/Roaming"));
        v.push(state_dir.join("AppData/Local"));
    }
    v
}

fn os(s: impl Into<OsString>) -> OsString {
    s.into()
}

fn base_invocation(program: &Path, job: &Job) -> Invocation {
    Invocation {
        program: program.to_owned(),
        args: Vec::new(),
        env: isolated_env(job.state_dir),
        cwd: job.out_dir.to_owned(),
        api: None,
    }
}

/// The last path segment of a URL, without query or fragment.
pub fn url_file_name(url: &str) -> String {
    let no_frag = url.split('#').next().unwrap_or(url);
    let no_query = no_frag.split('?').next().unwrap_or(no_frag);
    no_query
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("download.bin")
        .to_owned()
}

// ---------------------------------------------------------------- curl --

/// curl: one connection, the single-stream baseline.
#[derive(Debug, Default)]
pub struct Curl;

impl Adapter for Curl {
    fn id(&self) -> &str {
        "curl"
    }
    fn description(&self) -> &str {
        "curl, one connection (baseline); -L -O -J, no retries (curl's default)"
    }
    fn tool(&self) -> &str {
        "curl"
    }
    fn settings(&self) -> String {
        "-sS -f -L -O -J".into()
    }
    fn invocation(&self, program: &Path, job: &Job) -> Invocation {
        let mut inv = base_invocation(program, job);
        // -O -J saves under the Content-Disposition name (basename only;
        // curl strips directories), falling back to the URL's name. -f
        // turns HTTP errors into a failure instead of saving the error body.
        inv.args = ["-sS", "-f", "-L", "-O", "-J"].map(os).to_vec();
        for (k, v) in job.headers {
            inv.args.push(os("-H"));
            inv.args.push(os(format!("{k}: {v}")));
        }
        inv.args.push(os(job.url));
        inv
    }
}

// ---------------------------------------------------------------- wget --

/// GNU Wget: one connection, retries and resumes on its own.
#[derive(Debug, Default)]
pub struct Wget;

impl Adapter for Wget {
    fn id(&self) -> &str {
        "wget"
    }
    fn description(&self) -> &str {
        "GNU Wget, one connection; --content-disposition, default retries (20)"
    }
    fn tool(&self) -> &str {
        "wget"
    }
    fn settings(&self) -> String {
        "-nv --content-disposition".into()
    }
    fn invocation(&self, program: &Path, job: &Job) -> Invocation {
        let mut inv = base_invocation(program, job);
        inv.args = vec![
            os("-nv"),
            os("--content-disposition"),
            os("-P"),
            job.out_dir.as_os_str().to_owned(),
        ];
        for (k, v) in job.headers {
            inv.args.push(os(format!("--header={k}: {v}")));
        }
        inv.args.push(os(job.url));
        inv
    }
}

// --------------------------------------------------------------- aria2 --

/// aria2c, or its fork aria2-next (same command line), with a settings
/// profile.
#[derive(Debug)]
pub struct Aria2 {
    id: &'static str,
    tool: &'static str,
    description: &'static str,
    /// `-x16 -s16 -k1M` when true; aria2's defaults (`-x1 -s5 -k20M`, i.e.
    /// one connection to one server) otherwise.
    tuned: bool,
}

impl Aria2 {
    pub fn aria2c() -> Self {
        Self {
            id: "aria2c",
            tool: "aria2c",
            description: "aria2c with default settings (-x1: one connection per server)",
            tuned: false,
        }
    }
    pub fn aria2c_tuned() -> Self {
        Self {
            id: "aria2c-tuned",
            tool: "aria2c",
            description: "aria2c tuned: -x16 -s16 -k1M",
            tuned: true,
        }
    }
    pub fn aria2_next() -> Self {
        Self {
            id: "aria2-next",
            tool: "aria2-next",
            description: "aria2-next (Rayburst's engine) with default settings",
            tuned: false,
        }
    }
    pub fn aria2_next_tuned() -> Self {
        Self {
            id: "aria2-next-tuned",
            tool: "aria2-next",
            description: "aria2-next tuned: -x16 -s16 -k1M",
            tuned: true,
        }
    }
}

impl Adapter for Aria2 {
    fn id(&self) -> &str {
        self.id
    }
    fn description(&self) -> &str {
        self.description
    }
    fn tool(&self) -> &str {
        self.tool
    }
    fn settings(&self) -> String {
        if self.tuned {
            "-x16 -s16 -k1M".into()
        } else {
            "defaults".into()
        }
    }
    fn invocation(&self, program: &Path, job: &Job) -> Invocation {
        let mut inv = base_invocation(program, job);
        let mut args = vec![
            format!("--dir={}", job.out_dir.display()),
            "--console-log-level=error".into(),
            "--summary-interval=0".into(),
            "--show-console-readout=false".into(),
            "--download-result=hide".into(),
            // Never read the user's ~/.aria2/aria2.conf.
            "--no-conf=true".into(),
        ];
        match (job.conn, self.tuned) {
            (Conn::Fixed(n), _) => {
                args.push(format!("--max-connection-per-server={}", n.clamp(1, 16)));
                args.push(format!("--split={n}"));
                if self.tuned {
                    args.push("--min-split-size=1M".into());
                }
            }
            (Conn::Default, true) => {
                args.extend(["-x16", "-s16", "-k1M"].map(String::from));
            }
            (Conn::Default, false) => {}
        }
        for (k, v) in job.headers {
            args.push(format!("--header={k}: {v}"));
        }
        args.push(job.url.to_owned());
        inv.args = args.into_iter().map(OsString::from).collect();
        inv
    }
    fn is_control_file(&self, name: &str) -> bool {
        name.ends_with(".aria2")
    }
}

// ----------------------------------------------------------------- MDM --

/// Mister Download Manager's headless `misterdownloadmanager-cli get`.
#[derive(Debug, Default)]
pub struct Mdm;

impl Adapter for Mdm {
    fn id(&self) -> &str {
        "mdm"
    }
    fn description(&self) -> &str {
        "Mister Download Manager, misterdownloadmanager-cli get (default: up to 8 connections)"
    }
    fn tool(&self) -> &str {
        "mdm"
    }
    fn executable_names(&self) -> Vec<&str> {
        vec!["misterdownloadmanager-cli"]
    }
    fn settings(&self) -> String {
        "get -o <dir>".into()
    }
    fn invocation(&self, program: &Path, job: &Job) -> Invocation {
        let mut inv = base_invocation(program, job);
        let mut args = vec![
            os("--json"),
            os("get"),
            os(job.url),
            os("-o"),
            job.out_dir.as_os_str().to_owned(),
        ];
        if let Conn::Fixed(n) = job.conn {
            args.push(os("-c"));
            args.push(os(n.clamp(1, 128).to_string()));
        }
        for (k, v) in job.headers {
            args.push(os("-H"));
            args.push(os(format!("{k}: {v}")));
        }
        inv.args = args;
        inv
    }
    fn is_control_file(&self, name: &str) -> bool {
        name.ends_with(".mdm.json") || name.ends_with(".mdmpart") || name.ends_with(".part")
    }
    fn completion(&self) -> Completion {
        Completion::FinalFile
    }
}

// --------------------------------------------------------------- Surge --

/// Surge's headless server with one URL, exiting when the queue is done.
#[derive(Debug, Default)]
pub struct Surge;

impl Adapter for Surge {
    fn id(&self) -> &str {
        "surge"
    }
    fn description(&self) -> &str {
        "Surge, headless `surge server <url> --exit-when-done` (default: up to 16 connections)"
    }
    fn tool(&self) -> &str {
        "surge"
    }
    fn version_args(&self) -> &[&str] {
        &["--version"]
    }
    fn supports_headers(&self) -> bool {
        false
    }
    fn settings(&self) -> String {
        "server --exit-when-done --no-resume".into()
    }
    fn invocation(&self, program: &Path, job: &Job) -> Invocation {
        let mut inv = base_invocation(program, job);
        inv.args = vec![
            os("server"),
            os(job.url),
            os("--output"),
            job.out_dir.as_os_str().to_owned(),
            os("--exit-when-done"),
            os("--no-resume"),
            os("--port"),
            os(job.port.to_string()),
        ];
        if let Conn::Fixed(n) = job.conn {
            // Surge reads max_connections_per_host from its settings file;
            // the state dir is private to this run, so writing it is safe.
            let dir = job.state_dir.join("config").join("surge");
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(
                dir.join("settings.toml"),
                format!("[network]\nmax_connections_per_host = {}\n", n.clamp(1, 64)),
            );
        }
        inv
    }
    fn is_control_file(&self, name: &str) -> bool {
        name.ends_with(".surge") || name.ends_with(".part")
    }
    fn completion(&self) -> Completion {
        Completion::FinalFile
    }
}

// -------------------------------------------------------------- Gopeed --

/// Gopeed's engine through the headless `gopeed-web` server and its REST
/// API (the only headless Gopeed build on its GitHub releases).
#[derive(Debug, Default)]
pub struct Gopeed;

impl Adapter for Gopeed {
    fn id(&self) -> &str {
        "gopeed"
    }
    fn description(&self) -> &str {
        "Gopeed (gopeed-web server, REST API; default connections)"
    }
    fn tool(&self) -> &str {
        "gopeed"
    }
    fn mode(&self) -> Mode {
        Mode::Api
    }
    fn executable_names(&self) -> Vec<&str> {
        vec!["gopeed-web", "gopeed"]
    }
    fn version_args(&self) -> &[&str] {
        &[]
    }
    fn settings(&self) -> String {
        "REST: POST /api/v1/tasks".into()
    }
    fn invocation(&self, program: &Path, job: &Job) -> Invocation {
        let mut inv = base_invocation(program, job);
        let storage = job.state_dir.join("storage");
        let _ = std::fs::create_dir_all(&storage);
        inv.args = vec![
            os("-A"),
            os("127.0.0.1"),
            os("-P"),
            os(job.port.to_string()),
            os("-d"),
            storage.into_os_string(),
        ];
        // The server's working directory must not be the output directory:
        // it may create files there.
        inv.cwd = job.state_dir.to_owned();
        let header: serde_json::Map<String, serde_json::Value> = job
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        let mut opts_extra = serde_json::Map::new();
        if let Conn::Fixed(n) = job.conn {
            opts_extra.insert("connections".into(), n.into());
        }
        let body = serde_json::json!({
            "req": { "url": job.url, "extra": { "header": header } },
            "opts": { "path": job.out_dir, "extra": opts_extra },
        });
        inv.api = Some(ApiScript::Gopeed {
            base: format!("http://127.0.0.1:{}", job.port),
            body,
        });
        inv
    }
}

// ----------------------------------------------------------------- IDM --

/// Internet Download Manager on Windows, through its documented command
/// line: `IDMan.exe /d <url> /p <dir> /f <file> /n /q`.
///
/// `/n` suppresses dialogs, `/q` makes IDM exit after a successful
/// download. `/q` only works for the first instance, so IDM must not be
/// running when a run starts (the harness checks nothing; see
/// docs/bench/README.md). IDM's command line cannot send extra headers, and
/// `/f` fixes the file name, so Content-Disposition is not exercised.
/// IDM keeps settings in the registry: runs are not isolated from each
/// other the way other clients' are. Untested here (Linux development
/// machine).
#[derive(Debug, Default)]
pub struct Idm;

impl Adapter for Idm {
    fn id(&self) -> &str {
        "idm"
    }
    fn description(&self) -> &str {
        "Internet Download Manager (Windows), IDMan.exe /d /p /f /n /q"
    }
    fn tool(&self) -> &str {
        "idm"
    }
    fn executable_names(&self) -> Vec<&str> {
        vec!["IDMan.exe", "IDMan"]
    }
    fn version_args(&self) -> &[&str] {
        &[]
    }
    fn supports_headers(&self) -> bool {
        false
    }
    fn settings(&self) -> String {
        "IDM's own settings (connections per Options > Connection)".into()
    }
    fn invocation(&self, program: &Path, job: &Job) -> Invocation {
        Invocation {
            program: program.to_owned(),
            args: vec![
                os("/d"),
                os(job.url),
                os("/p"),
                job.out_dir.as_os_str().to_owned(),
                os("/f"),
                os(url_file_name(job.url)),
                os("/n"),
                os("/q"),
            ],
            // IDM reads its settings from the registry; overriding HOME etc.
            // would not isolate it and could confuse it.
            env: Vec::new(),
            cwd: job.out_dir.to_owned(),
            api: None,
        }
    }
}

// -------------------------------------------------------------- manual --

/// A GUI-only client (AB Download Manager, Free Download Manager, …): the
/// harness serves the file and prints the URL; a person starts the
/// download, and the run is measured from labserver's side.
#[derive(Debug)]
pub struct Manual {
    id: String,
    description: String,
}

impl Manual {
    pub fn new(id: &str, description: &str) -> Self {
        Self {
            id: id.to_owned(),
            description: description.to_owned(),
        }
    }
}

impl Adapter for Manual {
    fn id(&self) -> &str {
        &self.id
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn tool(&self) -> &str {
        &self.id
    }
    fn mode(&self) -> Mode {
        Mode::Manual
    }
    fn executable_names(&self) -> Vec<&str> {
        Vec::new()
    }
    fn supports_headers(&self) -> bool {
        false
    }
    fn invocation(&self, program: &Path, job: &Job) -> Invocation {
        base_invocation(program, job)
    }
}

// ---------------------------------------------------------------- whoosh --

/// Runs the daemon in the foreground, waits for its runtime file, then
/// `whoosh --no-start get`, then stops the daemon. `sh` is the measured
/// child and waits for both, so `RUSAGE_CHILDREN` counts the daemon's CPU
/// and memory too, and the time includes the daemon's start (like Gopeed's
/// server). Positional parameters avoid any quoting of URLs or headers.
const WHOOSH_SCRIPT: &str = r#"whoosh="$1"; daemon="$2"; shift 2
"$daemon" >"$WHOOSH_DATA_DIR.log" 2>&1 &
d=$!
i=0
while [ ! -s "$WHOOSH_DATA_DIR/runtime.json" ]; do
  kill -0 "$d" 2>/dev/null || exit 3
  i=$((i + 1))
  if [ "$i" -gt 2000 ]; then kill "$d"; wait "$d"; exit 3; fi
  sleep 0.005
done
"$whoosh" --no-start get "$@"
rc=$?
kill "$d"
wait "$d"
exit "$rc"
"#;

/// whoosh, cold one-shot: the daemon starts inside the measured window and
/// stops at its end (Unix only: the wrapper is a POSIX shell script).
/// Reported alongside, not gating (MASTER-PLAN §8, Phase 1 gate).
#[derive(Debug, Default)]
pub struct WhooshCold;

impl Adapter for WhooshCold {
    fn id(&self) -> &str {
        "whoosh-cold"
    }
    fn description(&self) -> &str {
        "whoosh (this project), cold one-shot: daemon start + `whoosh get` + daemon stop, all timed"
    }
    fn tool(&self) -> &str {
        "whoosh"
    }
    fn settings(&self) -> String {
        "whoosh-daemon (foreground) + whoosh --no-start get -o <dir> [-c N]".into()
    }
    fn invocation(&self, program: &Path, job: &Job) -> Invocation {
        let mut inv = base_invocation(program, job);
        inv.program = PathBuf::from("/bin/sh");
        let data_dir = job.state_dir.join("whoosh-data");
        inv.env
            .push((os("WHOOSH_DATA_DIR"), data_dir.into_os_string()));
        let mut args = vec![
            os("-c"),
            os(WHOOSH_SCRIPT),
            os("whoosh-bench-wrapper"),
            program.as_os_str().to_owned(),
            program.with_file_name("whoosh-daemon").into_os_string(),
            os(job.url),
        ];
        args.extend(whoosh_get_args(job));
        inv.args = args;
        inv
    }
    fn is_control_file(&self, name: &str) -> bool {
        name.ends_with(".whoosh-part")
    }
}

/// `-o DIR [-c N] [-H "K: V"]...` after the URL, for both whoosh adapters.
fn whoosh_get_args(job: &Job) -> Vec<OsString> {
    let mut args = vec![os("-o"), job.out_dir.as_os_str().to_owned()];
    if let Conn::Fixed(n) = job.conn {
        args.push(os("-c"));
        args.push(os(n.clamp(1, 64).to_string()));
    }
    for (k, v) in job.headers {
        args.push(os("-H"));
        args.push(os(format!("{k}: {v}")));
    }
    args
}

/// whoosh as a download manager runs: `whoosh-daemon` is already resident (the
/// harness starts it with a fresh data directory before the measured
/// window, waits until it is ready, and stops it afterwards), and the
/// measured process is `whoosh --no-start get`. The daemon's CPU and peak
/// memory during the run are added to the CLI's. This is the Phase 1 gate's
/// configuration (MASTER-PLAN §8).
#[derive(Debug, Default)]
pub struct Whoosh;

impl Whoosh {
    fn data_dir(job: &Job) -> PathBuf {
        job.state_dir.join("whoosh-data")
    }

    fn env(job: &Job) -> Vec<(OsString, OsString)> {
        let mut env = isolated_env(job.state_dir);
        env.push((os("WHOOSH_DATA_DIR"), Self::data_dir(job).into_os_string()));
        env
    }
}

impl Adapter for Whoosh {
    fn id(&self) -> &str {
        "whoosh"
    }
    fn description(&self) -> &str {
        "whoosh (this project), daemon resident: `whoosh --no-start get` against a running \
         whoosh-daemon (daemon start/stop not timed; its CPU and memory during the run counted)"
    }
    fn tool(&self) -> &str {
        "whoosh"
    }
    fn settings(&self) -> String {
        "resident whoosh-daemon --port 0; whoosh --no-start get -o <dir> [-c N]".into()
    }
    fn invocation(&self, program: &Path, job: &Job) -> Invocation {
        let mut inv = base_invocation(program, job);
        inv.env = Self::env(job);
        let mut args = vec![os("--no-start"), os("get"), os(job.url)];
        args.extend(whoosh_get_args(job));
        inv.args = args;
        inv
    }
    fn is_control_file(&self, name: &str) -> bool {
        name.ends_with(".whoosh-part")
    }
    fn resident(&self, program: &Path, job: &Job) -> Option<Resident> {
        Some(Resident {
            program: program.with_file_name(if cfg!(windows) {
                "whoosh-daemon.exe"
            } else {
                "whoosh-daemon"
            }),
            args: vec![os("--port"), os("0")],
            env: Self::env(job),
            cwd: job.state_dir.to_owned(),
            ready_file: Self::data_dir(job).join("runtime.json"),
            log: job.state_dir.join("whoosh-daemon.log"),
        })
    }
}

/// Every adapter the harness knows, in report order.
pub fn all() -> Vec<Arc<dyn Adapter>> {
    vec![
        Arc::new(Whoosh),
        Arc::new(WhooshCold),
        Arc::new(Curl),
        Arc::new(Wget),
        Arc::new(Aria2::aria2c()),
        Arc::new(Aria2::aria2c_tuned()),
        Arc::new(Aria2::aria2_next()),
        Arc::new(Aria2::aria2_next_tuned()),
        Arc::new(Mdm),
        Arc::new(Surge),
        Arc::new(Gopeed),
        Arc::new(Idm),
        Arc::new(Manual::new(
            "abdm",
            "AB Download Manager (GUI; manual mode)",
        )),
        Arc::new(Manual::new(
            "fdm",
            "Free Download Manager (GUI; manual mode)",
        )),
    ]
}

/// Looks an adapter up by id.
pub fn by_id(id: &str) -> Option<Arc<dyn Adapter>> {
    all().into_iter().find(|a| a.id() == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(inv: &Invocation) -> Vec<String> {
        inv.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn job<'a>(headers: &'a [(String, String)], conn: Conn) -> Job<'a> {
        Job {
            url: "http://127.0.0.1:1/s/clean/10m.bin?x=1",
            out_dir: Path::new("/run/out"),
            state_dir: Path::new("/run/state"),
            headers,
            conn,
            port: 4321,
        }
    }

    fn cookie() -> Vec<(String, String)> {
        vec![("Cookie".into(), "whoosh_session=lab-ok".into())]
    }

    #[test]
    fn curl_command_line() {
        let h = cookie();
        let inv = Curl.invocation(Path::new("/usr/bin/curl"), &job(&h, Conn::Default));
        assert_eq!(
            strs(&inv),
            [
                "-sS",
                "-f",
                "-L",
                "-O",
                "-J",
                "-H",
                "Cookie: whoosh_session=lab-ok",
                "http://127.0.0.1:1/s/clean/10m.bin?x=1"
            ]
        );
        assert_eq!(inv.cwd, Path::new("/run/out"), "-O saves into the cwd");
        assert!(
            inv.env
                .iter()
                .any(|(k, v)| k == "HOME" && v == Path::new("/run/state").as_os_str())
        );
        assert!(inv.api.is_none());
    }

    #[test]
    fn whoosh_cold_runs_daemon_and_cli_under_one_shell() {
        let h = cookie();
        let inv = WhooshCold.invocation(Path::new("/opt/whoosh/whoosh"), &job(&h, Conn::Fixed(99)));
        assert_eq!(inv.program, Path::new("/bin/sh"));
        let a = strs(&inv);
        assert_eq!(a[0], "-c");
        assert!(a[1].contains("--no-start get"));
        assert_eq!(
            a[2..],
            [
                "whoosh-bench-wrapper",
                "/opt/whoosh/whoosh",
                "/opt/whoosh/whoosh-daemon",
                "http://127.0.0.1:1/s/clean/10m.bin?x=1",
                "-o",
                "/run/out",
                "-c",
                "64",
                "-H",
                "Cookie: whoosh_session=lab-ok",
            ]
        );
        assert!(
            inv.env.iter().any(|(k, v)| k == "WHOOSH_DATA_DIR"
                && v == Path::new("/run/state/whoosh-data").as_os_str())
        );
        assert!(WhooshCold.is_control_file("10m.bin.whoosh-part"));
        assert!(
            WhooshCold
                .resident(Path::new("/opt/whoosh/whoosh"), &job(&h, Conn::Default))
                .is_none()
        );
    }

    #[test]
    fn whoosh_resident_measures_only_the_cli() {
        let h = cookie();
        let j = job(&h, Conn::Fixed(8));
        let prog = Path::new("/opt/whoosh/whoosh");
        let inv = Whoosh.invocation(prog, &j);
        assert_eq!(inv.program, prog, "the CLI itself is the measured process");
        assert_eq!(
            strs(&inv),
            [
                "--no-start",
                "get",
                "http://127.0.0.1:1/s/clean/10m.bin?x=1",
                "-o",
                "/run/out",
                "-c",
                "8",
                "-H",
                "Cookie: whoosh_session=lab-ok",
            ]
        );
        let data = Path::new("/run/state/whoosh-data");
        let has_data_dir = |env: &[(OsString, OsString)]| {
            env.iter()
                .any(|(k, v)| k == "WHOOSH_DATA_DIR" && v == data.as_os_str())
        };
        assert!(has_data_dir(&inv.env));
        assert_eq!(
            Whoosh.completion(),
            Completion::Exit,
            "`whoosh get` exits when done"
        );

        let r = Whoosh
            .resident(prog, &j)
            .expect("whoosh keeps its daemon resident");
        assert_eq!(r.program, Path::new("/opt/whoosh/whoosh-daemon"));
        assert_eq!(r.args, [os("--port"), os("0")]);
        assert!(
            has_data_dir(&r.env),
            "daemon and CLI share the data directory"
        );
        assert!(
            r.env
                .iter()
                .any(|(k, v)| k == "HOME" && v == Path::new("/run/state").as_os_str())
        );
        assert_eq!(r.ready_file, data.join("runtime.json"));
        assert!(Curl.resident(prog, &j).is_none());
    }

    #[test]
    fn wget_command_line() {
        let inv = Wget.invocation(Path::new("wget"), &job(&[], Conn::Fixed(8)));
        assert_eq!(
            strs(&inv),
            [
                "-nv",
                "--content-disposition",
                "-P",
                "/run/out",
                "http://127.0.0.1:1/s/clean/10m.bin?x=1"
            ]
        );
    }

    #[test]
    fn aria2_profiles() {
        let a = strs(&Aria2::aria2c().invocation(Path::new("aria2c"), &job(&[], Conn::Default)));
        assert!(a.contains(&"--dir=/run/out".to_string()));
        assert!(a.contains(&"--no-conf=true".to_string()));
        assert!(!a.iter().any(|s| s == "-x16"), "defaults stay default");
        assert_eq!(a.last().unwrap(), "http://127.0.0.1:1/s/clean/10m.bin?x=1");

        let h = cookie();
        let t =
            strs(&Aria2::aria2c_tuned().invocation(Path::new("aria2c"), &job(&h, Conn::Default)));
        for f in [
            "-x16",
            "-s16",
            "-k1M",
            "--header=Cookie: whoosh_session=lab-ok",
        ] {
            assert!(t.contains(&f.to_string()), "{f} in {t:?}");
        }

        let f = strs(&Aria2::aria2c().invocation(Path::new("aria2c"), &job(&[], Conn::Fixed(32))));
        assert!(
            f.contains(&"--max-connection-per-server=16".to_string()),
            "aria2 caps -x at 16"
        );
        assert!(f.contains(&"--split=32".to_string()));

        assert!(Aria2::aria2c().is_control_file("10m.bin.aria2"));
        assert!(!Aria2::aria2c().is_control_file("10m.bin"));
        assert_eq!(Aria2::aria2_next_tuned().tool(), "aria2-next");
    }

    #[test]
    fn mdm_command_line() {
        let h = cookie();
        let inv = Mdm.invocation(
            Path::new("misterdownloadmanager-cli"),
            &job(&h, Conn::Fixed(16)),
        );
        assert_eq!(
            strs(&inv),
            [
                "--json",
                "get",
                "http://127.0.0.1:1/s/clean/10m.bin?x=1",
                "-o",
                "/run/out",
                "-c",
                "16",
                "-H",
                "Cookie: whoosh_session=lab-ok"
            ]
        );
    }

    #[test]
    fn surge_command_line() {
        let inv = Surge.invocation(Path::new("surge"), &job(&[], Conn::Default));
        assert_eq!(
            strs(&inv),
            [
                "server",
                "http://127.0.0.1:1/s/clean/10m.bin?x=1",
                "--output",
                "/run/out",
                "--exit-when-done",
                "--no-resume",
                "--port",
                "4321"
            ]
        );
        assert!(!Surge.supports_headers());
        assert_eq!(Surge.completion(), Completion::FinalFile);
        assert!(Surge.is_control_file("10m.bin.surge"));
        assert_eq!(Mdm.completion(), Completion::FinalFile);
        assert!(Mdm.is_control_file("100m.bin.mdm.part"));
        assert_eq!(Curl.completion(), Completion::Exit);
        assert_eq!(Gopeed.completion(), Completion::Api);
    }

    #[test]
    fn gopeed_api_script() {
        let h = cookie();
        let inv = Gopeed.invocation(Path::new("gopeed"), &job(&h, Conn::Fixed(4)));
        assert_eq!(Gopeed.mode(), Mode::Api);
        assert_eq!(
            strs(&inv)[..4],
            ["-A", "127.0.0.1", "-P", "4321"].map(String::from)
        );
        let Some(ApiScript::Gopeed { base, body }) = inv.api else {
            panic!("gopeed needs an API script");
        };
        assert_eq!(base, "http://127.0.0.1:4321");
        assert_eq!(body["req"]["url"], "http://127.0.0.1:1/s/clean/10m.bin?x=1");
        assert_eq!(
            body["req"]["extra"]["header"]["Cookie"],
            "whoosh_session=lab-ok"
        );
        assert_eq!(body["opts"]["path"], "/run/out");
        assert_eq!(body["opts"]["extra"]["connections"], 4);
    }

    #[test]
    fn idm_command_line() {
        let inv = Idm.invocation(
            Path::new(r"C:\Program Files (x86)\Internet Download Manager\IDMan.exe"),
            &job(&[], Conn::Default),
        );
        assert_eq!(
            strs(&inv),
            [
                "/d",
                "http://127.0.0.1:1/s/clean/10m.bin?x=1",
                "/p",
                "/run/out",
                "/f",
                "10m.bin",
                "/n",
                "/q"
            ]
        );
        assert!(inv.env.is_empty());
    }

    #[test]
    fn registry() {
        let ids: Vec<String> = all().iter().map(|a| a.id().to_owned()).collect();
        let mut dedup = ids.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(ids.len(), dedup.len(), "ids are unique");
        assert_eq!(by_id("abdm").unwrap().mode(), Mode::Manual);
        assert!(by_id("nope").is_none());
    }

    #[test]
    fn url_names() {
        assert_eq!(url_file_name("http://h/s/x/a.bin?sig=1#f"), "a.bin");
        assert_eq!(url_file_name("http://h/"), "download.bin");
    }
}
