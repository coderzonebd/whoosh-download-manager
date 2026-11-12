//! The benchmark runner: the scenario matrix, the randomised schedule, one
//! measured run per (client, scenario, file, repetition), and the raw data
//! (`meta.json` + `runs.jsonl`) in a run-set directory.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::adapter::{self, Adapter, ApiScript, Completion, Conn, Job, Mode};
use crate::fetch::{self, Installed};
use crate::lab::Lab;
use crate::measure::{self, Measured, ResidentProc, ResidentUsage, unix_us};
use crate::names::{self, NameCheck};
use crate::sys::{self, Machine};

// ------------------------------------------------------------- matrix --

/// One scenario of the matrix and the files to fetch from it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixEntry {
    pub scenario: String,
    pub files: Vec<String>,
    /// Repetitions per (client, file); `None` = the run set's default.
    pub runs: Option<u32>,
    /// Per-run timeout.
    pub timeout_s: u64,
    /// Why this file size.
    pub why: String,
}

fn entry(
    scenario: &str,
    files: &[&str],
    runs: Option<u32>,
    timeout_s: u64,
    why: &str,
) -> MatrixEntry {
    MatrixEntry {
        scenario: scenario.to_owned(),
        files: files.iter().map(|s| (*s).to_owned()).collect(),
        runs,
        timeout_s,
        why: why.to_owned(),
    }
}

/// The Phase 0 baseline matrix: every built-in labserver scenario, with a
/// file size chosen so a single-connection client finishes in seconds on
/// a loaded laptop (see docs/bench/README.md for the reasoning).
pub fn default_matrix() -> Vec<MatrixEntry> {
    vec![
        entry(
            "clean",
            &["1g.bin"],
            None,
            120,
            "the speed ceiling; 1 GiB so client throughput, not startup, dominates",
        ),
        entry(
            "per-conn-throttle",
            &["10m.bin"],
            None,
            60,
            "2 MB/s per connection: one connection needs ~5 s",
        ),
        entry(
            "global-throttle",
            &["10m.bin"],
            None,
            60,
            "20 MB/s shared: ~0.5 s for everyone; measures politeness",
        ),
        entry(
            "conn-cap-2",
            &["10m.bin"],
            None,
            60,
            "5 MB/s per connection, 2 allowed: ~1-2 s",
        ),
        entry(
            "rate-limited",
            &["10m.bin"],
            None,
            60,
            "5 MB/s per connection, 4 allowed, 429s beyond",
        ),
        entry(
            "conn-cap-reset",
            &["10m.bin"],
            None,
            60,
            "5 MB/s per connection, 2 allowed, RST beyond",
        ),
        entry(
            "flaky",
            &["100m.bin"],
            None,
            90,
            "~1 reset per 50 MiB: a 100 MiB file sees about two",
        ),
        entry(
            "stall",
            &["100m.bin"],
            None,
            90,
            "8 s stalls, ~1 per 100 MiB per response",
        ),
        entry(
            "high-latency",
            &["10m.bin"],
            None,
            60,
            "500 ms before every response head",
        ),
        entry(
            "slow-drip",
            &["10m.bin"],
            None,
            60,
            "~3 MB/s per response: one connection needs ~3.5 s",
        ),
        entry("no-range", &["100m.bin"], None, 60, "segmenting impossible"),
        entry(
            "lying-ranges",
            &["100m.bin"],
            None,
            60,
            "Range ignored despite Accept-Ranges: corruption trap",
        ),
        entry(
            "no-length",
            &["100m.bin"],
            None,
            60,
            "chunked, size only in Content-Range",
        ),
        entry(
            "no-validators",
            &["100m.bin"],
            None,
            60,
            "no ETag / Last-Modified",
        ),
        entry(
            "changing-file",
            &["100m.bin"],
            None,
            60,
            "content changes after 32 MiB served: corruption trap",
        ),
        entry(
            "expiring-url",
            &["10m.bin"],
            None,
            60,
            "signed URL minted per run (60 s TTL)",
        ),
        entry(
            "cookie-required",
            &["10m.bin"],
            None,
            60,
            "needs a Cookie header",
        ),
        entry(
            "referer-required",
            &["10m.bin"],
            None,
            60,
            "needs a Referer header",
        ),
        entry("redirect-chain", &["100m.bin"], None, 60, "three 302 hops"),
        entry(
            "bangla-filename",
            &[
                "bangla.bin",
                "bangla-star-only.bin",
                "plain.bin",
                "latin1.bin",
                "raw-utf8.bin",
                "traversal.bin",
            ],
            None,
            30,
            "1 MiB files; a naming-correctness check, also judged by the gate, so the full run count",
        ),
    ]
}

/// A smaller matrix for smoke tests (`--quick`).
pub fn quick_matrix() -> Vec<MatrixEntry> {
    vec![
        entry("clean", &["10m.bin"], Some(3), 30, "smoke"),
        entry("flaky", &["10m.bin"], Some(3), 30, "smoke"),
        entry(
            "bangla-filename",
            &["bangla.bin", "traversal.bin"],
            Some(1),
            30,
            "smoke",
        ),
    ]
}

/// Public test files for the opt-in real-internet mode.
pub fn internet_targets() -> Vec<(String, String)> {
    [
        ("hetzner-100MB", "https://nbg1-speed.hetzner.com/100MB.bin"),
        ("ovh-100Mb", "https://proof.ovh.net/files/100Mb.dat"),
        ("tele2-100MB", "http://speedtest.tele2.net/100MB.zip"),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_owned(), b.to_owned()))
    .collect()
}

// --------------------------------------------------------------- data --

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// The file is byte-exact (some version of it, for changing files).
    Ok,
    /// Full size but wrong bytes: the worst outcome.
    Corrupt,
    /// A file of the wrong size (partial).
    Incomplete,
    /// No file at all.
    NoOutput,
    /// Killed after the per-run timeout.
    Timeout,
    /// Not run: the client timed out twice already in this block.
    Skipped,
    /// The harness could not run it (spawn failure, lab error).
    Error,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Corrupt => "CORRUPT",
            Outcome::Incomplete => "incomplete",
            Outcome::NoOutput => "no output",
            Outcome::Timeout => "timeout",
            Outcome::Skipped => "skipped",
            Outcome::Error => "error",
        }
    }
}

/// Everything measured about one run. One JSON line in `runs.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    /// Position in the executed schedule.
    pub seq: u32,
    pub client: String,
    pub scenario: String,
    pub file: String,
    pub rep: u32,
    pub size: Option<u64>,
    pub started_unix_us: u64,
    /// 1-minute load average when the run started.
    pub load1: Option<f64>,
    pub outcome: Outcome,
    /// Client spawn to the download being complete: process exit for
    /// clients that exit when done; the final file appearing (complete,
    /// control files gone) for clients that linger; the API reporting done
    /// for API clients. **The speed metric.**
    #[serde(default)]
    pub done_s: Option<f64>,
    /// Client spawn to the finished file being on disk: once completion is
    /// detected the harness fsyncs the output file and its directory, and
    /// this counts until that returns (MASTER-PLAN §8 rule 3: time to
    /// durability). A client that already synced pays almost nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_s: Option<f64>,
    /// How long the harness's own sync took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_s: Option<f64>,
    /// Why the sync failed or was skipped (no output file, I/O error).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_error: Option<String>,
    /// Client spawn to process exit (for lingering clients that the harness
    /// stopped once done: to that stop).
    pub wall_s: Option<f64>,
    /// Client spawn to the last response body byte labserver handed to a
    /// connection (capped at `wall_s`). Informational: kernel socket
    /// buffers hold megabytes on loopback, so this precedes the client's
    /// receipt, by more for clients with many connections.
    #[serde(default)]
    pub last_byte_s: Option<f64>,
    /// How completion was detected.
    #[serde(default)]
    pub completion: Option<Completion>,
    /// `size / done_s`, MiB/s, for successful runs.
    pub throughput_mib_s: Option<f64>,
    /// Client spawn to the first response body byte leaving labserver.
    pub ttfb_s: Option<f64>,
    /// labserver's first request to its last body byte.
    pub server_transfer_s: Option<f64>,
    /// Peak concurrent TCP connections the client held to the scenario
    /// (idle keep-alive included): the politeness metric.
    pub peak_connections: Option<u64>,
    /// Peak concurrent in-flight requests.
    pub peak_active_requests: Option<u64>,
    pub requests: Option<u64>,
    #[serde(default)]
    pub status: BTreeMap<String, u64>,
    pub refused: Option<u64>,
    pub resets_injected: Option<u64>,
    pub stalls_injected: Option<u64>,
    pub bytes_sent: Option<u64>,
    pub cpu_user_s: Option<f64>,
    pub cpu_sys_s: Option<f64>,
    pub max_rss_mib: Option<f64>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    /// File name the client saved (relative to its download directory).
    pub output_name: Option<String>,
    pub output_size: Option<u64>,
    pub sha256: Option<String>,
    pub expected_sha256: Option<String>,
    /// Which file version the bytes match (changing-file scenario).
    pub matched_version: Option<u64>,
    pub name_check: Option<NameCheck>,
    /// Other files left in the download directory (or outside it).
    #[serde(default)]
    pub extra_files: Vec<String>,
    pub note: Option<String>,
    pub stderr_tail: Option<String>,
    /// The client's resident helper (vayu's daemon) during this run; its
    /// CPU is included in `cpu_*_s` and its peak RSS in `max_rss_mib`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resident: Option<ResidentUsage>,
    /// CPUs' worth of work the rest of the machine did just before the run
    /// (see [`sys::busy_cpus`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busy_cpus: Option<f64>,
    /// The run started although the idle-machine guard said busy.
    #[serde(default)]
    pub machine_busy: bool,
    /// How long the guard waited for the machine to become idle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_wait_s: Option<f64>,
}

impl RunRecord {
    fn new(seq: u32, client: &str, scenario: &str, file: &str, rep: u32) -> Self {
        Self {
            seq,
            client: client.to_owned(),
            scenario: scenario.to_owned(),
            file: file.to_owned(),
            rep,
            size: None,
            started_unix_us: unix_us(),
            load1: sys::loadavg().map(|l| l[0]),
            outcome: Outcome::Error,
            done_s: None,
            durable_s: None,
            sync_s: None,
            sync_error: None,
            wall_s: None,
            last_byte_s: None,
            completion: None,
            throughput_mib_s: None,
            ttfb_s: None,
            server_transfer_s: None,
            peak_connections: None,
            peak_active_requests: None,
            requests: None,
            status: BTreeMap::new(),
            refused: None,
            resets_injected: None,
            stalls_injected: None,
            bytes_sent: None,
            cpu_user_s: None,
            cpu_sys_s: None,
            max_rss_mib: None,
            exit_code: None,
            signal: None,
            timed_out: false,
            output_name: None,
            output_size: None,
            sha256: None,
            expected_sha256: None,
            matched_version: None,
            name_check: None,
            extra_files: Vec::new(),
            note: None,
            stderr_tail: None,
            resident: None,
            busy_cpus: None,
            machine_busy: false,
            idle_wait_s: None,
        }
    }

    /// The run's duration for speed statistics: time to completion (older
    /// records without it: process wall time).
    pub fn time_s(&self) -> Option<f64> {
        self.done_s.or(self.wall_s)
    }

    /// Time to durability for statistics, and whether it is a fallback to
    /// [`RunRecord::time_s`] (run sets recorded before `durable_s`
    /// existed, or a failed sync).
    pub fn durable_time(&self) -> Option<(f64, bool)> {
        match self.durable_s {
            Some(d) => Some((d, false)),
            None => self.time_s().map(|t| (t, true)),
        }
    }

    /// The key that identifies a planned run (for `--resume`).
    pub fn key(&self) -> (String, String, String, u32) {
        (
            self.client.clone(),
            self.scenario.clone(),
            self.file.clone(),
            self.rep,
        )
    }
}

/// A client as the run set used it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientMeta {
    pub id: String,
    pub tool: String,
    pub description: String,
    pub settings: String,
    pub mode: Mode,
    pub program: Option<PathBuf>,
    pub version: Option<String>,
    pub binary_sha256: Option<String>,
    pub fetched: Option<Installed>,
}

/// A matrix row as run, with the file sizes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixRow {
    pub scenario: String,
    pub description: String,
    pub files: Vec<(String, u64)>,
    pub runs: u32,
    pub timeout_s: u64,
    pub why: String,
}

/// `meta.json` of a run set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub runset: String,
    pub started_utc: String,
    pub finished_utc: Option<String>,
    pub duration_s: Option<f64>,
    pub harness: String,
    pub git_commit: Option<String>,
    pub command: Vec<String>,
    pub machine: Machine,
    pub load_at_start: Option<[f64; 3]>,
    pub load_at_end: Option<[f64; 3]>,
    pub mem_available_mib_at_start: Option<u64>,
    /// `in-process` or the `--lab-url`.
    pub lab: String,
    pub lab_cache_mb: Option<usize>,
    pub internet: bool,
    pub seed: u64,
    pub runs: u32,
    pub conn: Conn,
    pub clients: Vec<ClientMeta>,
    pub matrix: Vec<MatrixRow>,
    #[serde(default)]
    pub notes: Vec<String>,
    /// The idle-machine guard's settings (absent in older run sets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<IdleGuard>,
    /// Runs that started while the guard said the machine was busy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busy_runs: Option<u32>,
}

// ------------------------------------------------------------ options --

/// The idle-machine guard: before every run, the harness checks the 1-min
/// load average and samples how busy the rest of the machine is
/// ([`sys::busy_cpus`], while the harness itself is quiet), and warns —
/// or, with `wait`, waits — when either is above its threshold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdleGuard {
    /// Busy when the 1-min load average is above this. The previous run
    /// keeps the load average up for a minute, so this must sit above what
    /// the benchmark itself produces.
    pub max_load: Option<f64>,
    /// Busy when the rest of the machine uses more than this many CPUs.
    pub max_busy_cpus: Option<f64>,
    /// Wait (re-checking every 2 s) instead of only warning.
    pub wait: bool,
    /// Give up waiting after this long and run anyway (marked busy).
    pub wait_timeout_s: u64,
    /// Length of the busy-CPU sample before each run, milliseconds.
    pub sample_ms: u64,
}

impl Default for IdleGuard {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        Self {
            max_load: Some(cpus as f64 / 2.0),
            max_busy_cpus: Some(1.0),
            wait: false,
            wait_timeout_s: 600,
            sample_ms: 200,
        }
    }
}

/// One guard check.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GuardCheck {
    pub load1: Option<f64>,
    pub busy_cpus: Option<f64>,
}

impl IdleGuard {
    /// Why the machine counts as busy, if it does.
    pub fn verdict(&self, c: &GuardCheck) -> Option<String> {
        let mut why = Vec::new();
        if let (Some(max), Some(l)) = (self.max_load, c.load1)
            && l > max
        {
            why.push(format!("1-min load {l:.2} > {max:.2}"));
        }
        if let (Some(max), Some(b)) = (self.max_busy_cpus, c.busy_cpus)
            && b > max
        {
            why.push(format!("other processes busy on {b:.2} CPUs > {max:.2}"));
        }
        (!why.is_empty()).then(|| why.join(", "))
    }

    fn check(&self) -> GuardCheck {
        GuardCheck {
            load1: sys::loadavg().map(|l| l[0]),
            busy_cpus: sys::busy_cpus(Duration::from_millis(self.sample_ms)),
        }
    }

    /// Checks before a run, waiting if configured. Fills the record's
    /// `busy_cpus`, `machine_busy` and `idle_wait_s`.
    fn before_run(&self, rec: &mut RunRecord, quiet: bool) {
        let t0 = Instant::now();
        let mut c = self.check();
        let mut warned = false;
        while let Some(why) = self.verdict(&c) {
            if !self.wait || t0.elapsed().as_secs() >= self.wait_timeout_s {
                rec.machine_busy = true;
                eprintln!(
                    "\n!!! WARNING: machine busy before run {} ({} {} {}): {why}. \
                     Numbers from this run are not idle-machine numbers.{}\n",
                    rec.seq,
                    rec.client,
                    rec.scenario,
                    rec.file,
                    if self.wait {
                        " (gave up waiting)"
                    } else {
                        " Pass --wait-for-idle to wait."
                    }
                );
                break;
            }
            if !warned && !quiet {
                eprintln!("waiting for an idle machine before run {}: {why}", rec.seq);
                warned = true;
            }
            std::thread::sleep(Duration::from_secs(2));
            c = self.check();
        }
        rec.busy_cpus = c.busy_cpus;
        rec.load1 = c.load1.or(rec.load1);
        if warned {
            rec.idle_wait_s = Some(t0.elapsed().as_secs_f64());
        }
    }
}

/// Options of `vayu-bench run`.
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub clients: Vec<Arc<dyn Adapter>>,
    pub matrix: Vec<MatrixEntry>,
    pub runs: u32,
    pub seed: u64,
    pub conn: Conn,
    /// Where `<label>/` with `meta.json` and `runs.jsonl` goes.
    pub out_root: PathBuf,
    pub label: Option<String>,
    /// Continue an existing run set (skips runs already recorded).
    pub resume: Option<PathBuf>,
    /// Scratch space for downloads (on a real disk, not tmpfs).
    pub work_dir: PathBuf,
    pub lab_url: Option<String>,
    pub cache_mb: usize,
    pub timeout_scale: f64,
    /// Overrides: tool -> executable.
    pub bins: BTreeMap<String, PathBuf>,
    pub tool_cache: PathBuf,
    /// The executable used as the measurement wrapper (this binary).
    pub shim_exe: PathBuf,
    /// Real-internet mode: download public test files instead.
    pub internet: Option<Vec<(String, String)>>,
    pub command: Vec<String>,
    pub quiet: bool,
    /// `None` disables the idle-machine guard.
    pub guard: Option<IdleGuard>,
}

// ---------------------------------------------------------- resolving --

/// Finds the executable for an adapter: `--bin tool=path`, then tools
/// installed by `fetch`, then `PATH`, then known install locations.
pub fn resolve_program(
    a: &dyn Adapter,
    bins: &BTreeMap<String, PathBuf>,
    tool_cache: &Path,
) -> Option<(PathBuf, Option<Installed>)> {
    if let Some(p) = bins.get(a.tool()) {
        return Some((p.clone(), None));
    }
    for name in a.executable_names() {
        if let Some((p, inst)) = fetch::installed_binary(tool_cache, name) {
            return Some((p, Some(inst)));
        }
    }
    for name in a.executable_names() {
        if let Some(p) = sys::which(name) {
            return Some((p, None));
        }
    }
    if a.tool() == "idm" {
        for base in ["ProgramFiles(x86)", "ProgramFiles"] {
            if let Some(d) = std::env::var_os(base) {
                let p = PathBuf::from(d)
                    .join("Internet Download Manager")
                    .join("IDMan.exe");
                if p.is_file() {
                    return Some((p, None));
                }
            }
        }
    }
    None
}

/// Describes an adapter as this machine would run it.
pub fn client_meta(
    a: &dyn Adapter,
    bins: &BTreeMap<String, PathBuf>,
    tool_cache: &Path,
) -> ClientMeta {
    let resolved = resolve_program(a, bins, tool_cache);
    let (program, fetched) = match resolved {
        Some((p, f)) => (Some(p), f),
        None => (None, None),
    };
    let version = program.as_deref().and_then(|p| {
        if a.version_args().is_empty() {
            return fetched.as_ref().map(|f| f.version.clone());
        }
        measure::capture(p, a.version_args(), Duration::from_secs(10)).and_then(|s| {
            s.lines()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_owned())
        })
    });
    let binary_sha256 = program.as_deref().and_then(|p| sys::sha256_file(p).ok());
    ClientMeta {
        id: a.id().to_owned(),
        tool: a.tool().to_owned(),
        description: a.description().to_owned(),
        settings: a.settings(),
        mode: a.mode(),
        program,
        version,
        binary_sha256,
        fetched,
    }
}

// --------------------------------------------------------------- run --

/// What the planner hands to one run.
struct Target<'a> {
    scenario: &'a str,
    file: &'a str,
    size: Option<u64>,
    /// `Some(url)` in real-internet mode.
    internet_url: Option<&'a str>,
    timeout: Duration,
}

struct Ctx<'a> {
    lab: Option<&'a Lab>,
    opts: &'a RunOptions,
    work: PathBuf,
    digests: HashMap<(String, String, u64), String>,
}

/// Runs the whole matrix and writes the run set. Returns its directory.
pub fn run(opts: &RunOptions) -> Result<PathBuf> {
    let started = unix_us();
    let t0 = Instant::now();
    let lab = if opts.internet.is_some() {
        None
    } else if let Some(url) = &opts.lab_url {
        Some(Lab::connect(url)?)
    } else {
        Some(Lab::start(opts.cache_mb)?)
    };

    // Clients that can run here.
    let mut clients = Vec::new();
    let mut client_metas = Vec::new();
    for a in &opts.clients {
        if a.mode() == Mode::Manual {
            eprintln!(
                "note: {} is manual-only; use `vayu-bench manual --client {}`",
                a.id(),
                a.id()
            );
            continue;
        }
        let m = client_meta(a.as_ref(), &opts.bins, &opts.tool_cache);
        match &m.program {
            Some(p) => {
                eprintln!(
                    "client {:<18} {} ({})",
                    a.id(),
                    p.display(),
                    m.version.as_deref().unwrap_or("?")
                );
                clients.push((a.clone(), p.clone()));
                client_metas.push(m);
            }
            None => eprintln!(
                "skip   {:<18} not installed (see `vayu-bench fetch`)",
                a.id()
            ),
        }
    }
    if clients.is_empty() {
        bail!("no client to run");
    }

    // The concrete matrix.
    let mut rows = Vec::new();
    match (&opts.internet, &lab) {
        (Some(targets), _) => rows.push(MatrixRow {
            scenario: "internet".into(),
            description: "public test files (real internet, user-run only)".into(),
            files: targets.iter().map(|(n, _)| (n.clone(), 0)).collect(),
            runs: opts.runs,
            timeout_s: (600.0 * opts.timeout_scale) as u64,
            why: "opt-in".into(),
        }),
        (None, Some(lab)) => {
            for e in &opts.matrix {
                let sc = lab
                    .scenario(&e.scenario)
                    .with_context(|| format!("labserver has no scenario {:?}", e.scenario))?;
                let mut files = Vec::new();
                for f in &e.files {
                    let fi = sc
                        .file(f)
                        .with_context(|| format!("scenario {} has no file {f}", e.scenario))?;
                    files.push((f.clone(), fi.size));
                }
                rows.push(MatrixRow {
                    scenario: e.scenario.clone(),
                    description: sc.description.clone(),
                    files,
                    runs: e.runs.unwrap_or(opts.runs),
                    timeout_s: ((e.timeout_s as f64) * opts.timeout_scale).ceil() as u64,
                    why: e.why.clone(),
                });
            }
        }
        (None, None) => unreachable!("lab exists unless internet mode"),
    }

    // Run-set directory.
    let (dir, done) = match &opts.resume {
        Some(d) => {
            let done = read_runs(d)?;
            (d.clone(), done)
        }
        None => {
            let label = opts.label.clone().unwrap_or_else(|| {
                format!("{}-{}", sys::date_utc(started), sanitize(&sys::hostname()))
            });
            let mut dir = opts.out_root.join(&label);
            let mut k = 2;
            while dir.exists() {
                dir = opts.out_root.join(format!("{label}-{k}"));
                k += 1;
            }
            std::fs::create_dir_all(&dir)?;
            (dir, Vec::new())
        }
    };
    let runset = dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut meta = Meta {
        runset: runset.clone(),
        started_utc: sys::datetime_utc(started),
        finished_utc: None,
        duration_s: None,
        harness: format!("vayu-bench {}", env!("CARGO_PKG_VERSION")),
        git_commit: measure::capture(
            Path::new("git"),
            &["rev-parse", "HEAD"],
            Duration::from_secs(5),
        )
        .map(|s| s.trim().to_owned())
        .filter(|s| s.len() == 40),
        command: opts.command.clone(),
        machine: Machine::detect(),
        load_at_start: sys::loadavg(),
        load_at_end: None,
        mem_available_mib_at_start: sys::meminfo_kib("MemAvailable").map(|k| k / 1024),
        lab: match (&opts.lab_url, &lab) {
            (_, None) => "none (real internet)".into(),
            (Some(u), _) => u.clone(),
            (None, Some(_)) => "in-process".into(),
        },
        lab_cache_mb: lab
            .as_ref()
            .filter(|l| l.in_process())
            .map(|_| opts.cache_mb),
        internet: opts.internet.is_some(),
        seed: opts.seed,
        runs: opts.runs,
        conn: opts.conn,
        clients: client_metas,
        matrix: rows.clone(),
        notes: Vec::new(),
        guard: opts.guard.clone(),
        busy_runs: None,
    };
    if opts.resume.is_some()
        && let Ok(old) = read_meta(&dir)
    {
        // Keep the original start, extend notes.
        meta.started_utc = old.started_utc;
        meta.notes = old.notes;
        meta.notes
            .push(format!("resumed at {}", sys::datetime_utc(started)));
    }
    write_meta(&dir, &meta)?;

    let done_keys: HashSet<_> = done.iter().map(RunRecord::key).collect();
    let mut seq = done.iter().map(|r| r.seq + 1).max().unwrap_or(0);
    let mut jsonl = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("runs.jsonl"))?;
    // A run set killed mid-write leaves a torn last line; start on a fresh
    // one so the next record stays parseable (`read_runs` skips the torn one).
    if std::fs::read(dir.join("runs.jsonl"))?
        .last()
        .is_some_and(|b| *b != b'\n')
    {
        jsonl.write_all(b"\n")?;
    }

    let work = opts.work_dir.join(&runset);
    std::fs::create_dir_all(&work)?;
    let mut ctx = Ctx {
        lab: lab.as_ref(),
        opts,
        work: work.clone(),
        digests: HashMap::new(),
    };

    let total: usize = rows
        .iter()
        .map(|r| r.files.len() * r.runs as usize * clients.len())
        .sum();
    let mut n_done = done_keys.len();
    let mut rng = fastrand::Rng::with_seed(opts.seed);
    for row in &rows {
        for (file, size) in &row.files {
            let internet_url = opts
                .internet
                .as_ref()
                .and_then(|t| t.iter().find(|(n, _)| n == file).map(|(_, u)| u.as_str()));
            let target = Target {
                scenario: &row.scenario,
                file,
                size: (*size > 0).then_some(*size),
                internet_url,
                timeout: Duration::from_secs(row.timeout_s),
            };
            // Every client x repetition of this block, in random order: drift
            // in machine load then spreads evenly over the clients compared.
            let mut schedule: Vec<(usize, u32)> = (0..clients.len())
                .flat_map(|c| (0..row.runs).map(move |r| (c, r)))
                .collect();
            rng.shuffle(&mut schedule);
            let mut timeouts: HashMap<usize, u32> = HashMap::new();
            for (ci, rep) in schedule {
                let (adapter, program) = &clients[ci];
                let key = (
                    adapter.id().to_owned(),
                    row.scenario.clone(),
                    file.clone(),
                    rep,
                );
                if done_keys.contains(&key) {
                    continue;
                }
                let mut rec = if timeouts.get(&ci).copied().unwrap_or(0) >= 2 {
                    let mut r = RunRecord::new(seq, adapter.id(), &row.scenario, file, rep);
                    r.outcome = Outcome::Skipped;
                    r.size = target.size;
                    r.note = Some("skipped: two timeouts already in this block".into());
                    r
                } else {
                    run_one(&mut ctx, adapter.as_ref(), program, &target, seq, rep)
                };
                if rec.outcome == Outcome::Timeout {
                    *timeouts.entry(ci).or_default() += 1;
                }
                rec.seq = seq;
                seq += 1;
                n_done += 1;
                append_record(&mut jsonl, &rec)?;
                if !opts.quiet {
                    eprintln!(
                        "[{n_done}/{total}] {:<17} {:<18} {:<20} #{rep} {:<10} {} (exit {})",
                        rec.client,
                        rec.scenario,
                        rec.file,
                        rec.outcome.label(),
                        rec.time_s()
                            .map(|w| format!("{w:.3} s"))
                            .unwrap_or_default(),
                        rec.wall_s.map(|w| format!("{w:.3} s")).unwrap_or_default()
                    );
                }
            }
        }
    }

    meta.finished_utc = Some(sys::datetime_utc(unix_us()));
    meta.duration_s = Some(t0.elapsed().as_secs_f64());
    meta.load_at_end = sys::loadavg();
    if opts.guard.is_some() {
        let busy = read_runs(&dir)?.iter().filter(|r| r.machine_busy).count() as u32;
        meta.busy_runs = Some(busy);
        if busy > 0 {
            eprintln!(
                "\n!!! {busy} run(s) started on a busy machine (see `machine_busy` in runs.jsonl); \
                 the report flags them.\n"
            );
        }
    }
    write_meta(&dir, &meta)?;
    let _ = std::fs::remove_dir_all(&work);
    Ok(dir)
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Run sets are published: write the home directory as `~` so paths do
/// not carry the local user name.
fn redact_home(json: String) -> String {
    match std::env::var("HOME") {
        Ok(h) if h.len() > 1 && !h.contains('"') && !h.contains('\\') => {
            json.replace(h.trim_end_matches('/'), "~")
        }
        _ => json,
    }
}

pub fn write_meta(dir: &Path, meta: &Meta) -> Result<()> {
    let tmp = dir.join("meta.json.tmp");
    std::fs::write(&tmp, redact_home(serde_json::to_string_pretty(meta)?))?;
    std::fs::rename(&tmp, dir.join("meta.json"))?;
    Ok(())
}

/// Appends one record to `runs.jsonl`.
fn append_record(jsonl: &mut std::fs::File, rec: &RunRecord) -> Result<()> {
    let mut line = redact_home(serde_json::to_string(rec)?);
    line.push('\n');
    jsonl.write_all(line.as_bytes())?;
    jsonl.flush()?;
    Ok(())
}

pub fn read_meta(dir: &Path) -> Result<Meta> {
    let b = std::fs::read(dir.join("meta.json"))
        .with_context(|| format!("read {}/meta.json", dir.display()))?;
    Ok(serde_json::from_slice(&b)?)
}

/// Reads `runs.jsonl` (skipping a torn last line).
pub fn read_runs(dir: &Path) -> Result<Vec<RunRecord>> {
    let p = dir.join("runs.jsonl");
    let s = match std::fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("read {}", p.display())),
    };
    Ok(s.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

/// Lays out one run's directories:
/// `<run>/root/a/b/out` (downloads), `<run>/state` (HOME etc.), logs in
/// `<run>`. The download directory is nested so a client that follows
/// `../../etc/passwd` lands inside `<run>/root` where it can be seen.
fn run_dirs(base: &Path) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let _ = std::fs::remove_dir_all(base);
    let root = base.join("root");
    let out = root.join("a").join("b").join("out");
    let state = base.join("state");
    std::fs::create_dir_all(&out)?;
    std::fs::create_dir_all(&state)?;
    for d in adapter::isolated_dirs(&state) {
        std::fs::create_dir_all(d)?;
    }
    Ok((root, out, state))
}

fn run_one(
    ctx: &mut Ctx,
    a: &dyn Adapter,
    program: &Path,
    t: &Target,
    seq: u32,
    rep: u32,
) -> RunRecord {
    let mut rec = RunRecord::new(seq, a.id(), t.scenario, t.file, rep);
    rec.size = t.size;
    if let Err(e) = run_one_inner(ctx, a, program, t, &mut rec) {
        rec.outcome = Outcome::Error;
        rec.note = Some(format!("harness error: {e:#}"));
    }
    rec
}

fn run_one_inner(
    ctx: &mut Ctx,
    a: &dyn Adapter,
    program: &Path,
    t: &Target,
    rec: &mut RunRecord,
) -> Result<()> {
    let base = ctx.work.join(format!("run-{}", rec.seq));
    let (root, out, state) = run_dirs(&base)?;
    if let Some(g) = &ctx.opts.guard {
        g.before_run(rec, ctx.opts.quiet);
    }

    let (url, headers) = match (t.internet_url, ctx.lab) {
        (Some(u), _) => (u.to_owned(), Vec::new()),
        (None, Some(lab)) => {
            let sc = lab.scenario(t.scenario).context("scenario vanished")?;
            if !lab.wait_idle(t.scenario, Duration::from_secs(5))? {
                rec.note = Some("previous connections still open after 5 s".into());
            }
            lab.reset(t.scenario)?;
            (lab.run_url(t.scenario, t.file)?, sc.required_headers())
        }
        (None, None) => bail!("no lab"),
    };
    if !headers.is_empty() && !a.supports_headers() {
        let n = format!(
            "client cannot send required header(s): {}",
            headers
                .iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        rec.note = Some(rec.note.take().map_or(n.clone(), |o| format!("{o}; {n}")));
    }
    let job = Job {
        url: &url,
        out_dir: &out,
        state_dir: &state,
        headers: &headers,
        conn: ctx.opts.conn,
        port: sys::free_port()?,
    };
    let inv = a.invocation(program, &job);

    // ---- resident helper (outside the measured window)
    let mut resident = match a.resident(program, &job) {
        None => None,
        Some(spec) => match ResidentProc::start(&spec, Duration::from_secs(30)) {
            Ok(p) => Some(p),
            Err(e) => {
                rec.outcome = Outcome::Error;
                rec.note = Some(format!("resident helper failed to start: {e:#}"));
                let _ = std::fs::remove_dir_all(&base);
                return Ok(());
            }
        },
    };

    // ---- run
    rec.started_unix_us = unix_us();
    if ctx.opts.guard.is_none() {
        rec.load1 = sys::loadavg().map(|l| l[0]);
    }
    if let Some(r) = resident.as_mut() {
        r.open_window();
    }
    let mut running = measure::start(&ctx.opts.shim_exe, &inv, &base)?;
    let completion = a.completion();
    rec.completion = Some(completion);
    // The sync runs the moment completion is detected, before the harness
    // stops a lingering client or closes a resident daemon's window.
    let sync = || sync_output(a, &root, &out);
    let w = match (&inv.api, completion) {
        (Some(script), _) => drive_api(&mut running, script, t.timeout, &sync),
        (None, Completion::FinalFile) => {
            wait_final_file(&mut running, a, &out, t.size, t.timeout, &sync)
        }
        (None, _) => match running.wait(t.timeout) {
            Some(m) => Waited {
                synced: Some(sync()),
                ..Waited::new(m)
            },
            None => Waited {
                timed_out: true,
                ..Waited::new(running.stop(Duration::from_secs(3)))
            },
        },
    };
    let Waited {
        measured,
        done_unix_us,
        timed_out,
        note: api_note,
        synced,
    } = w;
    drop(running);
    let usage = resident.as_mut().map(ResidentProc::close_window);
    if let Some(r) = resident.take() {
        r.stop(Duration::from_secs(5));
    }

    rec.timed_out = timed_out;
    rec.exit_code = measured.exit_code;
    rec.signal = measured.signal;
    rec.cpu_user_s = measured.cpu_user_us.map(|u| u as f64 / 1e6);
    rec.cpu_sys_s = measured.cpu_sys_us.map(|u| u as f64 / 1e6);
    rec.max_rss_mib = measured.max_rss_kib.map(|k| k as f64 / 1024.0);
    if let Some(u) = usage {
        // The helper's share counts: CPU adds up; peak memory is the sum
        // of both peaks (an upper bound: they need not coincide). Where the
        // helper could not be measured the totals stay unknown rather than
        // silently client-only.
        let add = |a: Option<f64>, b: Option<f64>| Some(a? + b?);
        rec.cpu_user_s = add(rec.cpu_user_s, u.cpu_user_s);
        rec.cpu_sys_s = add(rec.cpu_sys_s, u.cpu_sys_s);
        rec.max_rss_mib = add(rec.max_rss_mib, u.max_rss_mib);
        rec.resident = Some(u);
    }
    rec.wall_s = Some(measured.wall_us as f64 / 1e6);
    let done_us = match done_unix_us {
        Some(d) => d.saturating_sub(measured.spawn_unix_us),
        None => measured.wall_us,
    };
    if !timed_out {
        rec.done_s = Some(done_us as f64 / 1e6);
    }
    if let Some(s) = synced {
        rec.sync_s = Some(s.end_unix_us.saturating_sub(s.start_unix_us) as f64 / 1e6);
        match s.error {
            None if !timed_out => {
                rec.durable_s =
                    Some(s.end_unix_us.saturating_sub(measured.spawn_unix_us) as f64 / 1e6);
            }
            None => {}
            Some(e) => rec.sync_error = Some(e),
        }
    }
    if let Some(e) = &measured.spawn_error {
        rec.note = Some(e.clone());
    }
    if let Some(n) = api_note {
        rec.note = Some(rec.note.take().map_or(n.clone(), |o| format!("{o}; {n}")));
    }
    rec.stderr_tail = measure::tail(&base.join("stderr.txt"), 1500).or_else(|| {
        measure::tail(&base.join("stdout.txt"), 1500).filter(|_| measured.exit_code != Some(0))
    });

    // ---- server side
    if let (Some(lab), None) = (ctx.lab, t.internet_url) {
        let st = lab.stats(t.scenario)?;
        rec.peak_connections = Some(st.max_client_peak_connections());
        rec.peak_active_requests = Some(st.max_client_peak_requests());
        rec.requests = Some(st.requests);
        rec.status = st.status.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        rec.refused = Some(st.refused);
        rec.resets_injected = Some(st.resets_injected);
        rec.stalls_injected = Some(st.stalls_injected);
        rec.bytes_sent = Some(st.bytes_sent);
        rec.ttfb_s = st
            .first_byte_unix_us
            .map(|fb| fb.saturating_sub(measured.spawn_unix_us) as f64 / 1e6);
        rec.server_transfer_s = match (st.first_request_unix_us, st.last_byte_unix_us) {
            (Some(a), Some(b)) => Some(b.saturating_sub(a) as f64 / 1e6),
            _ => None,
        };
        rec.last_byte_s = st.last_byte_unix_us.map(|lb| {
            let s = lb.saturating_sub(measured.spawn_unix_us) as f64 / 1e6;
            rec.wall_s.map_or(s, |w| s.min(w))
        });
    }

    // ---- output
    let (output, extras, escaped) = find_output(a, &root, &out);
    rec.extra_files = extras;
    match output {
        None => {
            rec.outcome = if timed_out {
                Outcome::Timeout
            } else {
                Outcome::NoOutput
            };
        }
        Some(path) => {
            let rel = path.strip_prefix(&out).unwrap_or(&path);
            let name = rel
                .iter()
                .map(names::escape_name)
                .collect::<Vec<_>>()
                .join("/");
            let size = std::fs::metadata(&path)?.len();
            rec.output_size = Some(size);
            rec.name_check = names::grade(t.scenario, t.file, &name, escaped);
            rec.output_name = Some(name);
            let sha = sys::sha256_file(&path)?;
            rec.outcome = verify(ctx, t, rec, &sha, size)?;
            rec.sha256 = Some(sha);
            if timed_out && rec.outcome != Outcome::Ok {
                rec.outcome = Outcome::Timeout;
            }
        }
    }
    if rec.outcome == Outcome::Ok
        && let (Some(w), Some(s)) = (rec.done_s, rec.output_size)
        && w > 0.0
    {
        rec.throughput_mib_s = Some(s as f64 / (1024.0 * 1024.0) / w);
    }
    let _ = std::fs::remove_dir_all(&base);
    Ok(())
}

/// Checks the bytes. Lab: against labserver's digest of any version up to
/// the current one. Internet: size only here (the report compares hashes
/// across clients).
fn verify(ctx: &mut Ctx, t: &Target, rec: &mut RunRecord, sha: &str, size: u64) -> Result<Outcome> {
    let Some(lab) = ctx.lab.filter(|_| t.internet_url.is_none()) else {
        return Ok(match t.size {
            Some(s) if s != size => Outcome::Incomplete,
            _ if size == 0 => Outcome::Incomplete,
            _ => Outcome::Ok,
        });
    };
    let (current, cur_sha) = lab.digest(t.scenario, t.file, None)?;
    ctx.digests.insert(
        (t.scenario.to_owned(), t.file.to_owned(), current),
        cur_sha.clone(),
    );
    rec.expected_sha256 = Some(cur_sha.clone());
    for v in (0..=current).rev() {
        let key = (t.scenario.to_owned(), t.file.to_owned(), v);
        let expected = match ctx.digests.get(&key) {
            Some(h) => h.clone(),
            None => {
                let (_, h) = lab.digest(t.scenario, t.file, Some(v))?;
                ctx.digests.insert(key, h.clone());
                h
            }
        };
        if expected == sha {
            rec.matched_version = Some(v);
            rec.expected_sha256 = Some(expected);
            return Ok(Outcome::Ok);
        }
    }
    Ok(match t.size {
        Some(s) if s == size => Outcome::Corrupt,
        _ => Outcome::Incomplete,
    })
}

/// The downloaded file: the largest regular file in `out` that is not the
/// client's bookkeeping. Also returns other files found (in `out` or
/// anywhere else under `root`), and whether the chosen file lies outside
/// `out` (a path-traversal escape).
fn find_output(a: &dyn Adapter, root: &Path, out: &Path) -> (Option<PathBuf>, Vec<String>, bool) {
    let mut files = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(ft) if ft.is_dir() => stack.push(p),
                Ok(ft) if ft.is_file() => files.push(p),
                _ => {}
            }
        }
    }
    let is_control = |p: &Path| {
        p.file_name()
            .is_some_and(|n| a.is_control_file(&n.to_string_lossy()))
    };
    let size = |p: &Path| std::fs::metadata(p).map_or(0, |m| m.len());
    let inside: Vec<&PathBuf> = files
        .iter()
        .filter(|p| p.starts_with(out) && !is_control(p))
        .collect();
    let outside: Vec<&PathBuf> = files.iter().filter(|p| !p.starts_with(out)).collect();
    let chosen = inside
        .iter()
        .copied()
        .max_by_key(|p| size(p))
        .or_else(|| outside.iter().copied().max_by_key(|p| size(p)))
        .cloned();
    let escaped = chosen.as_ref().is_some_and(|c| !c.starts_with(out));
    let extras = files
        .iter()
        .filter(|p| Some(*p) != chosen.as_ref())
        .map(|p| {
            let rel = p
                .strip_prefix(out)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| Path::new("<outside>").join(p.strip_prefix(root).unwrap_or(p)));
            rel.to_string_lossy().into_owned()
        })
        .collect();
    (chosen, extras, escaped)
}

/// Whether `out` holds a finished download: a file of the expected size (any
/// size if unknown) and none of the client's control/partial files.
fn final_file_ready(a: &dyn Adapter, out: &Path, size: Option<u64>) -> bool {
    let Ok(rd) = std::fs::read_dir(out) else {
        return false;
    };
    let mut found = false;
    for e in rd.flatten() {
        if a.is_control_file(&e.file_name().to_string_lossy()) {
            return false;
        }
        if let Ok(m) = e.metadata()
            && m.is_file()
            && size.is_none_or(|s| m.len() == s)
        {
            found = true;
        }
    }
    found
}

/// Waits for a [`Completion::FinalFile`] client: polls the download
/// directory every 5 ms; once the file is complete, records the time and
/// stops the lingering process.
fn wait_final_file(
    running: &mut measure::Running,
    a: &dyn Adapter,
    out: &Path,
    size: Option<u64>,
    timeout: Duration,
    sync: &dyn Fn() -> Synced,
) -> Waited {
    let deadline = running.started + timeout;
    loop {
        if let Some(m) = running.wait(Duration::from_millis(5)) {
            // Exited before the poll saw the file: it may still be there.
            return Waited {
                synced: Some(sync()),
                ..Waited::new(m)
            };
        }
        if final_file_ready(a, out, size) {
            let done = unix_us();
            let synced = sync();
            let m = running.stop(Duration::from_secs(5));
            // `completion: final-file` in the record says it was stopped.
            return Waited {
                done_unix_us: Some(done),
                synced: Some(synced),
                ..Waited::new(m)
            };
        }
        if Instant::now() > deadline {
            return Waited {
                timed_out: true,
                ..Waited::new(running.stop(Duration::from_secs(3)))
            };
        }
    }
}

/// How waiting for a client ended.
struct Waited {
    measured: Measured,
    /// When completion was detected, if not at process exit.
    done_unix_us: Option<u64>,
    timed_out: bool,
    note: Option<String>,
    /// The harness's sync of the output, run right after completion.
    synced: Option<Synced>,
}

impl Waited {
    fn new(measured: Measured) -> Self {
        Self {
            measured,
            done_unix_us: None,
            timed_out: false,
            note: None,
            synced: None,
        }
    }
}

/// The harness forcing a finished download to disk.
#[derive(Debug, Clone, PartialEq)]
pub struct Synced {
    pub start_unix_us: u64,
    pub end_unix_us: u64,
    pub error: Option<String>,
}

/// fsyncs the downloaded file and the directories holding it (its own and
/// the download directory), the way a power-safe client does before it
/// reports done: data, size and the directory entry all reach the disk.
/// Directories are synced on Unix only (Windows has no directory fsync in
/// std; NTFS journals the entry).
pub fn sync_output(a: &dyn Adapter, root: &Path, out: &Path) -> Synced {
    let start_unix_us = unix_us();
    let (chosen, _, _) = find_output(a, root, out);
    let error = match chosen {
        None => Some("no output file to sync".to_owned()),
        Some(file) => sync_file_and_dirs(&file, out)
            .err()
            .map(|e| format!("{e:#}")),
    };
    Synced {
        start_unix_us,
        end_unix_us: unix_us(),
        error,
    }
}

fn sync_file_and_dirs(file: &Path, out: &Path) -> Result<()> {
    // fsync needs write access on Windows (FlushFileBuffers); on Unix a
    // read-only descriptor flushes the inode's dirty pages just the same.
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(cfg!(windows))
        .open(file)
        .with_context(|| format!("open {}", file.display()))?;
    f.sync_all()
        .with_context(|| format!("fsync {}", file.display()))?;
    #[cfg(unix)]
    {
        let mut dirs = vec![file.parent().unwrap_or(out).to_owned()];
        if !dirs.contains(&out.to_owned()) {
            dirs.push(out.to_owned());
        }
        for d in dirs {
            std::fs::File::open(&d)
                .and_then(|h| h.sync_all())
                .with_context(|| format!("fsync directory {}", d.display()))?;
        }
    }
    Ok(())
}

/// Drives an API client: waits for its server, submits the download, polls
/// until done, stops the server. Returns the measurement, when the task
/// reported done, whether it timed out, and a note.
fn drive_api(
    running: &mut measure::Running,
    script: &ApiScript,
    timeout: Duration,
    sync: &dyn Fn() -> Synced,
) -> Waited {
    let deadline = running.started + timeout;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false)
        .build()
        .into();
    let ApiScript::Gopeed { base, body } = script;
    let fail = |running: &mut measure::Running, note: String, timed_out: bool| Waited {
        timed_out,
        note: Some(note),
        ..Waited::new(running.stop(Duration::from_secs(5)))
    };

    // 1. Wait for the server.
    loop {
        if let Some(m) = running.poll() {
            return Waited {
                note: Some("API server exited before it was ready".into()),
                ..Waited::new(m)
            };
        }
        if agent
            .get(&format!("{base}/api/v1/info"))
            .call()
            .is_ok_and(|r| r.status() == 200)
        {
            break;
        }
        if Instant::now() > deadline {
            return fail(running, "API server never became ready".into(), true);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // 2. Submit.
    let id = match agent
        .post(&format!("{base}/api/v1/tasks"))
        .send_json(body)
        .map_err(anyhow::Error::from)
        .and_then(|mut r| Ok(r.body_mut().read_json::<serde_json::Value>()?))
    {
        Ok(v) if v["code"] == 0 && v["data"].is_string() => {
            v["data"].as_str().unwrap_or_default().to_owned()
        }
        Ok(v) => return fail(running, format!("task rejected: {v}"), false),
        Err(e) => return fail(running, format!("submitting the task failed: {e:#}"), false),
    };
    // 3. Poll.
    loop {
        if let Some(m) = running.poll() {
            return Waited {
                note: Some("API server exited during the download".into()),
                ..Waited::new(m)
            };
        }
        if let Ok(mut r) = agent.get(&format!("{base}/api/v1/tasks/{id}")).call()
            && let Ok(v) = r.body_mut().read_json::<serde_json::Value>()
        {
            match v["data"]["status"].as_str() {
                Some("done") => {
                    let done = unix_us();
                    let synced = sync();
                    let m = running.stop(Duration::from_secs(5));
                    return Waited {
                        done_unix_us: Some(done),
                        synced: Some(synced),
                        ..Waited::new(m)
                    };
                }
                Some("error") => {
                    let done = unix_us();
                    let m = running.stop(Duration::from_secs(5));
                    return Waited {
                        done_unix_us: Some(done),
                        note: Some("task reported error".into()),
                        ..Waited::new(m)
                    };
                }
                _ => {}
            }
        }
        if Instant::now() > deadline {
            return fail(running, "timed out".into(), true);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ------------------------------------------------------------- manual --

/// Options of `vayu-bench manual`.
#[derive(Debug, Clone)]
pub struct ManualOptions {
    pub client: String,
    pub scenario: String,
    pub file: String,
    pub runs: u32,
    /// Directory the GUI client saves into; the newest file there after a
    /// run is verified.
    pub watch_dir: Option<PathBuf>,
    pub lab_url: Option<String>,
    pub cache_mb: usize,
    pub out_root: PathBuf,
    pub timeout: Duration,
    pub command: Vec<String>,
}

/// Manual mode: serve the file, let a person download it with a GUI
/// client, and measure from labserver's side (first request to last byte).
pub fn manual(opts: &ManualOptions) -> Result<PathBuf> {
    use std::io::BufRead;
    let lab = match &opts.lab_url {
        Some(u) => Lab::connect(u)?,
        None => Lab::start(opts.cache_mb)?,
    };
    let sc = lab
        .scenario(&opts.scenario)
        .with_context(|| format!("no scenario {}", opts.scenario))?
        .clone();
    let size = sc
        .file(&opts.file)
        .with_context(|| format!("no file {}", opts.file))?
        .size;
    let started = unix_us();
    let label = format!(
        "{}-{}-manual-{}",
        sys::date_utc(started),
        sanitize(&sys::hostname()),
        sanitize(&opts.client)
    );
    let mut dir = opts.out_root.join(&label);
    let mut k = 2;
    while dir.exists() {
        dir = opts.out_root.join(format!("{label}-{k}"));
        k += 1;
    }
    std::fs::create_dir_all(&dir)?;
    let adapter = adapter::by_id(&opts.client);
    let meta = Meta {
        runset: dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        started_utc: sys::datetime_utc(started),
        finished_utc: None,
        duration_s: None,
        harness: format!("vayu-bench {}", env!("CARGO_PKG_VERSION")),
        git_commit: None,
        command: opts.command.clone(),
        machine: Machine::detect(),
        load_at_start: sys::loadavg(),
        load_at_end: None,
        mem_available_mib_at_start: sys::meminfo_kib("MemAvailable").map(|k| k / 1024),
        lab: opts.lab_url.clone().unwrap_or_else(|| "in-process".into()),
        lab_cache_mb: opts.lab_url.is_none().then_some(opts.cache_mb),
        internet: false,
        seed: 0,
        runs: opts.runs,
        conn: Conn::Default,
        clients: vec![ClientMeta {
            id: opts.client.clone(),
            tool: opts.client.clone(),
            description: adapter.as_ref().map_or_else(
                || format!("{} (manual)", opts.client),
                |a| a.description().to_owned(),
            ),
            settings: "as configured in the app".into(),
            mode: Mode::Manual,
            program: None,
            version: None,
            binary_sha256: None,
            fetched: None,
        }],
        matrix: vec![MatrixRow {
            scenario: opts.scenario.clone(),
            description: sc.description.clone(),
            files: vec![(opts.file.clone(), size)],
            runs: opts.runs,
            timeout_s: opts.timeout.as_secs(),
            why: "manual".into(),
        }],
        notes: vec!["manual mode: wall time is labserver's first request to last byte".into()],
        guard: None,
        busy_runs: None,
    };
    write_meta(&dir, &meta)?;
    let mut jsonl = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("runs.jsonl"))?;

    // Enter on stdin ends a run early.
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        for _ in std::io::stdin().lock().lines() {
            if tx.send(()).is_err() {
                break;
            }
        }
    });

    for rep in 0..opts.runs {
        lab.wait_idle(&opts.scenario, Duration::from_secs(30))?;
        lab.reset(&opts.scenario)?;
        while rx.try_recv().is_ok() {}
        let url = lab.run_url(&opts.scenario, &opts.file)?;
        let mut rec = RunRecord::new(rep, &opts.client, &opts.scenario, &opts.file, rep);
        rec.size = Some(size);
        eprintln!(
            "\nrun {}/{}: download this URL with {} now:\n\n  {url}\n",
            rep + 1,
            opts.runs,
            opts.client
        );
        for (k, v) in sc.required_headers() {
            eprintln!("  (the request needs the header  {k}: {v})");
        }
        if let Some(w) = &opts.watch_dir {
            eprintln!("  save it into {}", w.display());
        }
        eprintln!("  waiting for the download to start; press Enter to end the run early");
        let t0 = Instant::now();
        let mut finished_at: Option<Instant> = None;
        let st = loop {
            std::thread::sleep(Duration::from_millis(50));
            let st = lab.stats(&opts.scenario)?;
            let complete = st.bytes_sent >= size && st.active_requests == 0;
            if complete {
                let f = *finished_at.get_or_insert_with(Instant::now);
                // Settle: some clients open a last request to verify.
                if f.elapsed() > Duration::from_secs(1) {
                    break st;
                }
            } else {
                finished_at = None;
            }
            if rx.try_recv().is_ok() || t0.elapsed() > opts.timeout {
                rec.timed_out = t0.elapsed() > opts.timeout;
                break st;
            }
        };
        rec.peak_connections = Some(st.max_client_peak_connections());
        rec.peak_active_requests = Some(st.max_client_peak_requests());
        rec.requests = Some(st.requests);
        rec.status = st.status.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        rec.bytes_sent = Some(st.bytes_sent);
        rec.refused = Some(st.refused);
        rec.resets_injected = Some(st.resets_injected);
        rec.stalls_injected = Some(st.stalls_injected);
        if let (Some(a), Some(b)) = (st.first_request_unix_us, st.last_byte_unix_us) {
            rec.wall_s = Some(b.saturating_sub(a) as f64 / 1e6);
            rec.server_transfer_s = rec.wall_s;
            rec.last_byte_s = rec.wall_s;
            rec.done_s = rec.wall_s;
            rec.started_unix_us = a;
        }
        if let (Some(a), Some(b)) = (st.first_request_unix_us, st.first_byte_unix_us) {
            rec.ttfb_s = Some(b.saturating_sub(a) as f64 / 1e6);
        }
        rec.outcome = Outcome::NoOutput;
        if let Some(w) = &opts.watch_dir {
            // The newest file written since the run began.
            let since = std::time::SystemTime::UNIX_EPOCH
                + Duration::from_micros(rec.started_unix_us.saturating_sub(5_000_000));
            let newest = std::fs::read_dir(w)?
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
                .filter(|(m, _)| *m >= since)
                .max_by_key(|(m, _)| *m);
            if let Some((_, p)) = newest {
                let sha = sys::sha256_file(&p)?;
                let out_size = std::fs::metadata(&p)?.len();
                rec.output_name = p.file_name().map(|n| n.to_string_lossy().into_owned());
                rec.output_size = Some(out_size);
                let (cur, _) = lab.digest(&opts.scenario, &opts.file, None)?;
                rec.outcome = if out_size == size {
                    Outcome::Corrupt
                } else {
                    Outcome::Incomplete
                };
                for v in (0..=cur).rev() {
                    let (_, h) = lab.digest(&opts.scenario, &opts.file, Some(v))?;
                    if h == sha {
                        rec.outcome = Outcome::Ok;
                        rec.matched_version = Some(v);
                        rec.expected_sha256 = Some(h);
                        break;
                    }
                }
                rec.name_check = rec
                    .output_name
                    .as_deref()
                    .and_then(|n| names::grade(&opts.scenario, &opts.file, n, false));
                rec.sha256 = Some(sha);
            }
        } else if st.bytes_sent >= size && !rec.timed_out {
            rec.outcome = Outcome::Ok;
            rec.note = Some("bytes not verified (no --watch-dir)".into());
        }
        if rec.outcome == Outcome::Ok
            && let Some(w) = rec.wall_s.filter(|w| *w > 0.0)
        {
            rec.throughput_mib_s = Some(size as f64 / (1024.0 * 1024.0) / w);
        }
        if rec.timed_out && rec.outcome != Outcome::Ok {
            rec.outcome = Outcome::Timeout;
        }
        eprintln!(
            "  -> {} in {} (peak {} connections)",
            rec.outcome.label(),
            rec.wall_s.map_or("?".into(), |w| format!("{w:.3} s")),
            rec.peak_connections.unwrap_or(0)
        );
        append_record(&mut jsonl, &rec)?;
    }
    let mut meta = meta;
    meta.finished_utc = Some(sys::datetime_utc(unix_us()));
    meta.load_at_end = sys::loadavg();
    write_meta(&dir, &meta)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matrix_covers_every_builtin_scenario() {
        let m = default_matrix();
        let names: HashSet<&str> = m.iter().map(|e| e.scenario.as_str()).collect();
        for sc in vayu_labserver::scenario::builtin() {
            assert!(
                names.contains(sc.name.as_str()),
                "{} missing from the matrix",
                sc.name
            );
        }
    }

    #[test]
    fn find_output_ignores_control_files_and_spots_escapes() {
        let base = std::env::temp_dir().join(format!("vayu-bench-find-{}", std::process::id()));
        let (root, out, _) = run_dirs(&base).unwrap();
        std::fs::write(out.join("f.bin"), vec![0u8; 10]).unwrap();
        std::fs::write(out.join("f.bin.aria2"), vec![0u8; 100]).unwrap();
        let a = adapter::Aria2::aria2c();
        let (o, extra, esc) = find_output(&a, &root, &out);
        assert_eq!(o.unwrap(), out.join("f.bin"));
        assert_eq!(extra, vec!["f.bin.aria2".to_string()]);
        assert!(!esc);

        std::fs::remove_file(out.join("f.bin")).unwrap();
        std::fs::create_dir_all(root.join("a/etc")).unwrap();
        std::fs::write(root.join("a/etc/passwd"), b"x").unwrap();
        let (o, _, esc) = find_output(&a, &root, &out);
        assert_eq!(o.unwrap(), root.join("a/etc/passwd"));
        assert!(esc, "a file outside out/ is an escape");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn idle_guard_verdicts() {
        let g = IdleGuard {
            max_load: Some(4.0),
            max_busy_cpus: Some(1.0),
            wait: false,
            wait_timeout_s: 1,
            sample_ms: 10,
        };
        let c = |l, b| GuardCheck {
            load1: l,
            busy_cpus: b,
        };
        assert_eq!(g.verdict(&c(Some(3.9), Some(0.9))), None);
        assert_eq!(g.verdict(&c(None, None)), None, "unknown is not busy");
        assert_eq!(
            g.verdict(&c(Some(5.5), Some(0.2))).unwrap(),
            "1-min load 5.50 > 4.00"
        );
        assert_eq!(
            g.verdict(&c(Some(6.0), Some(2.25))).unwrap(),
            "1-min load 6.00 > 4.00, other processes busy on 2.25 CPUs > 1.00"
        );
        let off = IdleGuard {
            max_load: None,
            max_busy_cpus: None,
            ..g.clone()
        };
        assert_eq!(off.verdict(&c(Some(99.0), Some(8.0))), None);

        // A threshold nothing can meet: the run is marked busy, not stalled.
        let strict = IdleGuard {
            max_load: Some(-1.0),
            ..g
        };
        let mut rec = RunRecord::new(0, "curl", "clean", "1m.bin", 0);
        strict.before_run(&mut rec, true);
        if sys::loadavg().is_some() {
            assert!(rec.machine_busy);
            assert!(rec.idle_wait_s.is_none(), "warn-only does not wait");
            // Waiting with no time left gives up at once and marks the run.
            let waiting = IdleGuard {
                wait: true,
                wait_timeout_s: 0,
                ..strict.clone()
            };
            let mut rec = RunRecord::new(1, "curl", "clean", "1m.bin", 0);
            waiting.before_run(&mut rec, true);
            assert!(rec.machine_busy);
        }
        let d = IdleGuard::default();
        assert!(d.max_load.unwrap() >= 0.5 && !d.wait);
    }

    #[test]
    fn sync_forces_the_output_to_disk() {
        let base = std::env::temp_dir().join(format!("vayu-bench-sync-{}", std::process::id()));
        let (root, out, _) = run_dirs(&base).unwrap();
        let a = adapter::Aria2::aria2c();
        let s = sync_output(&a, &root, &out);
        assert_eq!(s.error.as_deref(), Some("no output file to sync"));
        std::fs::write(out.join("f.bin"), vec![7u8; 1 << 20]).unwrap();
        std::fs::write(out.join("f.bin.aria2"), b"control").unwrap();
        let s = sync_output(&a, &root, &out);
        assert_eq!(s.error, None, "the download itself, not the control file");
        assert!(s.end_unix_us >= s.start_unix_us);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn durable_time_falls_back_and_says_so() {
        let mut r = RunRecord::new(0, "curl", "clean", "1m.bin", 0);
        assert_eq!(r.durable_time(), None);
        r.wall_s = Some(0.5);
        r.done_s = Some(0.4);
        assert_eq!(
            r.durable_time(),
            Some((0.4, true)),
            "older records: time to file"
        );
        r.durable_s = Some(0.45);
        assert_eq!(r.durable_time(), Some((0.45, false)));
        // Old records (no durable fields) still parse.
        let mut v = serde_json::to_value(&r).unwrap();
        let o = v.as_object_mut().unwrap();
        o.remove("durable_s");
        o.remove("sync_s");
        let old: RunRecord = serde_json::from_value(v).unwrap();
        assert_eq!(old.durable_s, None);
    }

    #[test]
    fn final_file_detection() {
        let base = std::env::temp_dir().join(format!("vayu-bench-final-{}", std::process::id()));
        let (_, out, _) = run_dirs(&base).unwrap();
        let s = adapter::Surge;
        assert!(!final_file_ready(&s, &out, Some(4)), "empty");
        std::fs::write(out.join("f.bin.surge"), b"ab").unwrap();
        assert!(!final_file_ready(&s, &out, Some(4)), "partial only");
        std::fs::write(out.join("f.bin"), b"abcd").unwrap();
        assert!(
            !final_file_ready(&s, &out, Some(4)),
            "control file still there"
        );
        std::fs::remove_file(out.join("f.bin.surge")).unwrap();
        assert!(final_file_ready(&s, &out, Some(4)));
        assert!(!final_file_ready(&s, &out, Some(5)), "wrong size");
        assert!(final_file_ready(&s, &out, None));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn records_round_trip() {
        let mut r = RunRecord::new(3, "curl", "clean", "1m.bin", 2);
        r.outcome = Outcome::Corrupt;
        r.name_check = Some(NameCheck::UrlName);
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"outcome\":\"corrupt\""));
        assert!(s.contains("\"name_check\":\"url-name\""));
        let back: RunRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(
            back.key(),
            ("curl".into(), "clean".into(), "1m.bin".into(), 2)
        );
        let mut t = back;
        t.wall_s = Some(4.0);
        assert_eq!(t.time_s(), Some(4.0));
        t.last_byte_s = Some(0.1);
        assert_eq!(
            t.time_s(),
            Some(4.0),
            "server-side last byte is not completion"
        );
        t.done_s = Some(0.2);
        assert_eq!(t.time_s(), Some(0.2), "completion wins over exit");
    }
}
