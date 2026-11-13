//! Helpers shared by the integration tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::{FutureExt, SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use whoosh_daemon::{DaemonConfig, RunningDaemon};
use whoosh_proto::service::{Notification, Service, ServiceError, ServiceResult};
use whoosh_proto::*;

/// A fresh, empty directory under the system temp dir, removed on drop.
#[derive(Debug)]
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "whoosh-daemon-test-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A `Service` driven by the test: it answers a few methods canned, and the
/// test pushes notifications with [`ScriptedService::emit`].
#[derive(Default)]
pub struct ScriptedService {
    subscribers: Mutex<Vec<mpsc::UnboundedSender<Notification>>>,
    pub shutdown_called: AtomicBool,
}

impl ScriptedService {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Sends `n` to every live subscriber; returns how many got it.
    pub fn emit(&self, n: Notification) -> usize {
        let mut subs = self.subscribers.lock().unwrap();
        subs.retain(|tx| tx.send(n.clone()).is_ok());
        subs.len()
    }

    pub fn subscriber_count(&self) -> usize {
        let mut subs = self.subscribers.lock().unwrap();
        subs.retain(|tx| !tx.is_closed());
        subs.len()
    }

    pub async fn wait_for_subscribers(&self, n: usize) {
        for _ in 0..500 {
            if self.subscriber_count() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no subscriber appeared");
    }
}

fn ready<T: Send + 'static>(v: ServiceResult<T>) -> BoxFuture<'static, ServiceResult<T>> {
    futures::future::ready(v).boxed()
}

impl Service for ScriptedService {
    fn capabilities(&self) -> Vec<String> {
        vec!["http".into(), "scripted".into()]
    }
    fn probe(&self, p: ProbeParams) -> BoxFuture<'_, ServiceResult<ProbeResult>> {
        ready(Ok(ProbeResult {
            total_bytes: Some(1),
            resumable: true,
            filename: "x".into(),
            content_type: None,
            final_url: p.url,
        }))
    }
    fn add(&self, _: AddParams) -> BoxFuture<'_, ServiceResult<AddResult>> {
        ready(Ok(AddResult { id: "dl_1".into() }))
    }
    fn list(&self, _: ListParams) -> BoxFuture<'_, ServiceResult<Vec<DownloadSummary>>> {
        ready(Ok(Vec::new()))
    }
    fn get(&self, p: IdParams) -> BoxFuture<'_, ServiceResult<DownloadDetail>> {
        ready(Err(ServiceError::not_found(&p.id)))
    }
    fn pause(&self, p: IdsParams) -> BoxFuture<'_, ServiceResult<()>> {
        ready(match p.ids.first() {
            Some(id) => Err(ServiceError::not_found(id)),
            None => Ok(()),
        })
    }
    fn resume(&self, _: IdsParams) -> BoxFuture<'_, ServiceResult<()>> {
        ready(Err(ServiceError::invalid_state("nothing to resume")))
    }
    fn remove(&self, _: RemoveParams) -> BoxFuture<'_, ServiceResult<()>> {
        ready(Ok(()))
    }
    fn refresh_url(&self, _: RefreshUrlParams) -> BoxFuture<'_, ServiceResult<()>> {
        ready(Err(ServiceError::new(-32099, "custom engine error")))
    }
    fn preconnect(&self, _: PreconnectParams) -> BoxFuture<'_, ServiceResult<()>> {
        ready(Ok(()))
    }
    fn stats(&self) -> BoxFuture<'_, ServiceResult<Stats>> {
        ready(Ok(Stats {
            download_bps: 7,
            ..Stats::default()
        }))
    }
    fn settings(&self) -> BoxFuture<'_, ServiceResult<Settings>> {
        ready(Ok(settings()))
    }
    fn set_settings(&self, s: Settings) -> BoxFuture<'_, ServiceResult<Settings>> {
        ready(Ok(s))
    }
    fn subscribe(&self) -> BoxStream<'static, Notification> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subscribers.lock().unwrap().push(tx);
        futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|n| (n, rx)) })
            .boxed()
    }
    fn shutdown(&self) -> BoxFuture<'_, ()> {
        self.shutdown_called.store(true, Ordering::SeqCst);
        self.subscribers.lock().unwrap().clear();
        futures::future::ready(()).boxed()
    }
}

pub fn settings() -> Settings {
    Settings {
        default_directory: "/downloads".into(),
        max_active_downloads: 3,
        max_connections_per_download: 8,
        speed_limit_bps: None,
        language: "en".into(),
        theme: "system".into(),
        durability: Default::default(),
    }
}

/// A config for tests: a temp data dir and any free port.
pub fn config(dir: &Path) -> DaemonConfig {
    let mut c = DaemonConfig::new(dir);
    c.port = 0;
    c
}

pub async fn start(service: Arc<dyn Service>, dir: &Path) -> RunningDaemon {
    whoosh_daemon::serve(service, config(dir)).await.unwrap()
}

pub async fn start_with(
    service: Arc<dyn Service>,
    dir: &Path,
    tweak: impl FnOnce(&mut DaemonConfig),
) -> RunningDaemon {
    let mut c = config(dir);
    tweak(&mut c);
    whoosh_daemon::serve(service, c).await.unwrap()
}

pub type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Opens the WebSocket without saying hello.
pub async fn ws(daemon: &RunningDaemon) -> Ws {
    tokio_tungstenite::connect_async(daemon.rpc_url())
        .await
        .unwrap()
        .0
}

/// The nonce the tests send (PROTOCOL.md's example).
pub const NONCE: &str = "00112233445566778899aabbccddeeff";

pub fn hello_frame(token: &str, protocol: u32, id: Value) -> String {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "session.hello",
        "params": { "token": token, "nonce": NONCE, "protocol": protocol,
                    "client": { "name": "test", "version": "0" } }
    })
    .to_string()
}

/// Opens the WebSocket and completes `session.hello`.
pub async fn authed(daemon: &RunningDaemon) -> Ws {
    let mut ws = ws(daemon).await;
    say_hello(&mut ws, daemon).await;
    ws
}

pub async fn say_hello(ws: &mut Ws, daemon: &RunningDaemon) -> Value {
    ws.send(Message::text(hello_frame(
        &daemon.runtime_info().token,
        PROTOCOL_VERSION,
        json!("h"),
    )))
    .await
    .unwrap();
    let reply = next_json(ws).await;
    assert_eq!(reply["id"], "h", "{reply}");
    // The daemon proves it knows the token.
    let proof = reply["result"]["proof"].as_str().unwrap_or_default();
    assert!(
        whoosh_proto::auth::verify_hello_proof(&daemon.runtime_info().token, NONCE, proof),
        "bad proof in {reply}"
    );
    reply
}

pub async fn send(ws: &mut Ws, v: &Value) {
    ws.send(Message::text(v.to_string())).await.unwrap();
}

pub async fn send_text(ws: &mut Ws, text: &str) {
    ws.send(Message::text(text.to_owned())).await.unwrap();
}

/// The next text message as JSON (skipping pings); panics on close.
pub async fn next_json(ws: &mut Ws) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("timed out waiting for a message")
            .expect("connection ended")
            .expect("read failed");
        match msg {
            Message::Text(t) => return serde_json::from_str(t.as_str()).unwrap(),
            Message::Close(f) => panic!("unexpected close: {f:?}"),
            _ => continue,
        }
    }
}

/// The next response (a message without `method`), skipping notifications.
pub async fn next_response(ws: &mut Ws) -> Value {
    loop {
        let v = next_json(ws).await;
        if v.get("method").is_none() {
            return v;
        }
    }
}

pub async fn call(ws: &mut Ws, id: i64, method: &str, params: Value) -> Value {
    send(
        ws,
        &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
    )
    .await;
    let reply = next_response(ws).await;
    assert_eq!(reply["id"], id, "{reply}");
    reply
}

/// Reads until the daemon closes; returns the close code (None if the
/// connection just ended).
pub async fn close_code(ws: &mut Ws) -> Option<u16> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Close(frame))) => return frame.map(|f| f.code.into()),
                Some(Ok(_)) => continue,
                Some(Err(tungstenite::Error::ConnectionClosed)) | None => return None,
                Some(Err(_)) => return None,
            }
        }
    })
    .await
    .expect("the daemon did not close the connection")
}

/// A plain HTTP/1.1 request; returns (status, body).
pub async fn http(port: u16, request_head: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(request_head.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

pub fn get(path: &str, port: u16) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
}

/// Builds a binary of another workspace package with the same cargo and
/// target directory as this test run, and returns its path. `$<ENV>` (e.g.
/// `WHOOSH_NMHOST_BIN`) skips the build and uses that file instead.
pub fn workspace_bin(package: &str, bin: &str, env: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(env) {
        return PathBuf::from(path);
    }
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = std::process::Command::new(cargo)
        .args([
            "build",
            "--quiet",
            "--message-format=json",
            "-p",
            package,
            "--bin",
            bin,
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stderr(std::process::Stdio::inherit())
        .output()
        .expect("cannot run cargo");
    assert!(out.status.success(), "cargo build -p {package} failed");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|msg| msg["reason"] == "compiler-artifact" && msg["target"]["name"] == bin)
        .find_map(|msg| msg["executable"].as_str().map(PathBuf::from))
        .unwrap_or_else(|| panic!("cargo did not report the {bin} executable"))
}
