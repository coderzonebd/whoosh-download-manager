//! Finding, starting and stopping the daemon.
//!
//! Commands that need the daemon call [`ensure`]: it reads `runtime.json`
//! and says hello; if nothing answers, it starts `vayu-daemon` detached
//! (from `$VAYU_DAEMON_BIN`, else next to this `vayu` executable, else from
//! `PATH`) and waits for it to answer.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::client::{Client, ConnectError};
use crate::error::{CliError, Result};
use crate::runtime::{self, RuntimeInfo};

pub const DAEMON_BIN_ENV: &str = "VAYU_DAEMON_BIN";
/// `vayu-daemon`'s exit code when another daemon owns the data directory.
const DAEMON_EXIT_ALREADY_RUNNING: i32 = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const START_TIMEOUT: Duration = Duration::from_secs(20);

/// A connected daemon and what its runtime file says about it.
#[derive(Debug)]
pub struct Connected {
    pub client: Client,
    pub info: RuntimeInfo,
}

/// Connects to the daemon in `data_dir` if one is running.
pub async fn connect_running(data_dir: &Path) -> Result<Option<Connected>> {
    // Twice: the file may be replaced (a daemon restarting) between the read
    // and the hello, which shows up as a rejected token.
    for attempt in 0..2 {
        let info = match runtime::read(data_dir) {
            Ok(Some(info)) => info,
            Ok(None) => return Ok(None),
            Err(e) => {
                return Err(CliError::DaemonUnavailable(format!(
                    "cannot read {}: {e}",
                    data_dir.join(runtime::RUNTIME_FILE).display()
                )));
            }
        };
        match Client::connect(info.port, &info.token, CONNECT_TIMEOUT).await {
            Ok(client) => return Ok(Some(Connected { client, info })),
            Err(ConnectError::Unreachable) => return Ok(None),
            Err(ConnectError::Rejected { code, reason }) => {
                if attempt == 0 && code == Some(4401) {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                return Err(rejected(code, &reason));
            }
        }
    }
    unreachable!("the loop returns")
}

fn rejected(code: Option<u16>, reason: &str) -> CliError {
    match code {
        Some(4426) => CliError::Rejected(format!(
            "the daemon speaks a different protocol version ({reason}); \
             use a vayu CLI of the same version as the daemon"
        )),
        Some(4401) => CliError::Rejected(format!(
            "the daemon rejected the token in runtime.json ({reason})"
        )),
        _ => CliError::Rejected(format!("the daemon refused the connection ({reason})")),
    }
}

/// Connects, starting the daemon first if needed and allowed.
pub async fn ensure(data_dir: &Path, autostart: bool) -> Result<Connected> {
    if let Some(connected) = connect_running(data_dir).await? {
        return Ok(connected);
    }
    if !autostart {
        return Err(CliError::DaemonUnavailable(
            "the vayu daemon is not running (start it with `vayu daemon start`)".into(),
        ));
    }
    let child = spawn_detached(data_dir, &[])?;
    wait_until_up(data_dir, child).await
}

/// The `vayu-daemon` executable to start.
pub fn daemon_exe() -> PathBuf {
    if let Some(path) = std::env::var_os(DAEMON_BIN_ENV).filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    let name = format!("vayu-daemon{}", std::env::consts::EXE_SUFFIX);
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name(&name);
        if sibling.is_file() {
            return sibling;
        }
    }
    // Let the OS search PATH.
    PathBuf::from(name)
}

fn start_log(data_dir: &Path) -> PathBuf {
    data_dir.join("logs").join("daemon-start.log")
}

/// Starts the daemon in the background, detached from this terminal. Its
/// stderr (startup errors) goes to `<data dir>/logs/daemon-start.log`.
pub fn spawn_detached(data_dir: &Path, extra_args: &[String]) -> Result<Child> {
    let exe = daemon_exe();
    let log = start_log(data_dir);
    create_private_dirs(log.parent().unwrap_or(data_dir))?;
    let stderr = std::fs::File::create(&log)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    let mut cmd = Command::new(&exe);
    cmd.arg("--detached")
        .arg("--data-dir")
        .arg(data_dir)
        .args(extra_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group: Ctrl-C in this terminal must not stop it.
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn().map_err(|e| {
        CliError::DaemonUnavailable(format!(
            "cannot start {}: {e} (put vayu-daemon next to vayu or on PATH, \
             or set {DAEMON_BIN_ENV})",
            exe.display()
        ))
    })
}

fn create_private_dirs(dir: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| CliError::DaemonUnavailable(format!("cannot create {}: {e}", dir.display())))
}

/// Waits for a daemon to answer in `data_dir`. `child` is the one we
/// started; if it exits because another daemon won the race, that one is
/// just as good.
pub async fn wait_until_up(data_dir: &Path, mut child: Child) -> Result<Connected> {
    let deadline = Instant::now() + START_TIMEOUT;
    let mut child_done = false;
    loop {
        if let Some(connected) = connect_running(data_dir).await? {
            return Ok(connected);
        }
        if !child_done && let Ok(Some(status)) = child.try_wait() {
            child_done = true;
            if status.code() != Some(DAEMON_EXIT_ALREADY_RUNNING) {
                let detail = std::fs::read_to_string(start_log(data_dir)).unwrap_or_default();
                let detail = detail.trim();
                return Err(CliError::DaemonUnavailable(format!(
                    "vayu-daemon exited ({status}){}{}",
                    if detail.is_empty() { "" } else { ": " },
                    detail
                )));
            }
        }
        if Instant::now() > deadline {
            return Err(CliError::DaemonUnavailable(format!(
                "vayu-daemon did not come up within {}s; see {}",
                START_TIMEOUT.as_secs(),
                data_dir.join("logs").display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Asks the daemon to stop (SIGTERM) and waits until it has removed its
/// runtime file.
pub async fn stop(connected: Connected, data_dir: &Path) -> Result<()> {
    let Connected { client, info } = connected;
    client.close().await;
    let pid = info.pid.ok_or_else(|| {
        CliError::Other("runtime.json has no pid; stop the daemon by hand".into())
    })?;
    send_terminate(pid)?;
    // The daemon removes runtime.json as the last step of a clean shutdown.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let gone = match runtime::read(data_dir) {
            Ok(Some(now)) => now.pid != Some(pid),
            _ => true,
        };
        if gone {
            break;
        }
        if Instant::now() > deadline {
            return Err(CliError::Other(format!(
                "vayu-daemon (pid {pid}) did not stop within 30 s"
            )));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Then the process exits; give it a moment (a zombie still "exists"
    // until its parent reaps it, so this is best effort).
    let deadline = Instant::now() + Duration::from_secs(3);
    while process_alive(pid) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

#[cfg(unix)]
fn send_terminate(pid: u32) -> Result<()> {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let pid = i32::try_from(pid).map_err(|_| CliError::Other(format!("bad pid {pid}")))?;
    kill(Pid::from_raw(pid), Signal::SIGTERM)
        .map_err(|e| CliError::Other(format!("cannot signal vayu-daemon (pid {pid}): {e}")))
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    i32::try_from(pid).is_ok_and(|pid| kill(Pid::from_raw(pid), None).is_ok())
}

#[cfg(not(unix))]
fn send_terminate(_pid: u32) -> Result<()> {
    // A detached daemon has no console to send Ctrl-C to; stopping it needs
    // an RPC (`daemon.shutdown`, proposed for the protocol).
    Err(CliError::Unsupported(
        "`vayu daemon stop` is not supported on this platform yet; \
         end vayu-daemon from the task manager"
            .into(),
    ))
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    false
}
