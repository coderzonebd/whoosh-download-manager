//! The relay: browser (native messaging on stdio) ⇄ daemon (WebSocket).
//!
//! * Messages from the browser are JSON-RPC 2.0. `nm.status` is answered
//!   here; everything else is sent to the daemon as the exact text the
//!   browser wrote.
//! * Messages from the daemon (responses and `event.*` notifications) are
//!   written to the browser as the exact text the daemon sent, unless they
//!   exceed the browser's 1 MiB limit.
//! * The host performs `session.hello` itself; the extension never sees the
//!   token. When the daemon goes away the host fails in-flight requests,
//!   tells the extension (`event.nm.status`) and keeps reconnecting, re-reading
//!   the runtime file each time because the port and token may change.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::{self, Message, Utf8Bytes};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use whoosh_proto::{ClientInfo, HelloParams, HelloResult, PROTOCOL_VERSION, RPC_PATH, method};

use crate::framing::{self, FrameError, Inbound, MAX_FROM_BROWSER, MAX_TO_BROWSER};
use crate::log;
use crate::runtime::{RuntimeError, read_runtime};

/// Methods and events that exist only between the extension and the host.
pub const NM_STATUS: &str = "nm.status";
/// Sent by the host when the daemon connection goes up or down. Same payload
/// as the `nm.status` result. Proposed addition to PROTOCOL.md §1.
pub const NM_STATUS_EVENT: &str = "event.nm.status";

/// The id the host uses for its own `session.hello`; never relayed. Request
/// ids starting with `nmhost:` are reserved for the host.
const HELLO_ID: &str = "nmhost:hello";

/// JSON-RPC error codes the host produces itself.
pub mod code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// The daemon is not running, refused the host, or the connection broke.
    /// Proposed addition to PROTOCOL.md §5.
    pub const DAEMON_UNAVAILABLE: i64 = -32007;
    /// A response would exceed the browser's 1 MiB native messaging limit.
    /// Proposed addition to PROTOCOL.md §5.
    pub const MESSAGE_TOO_LARGE: i64 = -32008;
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Path of the daemon's `runtime.json`.
    pub runtime_file: PathBuf,
    /// What the host says it is in `session.hello`.
    pub client: ClientInfo,
    pub host_version: String,
    /// Upper bound for TCP connect + WebSocket upgrade + `session.hello`.
    pub connect_timeout: Duration,
    /// First delay before reconnecting after the daemon went away; doubles
    /// on every failure up to `retry_max`.
    pub retry_min: Duration,
    pub retry_max: Duration,
    /// While the daemon is down, a browser request triggers an immediate
    /// connection attempt unless one was made less than this long ago.
    pub on_demand_gap: Duration,
    /// Browser → host frames above this size are discarded and answered with
    /// an error.
    pub max_from_browser: usize,
}

impl Config {
    pub fn new(runtime_file: PathBuf, client: ClientInfo) -> Self {
        Self {
            runtime_file,
            client,
            host_version: env!("CARGO_PKG_VERSION").to_owned(),
            connect_timeout: Duration::from_secs(2),
            retry_min: Duration::from_millis(250),
            retry_max: Duration::from_secs(5),
            on_demand_gap: Duration::from_millis(250),
            max_from_browser: MAX_FROM_BROWSER,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error("WebSocket: {0}")]
    WebSocket(#[from] tungstenite::Error),
    #[error("timed out")]
    Timeout,
    #[error("the daemon closed the connection during session.hello (close code {0:?})")]
    Closed(Option<u16>),
    #[error("the daemon rejected session.hello: {0}")]
    Rejected(String),
    #[error("unexpected session.hello reply: {0}")]
    BadHello(String),
    /// Whatever listens on the port did not prove it knows the token: a stale
    /// runtime file whose port someone else now holds.
    #[error("the process on the daemon's port could not prove it is the daemon")]
    Impostor,
    #[error("connection task failed: {0}")]
    Task(String),
}

/// Only what the relay needs to route a message; the rest is skipped.
#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug)]
struct Daemon {
    ws: Ws,
    version: String,
}

#[derive(Debug)]
enum Conn {
    Down { retry_at: Instant },
    Connecting(JoinHandle<Result<Box<Daemon>, ConnectError>>),
    Up(Box<Daemon>),
}

#[derive(Debug)]
enum ConnEvent {
    RetryDue,
    Connected(Result<Box<Daemon>, ConnectError>),
    Daemon(Option<Result<Message, tungstenite::Error>>),
}

impl Conn {
    /// Waits for whatever the current state can produce. Cancel-safe: the
    /// relay's `select!` drops this future whenever the browser speaks first.
    async fn next_event(&mut self) -> ConnEvent {
        match self {
            Conn::Down { retry_at } => {
                tokio::time::sleep_until(*retry_at).await;
                ConnEvent::RetryDue
            }
            Conn::Connecting(handle) => ConnEvent::Connected(match handle.await {
                Ok(result) => result,
                Err(e) => Err(ConnectError::Task(e.to_string())),
            }),
            Conn::Up(daemon) => ConnEvent::Daemon(daemon.ws.next().await),
        }
    }
}

/// A browser message that arrived while a connection attempt was running.
#[derive(Debug)]
enum Waiter {
    Status { id: Value },
    Forward { text: String, id: Option<Value> },
}

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("reading from the browser: {0}")]
    Input(#[from] FrameError),
}

/// Runs the host until the browser closes its end of the pipe.
pub async fn run<R, W>(input: R, output: W, config: Config) -> Result<(), RelayError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (in_tx, mut in_rx) = mpsc::channel(64);
    let reader = tokio::spawn(read_loop(input, in_tx, config.max_from_browser));
    let (out_tx, out_rx) = mpsc::channel(256);
    let writer = tokio::spawn(write_loop(output, out_rx));

    let mut relay = Relay::new(config, out_tx);
    relay.start_connecting();

    let mut result = Ok(());
    while !relay.output_closed {
        tokio::select! {
            inbound = in_rx.recv() => match inbound {
                None => break,
                Some(Err(e)) => {
                    result = Err(RelayError::Input(e));
                    break;
                }
                Some(Ok(Inbound::TooLarge(len))) => {
                    let msg = format!(
                        "message is {len} bytes; the host accepts at most {}",
                        relay.cfg.max_from_browser
                    );
                    relay.reply_error(Value::Null, code::INVALID_REQUEST, &msg).await;
                }
                Some(Ok(Inbound::Message(bytes))) => relay.on_browser_message(bytes).await,
            },
            event = relay.conn.next_event() => relay.on_conn_event(event).await,
        }
    }

    relay.shutdown().await;
    drop(relay);
    reader.abort();
    if let Ok(Err(e)) = writer.await {
        log::debug(&format!("writing to the browser failed: {e}"));
    }
    result
}

async fn read_loop<R>(mut input: R, tx: mpsc::Sender<Result<Inbound, FrameError>>, max: usize)
where
    R: AsyncRead + Unpin,
{
    loop {
        let next = framing::read_frame(&mut input, max).await;
        let done = !matches!(next, Ok(Some(_)));
        let item = match next {
            Ok(Some(frame)) => Ok(frame),
            Ok(None) => return,
            Err(e) => Err(e),
        };
        if tx.send(item).await.is_err() || done {
            return;
        }
    }
}

async fn write_loop<W>(mut output: W, mut rx: mpsc::Receiver<Vec<u8>>) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    while let Some(frame) = rx.recv().await {
        framing::write_encoded(&mut output, &frame).await?;
    }
    Ok(())
}

struct Relay {
    cfg: Config,
    out: mpsc::Sender<Vec<u8>>,
    output_closed: bool,
    conn: Conn,
    waiting: Vec<Waiter>,
    /// Requests sent to the daemon and not yet answered, keyed by their id
    /// serialised as JSON (so `1` and `"1"` stay distinct).
    pending: HashMap<String, Value>,
    retry_delay: Duration,
    last_attempt: Option<Instant>,
    /// Last daemon state the extension knows about. `None` until the first
    /// attempt finishes; that first result is not an "event".
    reported: Option<bool>,
}

impl Relay {
    fn new(cfg: Config, out: mpsc::Sender<Vec<u8>>) -> Self {
        let retry_delay = cfg.retry_min;
        Self {
            cfg,
            out,
            output_closed: false,
            conn: Conn::Down {
                retry_at: Instant::now(),
            },
            waiting: Vec::new(),
            pending: HashMap::new(),
            retry_delay,
            last_attempt: None,
            reported: None,
        }
    }

    // ------------------------------------------------------------ browser side

    async fn on_browser_message(&mut self, bytes: Vec<u8>) {
        let Ok(text) = String::from_utf8(bytes) else {
            self.reply_error(Value::Null, code::PARSE_ERROR, "message is not UTF-8")
                .await;
            return;
        };
        let envelope: Envelope = match serde_json::from_str(&text) {
            Ok(envelope) => envelope,
            Err(e) => {
                let code = if e.is_data() {
                    code::INVALID_REQUEST
                } else {
                    code::PARSE_ERROR
                };
                self.reply_error(Value::Null, code, &e.to_string()).await;
                return;
            }
        };

        let Envelope { id, method } = envelope;
        let Some(method) = method else {
            if let Some(id) = id {
                self.reply_error(id, code::INVALID_REQUEST, "expected a request")
                    .await;
            }
            return;
        };

        if method == NM_STATUS {
            if let Some(id) = id {
                self.status_request(id).await;
            }
        } else if method == method::SESSION_HELLO {
            if let Some(id) = id {
                self.reply_error(
                    id,
                    code::INVALID_REQUEST,
                    "session.hello is performed by whoosh-nmhost; extensions must not send it",
                )
                .await;
            }
        } else if method.starts_with("nm.") {
            if let Some(id) = id {
                let msg = format!("unknown host method {method}");
                self.reply_error(id, code::METHOD_NOT_FOUND, &msg).await;
            }
        } else {
            self.forward(text, id).await;
        }
    }

    async fn status_request(&mut self, id: Value) {
        match self.conn {
            Conn::Up(_) => self.reply_status(id, true).await,
            Conn::Connecting(_) => self.waiting.push(Waiter::Status { id }),
            Conn::Down { .. } => {
                if self.may_attempt_now() {
                    self.start_connecting();
                    self.waiting.push(Waiter::Status { id });
                } else {
                    self.reply_status(id, false).await;
                }
            }
        }
    }

    async fn forward(&mut self, text: String, id: Option<Value>) {
        match self.conn {
            Conn::Up(_) => self.send_to_daemon(text, id).await,
            Conn::Connecting(_) => self.waiting.push(Waiter::Forward { text, id }),
            Conn::Down { .. } => {
                if self.may_attempt_now() {
                    self.start_connecting();
                    self.waiting.push(Waiter::Forward { text, id });
                } else if let Some(id) = id {
                    self.reply_unavailable(id, "the whoosh daemon is not running")
                        .await;
                }
            }
        }
    }

    async fn send_to_daemon(&mut self, text: String, id: Option<Value>) {
        let Conn::Up(daemon) = &mut self.conn else {
            if let Some(id) = id {
                self.reply_unavailable(id, "the connection to the daemon was lost")
                    .await;
            }
            return;
        };
        if let Some(id) = &id {
            self.pending.insert(id_key(id), id.clone());
        }
        if let Err(e) = daemon.ws.send(Message::text(text)).await {
            log::warn(&format!("sending to the daemon failed: {e}"));
            self.on_lost().await;
        }
    }

    // ------------------------------------------------------------ daemon side

    async fn on_conn_event(&mut self, event: ConnEvent) {
        match event {
            ConnEvent::RetryDue => self.start_connecting(),
            ConnEvent::Connected(Ok(daemon)) => {
                log::debug(&format!("connected to daemon {}", daemon.version));
                self.conn = Conn::Up(daemon);
                self.retry_delay = self.cfg.retry_min;
                self.report(true).await;
                for waiter in std::mem::take(&mut self.waiting) {
                    match waiter {
                        Waiter::Status { id } => {
                            let up = matches!(self.conn, Conn::Up(_));
                            self.reply_status(id, up).await;
                        }
                        Waiter::Forward { text, id } => self.send_to_daemon(text, id).await,
                    }
                }
            }
            ConnEvent::Connected(Err(e)) => {
                log::debug(&format!("cannot reach the daemon: {e}"));
                self.conn = Conn::Down {
                    retry_at: Instant::now() + self.retry_delay,
                };
                self.retry_delay = (self.retry_delay * 2).min(self.cfg.retry_max);
                self.report(false).await;
                let reason = format!("the whoosh daemon is not available: {e}");
                for waiter in std::mem::take(&mut self.waiting) {
                    match waiter {
                        Waiter::Status { id } => self.reply_status(id, false).await,
                        Waiter::Forward { id: Some(id), .. } => {
                            self.reply_unavailable(id, &reason).await;
                        }
                        Waiter::Forward { id: None, .. } => {}
                    }
                }
            }
            ConnEvent::Daemon(Some(Ok(Message::Text(text)))) => self.on_daemon_text(text).await,
            ConnEvent::Daemon(Some(Ok(Message::Binary(_)))) => {
                log::warn("ignoring a binary frame from the daemon");
            }
            ConnEvent::Daemon(Some(Ok(Message::Close(frame)))) => {
                log::debug(&format!("daemon closed the connection: {frame:?}"));
                self.on_lost().await;
            }
            ConnEvent::Daemon(Some(Ok(_))) => {} // ping / pong, handled by tungstenite
            ConnEvent::Daemon(Some(Err(e))) => {
                log::debug(&format!("daemon connection error: {e}"));
                self.on_lost().await;
            }
            ConnEvent::Daemon(None) => self.on_lost().await,
        }
    }

    async fn on_daemon_text(&mut self, text: Utf8Bytes) {
        let envelope: Envelope = match serde_json::from_str(text.as_str()) {
            Ok(envelope) => envelope,
            Err(e) => {
                log::warn(&format!("dropping malformed message from the daemon: {e}"));
                return;
            }
        };
        let is_response = envelope.method.is_none();
        if is_response && let Some(id) = &envelope.id {
            self.pending.remove(&id_key(id));
        }

        if text.len() > MAX_TO_BROWSER {
            match (is_response, envelope.id) {
                (true, Some(id)) => {
                    let msg = format!(
                        "the daemon's response is {} bytes; native messaging allows at most \
                         {MAX_TO_BROWSER}. Ask for less (for example, filter download.list by state).",
                        text.len()
                    );
                    self.reply_error(id, code::MESSAGE_TOO_LARGE, &msg).await;
                }
                _ => log::warn(&format!(
                    "dropping {} from the daemon: {} bytes is over the 1 MiB native messaging limit",
                    envelope.method.as_deref().unwrap_or("a message"),
                    text.len()
                )),
            }
            return;
        }
        self.send_raw(text.as_bytes()).await;
    }

    async fn on_lost(&mut self) {
        if let Conn::Up(daemon) = &mut self.conn {
            // Best effort; the socket is probably gone already.
            let _ = tokio::time::timeout(Duration::from_millis(100), daemon.ws.close(None)).await;
        }
        self.conn = Conn::Down {
            retry_at: Instant::now() + self.cfg.retry_min,
        };
        self.retry_delay = (self.cfg.retry_min * 2).min(self.cfg.retry_max);
        let pending: Vec<Value> = self.pending.drain().map(|(_, id)| id).collect();
        for id in pending {
            self.reply_unavailable(
                id,
                "the connection to the daemon was lost before it answered; \
                 the request may or may not have been carried out",
            )
            .await;
        }
        self.report(false).await;
    }

    // ------------------------------------------------------------ connection

    fn may_attempt_now(&self) -> bool {
        self.last_attempt
            .is_none_or(|at| at.elapsed() >= self.cfg.on_demand_gap)
    }

    fn start_connecting(&mut self) {
        if matches!(self.conn, Conn::Up(_) | Conn::Connecting(_)) {
            return;
        }
        self.last_attempt = Some(Instant::now());
        let cfg = self.cfg.clone();
        self.conn = Conn::Connecting(tokio::spawn(async move {
            match tokio::time::timeout(cfg.connect_timeout, connect(&cfg)).await {
                Ok(result) => result,
                Err(_) => Err(ConnectError::Timeout),
            }
        }));
    }

    async fn shutdown(&mut self) {
        match &mut self.conn {
            Conn::Up(daemon) => {
                let _ =
                    tokio::time::timeout(Duration::from_millis(200), daemon.ws.close(None)).await;
            }
            Conn::Connecting(handle) => handle.abort(),
            Conn::Down { .. } => {}
        }
    }

    // ------------------------------------------------------------ output

    fn status_json(&self, running: bool) -> Value {
        let mut status = json!({
            "daemonRunning": running,
            "hostVersion": self.cfg.host_version,
        });
        if let (true, Conn::Up(daemon)) = (running, &self.conn) {
            status["daemonVersion"] = Value::String(daemon.version.clone());
        }
        status
    }

    async fn report(&mut self, running: bool) {
        let previous = self.reported.replace(running);
        if previous.is_some() && previous != Some(running) {
            let event = json!({
                "jsonrpc": "2.0",
                "method": NM_STATUS_EVENT,
                "params": self.status_json(running),
            });
            self.send_raw(event.to_string().as_bytes()).await;
        }
    }

    async fn reply_status(&mut self, id: Value, running: bool) {
        let reply = json!({ "jsonrpc": "2.0", "id": id, "result": self.status_json(running) });
        self.send_raw(reply.to_string().as_bytes()).await;
    }

    async fn reply_unavailable(&mut self, id: Value, message: &str) {
        self.reply_error(id, code::DAEMON_UNAVAILABLE, message)
            .await;
    }

    async fn reply_error(&mut self, id: Value, code: i64, message: &str) {
        let reply = json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        });
        self.send_raw(reply.to_string().as_bytes()).await;
    }

    async fn send_raw(&mut self, payload: &[u8]) {
        let frame = match framing::encode_frame(payload) {
            Ok(frame) => frame,
            Err(e) => {
                log::warn(&format!("not sending a message to the browser: {e}"));
                return;
            }
        };
        if self.out.send(frame).await.is_err() {
            self.output_closed = true;
        }
    }
}

fn id_key(id: &Value) -> String {
    id.to_string()
}

/// Reads the runtime file, connects, and completes `session.hello`.
async fn connect(cfg: &Config) -> Result<Box<Daemon>, ConnectError> {
    let info = read_runtime(&cfg.runtime_file)?;
    let url = format!("ws://127.0.0.1:{}{RPC_PATH}", info.port);
    let (mut ws, _response) = tokio_tungstenite::connect_async(url).await?;

    let nonce = whoosh_proto::auth::new_nonce().map_err(|e| ConnectError::Task(e.to_string()))?;
    let token = info.token;
    let hello = json!({
        "jsonrpc": "2.0",
        "id": HELLO_ID,
        "method": method::SESSION_HELLO,
        "params": HelloParams {
            token: token.clone(),
            nonce: nonce.clone(),
            protocol: PROTOCOL_VERSION,
            client: cfg.client.clone(),
        },
    });
    ws.send(Message::text(hello.to_string())).await?;

    while let Some(message) = ws.next().await {
        let text = match message? {
            Message::Text(text) => text,
            Message::Close(frame) => {
                return Err(ConnectError::Closed(frame.map(|f| f.code.into())));
            }
            _ => continue,
        };
        let reply: Value =
            serde_json::from_str(&text).map_err(|e| ConnectError::BadHello(e.to_string()))?;
        if reply.get("id").and_then(Value::as_str) != Some(HELLO_ID) {
            // The daemon should not talk before the handshake; ignore it.
            log::debug("ignoring a message received before the session.hello reply");
            continue;
        }
        if let Some(error) = reply.get("error") {
            return Err(ConnectError::Rejected(error.to_string()));
        }
        let result = reply.get("result").cloned().unwrap_or(Value::Null);
        let hello: HelloResult =
            serde_json::from_value(result).map_err(|e| ConnectError::BadHello(e.to_string()))?;
        if !whoosh_proto::auth::verify_hello_proof(&token, &nonce, &hello.proof) {
            // Close without relaying anything: no request of the extension's
            // (cookies, headers) may reach an impostor.
            let _ = ws.close(None).await;
            return Err(ConnectError::Impostor);
        }
        return Ok(Box::new(Daemon {
            ws,
            version: hello.daemon_version,
        }));
    }
    Err(ConnectError::Closed(None))
}
