//! Per-connection state and the socket wrapper that can reset a connection.
//!
//! An injected reset sets the connection's kill flag and wakes its serving
//! task, which drops the whole hyper connection. [`LabIo`]'s `Drop` sees the
//! flag and sets `SO_LINGER` to zero first, so the kernel sends a TCP RST
//! instead of a FIN — the client sees `ECONNRESET`, as with a real
//! middlebox or overloaded server.

use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::Notify;

use crate::bucket::TokenBucket;
use crate::lock;

#[derive(Debug)]
pub(crate) struct ConnCtl {
    pub peer: IpAddr,
    killed: AtomicBool,
    kill_notify: Notify,
    /// Scenario indices this connection has sent requests to.
    touched: Mutex<Vec<usize>>,
    /// Per-connection bandwidth buckets, per scenario.
    buckets: Mutex<Vec<(usize, Arc<TokenBucket>)>>,
}

impl ConnCtl {
    pub fn new(peer: IpAddr) -> Self {
        Self {
            peer,
            killed: AtomicBool::new(false),
            kill_notify: Notify::new(),
            touched: Mutex::default(),
            buckets: Mutex::default(),
        }
    }

    /// Resets the TCP connection (RST) as soon as its task notices.
    pub fn kill(&self) {
        self.killed.store(true, Ordering::SeqCst);
        self.kill_notify.notify_one();
    }

    pub fn is_killed(&self) -> bool {
        self.killed.load(Ordering::SeqCst)
    }

    /// Resolves once [`ConnCtl::kill`] has been called.
    pub async fn killed(&self) {
        if !self.is_killed() {
            self.kill_notify.notified().await;
        }
    }

    /// Records a request to scenario `idx`; true the first time.
    pub fn touch(&self, idx: usize) -> bool {
        let mut t = lock(&self.touched);
        if t.contains(&idx) {
            false
        } else {
            t.push(idx);
            true
        }
    }

    pub fn touched(&self) -> Vec<usize> {
        lock(&self.touched).clone()
    }

    /// This connection's bucket for scenario `idx`.
    pub fn bucket(&self, idx: usize, rate: u64) -> Arc<TokenBucket> {
        let mut b = lock(&self.buckets);
        if let Some((_, bucket)) = b.iter().find(|(i, _)| *i == idx) {
            return bucket.clone();
        }
        let bucket = Arc::new(TokenBucket::new(rate));
        b.push((idx, bucket.clone()));
        bucket
    }
}

/// A `TcpStream` that fails I/O once killed and resets (RST) on drop.
#[derive(Debug)]
pub(crate) struct LabIo {
    inner: TcpStream,
    ctl: Arc<ConnCtl>,
}

impl LabIo {
    pub fn new(inner: TcpStream, ctl: Arc<ConnCtl>) -> Self {
        Self { inner, ctl }
    }

    fn reset_error() -> io::Error {
        io::Error::new(io::ErrorKind::ConnectionReset, "lab: injected reset")
    }
}

impl Drop for LabIo {
    fn drop(&mut self) {
        if self.ctl.is_killed() {
            let _ = socket2::SockRef::from(&self.inner).set_linger(Some(Duration::ZERO));
        }
    }
}

impl AsyncRead for LabIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.ctl.is_killed() {
            return Poll::Ready(Err(Self::reset_error()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for LabIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.ctl.is_killed() {
            return Poll::Ready(Err(Self::reset_error()));
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        if self.ctl.is_killed() {
            return Poll::Ready(Err(Self::reset_error()));
        }
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
