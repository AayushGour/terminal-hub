//! Async framed read/write helpers built on `hub_proto` framing.
//! Generic over any AsyncRead/AsyncWrite so they work with UnixStream
//! owned-halves AND in-memory duplex pipes in tests.

use hub_proto::{ControlMsg, Frame, FrameDecoder};
use std::path::Path;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// Client-side F1 dial, shared by every hub client/test that hand-rolls a
/// daemon/relay connection (`hub-tui`'s `ViewerClient`, the daemon-integration
/// and e2e test harnesses): connect to `sock`, then send the mandatory
/// `Hello { token }` as the FIRST frame. The token is read (or lazily created)
/// from `<base>/token` via `hub_transport::auth` — for the daemon socket
/// `<base>/hubd.sock`, `base` is that socket's parent dir. Returns the framed
/// reader plus the raw write half, positioned right after the handshake so the
/// caller issues its first real op (Open/List/Attach/…) next. This is the
/// client counterpart to the server-side `FrameReader::verify_hello`, so the
/// whole auth handshake lives in exactly two centralized places. The token is
/// never logged.
pub async fn dial_hello(
    sock: &Path,
    base: &Path,
) -> anyhow::Result<(FrameReader<OwnedReadHalf>, OwnedWriteHalf)> {
    let token = hub_transport::auth::ensure_token(base)?;
    let stream = tokio::net::UnixStream::connect(sock).await?;
    let (rd, mut wr) = stream.into_split();
    hub_transport::auth::send_hello(&mut wr, &token).await?;
    Ok((FrameReader::new(rd), wr))
}

pub struct FrameReader<R> {
    rd: R,
    dec: FrameDecoder,
    buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    pub fn new(rd: R) -> Self {
        Self { rd, dec: FrameDecoder::default(), buf: vec![0u8; 64 * 1024] }
    }

    /// Server-side auth gate (F1): read the mandatory first frame and require
    /// it to be `Hello { token }` with the expected token. Bounded by
    /// `hub_transport::auth::HELLO_TIMEOUT` so a silent/partial client can
    /// never hang the accept path — auth failure is always a prompt
    /// reject-and-close. Bytes pipelined after `Hello` stay buffered in this
    /// reader's decoder, so the caller keeps reading normally afterwards. The
    /// peer-uid check is done separately by the caller on the raw fd (which it
    /// still holds before splitting the stream). The error NEVER contains the
    /// token, so a rejection log leaks nothing.
    pub async fn verify_hello(&mut self, expected_token: &str) -> anyhow::Result<()> {
        let frame = tokio::time::timeout(hub_transport::auth::HELLO_TIMEOUT, self.next())
            .await
            .map_err(|_| anyhow::anyhow!("auth: timed out waiting for Hello"))??
            .ok_or_else(|| anyhow::anyhow!("auth: connection closed before Hello"))?;
        match frame {
            Frame::Control(ControlMsg::Hello { token }) if token == expected_token => Ok(()),
            Frame::Control(ControlMsg::Hello { .. }) => anyhow::bail!("auth: invalid token"),
            _ => anyhow::bail!("auth: first frame was not Hello"),
        }
    }

    /// Next complete frame, or `None` at clean EOF. Errors on malformed framing.
    pub async fn next(&mut self) -> anyhow::Result<Option<Frame>> {
        loop {
            if let Some(f) = self.dec.next_frame()? {
                return Ok(Some(f));
            }
            let n = self.rd.read(&mut self.buf).await?;
            if n == 0 {
                return Ok(None);
            }
            self.dec.push(&self.buf[..n]);
        }
    }
}

/// Write a pre-encoded frame (from `encode_control` / `encode_data`) and flush.
pub async fn write_frame<W: AsyncWrite + Unpin>(wr: &mut W, bytes: &[u8]) -> anyhow::Result<()> {
    wr.write_all(bytes).await?;
    wr.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_proto::{encode_control, ControlMsg, Frame};

    #[tokio::test]
    async fn frame_reader_reads_control_then_eof() {
        // A duplex pipe stands in for a socket.
        let (mut a, b) = tokio::io::duplex(64 * 1024);
        // Write two control frames then drop the writer -> EOF.
        let f1 = encode_control(&ControlMsg::List);
        let f2 = encode_control(&ControlMsg::Kill { id: hub_proto::SessionId(7) });
        tokio::io::AsyncWriteExt::write_all(&mut a, &f1).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut a, &f2).await.unwrap();
        drop(a);

        let mut rd = FrameReader::new(b);
        match rd.next().await.unwrap() {
            Some(Frame::Control(ControlMsg::List)) => {}
            other => panic!("expected List, got {other:?}"),
        }
        match rd.next().await.unwrap() {
            Some(Frame::Control(ControlMsg::Kill { id })) => assert_eq!(id.0, 7),
            other => panic!("expected Kill, got {other:?}"),
        }
        assert!(rd.next().await.unwrap().is_none(), "clean EOF -> None");
    }
}
