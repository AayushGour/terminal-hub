//! hub-transport: async FramedConn over tokio UnixStream.

pub mod auth;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::Path;

use hub_proto::{ControlMsg, Frame, FrameDecoder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

/// Async framed connection over a unix stream.
pub struct FramedConn {
    stream: UnixStream,
    decoder: FrameDecoder,
}

impl FramedConn {
    pub fn new(stream: UnixStream) -> FramedConn {
        FramedConn {
            stream,
            decoder: FrameDecoder::default(),
        }
    }

    /// Reads until one full frame is available.
    pub async fn read_frame(&mut self) -> anyhow::Result<Frame> {
        loop {
            if let Some(frame) = self.decoder.next_frame()? {
                return Ok(frame);
            }
            let mut buf = [0u8; 8192];
            let n = self.stream.read(&mut buf).await?;
            if n == 0 {
                anyhow::bail!("connection closed by peer");
            }
            self.decoder.push(&buf[..n]);
        }
    }

    /// Writes a pre-encoded frame (from encode_control/encode_data).
    pub async fn write_frame(&mut self, frame_bytes: &[u8]) -> anyhow::Result<()> {
        self.stream.write_all(frame_bytes).await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// Raw fd of the underlying socket (for `auth::peer_uid_ok`).
    pub fn raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// Send the mandatory `Hello { token }` first frame (client side).
    pub async fn send_hello(&mut self, token: &str) -> anyhow::Result<()> {
        self.write_frame(&auth::hello_frame(token)).await
    }

    /// Server-side auth gate (F1): verify the peer uid, then read the first
    /// frame and require it to be `Hello` with the expected token — bounded by
    /// `auth::HELLO_TIMEOUT` so a silent client can never hang the caller. On
    /// any failure the connection should be dropped (never processed); the
    /// error message NEVER contains the token.
    pub async fn verify_hello(&mut self, expected_token: &str) -> anyhow::Result<()> {
        if !auth::peer_uid_ok(self.raw_fd()) {
            anyhow::bail!("auth: peer uid mismatch");
        }
        let frame = tokio::time::timeout(auth::HELLO_TIMEOUT, self.read_frame())
            .await
            .map_err(|_| anyhow::anyhow!("auth: timed out waiting for Hello"))??;
        match frame {
            Frame::Control(ControlMsg::Hello { token }) if token == expected_token => Ok(()),
            Frame::Control(ControlMsg::Hello { .. }) => anyhow::bail!("auth: invalid token"),
            _ => anyhow::bail!("auth: first frame was not Hello"),
        }
    }
}

/// Bind a unix listener with 0700 dir + 0600 socket perms.
pub async fn bind_listener(path: &Path) -> anyhow::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    // Remove a stale socket so bind() doesn't fail with AddrInUse.
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub async fn connect(path: &Path) -> anyhow::Result<FramedConn> {
    let stream = UnixStream::connect(path).await?;
    Ok(FramedConn::new(stream))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_proto::{encode_control, encode_data, ControlMsg, Frame, SessionId};
    use std::os::unix::fs::PermissionsExt;

    fn temp_sock(name: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("hub-transport-test-{}-{}", std::process::id(), name));
        dir.push("hubd.sock");
        dir
    }

    #[tokio::test]
    async fn bind_sets_0700_dir_and_0600_socket() {
        let path = temp_sock("perms");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let _listener = bind_listener(&path).await.unwrap();

        let dir_mode = std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode();
        let sock_mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o777, 0o700, "socket dir must be 0700");
        assert_eq!(sock_mode & 0o777, 0o600, "socket must be 0600");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn control_frame_travels_end_to_end() {
        let path = temp_sock("control");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let listener = bind_listener(&path).await.unwrap();

        let server = tokio::spawn(async move {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut conn = FramedConn::new(stream);
            match conn.read_frame().await.unwrap() {
                Frame::Control(ControlMsg::Attach { id }) => id,
                other => panic!("unexpected frame: {other:?}"),
            }
        });

        let mut client = connect(&path).await.unwrap();
        client
            .write_frame(&encode_control(&ControlMsg::Attach { id: SessionId(77) }))
            .await
            .unwrap();

        let got = server.await.unwrap();
        assert_eq!(got, SessionId(77));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn many_queued_frames_do_not_stall_and_preserve_order() {
        // Writer sends N frames back-to-back; a slower reader drains them all in order.
        let path = temp_sock("backpressure");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let listener = bind_listener(&path).await.unwrap();

        let server = tokio::spawn(async move {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut conn = FramedConn::new(stream);
            let mut got = Vec::new();
            for _ in 0..100 {
                if let Frame::Data { id, bytes } = conn.read_frame().await.unwrap() {
                    got.push((id.0, bytes));
                }
            }
            got
        });

        let mut client = connect(&path).await.unwrap();
        for i in 0..100u64 {
            client
                .write_frame(&encode_data(SessionId(i), format!("chunk{i}").as_bytes()))
                .await
                .unwrap();
        }

        let got = server.await.unwrap();
        assert_eq!(got.len(), 100);
        assert_eq!(got[0], (0, b"chunk0".to_vec()));
        assert_eq!(got[99], (99, b"chunk99".to_vec()));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
