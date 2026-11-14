//! The real `whoosh-daemon` executable: startup, single instance, signals.

mod common;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::TempDir;
use whoosh_daemon::runtime;

const EXE: &str = env!("CARGO_BIN_EXE_whoosh-daemon");

fn daemon(data_dir: &Path) -> Command {
    let mut cmd = Command::new(EXE);
    #[cfg(windows)]
    {
        // A process group of its own, so `stop` can reach it alone.
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    cmd.args(["--mock", "--mock-scenario", "empty", "--port", "0"])
        .env("WHOOSH_DATA_DIR", data_dir)
        .env_remove("WHOOSH_DAEMON_MOCK")
        .env_remove("WHOOSH_DAEMON_PORT")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    cmd
}

fn wait_for_runtime(dir: &Path, child: &mut Child) -> runtime::RuntimeInfo {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(Some(info)) = runtime::read(dir) {
            return info;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("daemon exited early: {status}");
        }
        assert!(Instant::now() < deadline, "no runtime.json");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_exit(child: &mut Child, within: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + within;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("daemon did not exit within {within:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn stderr_of(child: &mut Child) -> String {
    use std::io::Read;
    let mut s = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut s);
    }
    s
}

#[cfg(unix)]
fn signal(child: &Child, sig: &str) {
    let status = Command::new("kill")
        .args([&format!("-{sig}"), &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}

/// Asks the daemon to stop the way its OS does: `SIGTERM` on Unix; on
/// Windows Ctrl-Break to its process group (a console's Ctrl-C cannot be
/// sent to one group alone; closing the console, log-off and shutdown
/// arrive through the same handler).
fn stop(child: &Child) {
    #[cfg(unix)]
    signal(child, "TERM");
    #[cfg(windows)]
    ctrl_break(child);
}

/// The interrupt: `SIGINT` on Unix, Ctrl-Break on Windows.
fn interrupt(child: &Child) {
    #[cfg(unix)]
    signal(child, "INT");
    #[cfg(windows)]
    ctrl_break(child);
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn ctrl_break(child: &Child) {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
    // SAFETY: a plain Win32 call with integer arguments.
    let ok = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) };
    assert!(
        ok != 0,
        "GenerateConsoleCtrlEvent: {}",
        std::io::Error::last_os_error()
    );
}

/// Owner-only: 0600 files and 0700 directories on Unix; an owner-only DACL
/// on Windows.
fn assert_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path).unwrap();
        let want = if meta.is_dir() { 0o700 } else { 0o600 };
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, want, "{} is {mode:o}", path.display());
    }
    #[cfg(windows)]
    {
        use whoosh_proto::runtime::{check_private_dir, check_private_file};
        let r = if path.is_dir() {
            check_private_dir(path)
        } else {
            check_private_file(path)
        };
        r.unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }
}

#[test]
fn serves_refuses_a_second_instance_and_stops_cleanly_when_asked() {
    let dir = TempDir::new("bin");
    let mut first = daemon(dir.path()).spawn().unwrap();
    let info = wait_for_runtime(dir.path(), &mut first);
    assert_eq!(info.pid, first.id());

    let rt = tokio::runtime::Runtime::new().unwrap();
    let token = whoosh_daemon::token::Token::from_string(info.token.clone());
    let hello = rt
        .block_on(whoosh_daemon::client::probe(
            info.port,
            &token,
            Duration::from_secs(5),
        ))
        .unwrap();
    assert_eq!(hello.daemon_version, whoosh_daemon::VERSION);
    assert!(hello.capabilities.iter().any(|c| c == "mock"));

    // A second daemon on the same data directory refuses to start.
    let mut second = daemon(dir.path()).spawn().unwrap();
    let status = wait_exit(&mut second, Duration::from_secs(20));
    assert_eq!(status.code(), Some(3), "{status}");
    let err = stderr_of(&mut second);
    assert!(err.contains("already running"), "{err}");
    assert!(err.contains(&format!("pid {}", first.id())), "{err}");
    // ...and leaves the first one's runtime file alone.
    assert_eq!(runtime::read(dir.path()).unwrap(), Some(info.clone()));

    stop(&first);
    let status = wait_exit(&mut first, Duration::from_secs(20));
    assert!(status.success(), "{status}");
    assert!(!dir.path().join("runtime.json").exists());

    // Logs went to <data dir>/logs, without the token.
    let logs: Vec<_> = std::fs::read_dir(dir.path().join("logs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert!(!logs.is_empty());
    for log in logs {
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("whoosh-daemon ready"), "{text}");
        assert!(!text.contains(&info.token), "token in {}", log.display());
        assert_private(&log);
    }
    assert_private(dir.path());
    assert_private(&dir.path().join("logs"));
    // Foreground mode also logs to stderr.
    assert!(stderr_of(&mut first).contains("whoosh-daemon ready"));
}

#[test]
fn an_interrupt_also_stops_it_and_detached_mode_is_quiet() {
    let dir = TempDir::new("bin-int");
    let mut child = daemon(dir.path()).arg("--detached").spawn().unwrap();
    wait_for_runtime(dir.path(), &mut child);
    // Detached daemons ignore SIGHUP (the terminal that started `whoosh` closed).
    #[cfg(unix)]
    {
        signal(&child, "HUP");
        std::thread::sleep(Duration::from_millis(300));
        assert!(child.try_wait().unwrap().is_none(), "SIGHUP killed it");
    }
    interrupt(&child);
    assert!(wait_exit(&mut child, Duration::from_secs(20)).success());
    assert!(!dir.path().join("runtime.json").exists());
    assert_eq!(
        stderr_of(&mut child),
        "",
        "detached mode writes nothing to stderr"
    );
}

#[test]
fn daemon_shutdown_stops_the_process() {
    let dir = TempDir::new("bin-rpc-stop");
    let mut child = daemon(dir.path()).spawn().unwrap();
    let info = wait_for_runtime(dir.path(), &mut child);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let reply = rt.block_on(async {
        let mut c = whoosh_daemon::client::Client::connect(info.port, &info.token, "test")
            .await
            .unwrap();
        c.call("daemon.shutdown", serde_json::Value::Null).await
    });
    assert_eq!(reply.unwrap(), serde_json::Value::Null);
    let status = wait_exit(&mut child, Duration::from_secs(20));
    assert!(status.success(), "{status}");
    assert!(!dir.path().join("runtime.json").exists());
}

#[test]
fn without_mock_it_runs_the_real_engine_and_stops_cleanly() {
    let dir = TempDir::new("bin-engine");
    let mut child = Command::new(EXE)
        .args(["--port", "0"])
        .env("WHOOSH_DATA_DIR", dir.path())
        .env_remove("WHOOSH_DAEMON_MOCK")
        .env_remove("WHOOSH_DAEMON_PORT")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let info = wait_for_runtime(dir.path(), &mut child);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (list, hello_caps) = rt.block_on(async {
        let mut c = whoosh_daemon::client::Client::connect(info.port, &info.token, "test")
            .await
            .unwrap();
        let caps = c.hello().capabilities.clone();
        let list = c
            .call("download.list", serde_json::json!({}))
            .await
            .unwrap();
        c.call("daemon.shutdown", serde_json::Value::Null)
            .await
            .unwrap();
        (list, caps)
    });
    assert_eq!(
        list,
        serde_json::json!([]),
        "a fresh engine has no downloads"
    );
    assert!(hello_caps.iter().any(|c| c == "https"), "{hello_caps:?}");
    let status = wait_exit(&mut child, Duration::from_secs(20));
    assert!(status.success(), "{status}");
    // The engine keeps its state in the data directory.
    assert!(dir.path().join("state.db").exists());
    assert!(dir.path().join("journal").is_dir());
    assert!(!dir.path().join("runtime.json").exists());
}

#[test]
fn a_second_daemon_never_starts_the_engine_of_the_first() {
    // The engine resumes stored downloads at start; the single-instance
    // claim must come first, so a second daemon exits before touching them.
    let dir = TempDir::new("bin-engine-twice");
    let mut first = daemon(dir.path()).spawn().unwrap();
    wait_for_runtime(dir.path(), &mut first);
    let out = Command::new(EXE)
        .args(["--port", "0"])
        .env("WHOOSH_DATA_DIR", dir.path())
        .env_remove("WHOOSH_DAEMON_MOCK")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(
        !dir.path().join("state.db").exists(),
        "the second daemon opened the engine's state"
    );
    let _ = first.kill();
    let _ = first.wait();
}

#[test]
fn mock_mode_can_come_from_the_environment() {
    let dir = TempDir::new("bin-env");
    let mut child = Command::new(EXE)
        .env("WHOOSH_DATA_DIR", dir.path())
        .env("WHOOSH_DAEMON_MOCK", "1")
        .env("WHOOSH_DAEMON_MOCK_SCENARIO", "demo")
        .env("WHOOSH_DAEMON_PORT", "0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let info = wait_for_runtime(dir.path(), &mut child);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let list = rt.block_on(async {
        let mut c = whoosh_daemon::client::Client::connect(info.port, &info.token, "test")
            .await
            .unwrap();
        c.call("download.list", serde_json::json!({}))
            .await
            .unwrap()
    });
    assert!(
        list.as_array().unwrap().len() >= 6,
        "the demo scenario: {list}"
    );
    let _ = child.kill();
    let _ = child.wait();
}
