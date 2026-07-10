//! `Content-Length` framing over the child's stdio — the child side of lb's supervisor wire.
//!
//! One frame = `Content-Length: N\r\n\r\n` followed by exactly N bytes of JSON. This is the
//! LSP/JSON-RPC framing lb's `lb-supervisor` speaks on the host side (`frame.rs`); a native
//! extension built on this SDK must frame identically or the supervisor cannot read it. The two
//! sides are a frozen wire contract — this file is the child mirror of lb's host framing, kept
//! byte-for-byte compatible (a length prefix, not a delimiter that could appear in the payload).
//!
//! A frame above [`MAX_FRAME`] is rejected so a malformed host cannot make the child allocate
//! unbounded memory (the symmetric guard lb's host applies to the child).

use std::io::Error;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The largest single frame this side will read (16 MiB) — matches lb's `frame::MAX_FRAME`.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Write `payload` as one `Content-Length`-framed message to `w`, flushing it.
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, payload: &[u8]) -> Result<(), Error> {
    let header = format!("Content-Length: {}\r\n\r\n", payload.len());
    w.write_all(header.as_bytes()).await?;
    w.write_all(payload).await?;
    w.flush().await?;
    Ok(())
}

/// Read exactly one `Content-Length`-framed message from `r`, returning its body bytes. Tolerates a
/// header split across reads. Errors on a closed stream (EOF — the host went away), a malformed
/// header, or an over-large frame.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Vec<u8>, Error> {
    let len = read_content_length(r).await?;
    if len > MAX_FRAME {
        return Err(Error::other(format!(
            "frame too large: {len} > {MAX_FRAME}"
        )));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(body)
}

/// Read the header block one byte at a time (a partial read can't desync the stream) up to the
/// terminating `\r\n\r\n`, parse the `Content-Length`, and return it.
async fn read_content_length<R: AsyncRead + Unpin>(r: &mut R) -> Result<usize, Error> {
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = r.read(&mut byte).await?;
        if n == 0 {
            return Err(Error::other("host closed stream"));
        }
        header.push(byte[0]);
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
        if header.len() > 8192 {
            return Err(Error::other("header too long"));
        }
    }
    parse_content_length(&header)
}

/// Parse `Content-Length: N` (case-insensitive on the field name) out of a header block.
fn parse_content_length(header: &[u8]) -> Result<usize, Error> {
    let text =
        std::str::from_utf8(header).map_err(|e| Error::other(format!("header not utf8: {e}")))?;
    for line in text.split("\r\n") {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                return value
                    .trim()
                    .parse::<usize>()
                    .map_err(|e| Error::other(format!("bad content-length: {e}")));
            }
        }
    }
    Err(Error::other("missing Content-Length header"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn round_trips_a_frame() {
        let (mut a, mut b) = duplex(1024);
        write_frame(&mut a, br#"{"hi":1}"#).await.unwrap();
        assert_eq!(read_frame(&mut b).await.unwrap(), br#"{"hi":1}"#);
    }

    #[tokio::test]
    async fn reads_two_frames_in_sequence() {
        let (mut a, mut b) = duplex(1024);
        write_frame(&mut a, b"one").await.unwrap();
        write_frame(&mut a, b"two").await.unwrap();
        assert_eq!(read_frame(&mut b).await.unwrap(), b"one");
        assert_eq!(read_frame(&mut b).await.unwrap(), b"two");
    }

    #[tokio::test]
    async fn eof_is_an_error() {
        let (a, mut b) = duplex(1024);
        drop(a);
        assert!(read_frame(&mut b).await.is_err());
    }

    #[test]
    fn parses_case_insensitive_header() {
        assert_eq!(
            parse_content_length(b"content-length: 42\r\n\r\n").unwrap(),
            42
        );
    }
}
