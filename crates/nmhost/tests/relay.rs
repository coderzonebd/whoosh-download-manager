//! End-to-end tests of the relay against a fake daemon.
//!
//! The "browser" is a pair of in-memory pipes speaking native messaging
//! framing; the "daemon" is a real WebSocket server on 127.0.0.1 that checks
//! `session.hello` and records what it receives.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, DuplexStream};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use vayu_nmhost::framing::{self, Inbound, MAX_TO_BROWSER};
use vayu_nmhost::relay::{self, Config, RelayError};
use vayu_proto::ClientInfo;

const WAIT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------- fixtures

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("vayu-nmhost-test-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The host refuses a runtime file in a directory others can write to.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(dir)
    }

    fn runtime_file(&self) -> PathBuf {
        self.0.join("runtime.json")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_runtime(path: &Path, port: u16, token: &str) {
    let tmp = path.with_extension("tmp");
    let body = json!({ "port": port, "token": token, "pid": 1, "version": "9.9.9-fake" });
    std::fs::write(&tmp, body.to_string()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    std::fs::rename(&tmp, path).unwrap();
}

enum Cmd {
    Send(String),
    Close,
}

/// A daemon that accepts one connection at a time, insists on a correct
/// `session.hello`, and hands everything after it to the test.
struct FakeDaemon {
    port: u16,
    received: mpsc::UnboundedReceiver<String>,
    commands: mpsc::UnboundedSender<Cmd>,
    hellos: Arc<Mutex<Vec<Value>>>,
    task: JoinHandle<()>,
}

impl FakeDaemon {
    async fn start(token: &str) -> Self {
        Self::start_as(token, false).await
    }

    /// Something else on the daemon's port: accepts any token, but cannot
    /// compute the hello proof because it does not know the real token.
    async fn impostor() -> Self {
        Self::start_as("", true).await
    }

    async fn start_as(token: &str, impostor: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (received_tx, received) = mpsc::unbounded_channel();
        let (commands, mut commands_rx) = mpsc::unbounded_channel();
        let hellos = Arc::new(Mutex::new(Vec::new()));
        let token = token.to_owned();
        let hellos_task = hellos.clone();

        let task = tokio::spawn(async move {
            loop {
                // Commands must be heard between connections too, or `stop()`
                // would wait forever once the host has already hung up.
                let stream = tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => stream,
                        Err(_) => return,
                    },
                    cmd = commands_rx.recv() => match cmd {
                        Some(Cmd::Send(_)) => continue, // nobody connected
                        Some(Cmd::Close) | None => return,
                    },
                };
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    continue;
                };
                // Handshake.
                let Some(Ok(Message::Text(first))) = ws.next().await else {
                    continue;
                };
                let hello: Value = serde_json::from_str(&first).unwrap();
                hellos_task.lock().unwrap().push(hello.clone());
                if hello["method"] != "session.hello"
                    || (!impostor && hello["params"]["token"] != token.as_str())
                {
                    let _ = ws
                        .close(Some(CloseFrame {
                            code: CloseCode::from(4401),
                            reason: "unauthorized".into(),
                        }))
                        .await;
                    continue;
                }
                let nonce = hello["params"]["nonce"].as_str().unwrap_or_default();
                let key = if impostor {
                    "not-the-token"
                } else {
                    token.as_str()
                };
                let reply = json!({
                    "jsonrpc": "2.0",
                    "id": hello["id"],
                    "result": {
                        "daemonVersion": "9.9.9-fake",
                        "protocol": 1,
                        "capabilities": ["http"],
                        "proof": vayu_proto::auth::hello_proof(key, nonce),
                    },
                });
                ws.send(Message::text(reply.to_string())).await.unwrap();

                loop {
                    tokio::select! {
                        msg = ws.next() => match msg {
                            Some(Ok(Message::Text(text))) => {
                                let _ = received_tx.send(text.to_string());
                            }
                            Some(Ok(_)) => {}
                            _ => break,
                        },
                        cmd = commands_rx.recv() => match cmd {
                            Some(Cmd::Send(text)) => ws.send(Message::text(text)).await.unwrap(),
                            Some(Cmd::Close) | None => {
                                let _ = ws.close(None).await;
                                return;
                            }
                        },
                    }
                }
            }
        });

        Self {
            port,
            received,
            commands,
            hellos,
            task,
        }
    }

    fn send(&self, text: impl Into<String>) {
        self.commands.send(Cmd::Send(text.into())).unwrap();
    }

    async fn recv(&mut self) -> String {
        tokio::time::timeout(WAIT, self.received.recv())
            .await
            .expect("daemon received nothing in time")
            .expect("daemon task ended")
    }

    fn hellos(&self) -> Vec<Value> {
        self.hellos.lock().unwrap().clone()
    }

    /// Simulates a crash: the listener and the connection vanish without a
    /// close frame.
    async fn crash(self) {
        self.task.abort();
        let _ = self.task.await;
    }

    /// A clean shutdown: close frame, then the listener goes away.
    async fn stop(mut self) {
        let _ = self.commands.send(Cmd::Close);
        if tokio::time::timeout(WAIT, &mut self.task).await.is_err() {
            self.task.abort();
            panic!("fake daemon did not stop");
        }
    }
}

/// The browser side of the native messaging pipe.
struct Browser {
    to_host: DuplexStream,
    from_host: DuplexStream,
    task: JoinHandle<Result<(), RelayError>>,
}

fn test_config(runtime_file: PathBuf) -> Config {
    let mut cfg = Config::new(
        runtime_file,
        ClientInfo {
            name: "extension-chromium".into(),
            version: "0.0.1".into(),
        },
    );
    cfg.connect_timeout = Duration::from_secs(1);
    cfg.retry_min = Duration::from_millis(20);
    cfg.retry_max = Duration::from_millis(100);
    cfg.on_demand_gap = Duration::ZERO;
    cfg
}

impl Browser {
    fn start(cfg: Config) -> Self {
        let (to_host, host_in) = tokio::io::duplex(64 * 1024);
        let (host_out, from_host) = tokio::io::duplex(4 * 1024 * 1024);
        let task = tokio::spawn(relay::run(host_in, host_out, cfg));
        Self {
            to_host,
            from_host,
            task,
        }
    }

    async fn send(&mut self, text: &str) {
        let frame = framing::encode_frame(text.as_bytes()).unwrap();
        self.to_host.write_all(&frame).await.unwrap();
    }

    async fn recv(&mut self) -> String {
        let frame =
            tokio::time::timeout(WAIT, framing::read_frame(&mut self.from_host, usize::MAX))
                .await
                .expect("host sent nothing in time")
                .expect("framing error")
                .expect("host closed stdout");
        match frame {
            Inbound::Message(bytes) => String::from_utf8(bytes).unwrap(),
            Inbound::TooLarge(_) => unreachable!(),
        }
    }

    async fn recv_json(&mut self) -> Value {
        serde_json::from_str(&self.recv().await).unwrap()
    }

    /// Reads until a message matches, returning it; fails after `WAIT`.
    async fn recv_until(&mut self, pred: impl Fn(&Value) -> bool) -> Value {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let msg = tokio::time::timeout_at(deadline, self.recv_json())
                .await
                .expect("expected message never arrived");
            if pred(&msg) {
                return msg;
            }
        }
    }

    async fn status(&mut self, id: u64) -> Value {
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":"nm.status"}).to_string())
            .await;
        let reply = self.recv_until(|m| m["id"] == id).await;
        reply["result"].clone()
    }

    async fn close(self) -> Result<(), RelayError> {
        drop(self.to_host);
        tokio::time::timeout(WAIT, self.task)
            .await
            .expect("host did not exit after stdin closed")
            .unwrap()
    }
}

fn is_status_event(msg: &Value, running: bool) -> bool {
    msg["method"] == "event.nm.status" && msg["params"]["daemonRunning"] == running
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn without_a_daemon_status_works_and_calls_fail_fast() {
    let dir = TempDir::new("nodaemon");
    let mut browser = Browser::start(test_config(dir.runtime_file()));

    let status = browser.status(1).await;
    assert_eq!(status["daemonRunning"], false);
    assert_eq!(status["hostVersion"], env!("CARGO_PKG_VERSION"));
    assert!(status.get("daemonVersion").is_none());

    browser
        .send(r#"{"jsonrpc":"2.0","id":2,"method":"download.add","params":{"url":"https://example.com/a.zip"}}"#)
        .await;
    let reply = browser.recv_until(|m| m["id"] == 2).await;
    assert_eq!(reply["error"]["code"], -32007);

    browser.close().await.unwrap();
}

#[tokio::test]
async fn performs_hello_and_relays_verbatim_in_both_directions() {
    let dir = TempDir::new("verbatim");
    let mut daemon = FakeDaemon::start("secret-1").await;
    write_runtime(&dir.runtime_file(), daemon.port, "secret-1");
    let mut browser = Browser::start(test_config(dir.runtime_file()));

    // Odd key order, spacing and an unknown field must survive untouched.
    let request = r#"{ "id": 7, "jsonrpc":"2.0", "method":"download.add",  "params":{"url":"https://example.com/a.iso","x-extra":[1, 2.50, "é"]} }"#;
    browser.send(request).await;
    assert_eq!(daemon.recv().await, request);

    let hellos = daemon.hellos();
    assert_eq!(hellos.len(), 1);
    assert_eq!(hellos[0]["params"]["token"], "secret-1");
    assert_eq!(hellos[0]["params"]["protocol"], 1);
    assert_eq!(hellos[0]["params"]["client"]["name"], "extension-chromium");

    let response = r#"{"jsonrpc":"2.0","result":{"id":"dl_01"} ,"id":7}"#;
    daemon.send(response);
    assert_eq!(browser.recv().await, response);

    let status = browser.status(8).await;
    assert_eq!(status["daemonRunning"], true);
    assert_eq!(status["daemonVersion"], "9.9.9-fake");

    browser.close().await.unwrap();
    daemon.stop().await;
}

#[tokio::test]
async fn forwards_daemon_events() {
    let dir = TempDir::new("events");
    let mut daemon = FakeDaemon::start("t").await;
    write_runtime(&dir.runtime_file(), daemon.port, "t");
    let mut browser = Browser::start(test_config(dir.runtime_file()));

    // Make sure the connection is up before the daemon speaks.
    browser
        .send(r#"{"jsonrpc":"2.0","id":"a","method":"stats.get"}"#)
        .await;
    daemon.recv().await;

    let event = r#"{"jsonrpc":"2.0","method":"event.download.progress","params":{"items":[{"id":"dl_1","downloadedBytes":1048576,"speedBps":5,"connections":8}]}}"#;
    daemon.send(event);
    assert_eq!(browser.recv().await, event);

    let stats = r#"{"jsonrpc":"2.0","method":"event.stats","params":{"downloadBps":1,"uploadBps":0,"active":1,"queued":0}}"#;
    daemon.send(stats);
    assert_eq!(browser.recv().await, stats);

    browser.close().await.unwrap();
    daemon.stop().await;
}

#[tokio::test]
async fn survives_a_daemon_restart_on_a_new_port_with_a_new_token() {
    let dir = TempDir::new("restart");
    let mut first = FakeDaemon::start("token-a").await;
    write_runtime(&dir.runtime_file(), first.port, "token-a");
    let mut browser = Browser::start(test_config(dir.runtime_file()));

    // A request the first daemon never answers.
    browser
        .send(r#"{"jsonrpc":"2.0","id":1,"method":"download.get","params":{"id":"dl_1"}}"#)
        .await;
    first.recv().await;
    first.crash().await;

    // The in-flight request fails and the extension hears the daemon is gone,
    // in either order.
    let mut got_error = false;
    let mut got_event = false;
    while !(got_error && got_event) {
        let msg = browser.recv_json().await;
        if msg["id"] == 1 {
            assert_eq!(msg["error"]["code"], -32007);
            got_error = true;
        } else if is_status_event(&msg, false) {
            got_event = true;
        } else {
            panic!("unexpected message {msg}");
        }
    }

    // Requests while the daemon is down fail instead of hanging.
    browser
        .send(r#"{"jsonrpc":"2.0","id":2,"method":"stats.get"}"#)
        .await;
    let reply = browser.recv_until(|m| m["id"] == 2).await;
    assert_eq!(reply["error"]["code"], -32007);

    // The daemon comes back elsewhere; the host finds it on its own.
    let mut second = FakeDaemon::start("token-b").await;
    write_runtime(&dir.runtime_file(), second.port, "token-b");
    let event = browser.recv_until(|m| is_status_event(m, true)).await;
    assert_eq!(event["params"]["daemonVersion"], "9.9.9-fake");
    assert_eq!(
        second.hellos().last().unwrap()["params"]["token"],
        "token-b"
    );

    browser
        .send(r#"{"jsonrpc":"2.0","id":3,"method":"stats.get"}"#)
        .await;
    assert_eq!(
        second.recv().await,
        r#"{"jsonrpc":"2.0","id":3,"method":"stats.get"}"#
    );
    second.send(r#"{"jsonrpc":"2.0","id":3,"result":{"downloadBps":0,"uploadBps":0,"active":0,"queued":0}}"#);
    let reply = browser.recv_until(|m| m["id"] == 3).await;
    assert_eq!(reply["result"]["active"], 0);

    browser.close().await.unwrap();
    second.stop().await;
}

#[tokio::test]
async fn reconnects_after_a_clean_daemon_shutdown() {
    let dir = TempDir::new("clean");
    let mut first = FakeDaemon::start("a").await;
    write_runtime(&dir.runtime_file(), first.port, "a");
    let mut browser = Browser::start(test_config(dir.runtime_file()));
    browser
        .send(r#"{"jsonrpc":"2.0","id":1,"method":"stats.get"}"#)
        .await;
    first.recv().await;
    first.send(r#"{"jsonrpc":"2.0","id":1,"result":{"downloadBps":0,"uploadBps":0,"active":0,"queued":0}}"#);
    browser.recv_until(|m| m["id"] == 1).await;

    first.stop().await;
    browser.recv_until(|m| is_status_event(m, false)).await;

    let second = FakeDaemon::start("b").await;
    write_runtime(&dir.runtime_file(), second.port, "b");
    browser.recv_until(|m| is_status_event(m, true)).await;

    browser.close().await.unwrap();
    second.stop().await;
}

#[tokio::test]
async fn a_wrong_token_is_reported_as_unavailable() {
    let dir = TempDir::new("badtoken");
    let daemon = FakeDaemon::start("right").await;
    write_runtime(&dir.runtime_file(), daemon.port, "wrong");
    let mut browser = Browser::start(test_config(dir.runtime_file()));

    browser
        .send(r#"{"jsonrpc":"2.0","id":1,"method":"stats.get"}"#)
        .await;
    let reply = browser.recv_until(|m| m["id"] == 1).await;
    assert_eq!(reply["error"]["code"], -32007);
    assert_eq!(browser.status(2).await["daemonRunning"], false);

    browser.close().await.unwrap();
    daemon.crash().await;
}

#[tokio::test]
async fn never_relays_to_an_impostor_on_the_daemons_port() {
    let dir = TempDir::new("impostor");
    let mut impostor = FakeDaemon::impostor().await;
    write_runtime(&dir.runtime_file(), impostor.port, "stale-token");
    let mut browser = Browser::start(test_config(dir.runtime_file()));

    browser
        .send(r#"{"jsonrpc":"2.0","id":1,"method":"download.add","params":{"url":"https://example.com/","request":{"cookies":"session=SECRET"}}}"#)
        .await;
    let reply = browser.recv_until(|m| m["id"] == 1).await;
    assert_eq!(reply["error"]["code"], -32007);
    assert_eq!(browser.status(2).await["daemonRunning"], false);
    // The hello went out (with the stale token), but nothing after it.
    assert!(!impostor.hellos().is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(300), impostor.received.recv())
            .await
            .is_err(),
        "a request reached the impostor"
    );

    browser.close().await.unwrap();
    impostor.crash().await;
}

#[tokio::test]
async fn sends_a_fresh_valid_nonce_on_every_connection() {
    let dir = TempDir::new("nonce");
    let mut daemon = FakeDaemon::start("t").await;
    write_runtime(&dir.runtime_file(), daemon.port, "t");
    let mut browser = Browser::start(test_config(dir.runtime_file()));
    browser
        .send(r#"{"jsonrpc":"2.0","id":1,"method":"stats.get"}"#)
        .await;
    daemon.recv().await;
    let nonce = daemon.hellos()[0]["params"]["nonce"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(vayu_proto::auth::is_valid_nonce(&nonce), "{nonce}");

    browser.close().await.unwrap();
    daemon.stop().await;
}

#[tokio::test]
async fn oversize_messages_from_the_daemon() {
    let dir = TempDir::new("oversize");
    let mut daemon = FakeDaemon::start("t").await;
    write_runtime(&dir.runtime_file(), daemon.port, "t");
    let mut browser = Browser::start(test_config(dir.runtime_file()));

    browser
        .send(r#"{"jsonrpc":"2.0","id":3,"method":"download.list","params":{}}"#)
        .await;
    daemon.recv().await;

    // A response over 1 MiB becomes an error for the same id.
    let big = "x".repeat(MAX_TO_BROWSER + 10);
    daemon.send(json!({"jsonrpc":"2.0","id":3,"result":[big]}).to_string());
    let reply = browser.recv_json().await;
    assert_eq!(reply["id"], 3);
    assert_eq!(reply["error"]["code"], -32008);
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("native messaging")
    );

    // An oversize notification is dropped; the next one still arrives.
    daemon.send(json!({"jsonrpc":"2.0","method":"event.stats","params":{"pad":big}}).to_string());
    let small = r#"{"jsonrpc":"2.0","method":"event.stats","params":{"downloadBps":0,"uploadBps":0,"active":0,"queued":0}}"#;
    daemon.send(small);
    assert_eq!(browser.recv().await, small);

    // A message of exactly the limit still goes through.
    let prefix = r#"{"jsonrpc":"2.0","method":"event.x","params":""#;
    let suffix = r#""}"#;
    let pad = "y".repeat(MAX_TO_BROWSER - prefix.len() - suffix.len());
    let exact = format!("{prefix}{pad}{suffix}");
    assert_eq!(exact.len(), MAX_TO_BROWSER);
    daemon.send(exact.clone());
    assert_eq!(browser.recv().await, exact);

    browser.close().await.unwrap();
    daemon.stop().await;
}

#[tokio::test]
async fn oversize_and_malformed_messages_from_the_browser() {
    let dir = TempDir::new("badinput");
    let mut cfg = test_config(dir.runtime_file());
    cfg.max_from_browser = 64;
    let mut browser = Browser::start(cfg);

    browser
        .send(&format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"x","params":"{}"}}"#,
            "z".repeat(100)
        ))
        .await;
    let reply = browser.recv_json().await;
    assert_eq!(reply["id"], Value::Null);
    assert_eq!(reply["error"]["code"], -32600);

    browser.send("{not json").await;
    let reply = browser.recv_json().await;
    assert_eq!(reply["error"]["code"], -32700);

    browser.send("[1,2]").await;
    let reply = browser.recv_json().await;
    assert_eq!(reply["error"]["code"], -32600);

    // Still alive afterwards.
    assert_eq!(browser.status(2).await["daemonRunning"], false);
    browser.close().await.unwrap();
}

#[tokio::test]
async fn refuses_session_hello_and_unknown_host_methods() {
    let dir = TempDir::new("hello");
    let mut browser = Browser::start(test_config(dir.runtime_file()));

    browser
        .send(r#"{"jsonrpc":"2.0","id":1,"method":"session.hello","params":{"token":"guess"}}"#)
        .await;
    let reply = browser.recv_until(|m| m["id"] == 1).await;
    assert_eq!(reply["error"]["code"], -32600);

    browser
        .send(r#"{"jsonrpc":"2.0","id":2,"method":"nm.selfDestruct"}"#)
        .await;
    let reply = browser.recv_until(|m| m["id"] == 2).await;
    assert_eq!(reply["error"]["code"], -32601);

    browser.close().await.unwrap();
}

#[tokio::test]
async fn exits_when_the_browser_closes_the_port() {
    let dir = TempDir::new("eof");
    let mut daemon = FakeDaemon::start("t").await;
    write_runtime(&dir.runtime_file(), daemon.port, "t");
    let mut browser = Browser::start(test_config(dir.runtime_file()));
    browser
        .send(r#"{"jsonrpc":"2.0","id":1,"method":"stats.get"}"#)
        .await;
    daemon.recv().await;

    // Returns promptly (and cleanly) even with a live daemon connection.
    browser.close().await.unwrap();
    daemon.crash().await;
}
