//! A read budget for connections that have not completed `session.hello`.
//!
//! The upgraded socket is wrapped before the WebSocket library sees it, so
//! a peer that has not authenticated can make the daemon read (and buffer,
//! and parse) at most `budget` bytes in total, however large the frame it
//! announces. After a valid hello the budget is lifted and reads go straight
//! through, without copying.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, ready};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

const UNLIMITED: usize = usize::MAX;

/// Shared between the wrapper and the session that lifts it.
#[derive(Debug, Clone)]
pub(crate) struct Budget(Arc<AtomicUsize>);

impl Budget {
    pub fn new(bytes: usize) -> Self {
        Self(Arc::new(AtomicUsize::new(bytes.min(UNLIMITED - 1))))
    }

    /// No limit from now on.
    pub fn lift(&self) {
        self.0.store(UNLIMITED, Ordering::SeqCst);
    }
}

/// The error a read returns once the budget is spent.
#[derive(Debug)]
pub(crate) struct OverBudget;

impl std::fmt::Display for OverBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("too much data before session.hello")
    }
}

impl std::error::Error for OverBudget {}

/// Whether an I/O error (possibly wrapped by the WebSocket library) is ours.
pub(crate) fn is_over_budget(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<OverBudget>())
}

#[derive(Debug)]
pub(crate) struct Limited<S> {
    inner: S,
    budget: Budget,
}

impl<S> Limited<S> {
    pub fn new(inner: S, budget: Budget) -> Self {
        Self { inner, budget }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Limited<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        let left = me.budget.0.load(Ordering::SeqCst);
        if left == UNLIMITED {
            return Pin::new(&mut me.inner).poll_read(cx, buf);
        }
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if left == 0 {
            return Poll::Ready(Err(io::Error::new(io::ErrorKind::InvalidData, OverBudget)));
        }
        // Read into a small scratch buffer (only before hello, at most the
        // budget in total), so `buf` is filled safely.
        let mut scratch = vec![0u8; buf.remaining().min(left).min(16 * 1024)];
        let mut limited = ReadBuf::new(&mut scratch);
        ready!(Pin::new(&mut me.inner).poll_read(cx, &mut limited))?;
        let n = limited.filled().len();
        buf.put_slice(limited.filled());
        let _ = me
            .budget
            .0
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                (v != UNLIMITED).then(|| v.saturating_sub(n))
            });
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Limited<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn stops_at_the_budget_until_lifted() {
        let (mut a, b) = tokio::io::duplex(1 << 20);
        let budget = Budget::new(10);
        let mut limited = Limited::new(b, budget.clone());
        a.write_all(&[7u8; 64]).await.unwrap();
        let mut got = vec![0u8; 64];
        let n = limited.read(&mut got).await.unwrap();
        assert_eq!(n, 10);
        let err = limited.read(&mut got).await.unwrap_err();
        assert!(is_over_budget(&err), "{err}");
        budget.lift();
        let n = limited.read(&mut got).await.unwrap();
        assert_eq!(n, 54);
    }
}
