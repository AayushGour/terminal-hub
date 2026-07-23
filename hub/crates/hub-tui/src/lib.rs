//! hub-tui: minimal headless viewer lib. `ViewerClient` connects to the
//! daemon's reverse proxy (`hubd.sock`), attaches to a session, and exposes
//! recv/send helpers reused by the `hub-tui` bin and by e2e tests.

use hub_proto::{encode_control, encode_data, ControlMsg, Frame, SessionId};
use hub_relay::conn::{write_frame, FrameReader};
use std::path::Path;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

pub struct ViewerClient {
    id: SessionId,
    fr: FrameReader<OwnedReadHalf>,
    wr: OwnedWriteHalf,
}

impl ViewerClient {
    /// Connect to the daemon and attach to `id`.
    pub async fn connect(daemon_sock: &Path, id: SessionId) -> anyhow::Result<Self> {
        // F1 auth: the daemon requires `Hello { token }` as the FIRST frame. The
        // token lives at `<base>/token`, where `base` is the daemon socket's
        // parent dir (`<base>/hubd.sock`). `dial_hello` sends it before Attach.
        let base = daemon_sock.parent().ok_or_else(|| {
            anyhow::anyhow!("daemon socket path has no parent dir for token lookup")
        })?;
        let (fr, mut wr) = hub_relay::conn::dial_hello(daemon_sock, base).await?;
        write_frame(&mut wr, &encode_control(&ControlMsg::Attach { id })).await?;
        Ok(Self { id, fr, wr })
    }

    pub async fn recv(&mut self) -> anyhow::Result<Option<Frame>> { self.fr.next().await }

    pub async fn send_input(&mut self, bytes: &[u8]) {
        let _ = write_frame(&mut self.wr, &encode_data(self.id, bytes)).await;
    }

    pub async fn claim_size(&mut self, cols: u16, rows: u16) {
        let _ = write_frame(&mut self.wr, &encode_control(&ControlMsg::ClaimSize { id: self.id, cols, rows })).await;
    }

    pub async fn detach(&mut self) {
        let _ = write_frame(&mut self.wr, &encode_control(&ControlMsg::Detach { id: self.id })).await;
    }
}
