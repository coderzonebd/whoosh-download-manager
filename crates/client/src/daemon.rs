//! Finding, starting and stopping the daemon.
//!
//! A client that needs the daemon calls [`ensure`]: it reads `runtime.json`
//! and says hello; if nothing answers, it starts `whoosh-daemon` detached
//! (the CLI finds it with [`daemon_exe`]; the desktop app passes its bundled
//! copy) and waits for it to answer.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use whoosh_proto::ClientInfo;

use crate::client::{Client, ConnectError};
use crate::error::{ClientError, Result};
use crate::runtime::{self, Lookup, RuntimeInfo};

pub const DAEMON_BIN_ENV: &str = "WHOOSH_DAEMON_BIN";
/// `whoosh-daemon`'s exit code when another daemon owns the data directory.
const DAEMON_EXIT_ALREADY_RUNNING: i32 = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const START_TIMEOUT: Duration = Duration::from_secs(20);

/// A connected daemon and what its runtime file says about it.
#[derive(Debug)]
pub struct Connected {
    pub client: Client,
    pub info: RuntimeInfo,
}

/// Whether the daemon of `data_dir` is up.
#[derive(Debug)]
pub enum Found {
    Running(Box<Connected>),
    /// No daemon answers for this runtime file; the reason is for messages.
    NotRunning(String),
}

/// Connects to the daemon in `data_dir` if one is running. A runtime file
/// whose port is held by something that cannot prove it knows the token, or
/// whose daemon rejects the token, is stale: that is "not running" (so a
/// new daemon may be started), never a reason to send it anything more.
pub async fn connect_running(data_dir: &Path, who: &ClientInfo) -> Result<Found> {
    // Twice: the file may be replaced (a daemon restarting) between the read
    // and the hello, which shows up as a rejected token.
    for attempt in 0..2 {
        let info = match runtime::read(data_dir) {
            Ok(Lookup::Found(info)) => info,
            Ok(Lookup::Unusable(why)) => return Ok(Found::NotRunning(why)),
            Err(e) => return Err(ClientError::DaemonUnavailable(e.to_string())),
        };
        let port = info.port;
        match Client::connect(port, &info.token, who, CONNECT_TIMEOUT).await {
            Ok(client) => return Ok(Found::Running(Box::new(Connected { client, info }))),
            Err(ConnectError::Unreachable) => {
                return Ok(Found::NotRunning(format!(
                    "nothing answers on port {port}; runtime.json is stale"
                )));
            }
            Err(ConnectError::BadProof) => {
                return Ok(Found::NotRunning(format!(
                    "the process on port {port} is not the daemon runtime.json describes \
                     (it could not prove it knows the token); runtime.json is stale"
                )));
            }
            Err(ConnectError::Local(e)) => return Err(ClientError::Other(e)),
            Err(ConnectError::Rejected { code, reason }) => match code {
                Some(4426) => {
                    return Err(ClientError::Rejected(format!(
                        "the daemon speaks a different protocol version ({reason}); \
                         use a client of the same version as the daemon"
                    )));
                }
                Some(4401) if attempt == 0 => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                _ => {
                    return Ok(Found::NotRunning(format!(
                        "the daemon on port {port} refused the token in runtime.json \
                         ({}); runtime.json is stale",
                        if reason.is_empty() {
                            "no reason given"
                        } else {
                            &reason
                        }
                    )));
                }
            },
        }
    }
    unreachable!("the second attempt always returns")
}

/// Connects as `who`, first starting `exe` as the daemon if none is running
/// and `autostart` allows it.
pub async fn ensure(
    data_dir: &Path,
    autostart: bool,
    who: &ClientInfo,
    exe: &Path,
) -> Result<Connected> {
    let why = match connect_running(data_dir, who).await? {
        Found::Running(connected) => return Ok(*connected),
        Found::NotRunning(why) => why,
    };
    if !autostart {
        return Err(ClientError::DaemonUnavailable(format!(
            "the whoosh daemon is not running ({why}); start it with `whoosh daemon start`"
        )));
    }
    let child = spawn_detached(data_dir, exe, &[])?;
    wait_until_up(data_dir, child, who).await
}

/// The `whoosh-daemon` executable a client finds on its own:
/// `$WHOOSH_DAEMON_BIN`, else next to the running executable, else `PATH`.
pub fn daemon_exe() -> PathBuf {
    if let Some(path) = std::env::var_os(DAEMON_BIN_ENV).filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    let name = format!("whoosh-daemon{}", std::env::consts::EXE_SUFFIX);
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
pub fn spawn_detached(data_dir: &Path, exe: &Path, extra_args: &[String]) -> Result<Child> {
    let log = start_log(data_dir);
    create_private_dirs(log.parent().unwrap_or(data_dir))?;
    let stderr = private_log(&log)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    let mut cmd = Command::new(exe);
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
        keep_std_handles_to_ourselves();
    }
    cmd.spawn().map_err(|e| {
        ClientError::DaemonUnavailable(format!(
            "cannot start {}: {e} (put whoosh-daemon next to this program or on PATH, \
             or set {DAEMON_BIN_ENV})",
            exe.display()
        ))
    })
}

/// Windows starts children with every inheritable handle of the parent, and
/// our standard handles are inheritable when whoever started us passed them
/// as pipes. The long-lived daemon would then hold the caller's pipes open,
/// and a script reading our output to the end would wait for the daemon to
/// exit. The daemon gets its own handles (null and the log) anyway, and
/// `std::process` duplicates any handle it hands to a child, so clearing the
/// flag on ours changes nothing else.
#[cfg(windows)]
#[allow(unsafe_code)]
fn keep_std_handles_to_ourselves() {
    use windows_sys::Win32::Foundation::{
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: plain Win32 calls on this process's own standard handles;
        // a missing or invalid handle is skipped, and a failure only leaves
        // the flag as it was.
        unsafe {
            let h = GetStdHandle(id);
            if !h.is_null() && h != INVALID_HANDLE_VALUE {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Truncates or creates `path`, owner-only (0600) on Unix.
fn private_log(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn create_private_dirs(dir: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir).map_err(|e| {
        ClientError::DaemonUnavailable(format!("cannot create {}: {e}", dir.display()))
    })
}

/// Waits for a daemon to answer in `data_dir`. `child` is the one we
/// started; if it exits because another daemon won the race, that one is
/// just as good.
pub async fn wait_until_up(
    data_dir: &Path,
    mut child: Child,
    who: &ClientInfo,
) -> Result<Connected> {
    let started = Instant::now();
    let deadline = started + START_TIMEOUT;
    let mut child_done = false;
    loop {
        if let Found::Running(connected) = connect_running(data_dir, who).await? {
            return Ok(*connected);
        }
        if !child_done && let Ok(Some(status)) = child.try_wait() {
            child_done = true;
            if status.code() != Some(DAEMON_EXIT_ALREADY_RUNNING) {
                let detail = std::fs::read_to_string(start_log(data_dir)).unwrap_or_default();
                let detail = detail.trim();
                return Err(ClientError::DaemonUnavailable(format!(
                    "whoosh-daemon exited ({status}){}{}",
                    if detail.is_empty() { "" } else { ": " },
                    detail
                )));
            }
        }
        if Instant::now() > deadline {
            return Err(ClientError::DaemonUnavailable(format!(
                "whoosh-daemon did not come up within {}s; see {}",
                START_TIMEOUT.as_secs(),
                data_dir.join("logs").display()
            )));
        }
        tokio::time::sleep(poll_delay(started.elapsed())).await;
    }
}

/// A daemon is usually up within ~10 ms: poll tightly at first, then back
/// off so a slow start does not cost CPU.
fn poll_delay(waited: Duration) -> Duration {
    if waited < Duration::from_millis(200) {
        Duration::from_millis(2)
    } else {
        Duration::from_millis(25)
    }
}

/// Asks the daemon to stop (`daemon.shutdown`, PROTOCOL.md §3) and waits
/// until it has removed its runtime file, the last step of a clean exit.
pub async fn stop(connected: Connected, data_dir: &Path) -> Result<()> {
    let Connected { client, info } = connected;
    client
        .call_value(whoosh_proto::method::DAEMON_SHUTDOWN, Value::Null)
        .await?;
    client.close().await;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let gone = match runtime::read(data_dir) {
            Ok(Lookup::Found(now)) => now.pid != info.pid || now.token != info.token,
            _ => true,
        };
        if gone {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(ClientError::Other(format!(
                "whoosh-daemon (pid {}) did not stop within 30 s",
                info.pid
            )));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
