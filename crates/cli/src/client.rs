//! The CLI's JSON-RPC client: one WebSocket, `session.hello`, then calls
//! (answered in any order) and a stream of notifications.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use vayu_proto::{ClientInfo, HelloParams, HelloResult, PROTOCOL_VERSION, RPC_PATH, auth, method};

use crate::error::CliError;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Reply = Result<Value, CliError>;

/// A daemon → client notification.
#[derive(Debug, Clone)]
pub struct Event {
    pub method: String,
    pub params: Value,
}

/// Why connecting failed; the caller decides whether to start a daemon.
#[derive(Debug)]
pub enum ConnectError {
    /// Nothing answers on that port (or not in time): no daemon, or a stale
    /// runtime file.
    Unreachable,
    /// Something answered without knowing the token: the runtime file is
    /// stale and another process holds its port. Nothing but the hello was
    /// sent to it.
    BadProof,
    /// A local failure (no random numbers).
    Local(String),
    /// A daemon answered and refused us (4401 / 4426).
    Rejected { code: Option<u16>, reason: String },
}

#[derive(Debug)]
pub struct Client {
    sink: tokio::sync::Mutex<SplitSink<Ws, Message>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>,
    closed: Arc<AtomicBool>,
    events: tokio::sync::Mutex<mpsc::UnboundedReceiver<Event>>,
    next_id: AtomicU64,
    reader: tokio::task::JoinHandle<()>,
    pub hello: HelloResult,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl Client {
    pub async fn connect(port: u16, token: &str, timeout: Duration) -> Result<Self, ConnectError> {
        tokio::time::timeout(timeout, Self::connect_inner(port, token))
            .await
            .unwrap_or(Err(ConnectError::Unreachable))
    }

    async fn connect_inner(port: u16, token: &str) -> Result<Self, ConnectError> {
        let url = format!("ws://127.0.0.1:{port}{RPC_PATH}");
        // No Nagle: every message is small and someone waits for it.
        let (mut ws, _) = tokio_tungstenite::connect_async_with_config(url, None, true)
            .await
            .map_err(|_| ConnectError::Unreachable)?;
        let nonce = auth::new_nonce().map_err(|e| ConnectError::Local(e.to_string()))?;
        let hello = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": method::SESSION_HELLO,
            "params": HelloParams {
                token: token.to_owned(),
                nonce: nonce.clone(),
                protocol: PROTOCOL_VERSION,
                client: ClientInfo { name: "cli".into(), version: env!("CARGO_PKG_VERSION").into() },
            },
        });
        ws.send(Message::text(hello.to_string()))
            .await
            .map_err(|_| ConnectError::Unreachable)?;
        let hello = loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    let reply: Value = serde_json::from_str(text.as_str())
                        .map_err(|_| ConnectError::Unreachable)?;
                    if reply.get("id") != Some(&json!(0)) {
                        continue;
                    }
                    if let Some(error) = reply.get("error") {
                        return Err(ConnectError::Rejected {
                            code: None,
                            reason: error["message"].as_str().unwrap_or_default().to_owned(),
                        });
                    }
                    let hello = serde_json::from_value::<HelloResult>(reply["result"].clone())
                        .map_err(|_| ConnectError::Unreachable)?;
                    // PROTOCOL.md §1: check the daemon's proof before sending
                    // anything else (requests carry cookies and headers).
                    if !auth::verify_hello_proof(token, &nonce, &hello.proof) {
                        let _ =
                            tokio::time::timeout(Duration::from_millis(200), ws.close(None)).await;
                        return Err(ConnectError::BadProof);
                    }
                    break hello;
                }
                Some(Ok(Message::Close(frame))) => {
                    return Err(ConnectError::Rejected {
                        code: frame.as_ref().map(|f| f.code.into()),
                        reason: frame.map(|f| f.reason.to_string()).unwrap_or_default(),
                    });
                }
                Some(Ok(_)) => continue,
                Some(Err(_)) | None => return Err(ConnectError::Unreachable),
            }
        };

        let (sink, stream) = ws.split();
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>> = Arc::default();
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let closed = Arc::new(AtomicBool::new(false));
        let reader = tokio::spawn(read_loop(
            stream,
            pending.clone(),
            closed.clone(),
            events_tx,
        ));
        Ok(Self {
            sink: tokio::sync::Mutex::new(sink),
            pending,
            closed,
            events: tokio::sync::Mutex::new(events_rx),
            next_id: AtomicU64::new(1),
            reader,
            hello,
        })
    }

    /// Calls `method` and decodes its result.
    pub async fn call<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
    ) -> Result<R, CliError> {
        let value = self
            .call_value(method, serde_json::to_value(params).unwrap_or(Value::Null))
            .await?;
        serde_json::from_value(value)
            .map_err(|e| CliError::Other(format!("unexpected answer to {method}: {e}")))
    }

    pub async fn call_value(&self, method: &str, params: Value) -> Result<Value, CliError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id, tx);
        // The reader marks the connection closed before failing what is
        // pending, so either it fails this call or we see the mark.
        if self.closed.load(Ordering::SeqCst) {
            lock(&self.pending).remove(&id);
            return Err(lost("connection closed"));
        }
        let mut request = json!({ "jsonrpc": "2.0", "id": id, "method": method });
        if !params.is_null() {
            request["params"] = params;
        }
        let sent = self
            .sink
            .lock()
            .await
            .send(Message::text(request.to_string()))
            .await;
        if let Err(e) = sent {
            lock(&self.pending).remove(&id);
            return Err(lost(&e.to_string()));
        }
        rx.await.unwrap_or_else(|_| Err(lost("connection closed")))
    }

    /// The next notification; `None` once the connection is gone.
    pub async fn next_event(&self) -> Option<Event> {
        self.events.lock().await.recv().await
    }

    pub async fn close(self) {
        let _ = tokio::time::timeout(Duration::from_millis(500), async {
            let _ = self.sink.lock().await.close().await;
        })
        .await;
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn lost(why: &str) -> CliError {
    CliError::DaemonUnavailable(format!("lost the connection to the daemon ({why})"))
}

async fn read_loop(
    mut stream: futures::stream::SplitStream<Ws>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>,
    closed: Arc<AtomicBool>,
    events: mpsc::UnboundedSender<Event>,
) {
    let mut why = "connection closed".to_owned();
    while let Some(msg) = stream.next().await {
        let text = match msg {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(frame)) => {
                if let Some(f) = frame {
                    why = format!("closed by the daemon: {} {}", u16::from(f.code), f.reason);
                }
                break;
            }
            Ok(_) => continue,
            Err(e) => {
                why = e.to_string();
                break;
            }
        };
        let Ok(msg) = serde_json::from_str::<Value>(text.as_str()) else {
            continue;
        };
        if let Some(method) = msg.get("method").and_then(Value::as_str) {
            let _ = events.send(Event {
                method: method.to_owned(),
                params: msg.get("params").cloned().unwrap_or(Value::Null),
            });
            continue;
        }
        let Some(id) = msg.get("id").and_then(Value::as_u64) else {
            continue;
        };
        let Some(tx) = lock(&pending).remove(&id) else {
            continue;
        };
        let reply = match msg.get("error") {
            Some(error) => Err(CliError::Rpc {
                code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_owned(),
            }),
            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = tx.send(reply);
    }
    closed.store(true, Ordering::SeqCst);
    for (_, tx) in lock(&pending).drain() {
        let _ = tx.send(Err(lost(&why)));
    }
}
