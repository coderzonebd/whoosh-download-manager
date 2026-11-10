//! `vayu-bench hardkill`: the Phase 1 gate's crash test, on real processes.
//!
//! Downloads from labserver through the real daemon (vayu-engine over
//! vayu-net and vayu-store), SIGKILLs the daemon at random moments (during
//! start-up and probing, mid-transfer, around the final rename), starts it
//! again and lets the engine resume on its own. Every finished file must
//! equal labserver's SHA-256 byte for byte, with no duplicate (`name (1)`)
//! and no part file left behind. Unix only (SIGKILL).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::lab::Lab;
use crate::sys::sha256_file;

/// Scenario × file pairs, chosen to keep transfers in flight long enough to
/// be killed (throttles, slow drip), to exercise resume without validators
/// (restart from zero) and without ranges (stream mode), and to kill around
/// the end of tiny files.
pub const MIX: &[(&str, &str)] = &[
    ("global-throttle", "100m.bin"),
    ("per-conn-throttle", "100m.bin"),
    ("slow-drip", "10m.bin"),
    ("flaky", "1g.bin"),
    ("no-validators", "100m.bin"),
    ("no-range", "100m.bin"),
    ("clean", "100m.bin"),
    ("clean", "1m.bin"),
    ("clean", "empty.bin"),
];

#[derive(Debug, Clone)]
pub struct Options {
    /// Stop after this many kills (the current round still finishes).
    pub kills: u32,
    pub seed: u64,
    /// The `vayu` CLI; `vayu-daemon` must be next to it.
    pub vayu: PathBuf,
    pub work_dir: PathBuf,
    /// Downloads running at once in each round.
    pub concurrent: usize,
    /// Longest wait between a daemon start and its kill. Keep it above the
    /// engine's checkpoint interval (2 s), or downloads that can only
    /// resume from a checkpoint never make progress.
    pub max_delay: Duration,
    /// Kills per round at most (1..=this, random); then the round's
    /// downloads finish undisturbed. Downloads that cannot resume by design
    /// (no ranges, no validators) restart from zero after every kill, so an
    /// unbounded kill loop would never let them finish.
    pub max_kills_per_round: u32,
    /// A round that has not finished after this long counts as failed.
    pub round_timeout: Duration,
    pub quiet: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub seed: u64,
    pub kills: u32,
    pub rounds: u32,
    pub downloads: u32,
    pub verified: u32,
    /// Finished file whose bytes differ from labserver's.
    pub corrupt: u32,
    /// More than one file, or a part file, left in a download's folder.
    pub leftovers: u32,
    /// Did not finish within the round timeout (or ended failed).
    pub unfinished: u32,
    pub elapsed_s: f64,
    /// Downloads and kills per `scenario/file`.
    pub per_case: BTreeMap<String, CaseStats>,
    pub problems: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct CaseStats {
    pub downloads: u32,
    pub kills_during: u32,
    pub ok: u32,
}

impl Summary {
    pub fn passed(&self) -> bool {
        self.corrupt == 0 && self.leftovers == 0 && self.unfinished == 0 && self.verified > 0
    }
}

struct Daemon {
    child: Child,
}

struct Paths {
    daemon: PathBuf,
    data_dir: PathBuf,
    log: PathBuf,
}

impl Daemon {
    fn start(p: &Paths) -> Result<Self> {
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p.log)?;
        let child = Command::new(&p.daemon)
            .args(["--port", "0"])
            .env("VAYU_DATA_DIR", &p.data_dir)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .with_context(|| format!("start {}", p.daemon.display()))?;
        let mut d = Self { child };
        d.wait_ready(&p.data_dir)?;
        Ok(d)
    }

    /// Waits for a runtime file written by this process (a killed daemon
    /// leaves its own behind).
    fn wait_ready(&mut self, data_dir: &Path) -> Result<()> {
        let pid = self.child.id();
        let t0 = Instant::now();
        loop {
            if let Ok(bytes) = fs::read(data_dir.join("runtime.json"))
                && let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && v["pid"].as_u64() == Some(u64::from(pid))
            {
                return Ok(());
            }
            if let Some(status) = self.child.try_wait()? {
                bail!("vayu-daemon exited during start-up: {status}");
            }
            if t0.elapsed() > Duration::from_secs(30) {
                bail!("vayu-daemon did not become ready in 30 s");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn kill(mut self) -> Result<()> {
        self.child.kill()?; // SIGKILL
        self.child.wait()?;
        Ok(())
    }
}

fn vayu(opts: &Options, data_dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new(&opts.vayu)
        .args(["--no-start", "--json"])
        .args(args)
        .env("VAYU_DATA_DIR", data_dir)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("run {}", opts.vayu.display()))?;
    if !out.status.success() {
        bail!(
            "vayu {}: {}: {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

struct Pending {
    case: String,
    file: String,
    dir: PathBuf,
    expected: String,
    id: String,
}

/// Done when the folder holds the final file and no part file.
fn finished(p: &Pending) -> bool {
    let Ok(entries) = fs::read_dir(&p.dir) else {
        return false;
    };
    let names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.contains(&p.file) && !names.iter().any(|n| n.ends_with(".vayu-part"))
}

pub fn run(lab: &Lab, opts: &Options) -> Result<Summary> {
    if !cfg!(unix) {
        bail!("hardkill needs Unix (SIGKILL)");
    }
    let daemon = opts.vayu.with_file_name("vayu-daemon");
    if !daemon.is_file() {
        bail!("{} not found next to vayu", daemon.display());
    }
    let root = opts.work_dir.join(format!("hardkill-{}", opts.seed));
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    fs::create_dir_all(&root)?;
    let paths = Paths {
        daemon,
        data_dir: root.join("data"),
        log: root.join("daemon.log"),
    };

    let mut rng = fastrand::Rng::with_seed(opts.seed);
    let mut s = Summary {
        seed: opts.seed,
        ..Summary::default()
    };
    let t0 = Instant::now();
    let mut d = Daemon::start(&paths)?;

    while s.kills < opts.kills {
        s.rounds += 1;
        let round_dir = root.join(format!("round-{}", s.rounds));
        let mut pending = Vec::new();
        for k in 0..opts.concurrent.max(1) {
            let (scenario, file) = MIX[rng.usize(..MIX.len())];
            let dir = round_dir.join(format!("d{k}"));
            fs::create_dir_all(&dir)?;
            let url = lab.run_url(scenario, file)?;
            let (_, expected) = lab.digest(scenario, file, None)?;
            let out = vayu(
                opts,
                &paths.data_dir,
                &["add", &url, "-o", &dir.to_string_lossy()],
            )?;
            let v: serde_json::Value = serde_json::from_str(&out).context("parse vayu add")?;
            let id = v[0]["id"]
                .as_str()
                .with_context(|| format!("vayu add answered {out}"))?
                .to_owned();
            let case = format!("{scenario}/{file}");
            s.per_case.entry(case.clone()).or_default().downloads += 1;
            s.downloads += 1;
            pending.push(Pending {
                case,
                file: file.to_owned(),
                dir,
                expected,
                id,
            });
        }

        let round_start = Instant::now();
        let budget = rng.u32(1..=opts.max_kills_per_round.max(1));
        let mut round_kills = 0;
        while round_kills < budget && s.kills < opts.kills {
            // Kill early sometimes (start-up, probe, preallocation), else
            // anywhere up to max_delay.
            let delay = if rng.u8(..5) == 0 {
                Duration::from_millis(rng.u64(0..30))
            } else {
                Duration::from_millis(rng.u64(0..=opts.max_delay.as_millis() as u64))
            };
            let until = Instant::now() + delay;
            while Instant::now() < until && !pending.iter().all(finished) {
                std::thread::sleep(Duration::from_millis(2));
            }
            if pending.iter().all(finished) || round_start.elapsed() > opts.round_timeout {
                break;
            }
            for p in pending.iter().filter(|p| !finished(p)) {
                s.per_case.entry(p.case.clone()).or_default().kills_during += 1;
            }
            d.kill()?;
            s.kills += 1;
            round_kills += 1;
            d = Daemon::start(&paths)?;
            if !opts.quiet && s.kills.is_multiple_of(50) {
                eprintln!(
                    "hardkill: {} kills, {} rounds, {} verified, {:.0} s",
                    s.kills,
                    s.rounds,
                    s.verified,
                    t0.elapsed().as_secs_f64()
                );
            }
        }
        // Let the last attempt finish without kills.
        while !pending.iter().all(finished) && round_start.elapsed() < opts.round_timeout {
            std::thread::sleep(Duration::from_millis(5));
        }

        let mut ids = Vec::new();
        for p in &pending {
            ids.push(p.id.clone());
            if !finished(p) {
                s.unfinished += 1;
                let info = vayu(opts, &paths.data_dir, &["info", &p.id]).unwrap_or_default();
                s.problems.push(format!(
                    "round {}: {} unfinished: {}",
                    s.rounds,
                    p.case,
                    info.trim()
                ));
                continue;
            }
            let names: Vec<String> = fs::read_dir(&p.dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            if names.len() != 1 {
                s.leftovers += 1;
                s.problems
                    .push(format!("round {}: {} left {names:?}", s.rounds, p.case));
            }
            let got = sha256_file(&p.dir.join(&p.file))?;
            if got == p.expected {
                s.verified += 1;
                s.per_case.entry(p.case.clone()).or_default().ok += 1;
            } else {
                s.corrupt += 1;
                s.problems.push(format!(
                    "round {}: {} CORRUPT: sha256 {got}, want {}",
                    s.rounds, p.case, p.expected
                ));
            }
        }
        // Forget the round (keep the list small) and free the disk.
        let mut args = vec!["rm"];
        args.extend(ids.iter().map(String::as_str));
        let _ = vayu(opts, &paths.data_dir, &args);
        fs::remove_dir_all(&round_dir)?;
    }

    let _ = vayu(opts, &paths.data_dir, &["daemon", "stop"]);
    let _ = d.child.wait();
    s.elapsed_s = t0.elapsed().as_secs_f64();
    Ok(s)
}
