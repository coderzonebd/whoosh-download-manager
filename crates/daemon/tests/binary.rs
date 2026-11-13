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

#[cfg(unix)]
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

#[cfg(unix)]
#[test]
fn serves_refuses_a_second_instance_and_stops_cleanly_on_sigterm() {
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

    signal(&first, "TERM");
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
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&log).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{} is {mode:o}", log.display());
    }
    let mode = |p: &Path| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    };
    assert_eq!(mode(dir.path()), 0o700, "the data dir was made private");
    assert_eq!(mode(&dir.path().join("logs")), 0o700);
    // Foreground mode also logs to stderr.
    assert!(stderr_of(&mut first).contains("whoosh-daemon ready"));
}

#[cfg(unix)]
#[test]
fn sigint_also_stops_it_and_detached_mode_is_quiet() {
    let dir = TempDir::new("bin-int");
    let mut child = daemon(dir.path()).arg("--detached").spawn().unwrap();
    wait_for_runtime(dir.path(), &mut child);
    // Detached daemons ignore SIGHUP (the terminal that started `whoosh` closed).
    signal(&child, "HUP");
    std::thread::sleep(Duration::from_millis(300));
    assert!(child.try_wait().unwrap().is_none(), "SIGHUP killed it");
    signal(&child, "INT");
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
