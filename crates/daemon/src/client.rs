//! A small, sequential client for the local API. The daemon uses it for the
//! single-instance check (does the daemon in `runtime.json` answer hello,
//! with a valid proof?), and tests use it to drive a daemon. Real clients
//! (CLI, desktop, nmhost) have their own.

use std::collections::VecDeque;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use vayu_proto::{ClientInfo, HelloParams, HelloResult, PROTOCOL_VERSION, RPC_PATH, auth, method};

use crate::token::Token;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("cannot connect: {0}")]
    Connect(#[from] tungstenite::Error),
    #[error("the daemon closed the connection (code {code:?}: {reason})")]
    Closed { code: Option<u16>, reason: String },
    #[error("unexpected reply: {0}")]
    BadReply(String),
    #[error("timed out")]
    Timeout,
    /// Whatever answered on that port does not know the token: not the
    /// daemon of this runtime file.
    #[error("the hello proof is wrong (not the daemon this runtime file describes)")]
    BadProof,
    #[error("JSON-RPC error {code}: {message}")]
    Rpc { code: i64, message: String },
}

impl ClientError {
    /// The WebSocket close code, if the daemon closed the connection.
    pub fn close_code(&self) -> Option<u16> {
        match self {
            Self::Closed { code, .. } => *code,
            _ => None,
        }
    }
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A connected, authenticated session.
#[derive(Debug)]
pub struct Client {
    ws: Ws,
    next_id: u64,
    events: VecDeque<(String, Value)>,
    hello: HelloResult,
}

/// Connects to `127.0.0.1:port` and completes `session.hello` within
/// `timeout`; returns the daemon's hello result.
pub async fn probe(
    port: u16,
    token: &Token,
    timeout: Duration,
) -> Result<HelloResult, ClientError> {
    let client = tokio::time::timeout(timeout, Client::connect(port, token.expose(), "probe"))
        .await
        .map_err(|_| ClientError::Timeout)??;
    let hello = client.hello.clone();
    client.close().await;
    Ok(hello)
}

impl Client {
    pub async fn connect(port: u16, token: &str, name: &str) -> Result<Self, ClientError> {
        let url = format!("ws://127.0.0.1:{port}{RPC_PATH}");
        let (ws, _) = tokio_tungstenite::connect_async_with_config(url, None, true).await?;
        let mut client = Self {
            ws,
            next_id: 1,
            events: VecDeque::new(),
            hello: HelloResult {
                daemon_version: String::new(),
                protocol: 0,
                capabilities: Vec::new(),
                proof: String::new(),
            },
        };
        let nonce = auth::new_nonce().map_err(|e| ClientError::BadReply(e.to_string()))?;
        let params = HelloParams {
            token: token.to_owned(),
            nonce: nonce.clone(),
            protocol: PROTOCOL_VERSION,
            client: ClientInfo {
                name: name.to_owned(),
                version: crate::VERSION.to_owned(),
            },
        };
        let result = client
            .call(
                method::SESSION_HELLO,
                serde_json::to_value(params).unwrap_or_default(),
            )
            .await?;
        let hello: HelloResult =
            serde_json::from_value(result).map_err(|e| ClientError::BadReply(e.to_string()))?;
        if !auth::verify_hello_proof(token, &nonce, &hello.proof) {
            return Err(ClientError::BadProof);
        }
        client.hello = hello;
        Ok(client)
    }

    pub fn hello(&self) -> &HelloResult {
        &self.hello
    }

    /// Sends a request and waits for its response. Notifications that arrive
    /// meanwhile are kept for [`Client::next_event`].
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.ws.send(Message::text(request.to_string())).await?;
        loop {
            let reply = self.read_json().await?;
            if reply.get("method").is_some() {
                self.queue_event(reply);
                continue;
            }
            if reply.get("id").and_then(Value::as_u64) != Some(id) {
                return Err(ClientError::BadReply(reply.to_string()));
            }
            if let Some(error) = reply.get("error") {
                return Err(ClientError::Rpc {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                });
            }
            return Ok(reply.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// The next notification as `(method, params)`.
    pub async fn next_event(&mut self) -> Result<(String, Value), ClientError> {
        if let Some(event) = self.events.pop_front() {
            return Ok(event);
        }
        loop {
            let msg = self.read_json().await?;
            if msg.get("method").is_some() {
                self.queue_event(msg);
                if let Some(event) = self.events.pop_front() {
                    return Ok(event);
                }
            }
        }
    }

    /// Waits up to `timeout` for a notification `method` whose params satisfy
    /// `pred`, skipping others.
    pub async fn wait_event(
        &mut self,
        method: &str,
        timeout: Duration,
        mut pred: impl FnMut(&Value) -> bool,
    ) -> Result<Value, ClientError> {
        tokio::time::timeout(timeout, async {
            loop {
                let (m, params) = self.next_event().await?;
                if m == method && pred(&params) {
                    return Ok(params);
                }
            }
        })
        .await
        .map_err(|_| ClientError::Timeout)?
    }

    pub async fn close(mut self) {
        let _ = self.ws.close(None).await;
        let _ = tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(Ok(_)) = self.ws.next().await {}
        })
        .await;
    }

    fn queue_event(&mut self, msg: Value) {
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        self.events.push_back((method, params));
    }

    async fn read_json(&mut self) -> Result<Value, ClientError> {
        loop {
            match self.ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    return serde_json::from_str(text.as_str())
                        .map_err(|e| ClientError::BadReply(e.to_string()));
                }
                Some(Ok(Message::Close(frame))) => {
                    return Err(ClientError::Closed {
                        code: frame.as_ref().map(|f| f.code.into()),
                        reason: frame.map(|f| f.reason.to_string()).unwrap_or_default(),
                    });
                }
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e.into()),
                None => {
                    return Err(ClientError::Closed {
                        code: None,
                        reason: "connection ended".into(),
                    });
                }
            }
        }
    }
}
