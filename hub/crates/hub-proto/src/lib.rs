//! hub-proto: frozen wire types + framing. No IO, no tokio.

mod framing;
mod types;

pub use framing::{encode_control, encode_data, Frame, FrameDecoder, ProtoError, MAX_FRAME};
pub use types::{ControlMsg, Origin, SessionId, SessionInfo};
