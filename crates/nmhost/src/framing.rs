//! Native messaging framing: a 4-byte length in native byte order, then that
//! many bytes of UTF-8 JSON. Same format in both directions, for Chromium
//! browsers and Firefox alike.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest message a host may send to the browser. Chrome and Firefox both
/// disconnect the port when a host writes more than 1 MiB in one message.
pub const MAX_TO_BROWSER: usize = 1024 * 1024;

/// Largest message we accept from the browser. Chrome caps extension → host
/// messages at 64 MiB (Firefox allows more); anything bigger is drained and
/// reported instead of buffered.
pub const MAX_FROM_BROWSER: usize = 64 * 1024 * 1024;

/// One frame read from the browser.
#[derive(Debug, PartialEq, Eq)]
pub enum Inbound {
    /// The payload of a frame (not yet validated as UTF-8 or JSON).
    Message(Vec<u8>),
    /// A frame larger than the limit; its payload was read and discarded so
    /// the stream stays in sync. Carries the announced length.
    TooLarge(usize),
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("the stream ended in the middle of a frame")]
    Truncated,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A payload that does not fit in one host → browser message.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("message is {len} bytes; native messaging allows at most {MAX_TO_BROWSER}")]
pub struct TooLarge {
    pub len: usize,
}

/// Reads one frame. Returns `Ok(None)` on a clean end of stream (the browser
/// closed the port), i.e. EOF before the first byte of a length header.
pub async fn read_frame<R>(reader: &mut R, max: usize) -> Result<Option<Inbound>, FrameError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0u8; 4];
    let mut filled = 0;
    while filled < header.len() {
        let n = reader.read(&mut header[filled..]).await?;
        if n == 0 {
            return if filled == 0 {
                Ok(None)
            } else {
                Err(FrameError::Truncated)
            };
        }
        filled += n;
    }

    let len = u32::from_ne_bytes(header) as usize;
    if len > max {
        let mut rest = (&mut *reader).take(len as u64);
        let drained = tokio::io::copy(&mut rest, &mut tokio::io::sink()).await?;
        if drained < len as u64 {
            return Err(FrameError::Truncated);
        }
        return Ok(Some(Inbound::TooLarge(len)));
    }

    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await.map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            FrameError::Truncated
        } else {
            FrameError::Io(e)
        }
    })?;
    Ok(Some(Inbound::Message(payload)))
}

/// Encodes one host → browser frame, refusing payloads over [`MAX_TO_BROWSER`].
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, TooLarge> {
    if payload.len() > MAX_TO_BROWSER {
        return Err(TooLarge { len: payload.len() });
    }
    let len = u32::try_from(payload.len()).map_err(|_| TooLarge { len: payload.len() })?;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&len.to_ne_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Writes one already-encoded frame and flushes, so the browser sees it now.
pub async fn write_encoded<W>(writer: &mut W, frame: &[u8]) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    writer.write_all(frame).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(payload: &[u8]) -> Vec<u8> {
        encode_frame(payload).expect("small payload")
    }

    #[tokio::test]
    async fn round_trip() {
        let bytes = frame(br#"{"jsonrpc":"2.0","id":1,"method":"nm.status"}"#);
        let mut reader = &bytes[..];
        let got = read_frame(&mut reader, MAX_FROM_BROWSER).await.unwrap();
        assert_eq!(
            got,
            Some(Inbound::Message(
                br#"{"jsonrpc":"2.0","id":1,"method":"nm.status"}"#.to_vec()
            ))
        );
        assert_eq!(
            read_frame(&mut reader, MAX_FROM_BROWSER).await.unwrap(),
            None
        );
    }

    #[test]
    fn header_is_native_endian_length() {
        let encoded = frame(b"abc");
        assert_eq!(&encoded[..4], &3u32.to_ne_bytes());
        assert_eq!(&encoded[4..], b"abc");
    }

    #[tokio::test]
    async fn several_frames_back_to_back() {
        let mut bytes = frame(b"{}");
        bytes.extend(frame(b"[]"));
        bytes.extend(frame(b""));
        let mut reader = &bytes[..];
        for want in [&b"{}"[..], b"[]", b""] {
            let got = read_frame(&mut reader, 1024).await.unwrap();
            assert_eq!(got, Some(Inbound::Message(want.to_vec())));
        }
        assert_eq!(read_frame(&mut reader, 1024).await.unwrap(), None);
    }

    #[tokio::test]
    async fn frames_split_across_reads() {
        // A duplex pipe delivers whatever the writer wrote so far, so writing
        // one byte at a time exercises partial header and payload reads.
        let (mut tx, mut rx) = tokio::io::duplex(1);
        let bytes = frame(b"{\"split\":true}");
        let writer = tokio::spawn(async move {
            for b in bytes {
                tx.write_all(&[b]).await.unwrap();
            }
        });
        let got = read_frame(&mut rx, 1024).await.unwrap();
        writer.await.unwrap();
        assert_eq!(got, Some(Inbound::Message(b"{\"split\":true}".to_vec())));
    }

    #[tokio::test]
    async fn eof_inside_header_is_an_error() {
        let bytes = [1u8, 0];
        let mut reader = &bytes[..];
        let err = read_frame(&mut reader, 1024).await.unwrap_err();
        assert!(matches!(err, FrameError::Truncated));
    }

    #[tokio::test]
    async fn eof_inside_payload_is_an_error() {
        let mut bytes = 10u32.to_ne_bytes().to_vec();
        bytes.extend_from_slice(b"short");
        let mut reader = &bytes[..];
        let err = read_frame(&mut reader, 1024).await.unwrap_err();
        assert!(matches!(err, FrameError::Truncated));
    }

    #[tokio::test]
    async fn oversize_inbound_is_drained_and_stream_stays_in_sync() {
        let mut bytes = frame(&[b'x'; 100]);
        bytes.extend(frame(b"{\"next\":1}"));
        let mut reader = &bytes[..];
        assert_eq!(
            read_frame(&mut reader, 64).await.unwrap(),
            Some(Inbound::TooLarge(100))
        );
        assert_eq!(
            read_frame(&mut reader, 64).await.unwrap(),
            Some(Inbound::Message(b"{\"next\":1}".to_vec()))
        );
    }

    #[test]
    fn outbound_limit_is_exactly_one_mebibyte() {
        assert!(encode_frame(&vec![b' '; MAX_TO_BROWSER]).is_ok());
        assert_eq!(
            encode_frame(&vec![b' '; MAX_TO_BROWSER + 1]),
            Err(TooLarge {
                len: MAX_TO_BROWSER + 1
            })
        );
    }

    #[tokio::test]
    async fn write_encoded_writes_whole_frame() {
        let mut out = Vec::new();
        write_encoded(&mut out, &frame(b"{}")).await.unwrap();
        assert_eq!(out, frame(b"{}"));
    }
}
