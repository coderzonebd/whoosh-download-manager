//! Running a client and measuring it.
//!
//! The harness never spawns a client directly. It spawns itself as a small
//! wrapper (`whoosh-bench __measure`), which spawns the client, waits for it
//! and then reads `getrusage(RUSAGE_CHILDREN)`. Because the wrapper is a
//! fresh process with exactly one child, that is the client's own CPU time
//! (user + system, including any helper processes the client waited for)
//! and its peak resident set size — per run, which the harness could not
//! get from its own `RUSAGE_CHILDREN` (peak RSS there is a running maximum
//! over every child ever). The wrapper also takes the wall-clock time from
//! just before `spawn` to just after the exit, and prints one JSON line.
//!
//! To stop a client (an API server whose download finished, or a run that
//! timed out) the harness writes `term` to the wrapper's stdin; the wrapper
//! sends the client SIGTERM. If that does not work within a grace period,
//! the harness kills the wrapper's whole process group.
//!
//! On Windows the wrapper measures wall time and exit status only.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::adapter::Invocation;

/// What the wrapper reports about one client process.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Measured {
    /// Wall clock (Unix microseconds) just before the client was spawned.
    pub spawn_unix_us: u64,
    /// Spawn to exit.
    pub wall_us: u64,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub cpu_user_us: Option<u64>,
    pub cpu_sys_us: Option<u64>,
    pub max_rss_kib: Option<u64>,
    /// Set when the client could not be started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_error: Option<String>,
}

pub fn unix_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

// ------------------------------------------------------------ wrapper --

/// Arguments of the hidden `__measure` subcommand.
#[derive(Debug, Clone)]
pub struct ShimArgs {
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    pub program: OsString,
    pub args: Vec<OsString>,
}

/// Body of `whoosh-bench __measure`: run the client, print a [`Measured`]
/// line on stdout. Always exits 0 itself unless it cannot report.
pub fn shim_main(a: ShimArgs) -> Result<()> {
    let out = std::fs::File::create(&a.stdout)
        .with_context(|| format!("create {}", a.stdout.display()))?;
    let err = std::fs::File::create(&a.stderr)
        .with_context(|| format!("create {}", a.stderr.display()))?;
    let spawn_unix_us = unix_us();
    let t0 = Instant::now();
    let child = Command::new(&a.program)
        .args(&a.args)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn();
    let mut m = Measured {
        spawn_unix_us,
        ..Measured::default()
    };
    match child {
        Err(e) => {
            m.spawn_error = Some(format!("{}: {e}", PathBuf::from(&a.program).display()));
        }
        Ok(child) => {
            let status = wait_controlled(child)?;
            m.wall_us = t0.elapsed().as_micros() as u64;
            m.exit_code = status.code();
            m.signal = exit_signal(&status);
            fill_rusage(&mut m);
        }
    }
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &m)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

#[cfg(unix)]
fn exit_signal(s: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    s.signal()
}

#[cfg(not(unix))]
fn exit_signal(_: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// Waits for `child` while obeying `term` lines on our stdin (EOF on stdin
/// means the harness is gone: kill the child).
#[cfg(unix)]
fn wait_controlled(mut child: Child) -> Result<std::process::ExitStatus> {
    use nix::sys::signal::{Signal, kill};
    #[cfg(target_os = "linux")]
    use nix::sys::wait::{Id, WaitPidFlag, waitid};
    use nix::unistd::Pid;
    use std::sync::{Arc, Mutex};

    let pid = Pid::from_raw(child.id() as i32);
    // `exited` is set, under the lock, after the child has exited but
    // before it is reaped: until then the pid cannot be reused, so the
    // signalling thread never hits an unrelated process.
    let exited = Arc::new(Mutex::new(false));
    let ex = exited.clone();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut lines = stdin.lock().lines();
        loop {
            let sig = match lines.next() {
                Some(Ok(l)) if l.trim() == "term" => Signal::SIGTERM,
                Some(Ok(_)) => continue,
                _ => Signal::SIGKILL,
            };
            let done = ex.lock().unwrap_or_else(|e| e.into_inner());
            if !*done {
                let _ = kill(pid, sig);
            }
            if sig == Signal::SIGKILL {
                return;
            }
        }
    });
    // Linux: wait for exit without reaping.
    #[cfg(target_os = "linux")]
    {
        loop {
            match waitid(Id::Pid(pid), WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT) {
                Ok(_) => break,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => bail!("waitid: {e}"),
            }
        }
        let mut done = exited.lock().unwrap_or_else(|e| e.into_inner());
        *done = true;
        let status = child.wait()?;
        drop(done);
        Ok(status)
    }
    // Elsewhere (no `waitid` in nix for macOS and the BSDs): poll, reaping
    // only while holding the lock, so the signalling thread still never
    // signals a reaped (reusable) pid.
    #[cfg(not(target_os = "linux"))]
    loop {
        let mut done = exited.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(status) = child.try_wait()? {
            *done = true;
            return Ok(status);
        }
        drop(done);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[cfg(not(unix))]
fn wait_controlled(mut child: Child) -> Result<std::process::ExitStatus> {
    use std::sync::mpsc::TryRecvError;
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) if l.trim() == "term" => {
                    let _ = tx.send(());
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let _ = tx.send(());
    });
    loop {
        if let Some(s) = child.try_wait()? {
            return Ok(s);
        }
        match rx.try_recv() {
            Ok(()) => {
                let _ = child.kill();
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {}
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(unix)]
fn fill_rusage(m: &mut Measured) {
    use nix::sys::resource::{UsageWho, getrusage};
    if let Ok(u) = getrusage(UsageWho::RUSAGE_CHILDREN) {
        let us = |t: nix::sys::time::TimeVal| (t.tv_sec() as u64) * 1_000_000 + t.tv_usec() as u64;
        m.cpu_user_us = Some(us(u.user_time()));
        m.cpu_sys_us = Some(us(u.system_time()));
        // Linux reports KiB; macOS reports bytes.
        let rss = u.max_rss().max(0) as u64;
        m.max_rss_kib = Some(if cfg!(target_os = "macos") {
            rss / 1024
        } else {
            rss
        });
    }
}

#[cfg(not(unix))]
fn fill_rusage(_: &mut Measured) {}

// ------------------------------------------------------------ harness --

/// A running client (inside its wrapper).
#[derive(Debug)]
pub struct Running {
    wrapper: Child,
    stdin: Option<ChildStdin>,
    result: Receiver<String>,
    pub started: Instant,
    pub started_unix_us: u64,
}

/// Starts `inv` inside the wrapper `shim_exe` (normally this executable).
/// The client's stdout and stderr go to `log_dir/stdout.txt` and
/// `log_dir/stderr.txt`.
pub fn start(shim_exe: &Path, inv: &Invocation, log_dir: &Path) -> Result<Running> {
    let mut cmd = Command::new(shim_exe);
    cmd.arg("__measure")
        .arg("--stdout")
        .arg(log_dir.join("stdout.txt"))
        .arg("--stderr")
        .arg(log_dir.join("stderr.txt"))
        .arg("--")
        .arg(&inv.program)
        .args(&inv.args)
        .envs(inv.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&inv.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let started_unix_us = unix_us();
    let started = Instant::now();
    let mut wrapper = cmd
        .spawn()
        .with_context(|| format!("start {}", shim_exe.display()))?;
    let stdin = wrapper.stdin.take();
    let mut stdout = wrapper.stdout.take().context("wrapper stdout")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        let _ = tx.send(s);
    });
    Ok(Running {
        wrapper,
        stdin,
        result: rx,
        started,
        started_unix_us,
    })
}

impl Running {
    /// Waits up to `timeout` for the client to exit. `None` on timeout.
    pub fn wait(&mut self, timeout: Duration) -> Option<Measured> {
        match self.result.recv_timeout(timeout) {
            Ok(s) => {
                let _ = self.wrapper.wait();
                Some(parse_measured(&s, self.started_unix_us))
            }
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                let _ = self.wrapper.wait();
                Some(parse_measured("", self.started_unix_us))
            }
        }
    }

    /// Whether the client has exited (non-blocking).
    pub fn poll(&mut self) -> Option<Measured> {
        self.wait(Duration::ZERO)
    }

    /// Asks the client to stop (SIGTERM), waits `grace`, then kills the
    /// whole process group. Returns whatever the wrapper reported.
    pub fn stop(&mut self, grace: Duration) -> Measured {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = stdin.write_all(b"term\n");
            let _ = stdin.flush();
        }
        if let Some(m) = self.wait(grace) {
            return m;
        }
        self.kill_group();
        let _ = self.wrapper.wait();
        match self.result.recv_timeout(Duration::from_secs(5)) {
            Ok(s) => parse_measured(&s, self.started_unix_us),
            Err(_) => parse_measured("", self.started_unix_us),
        }
    }

    fn kill_group(&mut self) {
        #[cfg(unix)]
        {
            use nix::sys::signal::{Signal, killpg};
            use nix::unistd::Pid;
            let _ = killpg(Pid::from_raw(self.wrapper.id() as i32), Signal::SIGKILL);
        }
        let _ = self.wrapper.kill();
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if matches!(self.wrapper.try_wait(), Ok(None)) {
            self.kill_group();
            let _ = self.wrapper.wait();
        }
    }
}

fn parse_measured(s: &str, started_unix_us: u64) -> Measured {
    s.lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Measured>(l).ok())
        .unwrap_or_else(|| Measured {
            spawn_unix_us: started_unix_us,
            wall_us: unix_us().saturating_sub(started_unix_us),
            spawn_error: Some("measurement wrapper reported nothing (killed?)".into()),
            ..Measured::default()
        })
}

// ---------------------------------------------------- other processes --

/// CPU time of a running process (all its threads, live and exited) from
/// `/proc/<pid>/stat`, in microseconds. Linux only; resolution 1/CLK_TCK
/// (10 ms on most systems).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProcCpu {
    pub user_us: u64,
    pub sys_us: u64,
}

/// Parses `utime` and `stime` (fields 14 and 15, clock ticks) out of a
/// `/proc/<pid>/stat` line. The command name (field 2) is in parentheses
/// and may itself contain spaces or parentheses, so fields are counted
/// from the last `)`.
pub fn parse_proc_stat(stat: &str, ticks_per_s: u64) -> Option<ProcCpu> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut f = rest.split_whitespace();
    // After `)`: field 3 (state) is index 0, so utime (14) is index 11.
    let utime: u64 = f.nth(11)?.parse().ok()?;
    let stime: u64 = f.next()?.parse().ok()?;
    let us = |t: u64| t.saturating_mul(1_000_000) / ticks_per_s.max(1);
    Some(ProcCpu {
        user_us: us(utime),
        sys_us: us(stime),
    })
}

/// Clock ticks per second (`sysconf(_SC_CLK_TCK)`; 100 if unknown).
pub fn clk_tck() -> u64 {
    #[cfg(unix)]
    if let Ok(Some(t)) = nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK)
        && t > 0
    {
        return t as u64;
    }
    100
}

/// CPU time used so far by process `pid` (Linux).
pub fn proc_cpu(pid: u32) -> Option<ProcCpu> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_proc_stat(&s, clk_tck())
}

/// A `kB` field (`VmHWM`, `VmRSS`) of `/proc/<pid>/status`.
pub fn parse_status_kib(status: &str, field: &str) -> Option<u64> {
    status
        .lines()
        .find(|l| l.starts_with(field) && l[field.len()..].starts_with(':'))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
}

/// Peak resident set (`VmHWM`) of process `pid`, KiB (Linux).
pub fn proc_hwm_kib(pid: u32) -> Option<u64> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    parse_status_kib(&s, "VmHWM")
}

/// Resets the peak resident set of `pid` to its current size, so a later
/// [`proc_hwm_kib`] reports the peak since now (`echo 5 >
/// /proc/<pid>/clear_refs`, Linux ≥ 4.0, own processes only). `false` if
/// the kernel refused.
pub fn proc_reset_hwm(pid: u32) -> bool {
    std::fs::write(format!("/proc/{pid}/clear_refs"), b"5").is_ok()
}

// ----------------------------------------------------------- resident --

/// What a resident helper (a client's daemon) used during one run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResidentUsage {
    /// Helper start until it was ready (not part of the measured time).
    pub startup_s: f64,
    /// CPU used between just before the client started and just after it
    /// finished (plus the settle time below). `None` where `/proc` is not
    /// available.
    pub cpu_user_s: Option<f64>,
    pub cpu_sys_s: Option<f64>,
    /// Peak resident set during the run (after resetting the kernel's
    /// high-water mark when the window opened), MiB.
    pub max_rss_mib: Option<f64>,
    /// `false` if the high-water mark could not be reset, so the peak may
    /// come from the helper's own start-up.
    pub rss_window_only: bool,
    /// How long after the client finished the harness kept counting the
    /// helper's CPU (until it went quiet, at most 0.5 s).
    pub settle_s: f64,
}

/// A resident helper process around one measured run.
#[derive(Debug)]
pub struct ResidentProc {
    child: Child,
    pid: u32,
    started: Instant,
    startup: Duration,
    cpu0: Option<ProcCpu>,
    hwm_reset: bool,
}

impl ResidentProc {
    /// Starts `r` and waits until its ready file names its pid.
    pub fn start(r: &crate::adapter::Resident, timeout: Duration) -> Result<Self> {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&r.log)
            .with_context(|| format!("open {}", r.log.display()))?;
        let mut cmd = Command::new(&r.program);
        cmd.args(&r.args)
            .envs(r.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&r.cwd)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let started = Instant::now();
        let child = cmd
            .spawn()
            .with_context(|| format!("start {}", r.program.display()))?;
        let pid = child.id();
        let mut p = Self {
            child,
            pid,
            started,
            startup: Duration::ZERO,
            cpu0: None,
            hwm_reset: false,
        };
        loop {
            if let Ok(bytes) = std::fs::read(&r.ready_file)
                && let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && v["pid"].as_u64() == Some(u64::from(pid))
            {
                break;
            }
            if let Some(status) = p.child.try_wait()? {
                bail!(
                    "{} exited during start-up ({status}): {}",
                    r.program.display(),
                    tail(&r.log, 400).unwrap_or_default()
                );
            }
            if started.elapsed() > timeout {
                bail!("{} not ready after {timeout:?}", r.program.display());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        p.startup = started.elapsed();
        Ok(p)
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Opens the measured window: resets the peak-RSS mark and records the
    /// CPU used so far. Call right before starting the client.
    pub fn open_window(&mut self) {
        self.hwm_reset = proc_reset_hwm(self.pid);
        self.cpu0 = proc_cpu(self.pid);
    }

    /// Closes the window right after the client finished: keeps sampling
    /// the helper's CPU every 50 ms until it stops rising (at most 0.5 s),
    /// so bookkeeping it does after the download counts too.
    pub fn close_window(&mut self) -> ResidentUsage {
        let t0 = Instant::now();
        let mut last = proc_cpu(self.pid);
        while t0.elapsed() < Duration::from_millis(500) {
            std::thread::sleep(Duration::from_millis(50));
            let now = proc_cpu(self.pid);
            if now == last {
                break;
            }
            last = now;
        }
        let (user, sys) = match (self.cpu0, last) {
            (Some(a), Some(b)) => (
                Some(b.user_us.saturating_sub(a.user_us) as f64 / 1e6),
                Some(b.sys_us.saturating_sub(a.sys_us) as f64 / 1e6),
            ),
            _ => (None, None),
        };
        ResidentUsage {
            startup_s: self.startup.as_secs_f64(),
            cpu_user_s: user,
            cpu_sys_s: sys,
            max_rss_mib: proc_hwm_kib(self.pid).map(|k| k as f64 / 1024.0),
            rss_window_only: self.hwm_reset,
            settle_s: t0.elapsed().as_secs_f64(),
        }
    }

    /// Stops the helper: SIGTERM (a clean shutdown), then SIGKILL to its
    /// process group after `grace`.
    pub fn stop(mut self, grace: Duration) {
        #[cfg(unix)]
        {
            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;
            let _ = kill(Pid::from_raw(self.pid as i32), Signal::SIGTERM);
            let t0 = Instant::now();
            while t0.elapsed() < grace {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let _ = grace;
        self.kill_now();
    }

    fn kill_now(&mut self) {
        #[cfg(unix)]
        {
            use nix::sys::signal::{Signal, killpg};
            use nix::unistd::Pid;
            let _ = killpg(Pid::from_raw(self.pid as i32), Signal::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// How long the helper has been running.
    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }
}

impl Drop for ResidentProc {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.kill_now();
        }
    }
}

/// Runs `program args` to completion and returns its stdout (for
/// `--version` probes). `None` if it fails to start or times out.
pub fn capture(program: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let mut err = child.stderr.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        let mut e = String::new();
        let _ = err.read_to_string(&mut e);
        let _ = tx.send(if s.trim().is_empty() { e } else { s });
    });
    match rx.recv_timeout(timeout) {
        Ok(s) => {
            let _ = child.wait();
            Some(s)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

/// Reads the last `max` bytes of a text file (lossy UTF-8).
pub fn tail(path: &Path, max: usize) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    if data.is_empty() {
        return None;
    }
    let start = data.len().saturating_sub(max);
    Some(String::from_utf8_lossy(&data[start..]).trim().to_owned())
}

/// Parses `__measure` arguments: `--stdout P --stderr P -- program args…`.
pub fn parse_shim_args(args: &[OsString]) -> Result<ShimArgs> {
    let mut stdout = None;
    let mut stderr = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].to_str() {
            Some("--stdout") => {
                stdout = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            Some("--stderr") => {
                stderr = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            Some("--") => {
                i += 1;
                break;
            }
            _ => bail!("unexpected __measure argument {:?}", args[i]),
        }
    }
    let program = args.get(i).cloned().context("__measure: missing program")?;
    Ok(ShimArgs {
        stdout: stdout.context("__measure: --stdout")?,
        stderr: stderr.context("__measure: --stderr")?,
        program,
        args: args[i + 1..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shim_args_round_trip() {
        let a: Vec<OsString> = [
            "--stdout", "/o", "--stderr", "/e", "--", "curl", "-O", "--", "x",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        let s = parse_shim_args(&a).unwrap();
        assert_eq!(s.stdout, Path::new("/o"));
        assert_eq!(s.stderr, Path::new("/e"));
        assert_eq!(s.program, "curl");
        assert_eq!(s.args, ["-O", "--", "x"].map(OsString::from));
        assert!(parse_shim_args(&a[..4]).is_err());
    }

    #[test]
    fn proc_stat_and_status_parsing() {
        // A command name with spaces and a parenthesis, as the kernel shows it.
        let stat = "4242 (whoosh (x) d) S 1 4242 4242 0 -1 4194560 900 0 0 0 \
                    250 75 0 0 20 0 3 0 12345 100000 2000 18446744073709551615";
        let c = parse_proc_stat(stat, 100).unwrap();
        assert_eq!(c.user_us, 2_500_000, "250 ticks at 100 Hz");
        assert_eq!(c.sys_us, 750_000);
        assert_eq!(parse_proc_stat(stat, 1000).unwrap().user_us, 250_000);
        assert!(parse_proc_stat("garbage", 100).is_none());
        assert!(
            parse_proc_stat("1 (x) S 2 3", 100).is_none(),
            "too few fields"
        );

        let status =
            "Name:\twhoosh-daemon\nVmPeak:\t  999 kB\nVmHWM:\t   20480 kB\nVmRSS:\t   10240 kB\n";
        assert_eq!(parse_status_kib(status, "VmHWM"), Some(20_480));
        assert_eq!(parse_status_kib(status, "VmRSS"), Some(10_240));
        assert_eq!(parse_status_kib(status, "VmSwap"), None);
        assert!(clk_tck() >= 1);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn proc_readers_see_this_process() {
        let me = std::process::id();
        let before = proc_cpu(me).unwrap();
        // Burn a little CPU so the counter can move.
        let mut x = 0u64;
        for i in 0..20_000_000u64 {
            x = x.wrapping_mul(31).wrapping_add(i);
        }
        std::hint::black_box(x);
        let after = proc_cpu(me).unwrap();
        assert!(after.user_us + after.sys_us >= before.user_us + before.sys_us);
        assert!(proc_hwm_kib(me).unwrap() > 0);
        // Resetting our own peak is allowed (Linux >= 4.0).
        assert!(proc_reset_hwm(me));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn resident_helper_lifecycle() {
        let dir =
            std::env::temp_dir().join(format!("whoosh-bench-resident-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A fake daemon: announces its pid in the ready file, then idles
        // (exec keeps the pid), the way whoosh-daemon writes runtime.json.
        let spec = crate::adapter::Resident {
            program: PathBuf::from("/bin/sh"),
            args: vec![
                "-c".into(),
                r#"sleep 0.05; printf '{"pid": %s}' $$ > "$READY.tmp" && mv "$READY.tmp" "$READY"; exec sleep 30"#.into(),
            ],
            env: vec![("READY".into(), dir.join("runtime.json").into_os_string())],
            cwd: dir.clone(),
            ready_file: dir.join("runtime.json"),
            log: dir.join("daemon.log"),
        };
        let mut p = ResidentProc::start(&spec, Duration::from_secs(10)).unwrap();
        assert!(
            p.uptime() >= Duration::from_millis(40),
            "waited for the ready file"
        );
        p.open_window();
        let u = p.close_window();
        assert!(u.startup_s >= 0.04);
        assert!(u.cpu_user_s.is_some() && u.cpu_sys_s.is_some(), "{u:?}");
        assert!(
            u.cpu_user_s.unwrap() < 0.5,
            "an idle helper used no CPU: {u:?}"
        );
        assert!(u.max_rss_mib.unwrap() > 0.0);
        assert!(u.rss_window_only, "own process: clear_refs is allowed");
        let pid = p.pid();
        p.stop(Duration::from_secs(2));
        assert!(!Path::new(&format!("/proc/{pid}")).exists() || proc_cpu(pid).is_none());

        // A helper that dies during start-up is reported, not waited for.
        let bad = crate::adapter::Resident {
            args: vec!["-c".into(), "echo broken config >&2; exit 2".into()],
            ..spec
        };
        let e = ResidentProc::start(&bad, Duration::from_secs(10)).unwrap_err();
        assert!(format!("{e:#}").contains("broken config"), "{e:#}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn measured_parse_fallback() {
        let m = parse_measured("garbage", 5);
        assert_eq!(m.spawn_unix_us, 5);
        assert!(m.spawn_error.is_some());
        let line = serde_json::to_string(&Measured {
            spawn_unix_us: 1,
            wall_us: 2,
            exit_code: Some(0),
            ..Measured::default()
        })
        .unwrap();
        let m = parse_measured(&format!("noise\n{line}\n"), 9);
        assert_eq!((m.spawn_unix_us, m.wall_us, m.exit_code), (1, 2, Some(0)));
    }
}
