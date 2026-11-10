//! The HTTP/3 listener: QUIC (quinn) on UDP, HTTP/3 framing (h3), and the
//! same handler and body streaming as the TCP listeners.
//!
//! Each QUIC connection gets one [`ConnCtl`], so per-connection bandwidth
//! buckets and the `open_connections` stats count QUIC connections, and
//! all request streams on a connection share them (like HTTP/2 streams on a
//! TCP connection).
//!
//! QUIC has no TCP RST. An injected reset (and `refuse = "reset"`) closes
//! the whole QUIC connection with an application CONNECTION_CLOSE:
//! [`RESET_CODE`] (`H3_INTERNAL_ERROR`) and the reason [`RESET_REASON`].
//! The streams still in flight are reset (RESET_STREAM, same code) just
//! before, which discards data quinn has not sent yet, as a RST discards
//! unsent TCP data (and works around a quinn quirk, see
//! [`close_after_reset`]).

use bytes::Bytes;
use h3::error::Code;
use h3::server::RequestResolver;
use http::{HeaderValue, Response, header};
use http_body_util::BodyExt;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::conn::ConnCtl;
use crate::handler::{self, ReqCtx};
use crate::state::Shared;

/// Application error code of an injected reset: `H3_INTERNAL_ERROR`.
pub const RESET_CODE: u32 = 0x0102;
/// Reason phrase sent with [`RESET_CODE`].
pub const RESET_REASON: &[u8] = b"lab: injected reset";
/// `H3_NO_ERROR`, for closing connections at shutdown.
const NO_ERROR: u32 = 0x0100;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for the end of a request after its response is sent.
const REQUEST_END_TIMEOUT: Duration = Duration::from_secs(1);
/// Longest wait for the RESET_STREAM frames before an injected close.
const RESET_FLUSH_TIMEOUT: Duration = Duration::from_secs(1);
/// UDP socket buffers. Linux caps them at `net.core.{r,w}mem_max`.
const SOCKET_BUFFER: usize = 4 << 20;

/// Binds a QUIC endpoint serving HTTP/3 on `addr` (UDP).
pub(crate) fn endpoint(
    addr: SocketAddr,
    crypto: Arc<quinn::crypto::rustls::QuicServerConfig>,
) -> io::Result<quinn::Endpoint> {
    let mut transport = quinn::TransportConfig::default();
    transport
        .max_concurrent_bidi_streams(1024u32.into())
        // Bytes in flight per connection, across its streams.
        .send_window(32 << 20)
        // Keeps a connection alive through long stalls (no data for
        // longer than the idle timeout), as a quiet TCP connection stays up.
        .keep_alive_interval(Some(Duration::from_secs(5)));
    let mut cfg = quinn::ServerConfig::with_crypto(crypto);
    cfg.transport_config(Arc::new(transport));

    let socket = std::net::UdpSocket::bind(addr)?;
    let sock = socket2::SockRef::from(&socket);
    let _ = sock.set_send_buffer_size(SOCKET_BUFFER);
    let _ = sock.set_recv_buffer_size(SOCKET_BUFFER);
    quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(cfg),
        socket,
        Arc::new(quinn::TokioRuntime),
    )
}

pub(crate) async fn accept_loop(
    endpoint: quinn::Endpoint,
    shared: Arc<Shared>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        let incoming = tokio::select! {
            i = endpoint.accept() => match i {
                Some(i) => i,
                None => break, // endpoint closed
            },
            _ = shutdown.changed() => break,
        };
        tokio::spawn(serve_conn(incoming, shared.clone(), shutdown.clone()));
    }
    endpoint.close(NO_ERROR.into(), b"server shutting down");
}

/// Why a connection's serving loop ended.
enum End {
    Reset,
    Shutdown,
    Closed,
}

async fn serve_conn(
    incoming: quinn::Incoming,
    shared: Arc<Shared>,
    mut shutdown: watch::Receiver<bool>,
) {
    let peer = incoming.remote_address();
    let conn = tokio::select! {
        r = tokio::time::timeout(HANDSHAKE_TIMEOUT, incoming) => match r {
            Ok(Ok(c)) => c,
            _ => return,
        },
        _ = shutdown.changed() => return,
    };
    let builder = h3::server::builder();
    let mut h3 = tokio::select! {
        r = builder.build::<_, Bytes>(h3_quinn::Connection::new(conn.clone())) => match r {
            Ok(h) => h,
            Err(e) => {
                tracing::debug!("h3 setup: {e}");
                return;
            }
        },
        _ = shutdown.changed() => return,
    };
    let ctl = Arc::new(ConnCtl::new(peer.ip().to_canonical()));
    let ctx = ReqCtx {
        shared: shared.clone(),
        conn: ctl.clone(),
        tls: true,
        alt_svc: None,
    };

    // Tells request tasks to reset their streams (see `close_after_reset`).
    let (reset_tx, reset_rx) = watch::channel(false);
    let mut requests = JoinSet::new();
    let end = loop {
        tokio::select! {
            biased;
            _ = ctl.killed() => break End::Reset,
            _ = shutdown.changed() => break End::Shutdown,
            Some(_) = requests.join_next(), if !requests.is_empty() => {}
            r = h3.accept() => match r {
                Ok(Some(resolver)) => {
                    requests.spawn(serve_request(resolver, ctx.clone(), reset_rx.clone()));
                }
                // GOAWAY from the client and every request done.
                Ok(None) => break End::Closed,
                Err(e) => {
                    if !e.is_h3_no_error() {
                        tracing::debug!("h3 connection: {e}");
                    }
                    break End::Closed;
                }
            },
        }
    };
    match end {
        End::Reset => close_after_reset(&conn, &reset_tx, &mut requests).await,
        End::Shutdown => conn.close(NO_ERROR.into(), b"server shutting down"),
        End::Closed => {}
    }
    // Stops responses still streaming (their bodies hold concurrency slots).
    requests.shutdown().await;
    drop(h3);
    for idx in ctl.touched() {
        shared.scenarios[idx].stats.conn_closed(ctl.peer);
    }
}

/// Closes the connection with [`RESET_CODE`] after resetting its streams.
///
/// quinn-proto 0.11 holds a CONNECTION_CLOSE back while stream data is
/// queued behind a full congestion window, and ignores ACKs once it is
/// closing, so such a close would never be sent: the client would only
/// notice at its idle timeout. A RESET_STREAM discards the stream's queued
/// data, so the request tasks reset their streams first, and the close
/// goes out once those frames have been sent.
async fn close_after_reset(
    conn: &quinn::Connection,
    reset: &watch::Sender<bool>,
    requests: &mut JoinSet<bool>,
) {
    let before = conn.stats().frame_tx.reset_stream;
    reset.send_replace(true);
    let mut resets = 0;
    let _ = tokio::time::timeout(RESET_FLUSH_TIMEOUT, async {
        while let Some(r) = requests.join_next().await {
            resets += u64::from(r.unwrap_or(false));
        }
        while conn.stats().frame_tx.reset_stream < before + resets {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    conn.close(RESET_CODE.into(), RESET_REASON);
}

/// Header fields HTTP/3 forbids in responses (RFC 9114 §4.2).
const CONNECTION_SPECIFIC: [&str; 5] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "upgrade",
];

type Stream = h3::server::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>;

/// Serves one request stream. Returns true if it reset an unfinished
/// response because the connection is being reset (such a reset always
/// sends a RESET_STREAM frame).
async fn serve_request(
    resolver: RequestResolver<h3_quinn::Connection, Bytes>,
    ctx: ReqCtx,
    mut reset: watch::Receiver<bool>,
) -> bool {
    let resolved = tokio::select! {
        biased;
        // No response started: nothing queued to discard.
        Ok(_) = reset.wait_for(|r| *r) => return false,
        r = resolver.resolve_request() => r,
    };
    let (req, mut stream) = match resolved {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("h3 request: {e}");
            return false;
        }
    };
    tokio::select! {
        biased;
        Ok(_) = reset.wait_for(|r| *r) => {
            stream.stop_stream(Code::H3_INTERNAL_ERROR);
            return true;
        }
        finished = respond(req, &mut stream, ctx) => if !finished {
            return false;
        }
    }
    // Read the request to its end (its FIN can trail the response), so
    // dropping the stream doesn't make quinn send STOP_SENDING to a
    // client that is about to finish its request. A client that never
    // finishes gets STOP_SENDING with H3_NO_ERROR (RFC 9114 §4.1.2).
    tokio::select! {
        biased;
        Ok(_) = reset.wait_for(|r| *r) => {
            // Discards response data quinn may still hold. Not counted:
            // a fully acknowledged stream sends no RESET_STREAM.
            stream.stop_stream(Code::H3_INTERNAL_ERROR);
        }
        drained = tokio::time::timeout(REQUEST_END_TIMEOUT, async {
            while let Ok(Some(_)) = stream.recv_data().await {}
        }) => if drained.is_err() {
            stream.stop_sending(Code::H3_NO_ERROR);
        }
    }
    false
}

/// Sends the handler's response. True once the stream is finished.
async fn respond(req: http::Request<()>, stream: &mut Stream, ctx: ReqCtx) -> bool {
    let Ok(resp) = handler::handle(req, ctx).await;
    let (mut parts, mut body) = resp.into_parts();
    for name in CONNECTION_SPECIFIC {
        parts.headers.remove(name);
    }
    // hyper adds `Date` on the TCP listeners; h3 does not.
    if let Ok(date) = HeaderValue::from_str(&httpdate::fmt_http_date(SystemTime::now())) {
        parts.headers.entry(header::DATE).or_insert(date);
    }
    if let Err(e) = stream.send_response(Response::from_parts(parts, ())).await {
        tracing::debug!("h3 response head: {e}");
        return false;
    }
    while let Some(frame) = body.frame().await {
        let data = match frame {
            Ok(f) => match f.into_data() {
                Ok(d) => d,
                Err(_) => continue, // trailers: never produced
            },
            Err(e) => {
                tracing::debug!("h3 body: {e}");
                stream.stop_stream(Code::H3_INTERNAL_ERROR);
                return false;
            }
        };
        if !data.is_empty() && stream.send_data(data).await.is_err() {
            return false;
        }
    }
    stream.finish().await.is_ok()
}
