//! TCP listeners and per-connection serving (HTTP/3 is in `quic.rs`).

use http::HeaderValue;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;

use crate::conn::{ConnCtl, LabIo};
use crate::handler::{self, ReqCtx};
use crate::state::Shared;

const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) async fn accept_loop(
    listener: TcpListener,
    tls: Option<TlsAcceptor>,
    alt_svc: Option<HeaderValue>,
    shared: Arc<Shared>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        let (tcp, peer) = tokio::select! {
            r = listener.accept() => match r {
                Ok(x) => x,
                Err(e) => {
                    // Usually EMFILE; back off instead of spinning.
                    tracing::warn!("accept: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            _ = shutdown.changed() => break,
        };
        tokio::spawn(serve_conn(
            tcp,
            peer,
            tls.clone(),
            alt_svc.clone(),
            shared.clone(),
            shutdown.clone(),
        ));
    }
}

fn builder() -> auto::Builder<TokioExecutor> {
    let mut b = auto::Builder::new(TokioExecutor::new());
    b.http1()
        .timer(TokioTimer::new())
        .writev(true)
        .max_buf_size(1 << 20);
    b.http2()
        .timer(TokioTimer::new())
        .initial_stream_window_size(8 << 20)
        .initial_connection_window_size(16 << 20)
        .max_send_buf_size(1 << 20)
        .max_concurrent_streams(1024);
    b
}

async fn serve_conn(
    tcp: TcpStream,
    peer: SocketAddr,
    tls: Option<TlsAcceptor>,
    alt_svc: Option<HeaderValue>,
    shared: Arc<Shared>,
    mut shutdown: watch::Receiver<bool>,
) {
    let _ = tcp.set_nodelay(true);
    let ctl = Arc::new(ConnCtl::new(peer.ip().to_canonical()));
    let io = LabIo::new(tcp, ctl.clone());
    let ctx = ReqCtx {
        shared: shared.clone(),
        conn: ctl.clone(),
        tls: tls.is_some(),
        alt_svc,
    };
    let svc = service_fn(move |req| handler::handle(req, ctx.clone()));
    let b = builder();
    match tls {
        Some(acceptor) => {
            let stream = tokio::select! {
                r = tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(io)) => match r {
                    Ok(Ok(s)) => s,
                    _ => return,
                },
                _ = shutdown.changed() => return,
            };
            run(
                b.serve_connection(TokioIo::new(stream), svc),
                &ctl,
                &mut shutdown,
            )
            .await;
        }
        None => {
            run(
                b.serve_connection(TokioIo::new(io), svc),
                &ctl,
                &mut shutdown,
            )
            .await
        }
    }
    for idx in ctl.touched() {
        shared.scenarios[idx].stats.conn_closed(ctl.peer);
    }
}

/// Drives a connection until it ends, is killed (dropped → RST) or the
/// server shuts down.
async fn run<F, E>(conn: F, ctl: &ConnCtl, shutdown: &mut watch::Receiver<bool>)
where
    F: Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    tokio::select! {
        biased;
        _ = ctl.killed() => {}
        _ = shutdown.changed() => {}
        r = conn => {
            if let Err(e) = r {
                tracing::debug!("connection: {e}");
            }
        }
    }
}
