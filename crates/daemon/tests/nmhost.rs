//! Compatibility with the real `vayu-nmhost`: the browser extension's path to
//! the daemon (PROTOCOL.md §1, native messaging). The host is built from the
//! workspace (or taken from `$VAYU_NMHOST_BIN`), pointed at the daemon through
//! the environment alone, and driven with native-messaging frames.

mod common;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use vayu_daemon::mock::{MockOptions, MockService, Scenario};
use vayu_daemon::paths;

fn nmhost() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| workspace_bin("vayu-nmhost", "vayu-nmhost", "VAYU_NMHOST_BIN"))
}

fn mock() -> std::sync::Arc<MockService> {
    MockService::new(MockOptions {
        scenario: Scenario::Empty,
        tick: Duration::from_millis(50),
        default_directory: "/downloads".into(),
        ..MockOptions::default()
    })
}

/// The host, started the way Chrome starts it, with its stdout parsed into
/// JSON messages on a channel.
struct Host {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: mpsc::Receiver<Value>,
}

impl Host {
    fn spawn(env: &[(&str, &Path)]) -> Self {
        let mut cmd = Command::new(nmhost());
        cmd.arg("chrome-extension://clhjgofaikdmabdnebgcmdffdjenagfl/");
        for var in ["VAYU_DATA_DIR", "XDG_DATA_HOME", "LOCALAPPDATA"] {
            cmd.env_remove(var);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            loop {
                let mut len = [0u8; 4];
                if stdout.read_exact(&mut len).is_err() {
                    return;
                }
                let mut body = vec![0u8; u32::from_ne_bytes(len) as usize];
                if stdout.read_exact(&mut body).is_err() {
                    return;
                }
                if tx.send(serde_json::from_slice(&body).unwrap()).is_err() {
                    return;
                }
            }
        });
        let stdin = child.stdin.take();
        Self { child, stdin, rx }
    }

    fn send(&mut self, v: Value) {
        let body = v.to_string();
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(&(body.len() as u32).to_ne_bytes()).unwrap();
        stdin.write_all(body.as_bytes()).unwrap();
        stdin.flush().unwrap();
    }

    fn recv(&self) -> Value {
        self.rx
            .recv_timeout(Duration::from_secs(15))
            .expect("nothing from the host")
    }

    /// Waits for the response to `id`, collecting notifications meanwhile.
    fn response(&self, id: i64, events: &mut Vec<Value>) -> Value {
        loop {
            let msg = self.recv();
            if msg["id"] == id {
                return msg;
            }
            assert!(msg.get("method").is_some(), "stray response {msg}");
            events.push(msg);
        }
    }

    fn wait_event(&self, events: &mut Vec<Value>, pred: impl Fn(&Value) -> bool) -> Value {
        if let Some(pos) = events.iter().position(&pred) {
            return events.remove(pos);
        }
        loop {
            let msg = self.recv();
            if pred(&msg) {
                return msg;
            }
            events.push(msg);
        }
    }

    fn status(&mut self, id: i64, events: &mut Vec<Value>) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": "nm.status"}));
        self.response(id, events)["result"].clone()
    }

    fn finish(mut self) {
        drop(self.stdin.take());
        let status = self.child.wait().unwrap();
        assert!(status.success(), "nmhost exited with {status}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_native_host_relays_to_this_daemon_end_to_end() {
    let dir = TempDir::new("nmhost");
    let d = start(mock(), dir.path()).await;
    let mut host = tokio::task::block_in_place(|| Host::spawn(&[("VAYU_DATA_DIR", dir.path())]));
    let mut events = Vec::new();

    tokio::task::block_in_place(|| {
        // nm.status: the host found the daemon through runtime.json and
        // completed session.hello with its token.
        let status = host.status(1, &mut events);
        assert_eq!(status["daemonRunning"], true, "{status}");
        assert_eq!(status["daemonVersion"], vayu_daemon::VERSION);

        // A method call, relayed verbatim both ways.
        host.send(json!({"jsonrpc": "2.0", "id": 2, "method": "download.add",
            "params": {"url": "https://example.com/pkg/file.zip?size=3MiB&bps=12MiB",
                       "request": {"referer": "https://example.com/pkg/"}}}));
        let added = host.response(2, &mut events);
        let id = added["result"]["id"].as_str().expect("an id").to_owned();

        // Events reach the extension.
        let ev = host.wait_event(&mut events, |m| m["method"] == "event.download.added");
        assert_eq!(ev["params"]["id"], id.as_str());
        assert_eq!(ev["params"]["filename"], "file.zip");
        let progress = host.wait_event(&mut events, |m| m["method"] == "event.download.progress");
        assert!(progress["params"]["items"].is_array());
        host.wait_event(&mut events, |m| {
            m["method"] == "event.download.state" && m["params"]["state"] == "completed"
        });

        // Lists, string ids, and daemon errors come back unchanged.
        host.send(json!({"jsonrpc": "2.0", "id": 3, "method": "download.list",
            "params": {"states": ["completed"], "limit": 10}}));
        let list = host.response(3, &mut events);
        assert_eq!(list["result"][0]["id"], id.as_str());
        host.send(
            json!({"jsonrpc": "2.0", "id": "x-4", "method": "download.get",
            "params": {"id": "dl_missing"}}),
        );
        let missing = loop {
            let m = host.recv();
            if m["id"] == "x-4" {
                break m;
            }
        };
        assert_eq!(missing["error"]["code"], -32002);
        host.send(json!({"jsonrpc": "2.0", "id": 5, "method": "no.such"}));
        assert_eq!(host.response(5, &mut events)["error"]["code"], -32601);
    });

    // The daemon goes away: the host tells the extension...
    d.shutdown().await;
    tokio::task::block_in_place(|| {
        let down = host.wait_event(&mut events, |m| m["method"] == "event.nm.status");
        assert_eq!(down["params"]["daemonRunning"], false, "{down}");
    });
    // ...and finds the restarted daemon (new port, new token) by itself.
    let d = start(mock(), dir.path()).await;
    tokio::task::block_in_place(|| {
        let up = host.wait_event(&mut events, |m| {
            m["method"] == "event.nm.status" && m["params"]["daemonRunning"] == true
        });
        assert_eq!(up["params"]["daemonVersion"], vayu_daemon::VERSION);
        host.send(json!({"jsonrpc": "2.0", "id": 6, "method": "stats.get"}));
        assert!(host.response(6, &mut events)["result"]["active"].is_number());
        host.finish();
    });
    d.shutdown().await;
}

/// Without `VAYU_DATA_DIR`, both sides must derive the same directory from
/// the platform rule (here Linux/XDG).
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_native_host_and_the_daemon_agree_on_the_xdg_data_dir() {
    let base = TempDir::new("nmhost-xdg");
    let env = paths::Env {
        xdg_data_home: Some(base.path().to_owned()),
        home: Some(base.path().join("home")),
        ..paths::Env::default()
    };
    let data_dir = paths::data_dir(paths::Os::Linux, &env).unwrap();
    assert_eq!(data_dir, base.path().join("vayu"));
    let d = start(mock(), &data_dir).await;
    let home = base.path().join("home");
    let mut host = tokio::task::block_in_place(|| {
        Host::spawn(&[("XDG_DATA_HOME", base.path()), ("HOME", &home)])
    });
    tokio::task::block_in_place(|| {
        let mut events = Vec::new();
        let status = host.status(1, &mut events);
        assert_eq!(status["daemonRunning"], true, "{status}");
        host.finish();
    });
    d.shutdown().await;
}

/// The two data-dir implementations agree for every OS and environment.
#[test]
fn data_dir_rule_matches_vayu_nmhost() {
    use vayu_nmhost::runtime as nm;
    let opt = |s: &str| (!s.is_empty()).then(|| PathBuf::from(s));
    let combos = [
        ("", "/home/u", "", r"C:\Users\u\AppData\Local"),
        ("/portable", "/home/u", "/xdg", ""),
        ("", "/home/u", "/xdg", ""),
        ("", "", "/xdg", ""),
        ("", "", "", ""),
        ("", "/home/u", "", ""),
        ("", "", "", r"C:\L"),
    ];
    for (vayu, home, xdg, local) in combos {
        let ours = paths::Env {
            vayu_data_dir: opt(vayu),
            home: opt(home),
            xdg_data_home: opt(xdg),
            local_app_data: opt(local),
        };
        let theirs = nm::Env {
            vayu_data_dir: opt(vayu),
            home: opt(home),
            xdg_data_home: opt(xdg),
            xdg_config_home: None,
            local_app_data: opt(local),
        };
        for (os, nm_os) in [
            (paths::Os::Linux, nm::Os::Linux),
            (paths::Os::MacOs, nm::Os::MacOs),
            (paths::Os::Windows, nm::Os::Windows),
        ] {
            let a = paths::data_dir(os, &ours).ok();
            let b = nm::data_dir(nm_os, &theirs).ok();
            assert_eq!(a, b, "{os:?} {vayu:?} {home:?} {xdg:?} {local:?}");
        }
    }
    assert_eq!(paths::Os::current() as u8, nm::Os::current() as u8);
}

/// The host's reader accepts the file the daemon writes.
#[tokio::test]
async fn the_native_host_reads_our_runtime_file() {
    let dir = TempDir::new("nmhost-rt");
    let d = start(mock(), dir.path()).await;
    let info = vayu_nmhost::runtime::read_runtime(&d.runtime_file()).unwrap();
    assert_eq!(info.port, d.port());
    assert_eq!(info.token, d.runtime_info().token);
    assert_eq!(info.pid, Some(std::process::id()));
    assert_eq!(info.version.as_deref(), Some(vayu_daemon::VERSION));
    d.shutdown().await;
}
