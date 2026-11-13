//! Limits on what an HTTP/3 server can make the client buffer.
//!
//! h3 0.0.8 reads every non-DATA frame (HEADERS, unknown/GREASE frames)
//! completely into memory before looking at it, whatever length the frame
//! header announces, and quinn keeps granting flow-control credit as h3
//! reads. A server announcing a 2^40-byte frame could make the client
//! buffer gigabytes. This module wraps h3-quinn's streams and checks the
//! bytes before h3 sees them:
//!
//! - **request streams** are parsed frame by frame (only the frame headers;
//!   DATA payload passes through untouched, zero-copy): a non-DATA frame
//!   may announce at most [`MAX_NON_DATA`] bytes, and at most
//!   [`MAX_NON_DATA`] bytes of non-DATA framing (frame headers included) may
//!   arrive without DATA payload in between — which also bounds everything
//!   before the response head;
//! - **unidirectional streams** (control, QPACK, unknown types) may carry at
//!   most [`MAX_UNI_BYTES`] bytes in total; an honest server sends a few
//!   dozen (SETTINGS, GOAWAY), since this client uses stateless QPACK.
//!
//! On a violation the stream fails, the QUIC connection is closed with
//! `H3_EXCESSIVE_LOAD`, and the reason is kept in the connection's
//! [`Violation`] so the request reports `SourceError::Fatal` (the server
//! is broken or hostile; retrying will not help).

use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll, ready};

use bytes::{Buf, Bytes};
use h3::quic::{self, ConnectionErrorIncoming, StreamErrorIncoming, StreamId, WriteBuf};

/// Largest non-DATA frame, and most non-DATA bytes between DATA payload.
pub(crate) const MAX_NON_DATA: u64 = 64 * 1024;
/// Most bytes a server may send on one unidirectional stream.
pub(crate) const MAX_UNI_BYTES: u64 = 64 * 1024;
/// Header size limit announced to the server (`SETTINGS_MAX_FIELD_SECTION_SIZE`).
pub(crate) const MAX_FIELD_SECTION: u64 = 64 * 1024;
/// RFC 9114 §8.1.
const H3_EXCESSIVE_LOAD: u32 = 0x107;

/// Why a connection was cut off; set once, shared by its streams.
pub(crate) type Violation = Arc<OnceLock<String>>;

/// Tracks HTTP/3 frames on a request stream from their headers alone.
#[derive(Debug, Default)]
pub(crate) struct FrameGuard {
    /// Bytes of the frame header read so far (type and length varints,
    /// 16 bytes at most).
    head: [u8; 16],
    head_len: usize,
    /// Payload bytes left in the current frame, and whether it is DATA.
    left: u64,
    data: bool,
    /// Non-DATA bytes since the last DATA payload byte.
    non_data: u64,
}

/// Length of a QUIC varint from its first byte (RFC 9000 §16).
fn varint_len(first: u8) -> usize {
    1 << (first >> 6)
}

fn varint(b: &[u8]) -> u64 {
    let mut v = u64::from(b[0] & 0x3f);
    for &x in &b[1..] {
        v = (v << 8) | u64::from(x);
    }
    v
}

impl FrameGuard {
    /// Accounts for `chunk`; `Err` when the server broke a limit.
    pub fn feed(&mut self, mut chunk: &[u8]) -> Result<(), String> {
        while !chunk.is_empty() {
            if self.left > 0 {
                let n = (chunk.len() as u64).min(self.left);
                self.left -= n;
                chunk = &chunk[n as usize..];
                if self.data {
                    self.non_data = 0;
                } else {
                    self.count(n)?;
                }
                continue;
            }
            // A frame header: type varint, then length varint.
            self.head[self.head_len] = chunk[0];
            self.head_len += 1;
            chunk = &chunk[1..];
            self.count(1)?;
            let t_len = varint_len(self.head[0]);
            if self.head_len <= t_len {
                continue;
            }
            let l_len = varint_len(self.head[t_len]);
            if self.head_len < t_len + l_len {
                continue;
            }
            let ty = varint(&self.head[..t_len]);
            let len = varint(&self.head[t_len..t_len + l_len]);
            self.head_len = 0;
            self.data = ty == 0x0;
            if !self.data && len > MAX_NON_DATA {
                return Err(format!(
                    "HTTP/3 frame of type {ty:#x} announces {len} bytes (limit {MAX_NON_DATA})"
                ));
            }
            self.left = len;
        }
        Ok(())
    }

    fn count(&mut self, n: u64) -> Result<(), String> {
        self.non_data += n;
        if self.non_data > MAX_NON_DATA {
            return Err(format!(
                "more than {MAX_NON_DATA} bytes of HTTP/3 framing without response data"
            ));
        }
        Ok(())
    }
}

enum Limit {
    Frames(FrameGuard),
    Bytes(u64),
}

/// A receive stream with a [`Limit`].
pub(crate) struct Guarded<R> {
    inner: R,
    limit: Limit,
    quic: quinn::Connection,
    violation: Violation,
}

impl<R: quic::RecvStream<Buf = Bytes>> Guarded<R> {
    fn check(&mut self, b: &Bytes) -> Result<(), String> {
        match &mut self.limit {
            Limit::Frames(g) => g.feed(b),
            Limit::Bytes(left) => match left.checked_sub(b.len() as u64) {
                Some(l) => {
                    *left = l;
                    Ok(())
                }
                None => Err(format!(
                    "more than {MAX_UNI_BYTES} bytes on an HTTP/3 unidirectional stream"
                )),
            },
        }
    }
}

impl<R: quic::RecvStream<Buf = Bytes>> quic::RecvStream for Guarded<R> {
    type Buf = Bytes;

    fn poll_data(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<Bytes>, StreamErrorIncoming>> {
        let r = ready!(self.inner.poll_data(cx));
        if let Ok(Some(b)) = &r
            && let Err(why) = self.check(b)
        {
            tracing::warn!(reason = %why, "closing HTTP/3 connection");
            let _ = self.violation.set(why.clone());
            self.inner.stop_sending(u64::from(H3_EXCESSIVE_LOAD));
            self.quic.close(H3_EXCESSIVE_LOAD.into(), b"excessive load");
            return Poll::Ready(Err(StreamErrorIncoming::Unknown(why.into())));
        }
        Poll::Ready(r)
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.inner.stop_sending(error_code)
    }

    fn recv_id(&self) -> StreamId {
        self.inner.recv_id()
    }
}

/// A request stream: h3-quinn's send half, a guarded receive half.
pub(crate) struct GuardedBidi<B: Buf> {
    send: h3_quinn::SendStream<B>,
    recv: Guarded<h3_quinn::RecvStream>,
}

impl<B: Buf> GuardedBidi<B> {
    fn new(s: h3_quinn::BidiStream<B>, quic: &quinn::Connection, violation: &Violation) -> Self {
        let (send, recv) = quic::BidiStream::split(s);
        Self {
            send,
            recv: Guarded {
                inner: recv,
                limit: Limit::Frames(FrameGuard::default()),
                quic: quic.clone(),
                violation: violation.clone(),
            },
        }
    }
}

impl<B: Buf> quic::RecvStream for GuardedBidi<B> {
    type Buf = Bytes;

    fn poll_data(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<Bytes>, StreamErrorIncoming>> {
        self.recv.poll_data(cx)
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.recv.stop_sending(error_code)
    }

    fn recv_id(&self) -> StreamId {
        self.recv.recv_id()
    }
}

impl<B: Buf> quic::SendStream<B> for GuardedBidi<B> {
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        self.send.poll_ready(cx)
    }

    fn send_data<T: Into<WriteBuf<B>>>(&mut self, data: T) -> Result<(), StreamErrorIncoming> {
        self.send.send_data(data)
    }

    fn poll_finish(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        self.send.poll_finish(cx)
    }

    fn reset(&mut self, reset_code: u64) {
        self.send.reset(reset_code)
    }

    fn send_id(&self) -> StreamId {
        self.send.send_id()
    }
}

impl<B: Buf> quic::BidiStream<B> for GuardedBidi<B> {
    type SendStream = h3_quinn::SendStream<B>;
    type RecvStream = Guarded<h3_quinn::RecvStream>;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        (self.send, self.recv)
    }
}

/// Opens request streams with guarded receive halves.
#[derive(Clone)]
pub(crate) struct GuardedOpen {
    inner: h3_quinn::OpenStreams,
    quic: quinn::Connection,
    violation: Violation,
}

impl<B: Buf> quic::OpenStreams<B> for GuardedOpen {
    type BidiStream = GuardedBidi<B>;
    type SendStream = h3_quinn::SendStream<B>;

    fn poll_open_bidi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::BidiStream, StreamErrorIncoming>> {
        let s = ready!(quic::OpenStreams::<B>::poll_open_bidi(&mut self.inner, cx))?;
        Poll::Ready(Ok(GuardedBidi::new(s, &self.quic, &self.violation)))
    }

    fn poll_open_send(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::SendStream, StreamErrorIncoming>> {
        quic::OpenStreams::<B>::poll_open_send(&mut self.inner, cx)
    }

    fn close(&mut self, code: h3::error::Code, reason: &[u8]) {
        quic::OpenStreams::<B>::close(&mut self.inner, code, reason)
    }
}

/// h3-quinn's connection with every stream the server can write to
/// guarded.
pub(crate) struct GuardedConn {
    inner: h3_quinn::Connection,
    quic: quinn::Connection,
    violation: Violation,
}

impl GuardedConn {
    pub fn new(quic: quinn::Connection, violation: Violation) -> Self {
        Self {
            inner: h3_quinn::Connection::new(quic.clone()),
            quic,
            violation,
        }
    }
}

impl<B: Buf> quic::OpenStreams<B> for GuardedConn {
    type BidiStream = GuardedBidi<B>;
    type SendStream = h3_quinn::SendStream<B>;

    fn poll_open_bidi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::BidiStream, StreamErrorIncoming>> {
        let s = ready!(quic::OpenStreams::<B>::poll_open_bidi(&mut self.inner, cx))?;
        Poll::Ready(Ok(GuardedBidi::new(s, &self.quic, &self.violation)))
    }

    fn poll_open_send(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::SendStream, StreamErrorIncoming>> {
        quic::OpenStreams::<B>::poll_open_send(&mut self.inner, cx)
    }

    fn close(&mut self, code: h3::error::Code, reason: &[u8]) {
        quic::OpenStreams::<B>::close(&mut self.inner, code, reason)
    }
}

impl<B: Buf> quic::Connection<B> for GuardedConn {
    type RecvStream = Guarded<h3_quinn::RecvStream>;
    type OpenStreams = GuardedOpen;

    fn poll_accept_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::RecvStream, ConnectionErrorIncoming>> {
        let s = ready!(quic::Connection::<B>::poll_accept_recv(&mut self.inner, cx))?;
        Poll::Ready(Ok(Guarded {
            inner: s,
            limit: Limit::Bytes(MAX_UNI_BYTES),
            quic: self.quic.clone(),
            violation: self.violation.clone(),
        }))
    }

    fn poll_accept_bidi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::BidiStream, ConnectionErrorIncoming>> {
        let s = ready!(quic::Connection::<B>::poll_accept_bidi(&mut self.inner, cx))?;
        Poll::Ready(Ok(GuardedBidi::new(s, &self.quic, &self.violation)))
    }

    fn opener(&self) -> Self::OpenStreams {
        GuardedOpen {
            inner: quic::Connection::<B>::opener(&self.inner),
            quic: self.quic.clone(),
            violation: self.violation.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A QUIC varint in its shortest form.
    fn vi(v: u64) -> Vec<u8> {
        match v {
            0..=63 => vec![v as u8],
            64..=16383 => (v as u16 | 0x4000).to_be_bytes().to_vec(),
            16384..=1_073_741_823 => (v as u32 | 0x8000_0000).to_be_bytes().to_vec(),
            _ => (v | 0xc000_0000_0000_0000).to_be_bytes().to_vec(),
        }
    }

    fn frame(ty: u64, len: u64) -> Vec<u8> {
        let mut f = vi(ty);
        f.extend(vi(len));
        f
    }

    /// Feeds `bytes` in pieces of `step` (to split varints across chunks).
    fn feed(g: &mut FrameGuard, bytes: &[u8], step: usize) -> Result<(), String> {
        bytes.chunks(step).try_for_each(|c| g.feed(c))
    }

    #[test]
    fn a_normal_response_passes() {
        for step in [1, 2, 3, 7, 1 << 20] {
            let mut g = FrameGuard::default();
            let mut b = frame(0x1, 40); // HEADERS
            b.extend([0u8; 40]);
            for _ in 0..50 {
                b.extend(frame(0x0, 100_000)); // DATA, larger than the limit
                b.extend(vec![7u8; 100_000]);
                b.extend(frame(0x21, 10)); // GREASE between DATA frames
                b.extend([0u8; 10]);
            }
            b.extend(frame(0x1, 30)); // trailers
            b.extend([0u8; 30]);
            feed(&mut g, &b, step).unwrap_or_else(|e| panic!("step {step}: {e}"));
        }
    }

    #[test]
    fn a_huge_frame_announcement_fails_at_once() {
        for (ty, len) in [
            (0x1, 1u64 << 40),
            (0x21, MAX_NON_DATA + 1),
            (0x7, u64::MAX >> 2),
        ] {
            let mut g = FrameGuard::default();
            let e = feed(&mut g, &frame(ty, len), 1).unwrap_err();
            assert!(e.contains("announces"), "{e}");
        }
        // Exactly at the limit is fine.
        let mut g = FrameGuard::default();
        feed(&mut g, &frame(0x1, MAX_NON_DATA - 2), 3).unwrap();
    }

    #[test]
    fn endless_small_frames_without_data_fail() {
        let mut g = FrameGuard::default();
        let mut b = Vec::new();
        for _ in 0..200 {
            b.extend(frame(0x21, 1000));
            b.extend([0u8; 1000]);
        }
        let e = feed(&mut g, &b, 4096).unwrap_err();
        assert!(e.contains("without response data"), "{e}");
        // Empty DATA frames do not count as data either.
        let mut g = FrameGuard::default();
        let b: Vec<u8> = (0..40_000).flat_map(|_| frame(0x0, 0)).collect();
        assert!(feed(&mut g, &b, 4096).is_err());
    }
}
