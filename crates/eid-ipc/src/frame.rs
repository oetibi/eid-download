//! Length-prefixed framing shared by the IPC pipe and the browser Native Messaging channel:
//! a 4-byte length (native byte order = little endian on every supported target) followed by that
//! many bytes of JSON. The length is checked **before** anything is allocated or read.

use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

// Native Messaging uses the machine's native byte order; the IPC pipe uses the same code. Every
// target we build for is little endian, so one implementation serves both.
#[cfg(not(target_endian = "little"))]
compile_error!("eid-ipc framing assumes a little-endian target");

/// Largest frame accepted in either direction (ADR-0002, ARCHITECTURE §2): 1 MiB.
pub const MAX_FRAME: usize = 1 << 20;

#[derive(Debug)]
pub enum FrameError {
    /// The peer closed the stream cleanly between frames.
    Closed,
    /// The stream ended inside a header or a body.
    Truncated,
    /// The declared length exceeds the limit (the body was never read).
    TooLarge(u64),
    /// A zero-length frame (never valid: every message is a JSON object).
    Empty,
    Io(io::Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Closed => write!(f, "connection closed"),
            FrameError::Truncated => write!(f, "truncated frame"),
            FrameError::TooLarge(n) => write!(f, "frame of {n} bytes exceeds the limit"),
            FrameError::Empty => write!(f, "empty frame"),
            FrameError::Io(e) => write!(f, "i/o error: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Read one frame of at most `max` bytes.
pub async fn read_frame_max<R: AsyncRead + Unpin>(
    r: &mut R,
    max: usize,
) -> Result<Vec<u8>, FrameError> {
    let mut hdr = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut hdr[got..]).await {
            Ok(0) if got == 0 => return Err(FrameError::Closed),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(if got == 0 {
                    FrameError::Closed
                } else {
                    FrameError::Truncated
                })
            }
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
    let len = u32::from_le_bytes(hdr) as u64;
    if len == 0 {
        return Err(FrameError::Empty);
    }
    if len > max as u64 {
        return Err(FrameError::TooLarge(len));
    }
    let mut body = vec![0u8; len as usize];
    match r.read_exact(&mut body).await {
        Ok(_) => Ok(body),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(FrameError::Truncated),
        Err(e) => Err(FrameError::Io(e)),
    }
}

/// Read one frame with the default [`MAX_FRAME`] limit.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Vec<u8>, FrameError> {
    read_frame_max(r, MAX_FRAME).await
}

/// Write one frame (header and body in a single write so a frame is never interleaved).
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, body: &[u8]) -> Result<(), FrameError> {
    if body.is_empty() {
        return Err(FrameError::Empty);
    }
    if body.len() > MAX_FRAME {
        return Err(FrameError::TooLarge(body.len() as u64));
    }
    let mut buf = Vec::with_capacity(4 + body.len());
    buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
    buf.extend_from_slice(body);
    w.write_all(&buf).await.map_err(FrameError::Io)?;
    w.flush().await.map_err(FrameError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip_and_clean_close() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"{\"a\":1}").await.unwrap();
        write_frame(&mut buf, b"{}").await.unwrap();
        let mut r = &buf[..];
        assert_eq!(read_frame(&mut r).await.unwrap(), b"{\"a\":1}");
        assert_eq!(read_frame(&mut r).await.unwrap(), b"{}");
        assert!(matches!(read_frame(&mut r).await, Err(FrameError::Closed)));
    }

    #[tokio::test]
    async fn oversized_is_refused_without_reading_the_body() {
        // The header claims 2 MiB but nothing follows: it must fail on the header alone.
        let hdr = (2u32 << 20).to_le_bytes();
        let mut r = &hdr[..];
        assert!(matches!(
            read_frame(&mut r).await,
            Err(FrameError::TooLarge(n)) if n == 2 << 20
        ));
        // u32::MAX must not allocate 4 GiB
        let hdr = u32::MAX.to_le_bytes();
        let mut r = &hdr[..];
        assert!(matches!(
            read_frame(&mut r).await,
            Err(FrameError::TooLarge(_))
        ));
        // exactly at the limit is fine, one more is not
        let mut ok = (MAX_FRAME as u32).to_le_bytes().to_vec();
        ok.extend(std::iter::repeat_n(b' ', MAX_FRAME));
        assert_eq!(read_frame(&mut &ok[..]).await.unwrap().len(), MAX_FRAME);
        let over = ((MAX_FRAME + 1) as u32).to_le_bytes();
        assert!(matches!(
            read_frame(&mut &over[..]).await,
            Err(FrameError::TooLarge(_))
        ));
    }

    #[tokio::test]
    async fn truncated_header_and_body_and_empty() {
        for hdr in [&[1u8][..], &[1, 0][..], &[1, 0, 0][..]] {
            assert!(matches!(
                read_frame(&mut &hdr[..]).await,
                Err(FrameError::Truncated)
            ));
        }
        let mut b = 10u32.to_le_bytes().to_vec();
        b.extend_from_slice(b"abc");
        assert!(matches!(
            read_frame(&mut &b[..]).await,
            Err(FrameError::Truncated)
        ));
        assert!(matches!(
            read_frame(&mut &0u32.to_le_bytes()[..]).await,
            Err(FrameError::Empty)
        ));
        assert!(write_frame(&mut Vec::new(), b"").await.is_err());
        assert!(write_frame(&mut Vec::new(), &vec![b'x'; MAX_FRAME + 1])
            .await
            .is_err());
    }
}
