//! Running a client and measuring it.
//!
//! The harness never spawns a client directly. It spawns itself as a small
//! wrapper (`vayu-bench __measure`), which spawns the client, waits for it
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

/// Body of `vayu-bench __measure`: run the client, print a [`Measured`]
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
    // Wait for exit without reaping.
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
