//! Response bodies: the transport-specific body plus the connection it
//! holds, and the checked stream handed to the engine.

use std::future::Future;
use std::pin::Pin;
use std::sync::Weak;
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant};

use bytes::{Buf, Bytes};
use futures::Stream;
use http_body::Body as _;
use vayu_core::SourceError;

use crate::classify::describe;
use crate::origin::Origin;
use crate::pool::{H1Conn, H2Sender, H3Sender, MuxLease, Pool};

pub(crate) type H3Stream = h3::client::RequestStream<crate::h3guard::GuardedBidi<Bytes>, Bytes>;

/// The connection a response is using.
pub(crate) enum Lease {
    /// HTTP/1.1: goes back to the idle pool if the response was read to
    /// the end, else it is closed.
    H1 {
        conn: Option<H1Conn>,
        origin: Origin,
        pool: Weak<Pool>,
        complete: bool,
    },
    H2(MuxLease<H2Sender>),
    H3(MuxLease<H3Sender>),
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Lease::H1 {
            conn,
            origin,
            pool,
            complete,
        } = self
            && let Some(c) = conn.take()
            && *complete
            && let Some(p) = pool.upgrade()
        {
            p.put_h1(origin.clone(), c);
        }
        // Otherwise `H1Conn`'s drop closes the socket.
    }
}

/// A request on this connection failed; see `MuxLease::retire`.
fn retire(lease: &Lease) {
    match lease {
        Lease::H2(l) => l.retire(),
        Lease::H3(l) => l.retire(),
        Lease::H1 { .. } => {} // never reused unless complete
    }
}

fn mark_complete(lease: &mut Lease) {
    if let Lease::H1 { complete, .. } = lease {
        *complete = true;
    }
}

enum Inner {
    Hyper(hyper::body::Incoming),
    H3(Box<H3Stream>),
}

/// A response body still attached to its connection.
pub(crate) struct RespBody {
    // Field order matters: the body is dropped before the lease, so an
    // unfinished HTTP/1.1 body has already closed its connection when the
    // lease decides whether to pool it.
    inner: Inner,
    lease: Lease,
}

impl RespBody {
    pub fn hyper(b: hyper::body::Incoming, lease: Lease) -> Self {
        Self {
            inner: Inner::Hyper(b),
            lease,
        }
    }

    pub fn h3(s: H3Stream, lease: Lease) -> Self {
        Self {
            inner: Inner::H3(Box::new(s)),
            lease,
        }
    }

    fn mark_complete(&mut self) {
        mark_complete(&mut self.lease);
    }

    /// The next chunk of body bytes (zero-copy), `None` at the end.
    pub fn poll_chunk(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes, SourceError>>> {
        match &mut self.inner {
            Inner::Hyper(b) => loop {
                match ready!(Pin::new(&mut *b).poll_frame(cx)) {
                    None => {
                        mark_complete(&mut self.lease);
                        return Poll::Ready(None);
                    }
                    Some(Ok(frame)) => {
                        if let Ok(data) = frame.into_data() {
                            return Poll::Ready(Some(Ok(data)));
                        } // trailers: skip
                    }
                    Some(Err(e)) => {
                        retire(&self.lease);
                        return Poll::Ready(Some(Err(SourceError::Transient(format!(
                            "reading body: {}",
                            describe(&e)
                        )))));
                    }
                }
            },
            Inner::H3(s) => match ready!(s.poll_recv_data(cx)) {
                Ok(Some(mut buf)) => {
                    let n = buf.remaining();
                    Poll::Ready(Some(Ok(buf.copy_to_bytes(n))))
                }
                Ok(None) => Poll::Ready(None),
                Err(e) => {
                    retire(&self.lease);
                    let err = match &self.lease {
                        Lease::H3(l) if l.conn.violation().is_some() => {
                            SourceError::Fatal(format!(
                                "HTTP/3 server exceeded a limit: {}",
                                l.conn.violation().unwrap_or_default()
                            ))
                        }
                        _ => SourceError::Transient(format!("reading body: {e}")),
                    };
                    Poll::Ready(Some(Err(err)))
                }
            },
        }
    }

    /// Whether the transport already knows the body is over (HTTP/1.1
    /// with a `Content-Length` that has been fully received).
    fn is_end_stream(&self) -> bool {
        match &self.inner {
            Inner::Hyper(b) => b.is_end_stream(),
            Inner::H3(_) => false,
        }
    }

    fn is_h1(&self) -> bool {
        matches!(self.lease, Lease::H1 { .. })
    }

    /// Reads and discards up to `limit` more bytes within `wait`, so an
    /// HTTP/1.1 connection can be reused; gives up (closing it) otherwise.
    pub async fn drain(mut self, limit: u64, wait: Duration) {
        if !self.is_h1() || limit == 0 {
            return; // Dropping resets the stream (HTTP/2, HTTP/3) or closes.
        }
        if self.is_end_stream() {
            self.mark_complete();
            return;
        }
        let mut read = 0u64;
        let _ = tokio::time::timeout(wait, async {
            loop {
                match std::future::poll_fn(|cx| self.poll_chunk(cx)).await {
                    Some(Ok(b)) => {
                        read += b.len() as u64;
                        if read > limit {
                            return;
                        }
                    }
                    Some(Err(_)) | None => return,
                }
            }
        })
        .await;
    }

    /// [`RespBody::drain`] in the background (if there is a runtime).
    pub fn discard(self, limit: u64, wait: Duration) {
        if !self.is_h1() {
            return;
        }
        if self.is_end_stream() {
            let mut s = self;
            s.mark_complete();
            return;
        }
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(self.drain(limit, wait));
        }
    }
}

/// How much of an unwanted body is read to keep an HTTP/1.1 connection.
pub(crate) const DRAIN_LIMIT: u64 = 64 * 1024;
/// How long a background drain may take.
pub(crate) const DRAIN_WAIT: Duration = Duration::from_secs(2);
/// How long a drain the caller waits for may take (small bodies sent with
/// the head: error pages, redirects, the probe's one byte).
pub(crate) const INLINE_WAIT: Duration = Duration::from_millis(300);

/// The stream the engine reads: exactly `remaining` bytes (when known),
/// cut off at the end, an error if the body ends early, and a backstop
/// error after `idle` without data. Contract points 2, 4 and 5.
pub(crate) struct CheckedBody {
    body: Option<RespBody>,
    remaining: Option<u64>,
    idle: Duration,
    timer: Pin<Box<tokio::time::Sleep>>,
    last: Instant,
    done: bool,
}

impl CheckedBody {
    pub fn new(body: RespBody, expected: Option<u64>, idle: Duration) -> Self {
        let mut me = Self {
            body: Some(body),
            remaining: expected,
            idle,
            timer: Box::pin(tokio::time::sleep(idle)),
            last: Instant::now(),
            done: false,
        };
        if expected == Some(0) {
            me.done = true;
            me.finish();
        }
        me
    }

    /// Everything wanted has arrived: release the connection (pooled if
    /// the transport saw the end, else drained in the background).
    fn finish(&mut self) {
        if let Some(b) = self.body.take() {
            b.discard(DRAIN_LIMIT, DRAIN_WAIT);
        }
    }

    fn fail(&mut self, e: SourceError) -> Poll<Option<Result<Bytes, SourceError>>> {
        self.done = true;
        self.body = None;
        Poll::Ready(Some(Err(e)))
    }
}

impl Stream for CheckedBody {
    type Item = Result<Bytes, SourceError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        let Some(body) = this.body.as_mut() else {
            this.done = true;
            return Poll::Ready(None);
        };
        loop {
            match body.poll_chunk(cx) {
                Poll::Ready(Some(Ok(mut b))) => {
                    if b.is_empty() {
                        continue;
                    }
                    this.last = Instant::now();
                    if let Some(rem) = this.remaining.as_mut() {
                        if b.len() as u64 >= *rem {
                            // The last wanted bytes; anything beyond the
                            // range is cut off (contract point 2).
                            b.truncate(*rem as usize);
                            *rem = 0;
                            this.done = true;
                            this.finish();
                        } else {
                            *rem -= b.len() as u64;
                        }
                    }
                    return Poll::Ready(Some(Ok(b)));
                }
                Poll::Ready(Some(Err(e))) => return this.fail(e),
                Poll::Ready(None) => {
                    if let Some(rem) = this.remaining.filter(|&r| r > 0) {
                        return this.fail(SourceError::Transient(format!(
                            "body ended {rem} bytes early"
                        )));
                    }
                    this.done = true;
                    this.body = None; // complete: pooled by the lease
                    return Poll::Ready(None);
                }
                Poll::Pending => loop {
                    if this.timer.as_mut().poll(cx).is_pending() {
                        return Poll::Pending;
                    }
                    // A huge configured `idle` means no backstop.
                    let Some(deadline) = this.last.checked_add(this.idle) else {
                        return Poll::Pending;
                    };
                    if Instant::now() >= deadline {
                        return this.fail(SourceError::Transient(format!(
                            "no data for {:?}",
                            this.idle
                        )));
                    }
                    this.timer.as_mut().reset(deadline.into());
                },
            }
        }
    }
}

/// A stream with nothing in it (an empty range).
pub(crate) fn empty() -> vayu_core::ByteStream {
    Box::pin(futures::stream::empty())
}
