use crate::{ControlMsg, SessionId};

/// Maximum accepted frame length (`tag + payload`), guarding against runaway allocation.
pub const MAX_FRAME: u32 = 16 * 1024 * 1024;

const TAG_CONTROL: u8 = 0;
const TAG_DATA: u8 = 1;

#[derive(Debug, PartialEq)]
pub enum Frame {
    Control(ControlMsg),
    Data { id: SessionId, bytes: Vec<u8> },
}

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("bad json: {0}")]
    Json(String),
    #[error("unknown tag {0}")]
    UnknownTag(u8),
    #[error("frame too large: {0}")]
    TooLarge(u32),
    #[error("malformed frame: {0}")]
    Malformed(String),
}

/// Encode a control message as a full wire frame: `[len BE][tag=0][json]`.
pub fn encode_control(msg: &ControlMsg) -> Vec<u8> {
    let json = serde_json::to_vec(msg).expect("ControlMsg is always serializable");
    let len = (json.len() + 1) as u32; // tag + payload
    let mut out = Vec::with_capacity(4 + len as usize);
    out.extend_from_slice(&len.to_be_bytes());
    out.push(TAG_CONTROL);
    out.extend_from_slice(&json);
    out
}

/// Encode a data message as a full wire frame: `[len BE][tag=1][id BE][bytes]`.
pub fn encode_data(id: SessionId, bytes: &[u8]) -> Vec<u8> {
    let len = (1 + 8 + bytes.len()) as u32; // tag + id + bytes
    let mut out = Vec::with_capacity(4 + len as usize);
    out.extend_from_slice(&len.to_be_bytes());
    out.push(TAG_DATA);
    out.extend_from_slice(&id.0.to_be_bytes());
    out.extend_from_slice(bytes);
    out
}

/// Streaming decoder that tolerates partial/split reads.
#[derive(Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Returns the next complete frame if one is fully buffered, else `None`.
    /// Call in a loop until it returns `None`.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, ProtoError> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]);
        if len > MAX_FRAME {
            // Reject before we ever buffer the oversized body.
            return Err(ProtoError::TooLarge(len));
        }
        if len < 1 {
            // A legal frame is at least 1 byte (the tag). Drain the 4 length
            // bytes so the stream doesn't wedge, then report the error.
            self.buf.drain(..4);
            return Err(ProtoError::Malformed(format!(
                "frame length {len} is too short: minimum is 1 (tag byte)"
            )));
        }
        let total = 4 + len as usize;
        if self.buf.len() < total {
            return Ok(None); // body not fully arrived yet
        }

        // Consume the whole frame first so a decode error can't wedge the stream.
        let frame_bytes: Vec<u8> = self.buf.drain(..total).collect();
        let tag = frame_bytes[4];
        let payload = &frame_bytes[5..];

        match tag {
            TAG_CONTROL => {
                let msg: ControlMsg = serde_json::from_slice(payload)
                    .map_err(|e| ProtoError::Json(e.to_string()))?;
                Ok(Some(Frame::Control(msg)))
            }
            TAG_DATA => {
                if payload.len() < 8 {
                    return Err(ProtoError::Json("data frame shorter than 8-byte id".to_string()));
                }
                let mut id_bytes = [0u8; 8];
                id_bytes.copy_from_slice(&payload[..8]);
                let id = SessionId(u64::from_be_bytes(id_bytes));
                Ok(Some(Frame::Data {
                    id,
                    bytes: payload[8..].to_vec(),
                }))
            }
            other => Err(ProtoError::UnknownTag(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ControlMsg, SessionId};

    #[test]
    fn control_frame_round_trips() {
        let msg = ControlMsg::Attach { id: SessionId(42) };
        let bytes = encode_control(&msg);
        let mut dec = FrameDecoder::default();
        dec.push(&bytes);
        match dec.next_frame().unwrap() {
            Some(Frame::Control(got)) => assert_eq!(got, msg),
            other => panic!("expected control frame, got {other:?}"),
        }
        assert!(dec.next_frame().unwrap().is_none());
    }

    #[test]
    fn data_frame_round_trips() {
        let bytes = encode_data(SessionId(9), b"hello pty");
        let mut dec = FrameDecoder::default();
        dec.push(&bytes);
        match dec.next_frame().unwrap() {
            Some(Frame::Data { id, bytes }) => {
                assert_eq!(id, SessionId(9));
                assert_eq!(bytes, b"hello pty".to_vec());
            }
            other => panic!("expected data frame, got {other:?}"),
        }
    }

    #[test]
    fn partial_and_split_reads_are_tolerated() {
        let frame = encode_data(SessionId(1), b"abcdef");
        let mut dec = FrameDecoder::default();
        // Feed one byte at a time: no frame until the last byte arrives.
        for (i, b) in frame.iter().enumerate() {
            dec.push(&[*b]);
            let got = dec.next_frame().unwrap();
            if i + 1 < frame.len() {
                assert!(got.is_none(), "frame emitted too early at byte {i}");
            } else {
                assert!(matches!(got, Some(Frame::Data { .. })), "final byte must complete the frame");
            }
        }
    }

    #[test]
    fn two_frames_in_one_buffer_decode_in_order() {
        let mut buf = encode_control(&ControlMsg::List);
        buf.extend_from_slice(&encode_data(SessionId(2), b"x"));
        let mut dec = FrameDecoder::default();
        dec.push(&buf);
        assert!(matches!(dec.next_frame().unwrap(), Some(Frame::Control(ControlMsg::List))));
        assert!(matches!(dec.next_frame().unwrap(), Some(Frame::Data { id: SessionId(2), .. })));
        assert!(dec.next_frame().unwrap().is_none());
    }

    #[test]
    fn unknown_tag_errors() {
        // len = 1 (tag only), tag = 7 (unknown).
        let bytes = [0u8, 0, 0, 1, 7];
        let mut dec = FrameDecoder::default();
        dec.push(&bytes);
        match dec.next_frame() {
            Err(ProtoError::UnknownTag(7)) => {}
            other => panic!("expected UnknownTag(7), got {other:?}"),
        }
    }

    #[test]
    fn oversized_frame_errors_before_buffering_body() {
        let too_big = MAX_FRAME + 1;
        let mut bytes = too_big.to_be_bytes().to_vec();
        bytes.push(0); // tag byte only; body never sent
        let mut dec = FrameDecoder::default();
        dec.push(&bytes);
        match dec.next_frame() {
            Err(ProtoError::TooLarge(n)) => assert_eq!(n, too_big),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn zero_len_frame_errors_not_panics() {
        // len = 0: no tag byte at all. Must be rejected, not panic on
        // frame_bytes[4] indexing.
        let mut dec = FrameDecoder::default();
        dec.push(&[0u8, 0, 0, 0]);
        match dec.next_frame() {
            Err(ProtoError::Malformed(_)) => {}
            other => panic!("expected Malformed error, got {other:?}"),
        }

        // Also confirm it doesn't panic with a trailing byte buffered after
        // the zero-length prefix (i.e. more data queued up behind it).
        let mut dec2 = FrameDecoder::default();
        dec2.push(&[0u8, 0, 0, 0, 99]);
        match dec2.next_frame() {
            Err(ProtoError::Malformed(_)) => {}
            other => panic!("expected Malformed error, got {other:?}"),
        }
    }

    #[test]
    fn bad_json_control_frame_does_not_wedge_stream() {
        // Control frame (tag = 0) with invalid JSON payload: len = 1 (tag) + 3 (bad json).
        let bad_payload = b"???";
        let len = (1 + bad_payload.len()) as u32;
        let mut buf = len.to_be_bytes().to_vec();
        buf.push(TAG_CONTROL);
        buf.extend_from_slice(bad_payload);

        // Followed by a valid control frame in the same buffer.
        buf.extend_from_slice(&encode_control(&ControlMsg::List));

        let mut dec = FrameDecoder::default();
        dec.push(&buf);

        match dec.next_frame() {
            Err(ProtoError::Json(_)) => {}
            other => panic!("expected Json error for bad payload, got {other:?}"),
        }

        // The bad frame must have been fully consumed, so the second call
        // decodes the valid frame that follows it -- proving the stream
        // wasn't wedged.
        match dec.next_frame() {
            Ok(Some(Frame::Control(ControlMsg::List))) => {}
            other => panic!("expected Control(List) frame, got {other:?}"),
        }
        assert!(dec.next_frame().unwrap().is_none());
    }

    #[test]
    fn short_data_frame_errors() {
        // Data frame (tag = 1) with a payload shorter than the 8-byte SessionId.
        let short_id = [1u8, 2, 3]; // only 3 bytes, need 8
        let len = (1 + short_id.len()) as u32;
        let mut bytes = len.to_be_bytes().to_vec();
        bytes.push(TAG_DATA);
        bytes.extend_from_slice(&short_id);

        let mut dec = FrameDecoder::default();
        dec.push(&bytes);
        match dec.next_frame() {
            Err(_) => {}
            other => panic!("expected error for short data frame, got {other:?}"),
        }
    }
}
