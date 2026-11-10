//! `vayu` end to end, against `vayu-daemon --mock` started by the CLI itself.
//!
//! The daemon binary is built from the workspace into the same target
//! directory, i.e. next to `vayu`, which is where the CLI looks first; set
//! `VAYU_DAEMON_BIN_FOR_TESTS` to use another build.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

const VAYU: &str = env!("CARGO_BIN_EXE_vayu");

/// Builds `vayu-daemon` (once per test run) and checks it landed next to
/// `vayu`.
fn daemon_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Some(path) = std::env::var_os("VAYU_DAEMON_BIN_FOR_TESTS") {
            return PathBuf::from(path);
        }
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let out = Command::new(cargo)
            .args([
                "build",
                "--quiet",
                "--message-format=json",
                "-p",
                "vayu-daemon",
                "--bin",
                "vayu-daemon",
            ])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .stderr(Stdio::inherit())
            .output()
            .expect("cannot run cargo");
        assert!(out.status.success(), "building vayu-daemon failed");
        let exe = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == "vayu-daemon")
            .find_map(|m| m["executable"].as_str().map(PathBuf::from))
            .expect("no vayu-daemon executable reported");
        assert_eq!(
            exe.parent(),
            Path::new(VAYU).parent(),
            "vayu-daemon must sit next to vayu for auto-start"
        );
        exe
    })
}

/// One isolated world: its own data directory, its own daemon (mock, any
/// port), stopped when dropped.
struct World {
    dir: PathBuf,
}

impl World {
    fn new(name: &str) -> Self {
        daemon_bin();
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vayu-cli-e2e-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    fn data(&self) -> PathBuf {
        self.dir.join("data")
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(VAYU);
        cmd.args(args)
            .env("VAYU_DATA_DIR", self.data())
            .env("VAYU_DAEMON_MOCK", "1")
            .env("VAYU_DAEMON_MOCK_SCENARIO", "empty")
            .env("VAYU_DAEMON_PORT", "0")
            .env_remove("VAYU_NO_START")
            .env_remove("VAYU_DAEMON_BIN")
            .stdin(Stdio::null());
        if let Some(path) = std::env::var_os("VAYU_DAEMON_BIN_FOR_TESTS") {
            cmd.env("VAYU_DAEMON_BIN", path);
        }
        cmd
    }

    fn run(&self, args: &[&str]) -> Run {
        run(self.cmd(args))
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let r = self.run(&all);
        assert_eq!(r.code, 0, "vayu {args:?}: {}", r.stderr);
        serde_json::from_str(&r.stdout).unwrap_or_else(|e| panic!("{e}: {}", r.stdout))
    }

    fn runtime(&self) -> Option<Value> {
        std::fs::read(self.data().join("runtime.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
    }

    /// Polls `vayu info --json` until `pred` holds.
    fn wait_info(&self, id: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let info = self.json(&["info", id]);
            if pred(&info) {
                return info;
            }
            assert!(Instant::now() < deadline, "gave up waiting: {info}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = self.run(&["daemon", "stop"]);
        // Safety net for a daemon that did not stop: only ever a process
        // that really is a vayu-daemon (runtime files in these tests may
        // name made-up pids).
        if let Some(pid) = self.runtime().and_then(|r| r["pid"].as_u64())
            && is_vayu_daemon(pid)
        {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn is_vayu_daemon(pid: u64) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .is_ok_and(|comm| comm.trim() == "vayu-daemon")
}

#[derive(Debug)]
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(mut cmd: Command) -> Run {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("vayu did not finish within 60 s: {cmd:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let Output {
        status,
        stdout,
        stderr,
    } = child.wait_with_output().unwrap();
    Run {
        code: status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    }
}

fn pid_alive(pid: u64) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

// ---------------------------------------------------------------- lifecycle

#[test]
fn status_and_no_start_never_start_a_daemon() {
    let w = World::new("nostart");
    let r = w.run(&["daemon", "status"]);
    assert_eq!(r.code, 3, "{r:?}");
    assert!(r.stdout.contains("not running"), "{r:?}");
    let r = w.run(&["--no-start", "list"]);
    assert_eq!(r.code, 3, "{r:?}");
    assert!(r.stderr.contains("not running"), "{r:?}");
    let mut cmd = w.cmd(&["list"]);
    cmd.env("VAYU_NO_START", "1");
    assert_eq!(run(cmd).code, 3);
    // Stopping a daemon that is not running is not an error.
    assert_eq!(w.run(&["daemon", "stop"]).code, 0);
    assert!(w.runtime().is_none());
    let r = w.run(&["--json", "daemon", "status"]);
    assert_eq!(r.code, 3);
    let status: Value = serde_json::from_str(&r.stdout).unwrap();
    assert_eq!(status["running"], false);
}

#[test]
fn any_command_starts_the_daemon_found_next_to_vayu() {
    let w = World::new("autostart");
    let r = w.run(&["list"]);
    assert_eq!(r.code, 0, "{r:?}");
    assert_eq!(r.stdout, "");
    assert!(r.stderr.contains("no downloads"), "{r:?}");
    let rt = w.runtime().expect("runtime.json");
    let pid = rt["pid"].as_u64().unwrap();
    assert!(pid_alive(pid));

    let status = w.json(&["daemon", "status"]);
    assert_eq!(status["running"], true);
    assert_eq!(status["pid"], pid);
    assert!(
        status["capabilities"]
            .as_array()
            .unwrap()
            .contains(&"mock".into())
    );
    // The daemon outlives the command that started it, and is reused.
    w.run(&["list"]);
    assert_eq!(w.runtime().unwrap()["pid"], pid);

    let r = w.run(&["daemon", "stop"]);
    assert_eq!(r.code, 0, "{r:?}");
    assert!(w.runtime().is_none(), "runtime.json removed on clean exit");
    assert_eq!(w.run(&["daemon", "status"]).code, 3);
}

#[test]
fn concurrent_commands_share_one_daemon() {
    let w = World::new("race");
    let children: Vec<Child> = (0..4)
        .map(|_| {
            w.cmd(&["list"])
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let pid = w.runtime().unwrap()["pid"].as_u64().unwrap();
    assert!(pid_alive(pid));
}

#[test]
fn daemon_start_stop_status() {
    let w = World::new("start");
    let r = w.run(&["daemon", "start"]);
    assert_eq!(r.code, 0, "{r:?}");
    assert!(r.stdout.contains("started"), "{r:?}");
    let r = w.run(&["daemon", "start"]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("already running"), "{r:?}");
    let r = w.run(&["daemon", "status"]);
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("is running"), "{r:?}");
    let stopped = w.json(&["daemon", "stop"]);
    assert_eq!(stopped["stopped"], true);
    assert_eq!(w.run(&["daemon", "status"]).code, 3);

    // Extra daemon arguments go after `--`.
    let mut cmd = w.cmd(&["daemon", "start", "--", "--mock-scenario", "demo"]);
    cmd.env_remove("VAYU_DAEMON_MOCK_SCENARIO");
    assert_eq!(run(cmd).code, 0);
    let list = w.json(&["list"]);
    assert!(list.as_array().unwrap().len() >= 6, "demo data: {list}");
}

#[test]
fn the_real_engine_starts_without_mock() {
    let w = World::new("engine");
    let mut cmd = w.cmd(&["--json", "list"]);
    cmd.env_remove("VAYU_DAEMON_MOCK");
    let r = run(cmd);
    assert_eq!(r.code, 0, "{r:?}");
    assert_eq!(
        r.stdout.trim(),
        "[]",
        "a fresh engine has no downloads: {r:?}"
    );
    assert!(
        w.data().join("state.db").exists(),
        "the engine keeps its state in the data dir"
    );
    let mut cmd = w.cmd(&["daemon", "stop"]);
    cmd.env_remove("VAYU_DAEMON_MOCK");
    assert_eq!(run(cmd).code, 0);
}

#[test]
fn a_daemon_that_cannot_start_is_reported() {
    let w = World::new("nostart-engine");
    let mut cmd = w.cmd(&["list"]);
    cmd.env("VAYU_DAEMON_PORT", "not-a-port");
    let r = run(cmd);
    assert_eq!(r.code, 3, "{r:?}");
    assert!(
        r.stderr.contains("not-a-port"),
        "the daemon's own message: {r:?}"
    );

    let mut cmd = w.cmd(&["list"]);
    cmd.env("VAYU_DAEMON_BIN", w.dir.join("no-such-daemon"));
    let r = run(cmd);
    assert_eq!(r.code, 3, "{r:?}");
    assert!(r.stderr.contains("cannot start"), "{r:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn the_xdg_data_dir_is_shared_with_the_daemon() {
    let w = World::new("xdg");
    let mut cmd = w.cmd(&["list"]);
    cmd.env_remove("VAYU_DATA_DIR")
        .env("XDG_DATA_HOME", &w.dir)
        .env("HOME", w.dir.join("home"));
    assert_eq!(run(cmd).code, 0);
    let rt = w.dir.join("vayu").join("runtime.json");
    assert!(rt.exists(), "the daemon used $XDG_DATA_HOME/vayu");
    let mut cmd = w.cmd(&["daemon", "stop"]);
    cmd.env_remove("VAYU_DATA_DIR")
        .env("XDG_DATA_HOME", &w.dir)
        .env("HOME", w.dir.join("home"));
    assert_eq!(run(cmd).code, 0);
    assert!(!rt.exists());
}

// ---------------------------------------------------------------- get

#[test]
fn get_prints_the_saved_path_and_exits_0() {
    let w = World::new("get");
    let out_dir = w.dir.join("out");
    let r = w.run(&[
        "get",
        "https://example.com/files/data.bin?size=4MiB&bps=32MiB",
        "-o",
        out_dir.to_str().unwrap(),
        "--referer",
        "https://example.com/",
        "-H",
        "X-Test: 1",
    ]);
    assert_eq!(r.code, 0, "{r:?}");
    assert_eq!(
        r.stdout,
        format!("{}\n", out_dir.join("data.bin").display())
    );
    assert_eq!(
        r.stderr, "",
        "no progress bar when stderr is not a terminal"
    );

    let v = w.json(&[
        "get",
        "https://example.com/a/b/report.pdf?size=1MiB&bps=64MiB",
        "-n",
        "custom.pdf",
    ]);
    assert_eq!(v["state"], "completed");
    assert_eq!(v["filename"], "custom.pdf");
    assert_eq!(v["downloadedBytes"], 1 << 20);
    assert!(v["path"].as_str().unwrap().ends_with("custom.pdf"));
    assert!(v["elapsedMs"].is_u64());

    // A checksum that verifies.
    let sha = format!("sha256:{}", "a".repeat(64));
    let r = w.run(&[
        "get",
        "https://example.com/v.iso?size=1MiB&bps=64MiB",
        "--checksum",
        &sha,
    ]);
    assert_eq!(r.code, 0, "{r:?}");
}

#[test]
fn get_exit_codes() {
    let w = World::new("get-codes");
    let cases: &[(&[&str], i32, &str)] = &[
        (
            &[
                "get",
                "https://example.com/f.bin?size=8MiB&bps=32MiB&fail=0.3",
            ],
            11,
            "download failed",
        ),
        (
            &[
                "get",
                "https://example.com/c.bin?size=8MiB&bps=32MiB&fail=0.2&fail_code=remote_changed",
            ],
            11,
            "remote_changed",
        ),
        (
            &[
                "get",
                "https://example.com/e.bin?size=8MiB&bps=32MiB&expire=0.3",
            ],
            12,
            "expired",
        ),
        (
            &[
                "get",
                "https://example.com/x.iso?size=2MiB&bps=64MiB&corrupt=1",
                "--checksum",
                "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            ],
            13,
            "",
        ),
        (&["get", "https://example.com/404/gone.zip"], 8, "404"),
        (&["get", "https://nowhere.invalid/x"], 8, "resolve"),
        (&["get", "ftp://example.com/x"], 10, "not supported"),
        (&["get", "not a url"], 7, "not a valid URL"),
        (&["get", "https://example.com/x", "-H", "no colon"], 2, "-H"),
        (
            &["get", "https://example.com/x", "--checksum", "sha256:12"],
            2,
            "64 hex",
        ),
        (
            &["get", "https://example.com/x", "--queue", " "],
            7,
            "queue",
        ),
        (&["get", "https://example.com/x", "-c", "65"], 2, "65"),
        (&["get"], 2, ""),
        (&["frobnicate"], 2, ""),
    ];
    for (args, code, needle) in cases {
        let r = w.run(args);
        assert_eq!(r.code, *code, "{args:?}: {r:?}");
        assert!(r.stderr.contains(needle), "{args:?}: {r:?}");
        assert_eq!(r.stdout, "", "{args:?}: nothing on stdout on failure");
    }
    // --json errors are JSON on stderr.
    let r = w.run(&["--json", "get", "https://example.com/404/gone.zip"]);
    let err: Value = serde_json::from_str(r.stderr.trim()).unwrap();
    assert_eq!(err["error"]["exitCode"], 8);
    assert_eq!(err["error"]["code"], -32005);
}

#[cfg(unix)]
#[test]
fn ctrl_c_pauses_the_download_and_exits_130() {
    let w = World::new("get-int");
    assert_eq!(w.run(&["daemon", "start"]).code, 0);
    let child = w
        .cmd(&["get", "https://example.com/slow.bin?size=1GiB&bps=1MiB"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Wait until it is downloading.
    let deadline = Instant::now() + Duration::from_secs(20);
    let id = loop {
        let list = w.json(&["list", "--state", "downloading"]);
        if let Some(id) = list[0]["id"].as_str() {
            break id.to_owned();
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(100));
    };
    Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(130));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("paused"), "{stderr}");
    assert_eq!(w.json(&["info", &id])["state"], "paused");
}

// ---------------------------------------------------------------- the rest

#[test]
fn add_list_info_pause_resume_rm() {
    let w = World::new("crud");
    let dir = w.dir.join("dl");
    let r = w.run(&[
        "add",
        "https://example.com/one.zip?size=2MiB&bps=64MiB",
        "https://example.com/two.mkv?size=2MiB&bps=64MiB",
        "--paused",
        "-o",
        dir.to_str().unwrap(),
        "--category",
        "stuff",
    ]);
    assert_eq!(r.code, 0, "{r:?}");
    let ids: Vec<&str> = r.stdout.lines().collect();
    assert_eq!(ids.len(), 2);
    let (one, two) = (ids[0], ids[1]);

    let list = w.json(&["list"]);
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["id"], two, "newest first");
    for d in list {
        assert_eq!(d["state"], "paused");
        assert_eq!(d["directory"], dir.to_str().unwrap());
        assert_eq!(d["category"], "stuff");
    }
    let r = w.run(&["list", "--state", "paused,failed"]);
    assert!(r.stdout.starts_with("ID"), "{r:?}");
    assert!(
        r.stdout.contains(one) && r.stdout.contains("two.mkv"),
        "{r:?}"
    );
    assert_eq!(
        w.json(&["list", "--state", "completed"]),
        Value::Array(vec![])
    );
    assert_eq!(
        w.json(&["list", "--limit", "1", "--offset", "1"])[0]["id"],
        one
    );

    let r = w.run(&["info", one]);
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.contains("paused") && r.stdout.contains("one.zip"),
        "{r:?}"
    );

    assert_eq!(w.run(&["resume", one]).code, 0);
    let info = w.wait_info(one, |i| i["state"] == "completed");
    assert_eq!(info["downloadedBytes"], 2 << 20);
    assert!(!info["segments"].as_array().unwrap().is_empty());
    let r = w.run(&["info", one]);
    assert!(r.stdout.contains("segments:"), "{r:?}");

    // Errors map to their exit codes.
    let r = w.run(&["pause", one]);
    assert_eq!(r.code, 6, "pausing a completed download: {r:?}");
    assert_eq!(w.run(&["resume", one]).code, 6);
    assert_eq!(w.run(&["info", "dl_nope"]).code, 5);
    assert_eq!(w.run(&["pause", "dl_nope"]).code, 5);
    assert_eq!(w.run(&["rm", one, "dl_nope"]).code, 5, "all or nothing");
    let r = w.run(&["--json", "info", "dl_nope"]);
    let err: Value = serde_json::from_str(r.stderr.trim()).unwrap();
    assert_eq!(err["error"]["code"], -32002);
    let r = w.run(&["add", "https://a/1", "https://a/2", "-n", "x"]);
    assert_eq!(r.code, 2, "{r:?}");

    // One bad URL among good ones: the good ones are added, the code says
    // what went wrong.
    let r = w.run(&[
        "--json",
        "add",
        "https://example.com/ok.bin?size=1MiB",
        "https://example.com/404/x",
        "--paused",
    ]);
    assert_eq!(r.code, 8, "{r:?}");
    let results: Value = serde_json::from_str(&r.stdout).unwrap();
    assert!(results[0]["id"].is_string());
    assert_eq!(results[1]["error"]["code"], -32005);

    let ok = w.json(&["pause", two]);
    assert_eq!(ok["ok"], true);
    assert_eq!(w.run(&["rm", "--delete-files", one, two]).code, 0);
    let left = w.json(&["list"]);
    assert_eq!(left.as_array().unwrap().len(), 1, "{left}");
}

#[test]
fn settings_get_and_set() {
    let w = World::new("settings");
    let r = w.run(&["settings"]);
    assert_eq!(r.code, 0, "{r:?}");
    assert!(r.stdout.contains("maxActiveDownloads"), "{r:?}");
    let r = w.run(&[
        "settings",
        "set",
        "maxActiveDownloads=5",
        "speed_limit=10MiB",
        "theme=dark",
    ]);
    assert_eq!(r.code, 0, "{r:?}");
    assert!(r.stdout.contains("10485760"), "{r:?}");
    let s = w.json(&["settings", "get"]);
    assert_eq!(s["maxActiveDownloads"], 5);
    assert_eq!(s["speedLimitBps"], 10 << 20);
    assert_eq!(s["theme"], "dark");
    let s = w.json(&["settings", "set", "speedLimitBps=none"]);
    assert!(s.get("speedLimitBps").is_none(), "{s}");
    assert_eq!(w.run(&["settings", "set", "colour=red"]).code, 2);
    assert_eq!(w.run(&["settings", "set", "maxActiveDownloads=0"]).code, 7);
    assert_eq!(w.run(&["settings", "set", "theme=neon"]).code, 7);
}

#[test]
fn preconnect_and_watch() {
    let w = World::new("watch");
    assert_eq!(w.run(&["preconnect", "https://example.com/"]).code, 0);
    assert_eq!(w.run(&["preconnect", "gopher://x"]).code, 10);

    // Not a terminal: one JSON event per line.
    let mut watch = w
        .cmd(&["watch"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = watch.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    // `watch` is subscribed once it prints anything (stats come every
    // second); only then add a download, and see its whole life.
    let first: Value =
        serde_json::from_str(&rx.recv_timeout(Duration::from_secs(20)).unwrap()).unwrap();
    assert_eq!(first["method"], "event.stats", "{first}");
    w.run(&["add", "https://example.com/w.bin?size=1MiB&bps=64MiB"]);
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut methods = Vec::new();
    while !methods.iter().any(|m: &Value| {
        m["method"] == "event.download.state" && m["params"]["state"] == "completed"
    }) {
        assert!(Instant::now() < deadline, "watch saw only {methods:?}");
        if let Ok(line) = rx.recv_timeout(Duration::from_millis(500)) {
            let v: Value = serde_json::from_str(&line).unwrap();
            assert!(v["params"].is_object(), "{v}");
            methods.push(v);
        }
    }
    assert!(
        methods
            .iter()
            .any(|m| m["method"] == "event.download.added"),
        "{methods:?}"
    );
    assert!(
        methods
            .iter()
            .any(|m| m["method"] == "event.download.progress"),
        "{methods:?}"
    );
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-INT", &watch.id().to_string()])
            .status()
            .unwrap();
        assert_eq!(watch.wait().unwrap().code(), Some(0));
    }
    #[cfg(not(unix))]
    {
        let _ = watch.kill();
    }
}

// ---------------------------------------------------------------- stale runtime files

/// A WebSocket server standing in for "whatever holds the port now". With
/// `token`, it proves itself like a real daemon (and records requests);
/// without, it answers hello with a wrong proof, like an impostor on the
/// port of a stale runtime file. Every message it receives is recorded.
struct Fake {
    rt: tokio::runtime::Runtime,
    port: u16,
    received: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
}

impl Fake {
    fn start(token: Option<&str>) -> Self {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let listener = rt
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let received: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
        let (rec, token) = (received.clone(), token.map(str::to_owned));
        rt.spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (rec, token) = (rec.clone(), token.clone());
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    while let Some(Ok(Message::Text(t))) = ws.next().await {
                        let msg: Value = serde_json::from_str(t.as_str()).unwrap_or_default();
                        rec.lock().unwrap().push(msg.clone());
                        let result = match msg["method"].as_str() {
                            Some("session.hello") => {
                                let nonce = msg["params"]["nonce"].as_str().unwrap_or_default();
                                let proof = match &token {
                                    Some(t) => vayu_proto::auth::hello_proof(t, nonce),
                                    None => "0".repeat(64),
                                };
                                serde_json::json!({"daemonVersion": "fake", "protocol": 1,
                                                   "capabilities": [], "proof": proof})
                            }
                            Some("download.add") => serde_json::json!({"id": "dl_fake"}),
                            _ => Value::Null,
                        };
                        let reply = serde_json::json!({"jsonrpc": "2.0", "id": msg["id"], "result": result});
                        if ws.send(Message::text(reply.to_string())).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Self { rt, port, received }
    }

    fn received(&self) -> Vec<Value> {
        self.received.lock().unwrap().clone()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = &self.rt;
    }
}

/// Writes `runtime.json` the way a daemon does (private dir, 0600 file).
#[cfg(unix)]
fn write_runtime(data: &Path, port: u16, token: &str, pid: u32) {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    std::fs::create_dir_all(data).unwrap();
    std::fs::set_permissions(data, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = data.join("runtime.json");
    let _ = std::fs::remove_file(&path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    write!(
        f,
        r#"{{"port":{port},"token":"{token}","pid":{pid},"version":"0"}}"#
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn a_stale_runtime_file_whose_port_was_taken_gets_nothing_but_a_hello() {
    let w = World::new("impostor");
    let impostor = Fake::start(None);
    write_runtime(&w.data(), impostor.port, "stale-token", 1);
    let cookie = w.dir.join("cookie");
    std::fs::write(&cookie, "session=s3cret-cookie").unwrap();

    // Without auto-start: refused, and only the hello reached the impostor.
    let r = w.run(&["--no-start", "list"]);
    assert_eq!(r.code, 3, "{r:?}");
    assert!(r.stderr.contains("stale"), "{r:?}");
    // `daemon stop` sends nothing to it either (and signals no pid).
    let r = w.run(&["daemon", "stop"]);
    assert_eq!(r.code, 0, "{r:?}");
    assert!(r.stdout.contains("not running"), "{r:?}");

    // With auto-start: a real daemon replaces the stale file.
    let r = w.run(&[
        "add",
        "https://example.com/a.bin?size=1MiB",
        "--paused",
        "--cookie-file",
        cookie.to_str().unwrap(),
        "-H",
        "X-Secret: s3cret-header",
    ]);
    assert_eq!(r.code, 0, "{r:?}");
    let rt = w.runtime().unwrap();
    assert_ne!(rt["port"], impostor.port);
    assert_ne!(rt["pid"], 1);

    let received = impostor.received();
    assert!(!received.is_empty());
    for msg in &received {
        assert_eq!(msg["method"], "session.hello", "the impostor got {msg}");
        assert!(!msg.to_string().contains("s3cret"), "{msg}");
    }
}

#[test]
fn a_daemon_of_another_data_dir_on_the_port_means_start_our_own() {
    let a = World::new("other-a");
    assert_eq!(a.run(&["daemon", "start"]).code, 0);
    let a_rt = a.runtime().unwrap();
    let b = World::new("other-b");
    #[cfg(unix)]
    write_runtime(
        &b.data(),
        a_rt["port"].as_u64().unwrap() as u16,
        "not-a-token",
        a_rt["pid"].as_u64().unwrap() as u32,
    );
    let r = b.run(&["list"]);
    assert_eq!(r.code, 0, "{r:?}");
    let b_rt = b.runtime().unwrap();
    assert_ne!(b_rt["port"], a_rt["port"]);
    assert_ne!(b_rt["pid"], a_rt["pid"]);
    // A was neither stopped nor touched.
    assert_eq!(a.run(&["daemon", "status"]).code, 0);
}

#[cfg(unix)]
#[test]
fn requests_carry_files_headers_connections_and_queue() {
    let w = World::new("recorded");
    let fake = Fake::start(Some("good-token"));
    write_runtime(&w.data(), fake.port, "good-token", 4_000_000);
    let headers = w.dir.join("headers");
    std::fs::write(&headers, "# secrets\nAuthorization: Bearer t0k\n\nX-A: 1\n").unwrap();
    let mut cmd = w.cmd(&[
        "--no-start",
        "add",
        "https://example.com/r.bin",
        "--cookie-file",
        "-",
        "--headers-file",
        headers.to_str().unwrap(),
        "-c",
        "4",
        "--queue",
        "night",
        "--paused",
    ]);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"session=from-stdin\n").unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "dl_fake\n");

    let received = fake.received();
    let add = received
        .iter()
        .find(|m| m["method"] == "download.add")
        .expect("download.add sent");
    let p = &add["params"];
    assert_eq!(p["connections"], 4);
    assert_eq!(p["queue"], "night");
    assert_eq!(p["start"], false);
    assert_eq!(p["request"]["cookie"], "session=from-stdin");
    assert_eq!(
        p["request"]["headers"],
        serde_json::json!([{"name": "Authorization", "value": "Bearer t0k"}, {"name": "X-A", "value": "1"}])
    );
    let hello = &received[0];
    assert_eq!(hello["method"], "session.hello");
    assert!(vayu_proto::auth::is_valid_nonce(
        hello["params"]["nonce"].as_str().unwrap()
    ));
    // Not a daemon of ours: nothing for the world to stop when dropped.
    std::fs::remove_file(w.data().join("runtime.json")).unwrap();
}

#[cfg(unix)]
#[test]
fn a_loose_data_dir_is_tightened_and_files_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let w = World::new("loose");
    let data = w.data();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o775)).unwrap();
    // A world-readable runtime file (an old build?) is not trusted...
    std::fs::write(
        data.join("runtime.json"),
        r#"{"port":1,"token":"t","pid":1,"version":"0"}"#,
    )
    .unwrap();
    std::fs::set_permissions(
        data.join("runtime.json"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let r = w.run(&["--no-start", "list"]);
    assert_eq!(r.code, 3, "{r:?}");
    // ...and starting a daemon fixes everything.
    let r = w.run(&["list"]);
    assert_eq!(r.code, 0, "{r:?}");
    let mode = |p: PathBuf| std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(data.clone()), 0o700);
    assert_eq!(mode(data.join("runtime.json")), 0o600);
    assert_eq!(mode(data.join("logs")), 0o700);
    for entry in std::fs::read_dir(data.join("logs")).unwrap() {
        let path = entry.unwrap().path();
        assert_eq!(mode(path.clone()), 0o600, "{}", path.display());
    }
}
