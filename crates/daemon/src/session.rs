//! One WebSocket connection: `session.hello`, then JSON-RPC requests in and
//! responses and notifications out.

use std::sync::Arc;
use std::time::Duration;

use futures::stream::{BoxStream, SplitSink};
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use whoosh_proto::service::Notification;
use whoosh_proto::{HelloParams, HelloResult, PROTOCOL_VERSION, auth, method};

use crate::limit::{Budget, is_over_budget};
use crate::outbox::{Item, Outbox, Pushed};
use crate::rpc::{self, Incoming};
use crate::server::Shared;

/// Close codes (PROTOCOL.md §1 and this crate's README).
pub mod close {
    /// Normal closure (the client closed first).
    pub const NORMAL: u16 = 1000;
    /// The daemon is shutting down, or the client stopped answering pings.
    pub const GOING_AWAY: u16 = 1001;
    /// A message over the size limit (64 KiB before hello, 16 MiB after).
    pub const TOO_BIG: u16 = 1009;
    /// Something other than `session.hello` first, a wrong token, or a
    /// missing or malformed nonce.
    pub const UNAUTHORIZED: u16 = whoosh_proto::close_code::UNAUTHORIZED;
    /// No `session.hello` within the hello timeout.
    pub const HELLO_TIMEOUT: u16 = whoosh_proto::close_code::HELLO_TIMEOUT;
    /// `session.hello` with a protocol version this daemon does not speak.
    pub const PROTOCOL: u16 = whoosh_proto::close_code::PROTOCOL_MISMATCH;
    /// The client fell too far behind on lossless notifications.
    pub const TOO_SLOW: u16 = whoosh_proto::close_code::TOO_SLOW;
}

enum HandshakeEnd {
    Close(u16, String),
    /// The peer went away, or the socket failed.
    Gone,
}

/// Runs one connection. `budget` limits what an unauthenticated peer can
/// make us read; `slot` is its place among the connections waiting for
/// their hello. Both are released once the hello succeeds.
pub(crate) async fn run<S>(
    mut ws: WebSocketStream<S>,
    shared: Arc<Shared>,
    budget: Budget,
    slot: OwnedSemaphorePermit,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let _alive = shared.tracker.enter();
    let (hello_id, hello) = match handshake(&mut ws, &shared).await {
        Ok(ok) => ok,
        Err(HandshakeEnd::Close(code, reason)) => {
            tracing::info!(code, %reason, "closing an unauthenticated connection");
            close_unsplit(ws, code, &reason).await;
            return;
        }
        Err(HandshakeEnd::Gone) => return,
    };
    budget.lift();
    drop(slot);

    // Subscribe before answering hello, so no event falls in between; the
    // events are only sent after the hello result.
    let events = shared.service.subscribe();
    let result = HelloResult {
        daemon_version: shared.config.version.clone(),
        protocol: PROTOCOL_VERSION,
        capabilities: shared.service.capabilities(),
        proof: auth::hello_proof(shared.token.expose(), &hello.nonce),
    };
    let reply = rpc::result_response(
        hello_id,
        serde_json::to_value(result).unwrap_or(Value::Null),
    );
    if ws.send(Message::text(reply.to_string())).await.is_err() {
        return;
    }
    tracing::info!(client = ?hello.client.name, version = ?hello.client.version, "session started");

    let cfg = &shared.config;
    let (sink, mut stream) = ws.split();
    let outbox = Arc::new(Outbox::new(cfg.max_pending_events));
    let mut writer = tokio::spawn(write_loop(sink, outbox.clone(), cfg.ping_interval));
    let pump = tokio::spawn(pump(events, outbox.clone()));

    let inflight = Arc::new(Semaphore::new(cfg.max_inflight_requests.max(1)));
    let mut tasks: JoinSet<()> = JoinSet::new();
    let mut shutdown = shared.shutdown.clone();
    let dead_after = cfg.ping_interval + cfg.pong_timeout;
    let mut keepalive = tokio::time::interval(cfg.ping_interval.min(cfg.pong_timeout) / 2);
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_seen = Instant::now();
    let mut permit: Option<OwnedSemaphorePermit> = None;
    let mut writer_done = false;

    loop {
        tokio::select! {
            biased;
            _ = shutdown.changed() => {
                // After what is queued: the answer to `daemon.shutdown`, the
                // events of the service's own shutdown.
                outbox.close_after_pending(close::GOING_AWAY, "daemon shutting down");
                break;
            }
            _ = &mut writer => {
                // The close frame went out (overflow, or a closing peer), or
                // the socket failed.
                writer_done = true;
                break;
            }
            _ = keepalive.tick() => {
                // Only judge a client whose messages we are reading; with
                // every request slot busy we are not reading its pongs.
                if permit.is_some() && last_seen.elapsed() > dead_after {
                    tracing::info!("client stopped answering pings; closing");
                    outbox.close(close::GOING_AWAY, "ping timeout");
                    break;
                }
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
            p = inflight.clone().acquire_owned(), if permit.is_none() => {
                permit = p.ok();
                last_seen = Instant::now();
            }
            msg = stream.next(), if permit.is_some() => {
                last_seen = Instant::now();
                let msg = match msg {
                    Some(Ok(msg)) => msg,
                    Some(Err(e)) => {
                        handle_read_error(&outbox, &e);
                        break;
                    }
                    None => break,
                };
                match msg {
                    Message::Text(text) => {
                        let p = permit.take();
                        on_text(text.as_str(), p, &shared, &outbox, &mut tasks);
                    }
                    Message::Binary(_) => {
                        let reply = rpc::error_response(
                            Value::Null,
                            rpc::INVALID_REQUEST,
                            "binary frames are not supported; send JSON text frames",
                        );
                        outbox.push_response(reply.to_string(), permit.take());
                    }
                    Message::Close(_) => {
                        outbox.close(close::NORMAL, "");
                        break;
                    }
                    Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
                }
            }
        }
    }

    pump.abort();
    tasks.abort_all();
    if !writer_done {
        // Give the writer a moment to deliver the close frame; a client that
        // is not reading at all does not get to hold the task forever.
        if !outbox.is_closing() {
            outbox.close(close::GOING_AWAY, "");
        }
        if tokio::time::timeout(Duration::from_secs(2), &mut writer)
            .await
            .is_err()
        {
            writer.abort();
        }
    }
    tracing::info!(
        client = ?hello.client.name,
        coalesced = outbox.coalesced(),
        "session ended"
    );
}

/// Waits for a valid `session.hello` (PROTOCOL.md §1).
async fn handshake<S>(
    ws: &mut WebSocketStream<S>,
    shared: &Shared,
) -> Result<(Value, HelloParams), HandshakeEnd>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let deadline = Instant::now() + shared.config.hello_timeout;
    let mut shutdown = shared.shutdown.clone();
    let first = || {
        HandshakeEnd::Close(
            close::UNAUTHORIZED,
            "the first message must be session.hello".into(),
        )
    };
    loop {
        let msg = tokio::select! {
            m = tokio::time::timeout_at(deadline, ws.next()) => m,
            _ = shutdown.changed() => {
                return Err(HandshakeEnd::Close(close::GOING_AWAY, "daemon shutting down".into()));
            }
        };
        let msg = match msg {
            Err(_elapsed) => {
                return Err(HandshakeEnd::Close(
                    close::HELLO_TIMEOUT,
                    "session.hello was not received in time".into(),
                ));
            }
            Ok(None) => return Err(HandshakeEnd::Gone),
            Ok(Some(Err(e))) => {
                let over = too_big(&e)
                    || matches!(&e, tokio_tungstenite::tungstenite::Error::Io(io) if is_over_budget(io));
                return Err(match over {
                    true => HandshakeEnd::Close(
                        close::TOO_BIG,
                        "too much data before session.hello".into(),
                    ),
                    false => HandshakeEnd::Gone,
                });
            }
            Ok(Some(Ok(msg))) => msg,
        };
        let text = match msg {
            Message::Text(text) => text,
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
            Message::Close(_) => return Err(HandshakeEnd::Gone),
            Message::Binary(_) => return Err(first()),
        };
        // Never parse a large unauthenticated message (PROTOCOL.md §1).
        if text.len() > shared.config.max_hello_bytes {
            return Err(HandshakeEnd::Close(
                close::TOO_BIG,
                "session.hello is too large".into(),
            ));
        }
        let req = match rpc::parse(text.as_str()) {
            Incoming::Request(req) if req.method == method::SESSION_HELLO => req,
            _ => return Err(first()),
        };
        let params = req.params.unwrap_or(Value::Null);
        let token_ok = params
            .get("token")
            .and_then(Value::as_str)
            .is_some_and(|t| shared.token.verify(t));
        if !token_ok {
            return Err(HandshakeEnd::Close(
                close::UNAUTHORIZED,
                "invalid or missing token".into(),
            ));
        }
        let nonce_ok = params
            .get("nonce")
            .and_then(Value::as_str)
            .is_some_and(auth::is_valid_nonce);
        if !nonce_ok {
            return Err(HandshakeEnd::Close(
                close::UNAUTHORIZED,
                "missing or malformed nonce (32-128 lower-case hex characters)".into(),
            ));
        }
        if params.get("protocol").and_then(Value::as_u64) != Some(u64::from(PROTOCOL_VERSION)) {
            return Err(HandshakeEnd::Close(
                close::PROTOCOL,
                format!("unsupported protocol; this daemon speaks protocol {PROTOCOL_VERSION}"),
            ));
        }
        let Some(id) = req.id else {
            return Err(HandshakeEnd::Close(
                close::UNAUTHORIZED,
                "session.hello must be a request with an id".into(),
            ));
        };
        match serde_json::from_value::<HelloParams>(params) {
            Ok(hello) => return Ok((id, hello)),
            Err(e) => {
                // Right token and protocol, malformed otherwise (e.g. no
                // `client`): say what is wrong and wait for a better hello.
                let reply = rpc::error_response(id, rpc::INVALID_PARAMS, &e.to_string());
                if ws.send(Message::text(reply.to_string())).await.is_err() {
                    return Err(HandshakeEnd::Gone);
                }
            }
        }
    }
}

fn on_text(
    text: &str,
    permit: Option<OwnedSemaphorePermit>,
    shared: &Arc<Shared>,
    outbox: &Arc<Outbox>,
    tasks: &mut JoinSet<()>,
) {
    let req = match rpc::parse(text) {
        Incoming::Request(req) => req,
        Incoming::Invalid(reply) => {
            outbox.push_response(reply.to_string(), permit);
            return;
        }
        Incoming::Ignore => return,
    };
    if req.method == method::DAEMON_SHUTDOWN {
        let no_params = match &req.params {
            None => true,
            Some(Value::Object(m)) => m.is_empty(),
            Some(Value::Array(a)) => a.is_empty(),
            Some(_) => false,
        };
        if !no_params {
            if let Some(id) = req.id {
                let reply =
                    rpc::error_response(id, rpc::INVALID_PARAMS, "this method takes no params");
                outbox.push_response(reply.to_string(), permit);
            }
            return;
        }
        tracing::info!("daemon.shutdown requested by a client");
        // Answer first: the session closes with 1001 only after this is sent.
        if let Some(id) = req.id {
            let reply = rpc::result_response(id, Value::Null);
            outbox.push_response(reply.to_string(), permit);
        }
        shared.request_stop();
        return;
    }
    if req.method == method::SESSION_HELLO {
        if let Some(id) = req.id {
            let reply = rpc::error_response(
                id,
                rpc::INVALID_REQUEST,
                "session already established; session.hello is only valid as the first message",
            );
            outbox.push_response(reply.to_string(), permit);
        }
        return;
    }
    // Method names only: params can carry cookies and must never be logged.
    tracing::debug!(method = ?req.method, notification = req.id.is_none(), "request");
    let service = shared.service.clone();
    let outbox = outbox.clone();
    tasks.spawn(async move {
        let id = req.id.clone();
        let method = req.method.clone();
        let result = rpc::dispatch(&service, req).await;
        let Some(id) = id else {
            if let Err(e) = result {
                tracing::debug!(method = ?method, code = e.code, "notification failed");
            }
            return;
        };
        let reply = match result {
            Ok(value) => rpc::result_response(id, value),
            Err(e) => rpc::error_response(id, e.code, &e.message),
        };
        outbox.push_response(reply.to_string(), permit);
    });
}

async fn pump(mut events: BoxStream<'static, Notification>, outbox: Arc<Outbox>) {
    while let Some(n) = events.next().await {
        match outbox.push_notification(n) {
            Pushed::Overflow => {
                tracing::warn!(
                    "client too slow: lossless event backlog exceeded; closing with {}",
                    close::TOO_SLOW
                );
                return;
            }
            Pushed::Closed => return,
            Pushed::Queued | Pushed::Coalesced => {}
        }
    }
}

async fn write_loop<S>(
    mut sink: SplitSink<WebSocketStream<S>, Message>,
    outbox: Arc<Outbox>,
    ping_interval: Duration,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut ping = tokio::time::interval_at(Instant::now() + ping_interval, ping_interval);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let item = tokio::select! {
            item = outbox.pop() => item,
            _ = ping.tick() => {
                if sink.send(Message::Ping(bytes::Bytes::new())).await.is_err() {
                    return;
                }
                continue;
            }
        };
        let text = match item {
            Item::Response(text, permit) => {
                let sent = sink.send(Message::text(text)).await;
                drop(permit);
                if sent.is_err() {
                    return;
                }
                continue;
            }
            Item::Event(n) => notification_text(&n),
            Item::Progress(slot) => {
                notification_text(&Notification::DownloadProgress(slot.into_event()))
            }
            Item::Stats(stats) => notification_text(&Notification::Stats(stats)),
            Item::Close(code, reason) => {
                let frame = CloseFrame {
                    code: code.into(),
                    reason: reason.into(),
                };
                let _ = sink.send(Message::Close(Some(frame))).await;
                let _ = tokio::time::timeout(Duration::from_secs(2), sink.close()).await;
                return;
            }
        };
        if sink.send(Message::text(text)).await.is_err() {
            return;
        }
    }
}

pub(crate) fn notification_text(n: &Notification) -> String {
    serde_json::json!({ "jsonrpc": "2.0", "method": n.method(), "params": n.params() }).to_string()
}

fn too_big(e: &tokio_tungstenite::tungstenite::Error) -> bool {
    matches!(e, tokio_tungstenite::tungstenite::Error::Capacity(_))
}

fn handle_read_error(outbox: &Outbox, e: &tokio_tungstenite::tungstenite::Error) {
    if too_big(e) {
        tracing::info!("message over the size limit; closing with 1009");
        outbox.close(close::TOO_BIG, "message too big");
    } else {
        tracing::debug!(error = %e, "read error");
        outbox.close(close::GOING_AWAY, "");
    }
}

/// Sends a close frame before the session is split, then waits briefly for
/// the peer's reply so the frame is not lost to a TCP reset.
async fn close_unsplit<S>(mut ws: WebSocketStream<S>, code: u16, reason: &str)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let frame = CloseFrame {
        code: code.into(),
        reason: reason.to_owned().into(),
    };
    if ws.close(Some(frame)).await.is_err() {
        return;
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(Ok(_)) = ws.next().await {}
    })
    .await;
}
