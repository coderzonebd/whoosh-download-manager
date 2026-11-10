//! `vayu-bench` command line. See crates/bench/README.md.

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use vayu_bench::adapter::{self, Adapter, Conn, Mode};
use vayu_bench::{fetch, measure, report, runner};

#[derive(Debug, Parser)]
#[command(
    name = "vayu-bench",
    version,
    about = "Benchmarks download managers against vayu-labserver: speed, politeness, correctness"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Run the benchmark matrix and write a run set (meta.json + runs.jsonl).
    Run(RunArgs),
    /// Render a run set as Markdown.
    Report {
        /// Run-set directory (docs/bench/data/<date>-<host>).
        dir: PathBuf,
        /// Write here instead of stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Download pinned competitor builds from their official GitHub releases
    /// (opt-in; never run automatically).
    Fetch {
        /// Tools to fetch (default: all pinned for this platform).
        tools: Vec<String>,
        /// Accept an asset whose release publishes no checksum.
        #[arg(long)]
        allow_unverified: bool,
        /// List the pins and what is installed, fetch nothing.
        #[arg(long)]
        list: bool,
        /// Fetch for another platform (e.g. windows-x86_64).
        #[arg(long)]
        platform: Option<String>,
    },
    /// List the adapters and whether each can run here.
    List(BinArgs),
    /// Manual mode for GUI-only clients: serve a file, a person downloads it,
    /// measure from labserver's side.
    Manual(ManualArgs),
    /// Crash test (Phase 1 gate): SIGKILL the real vayu daemon at random
    /// moments while it downloads from labserver, restart it, and verify
    /// every finished file byte for byte. Exits 1 on any corrupt, duplicate
    /// or unfinished download.
    Hardkill(HardkillArgs),
    /// Internal: measurement wrapper around one client process.
    #[command(name = "__measure", hide = true)]
    Measure {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
}

#[derive(Debug, Args)]
struct BinArgs {
    /// Use this executable for a tool: --bin aria2c=/opt/aria2/aria2c
    #[arg(long = "bin", value_name = "TOOL=PATH")]
    bins: Vec<String>,
}

#[derive(Debug, Args)]
struct RunArgs {
    /// Comma-separated adapter ids (default: every installed CLI/API client).
    #[arg(long, value_delimiter = ',')]
    clients: Vec<String>,
    /// Comma-separated scenarios to keep from the matrix.
    #[arg(long, value_delimiter = ',')]
    scenarios: Vec<String>,
    /// Override the files for every selected scenario (comma-separated).
    #[arg(long, value_delimiter = ',')]
    files: Vec<String>,
    /// Repetitions per client × scenario × file.
    #[arg(long, default_value_t = 7)]
    runs: u32,
    /// Also override per-scenario repetition counts with --runs.
    #[arg(long)]
    force_runs: bool,
    /// Seed of the randomised order (default: from the clock; recorded).
    #[arg(long)]
    seed: Option<u64>,
    /// Connections: `default` (each client's own) or a number.
    #[arg(long, default_value = "default")]
    connections: String,
    /// Small smoke-test matrix.
    #[arg(long)]
    quick: bool,
    /// Use a running labserver instead of starting one (same host).
    #[arg(long)]
    lab_url: Option<String>,
    /// labserver content cache for the in-process server, MiB.
    #[arg(long, default_value_t = 256)]
    cache_mb: usize,
    /// Multiply every per-run timeout.
    #[arg(long, default_value_t = 1.0)]
    timeout_scale: f64,
    /// Where run sets go.
    #[arg(long, default_value = "docs/bench/data")]
    out: PathBuf,
    /// Run-set directory name (default <date>-<host>).
    #[arg(long)]
    label: Option<String>,
    /// Continue an interrupted run set (same seed and matrix).
    #[arg(long)]
    resume: Option<PathBuf>,
    /// Scratch space for downloads; use a real disk (default
    /// ~/.cache/vayu-bench/work).
    #[arg(long)]
    work_dir: Option<PathBuf>,
    /// Download public test files over the real internet instead of from
    /// labserver. Off by default; user-run only. Uses bandwidth on
    /// third-party servers.
    #[arg(long)]
    real_internet: bool,
    /// Real-internet target, NAME=URL (repeatable; replaces the defaults).
    #[arg(long = "url", value_name = "NAME=URL")]
    urls: Vec<String>,
    #[arg(long)]
    quiet: bool,
    #[command(flatten)]
    bins: BinArgs,
}

#[derive(Debug, Args)]
struct HardkillArgs {
    /// Stop after this many kills.
    #[arg(long, default_value_t = 1000)]
    kills: u32,
    /// Seed of the kill schedule and download mix (default: from the clock).
    #[arg(long)]
    seed: Option<u64>,
    /// The `vayu` CLI to test; `vayu-daemon` must be next to it.
    #[arg(long, default_value = "target/release/vayu")]
    vayu: PathBuf,
    /// Downloads running at once in each round.
    #[arg(long, default_value_t = 3)]
    concurrent: usize,
    /// Longest wait between a daemon start and its kill, milliseconds (keep
    /// it above the engine's 2 s checkpoint interval).
    #[arg(long, default_value_t = 4000)]
    max_delay_ms: u64,
    /// Kills per round at most; then the round finishes undisturbed.
    #[arg(long, default_value_t = 4)]
    max_kills_per_round: u32,
    /// Scratch space; use a real disk (default ~/.cache/vayu-bench/work).
    #[arg(long)]
    work_dir: Option<PathBuf>,
    #[arg(long)]
    lab_url: Option<String>,
    #[arg(long, default_value_t = 256)]
    cache_mb: usize,
    /// Write the summary (JSON) here as well.
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long)]
    quiet: bool,
}

#[derive(Debug, Args)]
struct ManualArgs {
    /// Client label (abdm, fdm, idm-gui, …).
    #[arg(long)]
    client: String,
    #[arg(long, default_value = "clean")]
    scenario: String,
    #[arg(long, default_value = "100m.bin")]
    file: String,
    #[arg(long, default_value_t = 3)]
    runs: u32,
    /// Folder the client saves into (to verify the bytes).
    #[arg(long)]
    watch_dir: Option<PathBuf>,
    #[arg(long)]
    lab_url: Option<String>,
    #[arg(long, default_value_t = 256)]
    cache_mb: usize,
    /// Give up on a run after this many seconds.
    #[arg(long, default_value_t = 600)]
    timeout: u64,
    #[arg(long, default_value = "docs/bench/data")]
    out: PathBuf,
}

fn cmd_hardkill(a: HardkillArgs) -> Result<()> {
    let lab = match &a.lab_url {
        Some(url) => vayu_bench::lab::Lab::connect(url)?,
        None => vayu_bench::lab::Lab::start(a.cache_mb)?,
    };
    let seed = a.seed.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    });
    let opts = vayu_bench::hardkill::Options {
        kills: a.kills,
        seed,
        vayu: std::path::absolute(&a.vayu)?,
        work_dir: a
            .work_dir
            .unwrap_or_else(|| fetch::cache_dir().join("work")),
        concurrent: a.concurrent,
        max_delay: std::time::Duration::from_millis(a.max_delay_ms),
        max_kills_per_round: a.max_kills_per_round,
        round_timeout: std::time::Duration::from_secs(600),
        quiet: a.quiet,
    };
    let summary = vayu_bench::hardkill::run(&lab, &opts)?;
    let json = serde_json::to_string_pretty(&summary)?;
    println!("{json}");
    if let Some(p) = &a.output {
        std::fs::write(p, &json)?;
    }
    if !summary.passed() {
        anyhow::bail!(
            "hardkill FAILED: {} corrupt, {} with leftovers, {} unfinished",
            summary.corrupt,
            summary.leftovers,
            summary.unfinished
        );
    }
    eprintln!(
        "hardkill passed: {} kills, {} downloads verified, 0 corrupt",
        summary.kills, summary.verified
    );
    Ok(())
}

fn parse_bins(b: &BinArgs) -> Result<BTreeMap<String, PathBuf>> {
    b.bins
        .iter()
        .map(|s| {
            let (k, v) = s.split_once('=').context("--bin expects TOOL=PATH")?;
            Ok((k.to_owned(), PathBuf::from(v)))
        })
        .collect()
}

fn main() -> Result<()> {
    // The wrapper must not pay for clap: parse its arguments by hand.
    let raw: Vec<OsString> = std::env::args_os().collect();
    if raw.get(1).is_some_and(|a| a == "__measure") {
        return measure::shim_main(measure::parse_shim_args(&raw[2..])?);
    }
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Run(a) => cmd_run(a, &raw),
        Cmd::Hardkill(a) => cmd_hardkill(a),
        Cmd::Report { dir, output } => {
            let meta = runner::read_meta(&dir)?;
            let runs = runner::read_runs(&dir)?;
            let md = report::render(&meta, &runs);
            match output {
                Some(p) => {
                    std::fs::write(&p, md)?;
                    eprintln!("wrote {}", p.display());
                }
                None => print!("{md}"),
            }
            Ok(())
        }
        Cmd::Fetch {
            tools,
            allow_unverified,
            list,
            platform,
        } => {
            let cache = fetch::cache_dir();
            if list {
                println!("pinned builds:");
                for p in fetch::PINS {
                    println!(
                        "  {:<11} {:<7} {:<15} {}  ({})",
                        p.tool,
                        p.version,
                        p.platform,
                        p.asset,
                        p.checksum_asset.unwrap_or("GitHub asset digest")
                    );
                }
                println!("installed under {}:", cache.join("tools").display());
                for t in fetch::installed(&cache) {
                    println!(
                        "  {} {} ({}, verified: {})",
                        t.tool, t.version, t.asset, t.verified_by
                    );
                }
                return Ok(());
            }
            let opts = fetch::FetchOptions {
                tools,
                allow_unverified,
                cache,
                platform: platform.unwrap_or_else(fetch::platform),
            };
            for t in fetch::fetch(&opts)? {
                println!(
                    "{} {}: {}",
                    t.tool,
                    t.version,
                    t.binaries
                        .values()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            Ok(())
        }
        Cmd::List(b) => {
            let bins = parse_bins(&b)?;
            let cache = fetch::cache_dir();
            for a in adapter::all() {
                if a.mode() == Mode::Manual {
                    println!("{:<18} manual   {}", a.id(), a.description());
                    continue;
                }
                let m = runner::client_meta(a.as_ref(), &bins, &cache);
                println!(
                    "{:<18} {:<8} {}\n{:<18} {:<8} {}",
                    a.id(),
                    match a.mode() {
                        Mode::Cli => "cli",
                        Mode::Api => "api",
                        Mode::Manual => "manual",
                    },
                    a.description(),
                    "",
                    "",
                    match (&m.program, &m.version) {
                        (Some(p), v) =>
                            format!("{} ({})", p.display(), v.as_deref().unwrap_or("?")),
                        (None, _) => "not installed".into(),
                    }
                );
            }
            Ok(())
        }
        Cmd::Manual(a) => {
            let dir = runner::manual(&runner::ManualOptions {
                client: a.client,
                scenario: a.scenario,
                file: a.file,
                runs: a.runs,
                watch_dir: a.watch_dir,
                lab_url: a.lab_url,
                cache_mb: a.cache_mb,
                out_root: a.out,
                timeout: Duration::from_secs(a.timeout),
                command: raw
                    .iter()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect(),
            })?;
            eprintln!("wrote {}", dir.display());
            Ok(())
        }
        Cmd::Measure { args } => measure::shim_main(measure::parse_shim_args(&args)?),
    }
}

fn cmd_run(a: RunArgs, raw: &[OsString]) -> Result<()> {
    let bins = parse_bins(&a.bins)?;
    let clients: Vec<Arc<dyn Adapter>> = if a.clients.is_empty() {
        adapter::all()
            .into_iter()
            .filter(|c| {
                c.mode() != Mode::Manual && c.tool() != "idm" || cfg!(windows) && c.tool() == "idm"
            })
            .collect()
    } else {
        a.clients
            .iter()
            .map(|id| {
                adapter::by_id(id)
                    .with_context(|| format!("unknown client {id:?} (see `vayu-bench list`)"))
            })
            .collect::<Result<_>>()?
    };
    let mut matrix = if a.quick {
        runner::quick_matrix()
    } else {
        runner::default_matrix()
    };
    if !a.scenarios.is_empty() {
        for s in &a.scenarios {
            if !matrix.iter().any(|e| &e.scenario == s) {
                // A scenario outside the default matrix (e.g. a custom one on
                // --lab-url): default to 10m.bin.
                matrix.push(runner::MatrixEntry {
                    scenario: s.clone(),
                    files: vec!["10m.bin".into()],
                    runs: None,
                    timeout_s: 60,
                    why: "selected on the command line".into(),
                });
            }
        }
        matrix.retain(|e| a.scenarios.contains(&e.scenario));
    }
    if !a.files.is_empty() {
        for e in &mut matrix {
            e.files = a.files.clone();
        }
    }
    if a.force_runs {
        for e in &mut matrix {
            e.runs = Some(a.runs);
        }
    }
    let conn = match a.connections.as_str() {
        "default" => Conn::Default,
        n => Conn::Fixed(
            n.parse()
                .context("--connections takes `default` or a number")?,
        ),
    };
    let internet = if a.real_internet {
        let targets = if a.urls.is_empty() {
            runner::internet_targets()
        } else {
            a.urls
                .iter()
                .map(|s| {
                    let (n, u) = s.split_once('=').context("--url expects NAME=URL")?;
                    Ok((n.to_owned(), u.to_owned()))
                })
                .collect::<Result<_>>()?
        };
        eprintln!(
            "real-internet mode: {} public file(s) × {} client(s) × {} runs; this uses \
             third-party bandwidth and the numbers depend on your connection",
            targets.len(),
            clients.len(),
            a.runs
        );
        Some(targets)
    } else {
        if !a.urls.is_empty() {
            bail!("--url needs --real-internet");
        }
        None
    };
    let seed = a.seed.unwrap_or_else(|| measure::unix_us() % 1_000_000);
    let work_dir = a
        .work_dir
        .unwrap_or_else(|| fetch::cache_dir().join("work"));
    let dir = runner::run(&runner::RunOptions {
        clients,
        matrix,
        runs: a.runs,
        seed,
        conn,
        out_root: a.out,
        label: a.label,
        resume: a.resume,
        work_dir,
        lab_url: a.lab_url,
        cache_mb: a.cache_mb,
        timeout_scale: a.timeout_scale,
        bins,
        tool_cache: fetch::cache_dir(),
        shim_exe: std::env::current_exe()?,
        internet,
        command: raw
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect(),
        quiet: a.quiet,
    })?;
    eprintln!("wrote {}", dir.display());
    eprintln!("report: vayu-bench report {}", dir.display());
    Ok(())
}
