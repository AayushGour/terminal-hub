# Foundations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the four pure/testable library crates (`hub-proto`, `hub-pty`, `hub-term`, `hub-transport`) that Plans 2–4 consume, using the frozen Interface Contract types verbatim.

**Architecture:** A Cargo workspace at `hub/` holds four sibling library crates. `hub-proto` is IO-free wire types + framing; `hub-term` wraps `vt100` for a headless screen + replay snapshot; `hub-pty` wraps `portable-pty` and bridges blocking pty reads onto `std::sync::mpsc` channels via an internal reader thread; `hub-transport` layers an async `FramedConn` over a tokio `UnixStream` and binds a listener with locked-down permissions. No binaries are produced in this plan.

**Tech Stack:** Rust 2021, `portable-pty` 0.8, `vt100` 0.15, `tokio` 1 (full), `serde`/`serde_json` 1, `thiserror` 1, `anyhow` 1.

## Global Constraints

Copied verbatim from `INTERFACE-CONTRACT.md` and the spec — every task's requirements implicitly include this section.

- **Workspace:** Cargo workspace at repo root `hub/` (spec §15). Rust **edition 2021**.
- **Shared deps pinned in root `Cargo.toml` `[workspace.dependencies]`** (exact floors):
  - `tokio = { version = "1", features = ["full"] }`
  - `serde = { version = "1", features = ["derive"] }`
  - `serde_json = "1"`
  - `portable-pty = "0.8"`
  - `vt100 = "0.15"`
  - `anyhow = "1"`
  - `thiserror = "1"`
  - `tracing = "0.1"`, `tracing-subscriber = "0.3"`
- **Crate boundaries (who owns what):**
  - `hub-proto` — pure types + framing. **NO IO, NO tokio.**
  - `hub-pty` — pty spawn/resize/io + child-death. Wraps `portable-pty`.
  - `hub-term` — headless vt parse + scrollback + replay snapshot. Wraps `vt100`.
  - `hub-transport` — async framed conn + unix listener (0600). Depends on `hub-proto` + tokio.
- **Wire frame (frozen):** `[len: u32 BE][tag: u8][payload...]` where `len` counts `tag + payload`. `tag = 0` → control (JSON of `ControlMsg`). `tag = 1` → data (`[id: u64 BE][raw pty bytes...]`).
- **Max frame length:** `pub const MAX_FRAME: u32 = 16 * 1024 * 1024;`
- **Platform:** v1 targets **mac/linux** (shared unix code). Windows is phase 2 behind trait seams — not in scope here.
- **Filesystem perms (spec §11):** socket dir `0700`, socket `0600`.
- **Naming:** Use the Contract type/function names **verbatim** — `SessionId`, `Origin`, `SessionInfo`, `ControlMsg`, `Frame`, `encode_control`, `encode_data`, `FrameDecoder`, `ProtoError`, `MAX_FRAME`, `Pty`, `PtyOutput`, `PtySize`, `Screen`, `FramedConn`, `bind_listener`, `connect`. Do not rename.
- **Security:** no pty-byte or env logging anywhere (not exercised by these crates, but keep it out of any debug prints).

---

### Task 1: Workspace scaffold

**Files:**
- Create: `hub/Cargo.toml` (workspace root)
- Create: `hub/crates/hub-proto/Cargo.toml`
- Create: `hub/crates/hub-proto/src/lib.rs`
- Create: `hub/crates/hub-term/Cargo.toml`
- Create: `hub/crates/hub-term/src/lib.rs`
- Create: `hub/crates/hub-pty/Cargo.toml`
- Create: `hub/crates/hub-pty/src/lib.rs`
- Create: `hub/crates/hub-transport/Cargo.toml`
- Create: `hub/crates/hub-transport/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: a buildable 4-crate workspace with empty library roots that later tasks fill in.

- [ ] **Step 1: Create the workspace root manifest**

Create `hub/Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = [
    "crates/hub-proto",
    "crates/hub-term",
    "crates/hub-pty",
    "crates/hub-transport",
]

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
portable-pty = "0.8"
vt100 = "0.15"
anyhow = "1"
thiserror = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
```

- [ ] **Step 2: Create each crate manifest**

Create `hub/crates/hub-proto/Cargo.toml`:

```toml
[package]
name = "hub-proto"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
```

Create `hub/crates/hub-term/Cargo.toml`:

```toml
[package]
name = "hub-term"
version = "0.1.0"
edition = "2021"

[dependencies]
vt100 = { workspace = true }
```

Create `hub/crates/hub-pty/Cargo.toml`:

```toml
[package]
name = "hub-pty"
version = "0.1.0"
edition = "2021"

[dependencies]
portable-pty = { workspace = true }
anyhow = { workspace = true }
```

Create `hub/crates/hub-transport/Cargo.toml`:

```toml
[package]
name = "hub-transport"
version = "0.1.0"
edition = "2021"

[dependencies]
hub-proto = { path = "../hub-proto" }
tokio = { workspace = true }
anyhow = { workspace = true }
```

- [ ] **Step 3: Create empty library roots**

Create each of these four files with a single doc line so the crates compile:

`hub/crates/hub-proto/src/lib.rs`:
```rust
//! hub-proto: frozen wire types + framing. No IO, no tokio.
```

`hub/crates/hub-term/src/lib.rs`:
```rust
//! hub-term: headless vt100 screen + replay snapshot.
```

`hub/crates/hub-pty/src/lib.rs`:
```rust
//! hub-pty: portable-pty wrapper with a blocking-read -> mpsc bridge.
```

`hub/crates/hub-transport/src/lib.rs`:
```rust
//! hub-transport: async FramedConn over tokio UnixStream.
```

- [ ] **Step 4: Verify the workspace builds**

Run: `cd hub && cargo build --workspace`
Expected: downloads deps then `Finished` with no errors. The four crates compile as empty libraries.

- [ ] **Step 5: Checkpoint**

Run: `cd hub && cargo test --workspace`
Expected: each crate reports `running 0 tests` and `test result: ok. 0 passed; 0 failed`. Do not commit.

---

### Task 2: `hub-proto` — frozen types

**Files:**
- Modify: `hub/crates/hub-proto/src/lib.rs`
- Create: `hub/crates/hub-proto/src/types.rs`
- Test: inline `#[cfg(test)] mod tests` in `hub/crates/hub-proto/src/types.rs`

**Interfaces:**
- Consumes: nothing.
- Produces (frozen — used by every other crate):
  - `pub struct SessionId(pub u64)` — derives `Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize`.
  - `pub enum Origin { External, Hub }` — derives `Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize`.
  - `pub struct SessionInfo { id, origin, title, pid, started_unix, cols, rows }`.
  - `pub enum ControlMsg { Open{..}, Opened{..}, Closed{..}, List, Sessions{..}, Attach{..}, Detach{..}, Replay{..}, Resize{..}, ClaimSize{..}, Release{..}, Kill{..}, Error{..} }`.

- [ ] **Step 1: Write the failing test**

Create `hub/crates/hub-proto/src/types.rs` with only the test module at the bottom first (types not yet defined), so the test drives the shapes:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_msg_json_round_trips() {
        let msg = ControlMsg::Open {
            shell: "/bin/zsh".to_string(),
            cwd: "/home/u".to_string(),
            env: vec![("TERM".to_string(), "xterm-256color".to_string())],
            cols: 80,
            rows: 24,
            term: "xterm-256color".to_string(),
            origin: Origin::External,
            title: "zsh".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ControlMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn session_info_round_trips_and_ids_are_hashable() {
        use std::collections::HashSet;
        let info = SessionInfo {
            id: SessionId(7),
            origin: Origin::Hub,
            title: "build".to_string(),
            pid: 4242,
            started_unix: 1_700_000_000,
            cols: 120,
            rows: 40,
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, back);

        let mut set = HashSet::new();
        set.insert(SessionId(7));
        assert!(set.contains(&SessionId(7)));
        assert!(!set.contains(&SessionId(8)));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd hub && cargo test -p hub-proto --lib -- --nocolor`
Expected: FAIL to compile — `cannot find type ControlMsg in this scope`, `cannot find type SessionId in this scope` (types not defined yet; `types` module also not wired into `lib.rs`).

- [ ] **Step 3: Write minimal implementation**

Prepend the type definitions to `hub/crates/hub-proto/src/types.rs` (above the test module):

```rust
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    External,
    Hub,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub origin: Origin,
    pub title: String,
    pub pid: u32,
    pub started_unix: u64,
    pub cols: u16,
    pub rows: u16,
}

/// Control-plane messages (serialized as JSON in a control frame).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ControlMsg {
    // relay -> daemon
    Open {
        shell: String,
        cwd: String,
        env: Vec<(String, String)>,
        cols: u16,
        rows: u16,
        term: String,
        origin: Origin,
        title: String,
    },
    Opened {
        id: SessionId,
    },
    Closed {
        id: SessionId,
        exit_code: Option<i32>,
    },
    // hub <-> daemon
    List,
    Sessions {
        sessions: Vec<SessionInfo>,
    },
    Attach {
        id: SessionId,
    },
    Detach {
        id: SessionId,
    },
    Replay {
        id: SessionId,
        screen: Vec<u8>,
    },
    Resize {
        id: SessionId,
        cols: u16,
        rows: u16,
    },
    ClaimSize {
        id: SessionId,
        cols: u16,
        rows: u16,
    },
    Release {
        id: SessionId,
    },
    Kill {
        id: SessionId,
    },
    Error {
        message: String,
    },
}
```

Wire the module into `hub/crates/hub-proto/src/lib.rs`:

```rust
//! hub-proto: frozen wire types + framing. No IO, no tokio.

mod types;

pub use types::{ControlMsg, Origin, SessionId, SessionInfo};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd hub && cargo test -p hub-proto --lib -- --nocolor`
Expected: PASS — `test result: ok. 2 passed; 0 failed`.

- [ ] **Step 5: Checkpoint**

Run: `cd hub && cargo test -p hub-proto`
Expected: `test result: ok. 2 passed; 0 failed`. Do not commit.

---

### Task 3: `hub-proto` — framing (encode + streaming decode)

**Files:**
- Create: `hub/crates/hub-proto/src/framing.rs`
- Modify: `hub/crates/hub-proto/src/lib.rs`
- Test: inline `#[cfg(test)] mod tests` in `hub/crates/hub-proto/src/framing.rs`

**Interfaces:**
- Consumes: `SessionId`, `ControlMsg` from Task 2.
- Produces (frozen):
  - `pub enum Frame { Control(ControlMsg), Data { id: SessionId, bytes: Vec<u8> } }`
  - `pub fn encode_control(msg: &ControlMsg) -> Vec<u8>` — full frame incl length.
  - `pub fn encode_data(id: SessionId, bytes: &[u8]) -> Vec<u8>` — full frame incl length.
  - `pub struct FrameDecoder` (`#[derive(Default)]`) with `pub fn push(&mut self, bytes: &[u8])` and `pub fn next_frame(&mut self) -> Result<Option<Frame>, ProtoError>`.
  - `pub enum ProtoError { Json(String), UnknownTag(u8), TooLarge(u32) }` (`thiserror`).
  - `pub const MAX_FRAME: u32 = 16 * 1024 * 1024;`

- [ ] **Step 1: Write the failing test**

Create `hub/crates/hub-proto/src/framing.rs` with the test module first:

```rust
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
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd hub && cargo test -p hub-proto --lib -- --nocolor`
Expected: FAIL to compile — `cannot find function encode_control`, `cannot find type FrameDecoder`, `cannot find type Frame`, `cannot find type ProtoError`, `cannot find value MAX_FRAME`.

- [ ] **Step 3: Write minimal implementation**

Prepend to `hub/crates/hub-proto/src/framing.rs` (above the test module):

```rust
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
```

Wire the module and re-exports into `hub/crates/hub-proto/src/lib.rs`:

```rust
//! hub-proto: frozen wire types + framing. No IO, no tokio.

mod framing;
mod types;

pub use framing::{encode_control, encode_data, Frame, FrameDecoder, ProtoError, MAX_FRAME};
pub use types::{ControlMsg, Origin, SessionId, SessionInfo};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd hub && cargo test -p hub-proto --lib -- --nocolor`
Expected: PASS — `test result: ok. 8 passed; 0 failed` (2 from Task 2 + 6 here).

- [ ] **Step 5: Checkpoint**

Run: `cd hub && cargo test -p hub-proto`
Expected: `test result: ok. 8 passed; 0 failed`. Do not commit.

---

### Task 4: `hub-term` — headless vt100 screen + replay

**Files:**
- Modify: `hub/crates/hub-term/src/lib.rs`
- Test: inline `#[cfg(test)] mod tests` in `hub/crates/hub-term/src/lib.rs`

**Interfaces:**
- Consumes: `vt100` 0.15 (`Parser::new(rows, cols, scrollback_len)`, `Parser::process`, `Parser::screen`, `Parser::set_size`, `Screen::contents_formatted() -> Vec<u8>`, `Screen::contents() -> String`).
- Produces (frozen):
  - `pub struct Screen`
  - `pub fn new(rows: u16, cols: u16, scrollback: usize) -> Screen`
  - `pub fn feed(&mut self, bytes: &[u8])`
  - `pub fn resize(&mut self, rows: u16, cols: u16)`
  - `pub fn replay_bytes(&self) -> Vec<u8>` — ANSI bytes reproducing the current screen (for REPLAY on attach).

⚠️ executor: verify against `cargo doc` for `vt100` 0.15 that `Parser::set_size(&mut self, rows: u16, cols: u16)` and `Screen::contents_formatted(&self) -> Vec<u8>` exist. (Confirmed present in 0.15; `set_size` moved to `Screen` in 0.16 — if the resolved version is ≥ 0.16, call `self.parser.screen_mut().set_size(rows, cols)` instead.)

- [ ] **Step 1: Write the failing test**

Replace `hub/crates/hub-term/src/lib.rs` test placeholder by appending a test module (types not yet defined so it fails to compile):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_then_replay_contains_visible_text() {
        let mut screen = Screen::new(24, 80, 10_000);
        screen.feed(b"hello world");
        let replay = screen.replay_bytes();
        // contents_formatted embeds escape codes but the literal glyphs survive.
        let as_str = String::from_utf8_lossy(&replay);
        assert!(as_str.contains("hello world"), "replay missing text: {as_str:?}");
    }

    #[test]
    fn replay_reproduces_screen_in_a_fresh_terminal() {
        let mut a = Screen::new(10, 40, 100);
        a.feed(b"line one\r\nline two\r\n");
        let replay = a.replay_bytes();

        let mut b = Screen::new(10, 40, 100);
        b.feed(&replay);
        // Re-rendering A's replay into B yields the identical current screen.
        assert_eq!(a.replay_bytes(), b.replay_bytes());
    }

    #[test]
    fn resize_does_not_panic_and_new_width_takes_effect() {
        let mut screen = Screen::new(24, 80, 100);
        screen.feed(b"before resize");
        screen.resize(10, 20);
        screen.feed(b"\r\nafter resize");
        let replay = screen.replay_bytes();
        assert!(String::from_utf8_lossy(&replay).contains("after resize"));
    }

    #[test]
    fn scrollback_cap_keeps_latest_lines_on_screen() {
        // 2 visible rows, small scrollback; write far more lines than fit.
        let mut screen = Screen::new(2, 20, 3);
        for i in 0..50 {
            screen.feed(format!("row{i}\r\n").as_bytes());
        }
        let visible = String::from_utf8_lossy(&screen.replay_bytes()).to_string();
        // The most recent lines are still present; ancient lines have scrolled away.
        assert!(visible.contains("row49"), "latest line should be on screen");
        assert!(!visible.contains("row0\n") && !visible.contains("row00"), "oldest line should be gone");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd hub && cargo test -p hub-term --lib -- --nocolor`
Expected: FAIL to compile — `cannot find type Screen in this scope` / `no function new found`.

- [ ] **Step 3: Write minimal implementation**

Set `hub/crates/hub-term/src/lib.rs` to (keep the test module at the bottom):

```rust
//! hub-term: headless vt100 screen + replay snapshot.

/// A headless terminal screen backed by `vt100`, used to build REPLAY snapshots.
pub struct Screen {
    parser: vt100::Parser,
}

impl Screen {
    /// `scrollback` = max scrollback lines (default 10_000, configurable later).
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Screen {
        Screen {
            parser: vt100::Parser::new(rows, cols, scrollback),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        // vt100 0.15: Parser::set_size. (0.16+: Screen::set_size — see executor note.)
        self.parser.set_size(rows, cols);
    }

    /// ANSI byte stream that reproduces the CURRENT screen when written to a
    /// fresh terminal. Used for REPLAY on attach.
    pub fn replay_bytes(&self) -> Vec<u8> {
        self.parser.screen().contents_formatted()
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd hub && cargo test -p hub-term --lib -- --nocolor`
Expected: PASS — `test result: ok. 4 passed; 0 failed`.

⚠️ executor: if `scrollback_cap_keeps_latest_lines_on_screen` fails on the exact "oldest line gone" assertion, it is an assertion-tightness issue, not an API issue — `contents_formatted` only renders the visible grid (not scrollback), so `row0` cannot appear with 2 visible rows. Confirm the failure text before adjusting; the API calls are correct.

- [ ] **Step 5: Checkpoint**

Run: `cd hub && cargo test -p hub-term`
Expected: `test result: ok. 4 passed; 0 failed`. Do not commit.

---

### Task 5: `hub-pty` — portable-pty wrapper + blocking-read→mpsc bridge

**Files:**
- Modify: `hub/crates/hub-pty/src/lib.rs`
- Test: inline `#[cfg(test)] mod tests` in `hub/crates/hub-pty/src/lib.rs`

**Interfaces:**
- Consumes: `portable-pty` 0.8 (`native_pty_system`, `PtySystem::openpty`, `PtySize`, `CommandBuilder`, `SlavePty::spawn_command`, `MasterPty::try_clone_reader`, `MasterPty::take_writer`, `MasterPty::resize`, `Child::process_id`, `Child::try_wait`, `Child::kill`, `ExitStatus::exit_code`).
- Produces (frozen):
  - `pub struct PtySize { pub cols: u16, pub rows: u16 }`
  - `pub struct Pty`
  - `pub struct PtyOutput { pub rx: std::sync::mpsc::Receiver<Vec<u8>>, pub exit_rx: std::sync::mpsc::Receiver<Option<i32>> }`
  - `Pty::spawn(shell, cwd, env, size) -> anyhow::Result<(Pty, PtyOutput)>`
  - `Pty::write(&mut self, &[u8]) -> anyhow::Result<()>`
  - `Pty::resize(&mut self, PtySize) -> anyhow::Result<()>`
  - `Pty::child_pid(&self) -> Option<u32>`
  - `Pty::kill(&mut self) -> anyhow::Result<()>`

**The bridge (key systems piece):** `portable-pty` I/O is **blocking**. `spawn` starts TWO internal `std::thread`s: (1) a **reader thread** that owns a cloned blocking reader and loops `read()` → `tx.send(Vec<u8>)`, breaking on EOF (`Ok(0)`) or a dropped receiver; (2) a **waiter thread** that polls `child.try_wait()` (child shared via `Arc<Mutex<..>>`) and, on exit, sends the exit code once on `exit_tx`. Polling (not blocking `wait()`) is deliberate: it never holds the mutex across a blocking call, so `kill()` can always acquire the lock. Plan 2 wraps `rx`/`exit_rx` into tokio via `spawn_blocking` or a bridge task — that is out of scope here.

⚠️ executor: verify against `cargo doc` for `portable-pty` 0.8 that `ExitStatus::exit_code() -> u32` and that `Child::kill()` on unix sends a terminating signal. The Contract says `kill` = "SIGHUP + drop". `portable-pty`'s `Child::kill` is the portable primitive used here; if a specific SIGHUP is later required, add `libc` to `[workspace.dependencies]` and send `libc::kill(pid, libc::SIGHUP)` — do NOT add that dep speculatively in this plan.

- [ ] **Step 1: Write the failing test**

Append a test module to `hub/crates/hub-pty/src/lib.rs` (types not yet defined → fails to compile). These are integration-style but live in-crate; they spawn a real `/bin/sh` and drive it.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Drain `rx` for up to `timeout`, returning all bytes seen as a String.
    fn collect_until(rx: &std::sync::mpsc::Receiver<Vec<u8>>, needle: &str, timeout: Duration) -> String {
        let start = Instant::now();
        let mut acc = String::new();
        while start.elapsed() < timeout {
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
                acc.push_str(&String::from_utf8_lossy(&chunk));
                if acc.contains(needle) {
                    break;
                }
            }
        }
        acc
    }

    #[test]
    fn spawn_write_read_echoes_output() {
        let (mut pty, out) = Pty::spawn(
            "/bin/sh",
            ".",
            &[("PS1".to_string(), "".to_string())],
            PtySize { cols: 80, rows: 24 },
        )
        .expect("spawn sh");
        pty.write(b"echo hub-marker-123\n").unwrap();
        let seen = collect_until(&out.rx, "hub-marker-123", Duration::from_secs(5));
        assert!(seen.contains("hub-marker-123"), "did not see echoed marker; saw: {seen:?}");
    }

    #[test]
    fn child_pid_is_present_after_spawn() {
        let (pty, _out) = Pty::spawn("/bin/sh", ".", &[], PtySize { cols: 80, rows: 24 }).unwrap();
        assert!(pty.child_pid().is_some(), "expected a child pid");
    }

    #[test]
    fn resize_reports_new_size_via_stty() {
        let (mut pty, out) = Pty::spawn("/bin/sh", ".", &[], PtySize { cols: 80, rows: 24 }).unwrap();
        pty.resize(PtySize { cols: 100, rows: 30 }).unwrap();
        // stty size prints "rows cols".
        pty.write(b"stty size\n").unwrap();
        let seen = collect_until(&out.rx, "30 100", Duration::from_secs(5));
        assert!(seen.contains("30 100"), "expected '30 100' from stty size; saw: {seen:?}");
    }

    #[test]
    fn child_exit_fires_exit_channel() {
        let (mut pty, out) = Pty::spawn("/bin/sh", ".", &[], PtySize { cols: 80, rows: 24 }).unwrap();
        pty.write(b"exit 0\n").unwrap();
        let code = out
            .exit_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("exit channel should fire when the shell exits");
        assert_eq!(code, Some(0), "clean exit should report code 0");
    }

    #[test]
    fn kill_ends_the_shell() {
        let (mut pty, out) = Pty::spawn("/bin/sh", ".", &[], PtySize { cols: 80, rows: 24 }).unwrap();
        pty.kill().unwrap();
        // After kill, the waiter thread must eventually report an exit.
        let got = out.exit_rx.recv_timeout(Duration::from_secs(5));
        assert!(got.is_ok(), "exit channel should fire after kill");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd hub && cargo test -p hub-pty --lib -- --nocolor`
Expected: FAIL to compile — `cannot find type Pty`, `cannot find type PtySize`, `cannot find type PtyOutput`, `no function spawn found`.

- [ ] **Step 3: Write minimal implementation**

Prepend the implementation to `hub/crates/hub-pty/src/lib.rs` (above the test module), and update the doc line:

```rust
//! hub-pty: portable-pty wrapper with a blocking-read -> mpsc bridge.

use std::io::{Read, Write};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use portable_pty::{
    native_pty_system, Child, CommandBuilder, MasterPty, PtySize as PortablePtySize,
};

#[derive(Clone, Copy, Debug)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
}

/// Owns the pty master + child handle. The blocking reader/waiter run on
/// internal threads; their output is delivered on `PtyOutput`.
pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    child_pid: Option<u32>,
}

pub struct PtyOutput {
    /// Blocking pty reads are bridged onto this channel by an internal thread.
    pub rx: Receiver<Vec<u8>>,
    /// Fires once with the exit code when the child exits (EOF on pty).
    pub exit_rx: Receiver<Option<i32>>,
}

impl Pty {
    /// Spawn `shell` in a fresh pty. `env` overrides/extends inherited env.
    pub fn spawn(
        shell: &str,
        cwd: &str,
        env: &[(String, String)],
        size: PtySize,
    ) -> anyhow::Result<(Pty, PtyOutput)> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PortablePtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(shell);
        cmd.cwd(cwd);
        for (k, v) in env {
            cmd.env(k, v);
        }

        let child = pair.slave.spawn_command(cmd)?;
        let child_pid = child.process_id();

        // Clone a blocking reader and take the writer BEFORE moving master into `Pty`.
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let child = Arc::new(Mutex::new(child));

        let (tx, rx) = channel::<Vec<u8>>();
        let (exit_tx, exit_rx) = channel::<Option<i32>>();

        // (1) Reader thread: bridge blocking pty reads onto the mpsc channel.
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,               // EOF: child closed the pty
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;                // receiver dropped
                        }
                    }
                    Err(_) => break,              // fd closed / error
                }
            }
        });

        // (2) Waiter thread: poll try_wait so kill() can always take the lock.
        let child_for_wait = Arc::clone(&child);
        thread::spawn(move || loop {
            let status = {
                let mut guard = child_for_wait.lock().unwrap();
                guard.try_wait()
            };
            match status {
                Ok(Some(exit)) => {
                    let code = exit.exit_code() as i32;
                    let _ = exit_tx.send(Some(code));
                    break;
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(_) => {
                    let _ = exit_tx.send(None);
                    break;
                }
            }
        });

        let pty = Pty {
            master: pair.master,
            writer,
            child,
            child_pid,
        };
        Ok((pty, PtyOutput { rx, exit_rx }))
    }

    pub fn write(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn resize(&mut self, size: PtySize) -> anyhow::Result<()> {
        self.master.resize(PortablePtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    pub fn child_pid(&self) -> Option<u32> {
        self.child_pid
    }

    /// SIGHUP + drop → ends the shell.
    pub fn kill(&mut self) -> anyhow::Result<()> {
        let mut guard = self.child.lock().unwrap();
        guard.kill()?;
        Ok(())
    }
}
```

⚠️ executor: `pair.master.try_clone_reader()` and `pair.master.take_writer()` take `&self` in `portable-pty` 0.8, so calling them before moving `pair.master` into `Pty` is fine. If the resolved API instead consumes `master`, clone the reader/take the writer first and store the returned handles only. Verify via `cargo doc`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cd hub && cargo test -p hub-pty --lib -- --nocolor --test-threads=1`
Expected: PASS — `test result: ok. 5 passed; 0 failed`. (`--test-threads=1` avoids many concurrent pty allocations racing on CI; the tests are independent but pty-heavy.)

⚠️ executor: `resize_reports_new_size_via_stty` depends on the shell being ready to run `stty` and can be timing-sensitive. If it flakes, increase the `collect_until` timeout to 10s before suspecting the API. The `stty size` output format "rows cols" is stable on mac/linux.

- [ ] **Step 5: Checkpoint**

Run: `cd hub && cargo test -p hub-pty -- --test-threads=1`
Expected: `test result: ok. 5 passed; 0 failed`. Do not commit.

---

### Task 6: `hub-transport` — async FramedConn + bind_listener/connect

**Files:**
- Modify: `hub/crates/hub-transport/src/lib.rs`
- Test: inline `#[cfg(test)] mod tests` in `hub/crates/hub-transport/src/lib.rs`

**Interfaces:**
- Consumes: `hub_proto::{Frame, FrameDecoder, ControlMsg, SessionId, encode_control, encode_data}`; tokio `UnixStream`/`UnixListener`, `AsyncReadExt`/`AsyncWriteExt`; `std::os::unix::fs::PermissionsExt`.
- Produces (frozen):
  - `pub struct FramedConn`
  - `FramedConn::new(stream: tokio::net::UnixStream) -> FramedConn`
  - `FramedConn::read_frame(&mut self) -> anyhow::Result<hub_proto::Frame>` — reads until one full frame is available.
  - `FramedConn::write_frame(&mut self, frame_bytes: &[u8]) -> anyhow::Result<()>` — writes a pre-encoded frame.
  - `pub async fn bind_listener(path: &std::path::Path) -> anyhow::Result<tokio::net::UnixListener>` — 0700 dir + 0600 socket.
  - `pub async fn connect(path: &std::path::Path) -> anyhow::Result<FramedConn>`

- [ ] **Step 1: Write the failing test**

Append a test module to `hub/crates/hub-transport/src/lib.rs` (types not yet defined → fails to compile):

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd hub && cargo test -p hub-transport --lib -- --nocolor`
Expected: FAIL to compile — `cannot find type FramedConn`, `cannot find function bind_listener`, `cannot find function connect`.

- [ ] **Step 3: Write minimal implementation**

Set `hub/crates/hub-transport/src/lib.rs` to (keep the test module at the bottom):

```rust
//! hub-transport: async FramedConn over tokio UnixStream.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hub_proto::{Frame, FrameDecoder};
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd hub && cargo test -p hub-transport --lib -- --nocolor`
Expected: PASS — `test result: ok. 3 passed; 0 failed`.

- [ ] **Step 5: Checkpoint**

Run: `cd hub && cargo test -p hub-transport`
Expected: `test result: ok. 3 passed; 0 failed`. Do not commit.

Then run the full workspace once:

Run: `cd hub && cargo test --workspace -- --test-threads=1`
Expected: all four crates green — `hub-proto` 8 passed, `hub-term` 4 passed, `hub-pty` 5 passed, `hub-transport` 3 passed. Do not commit.

---

## Self-Review (applied inline)

**1. Spec / Contract coverage for the 4 crates**

- `hub-proto` types — Task 2 defines `SessionId`, `Origin`, `SessionInfo`, `ControlMsg` verbatim incl. all 14 `ControlMsg` variants (Open/Opened/Closed/List/Sessions/Attach/Detach/Replay/Resize/ClaimSize/Release/Kill/Error). ✅
- `hub-proto` framing — Task 3 implements `encode_control`, `encode_data`, `FrameDecoder{push,next_frame}`, `Frame`, `ProtoError{Json,UnknownTag,TooLarge}`, `MAX_FRAME`, wire layout `[len BE][tag][payload]`, tag 0 = JSON control, tag 1 = `[id BE][bytes]`. Partial/split-read + oversized-frame + unknown-tag covered by tests (spec §17 "fuzz framing partial/split reads"). ✅
- `hub-pty` — Task 5 implements `PtySize`, `Pty`, `PtyOutput{rx,exit_rx}`, `spawn/write/resize/child_pid/kill`; reader thread bridges blocking reads → mpsc; waiter thread → `exit_rx`. Tests: echo output (spec §17 "spawn echo assert output"), `stty size` after resize (spec §17), child-death fires (spec §17). ✅
- `hub-term` — Task 4 implements `Screen::new/feed/resize/replay_bytes`, scrollback cap via `Parser::new(rows,cols,scrollback)`. Tests: known text → replay, ring behavior at cap (spec §17 "ring wraps at buffer cap"). ✅
- `hub-transport` — Task 6 implements `FramedConn::new/read_frame/write_frame`, `bind_listener` (0700 dir + 0600 sock), `connect`. Tests: two-proc-style UDS round trip, perms 0600/0700, queued-frames-don't-stall (spec §17 "two procs over UDS; perms 0600; slow reader doesn't stall writer"). ✅
- Workspace scaffold — Task 1 pins `[workspace.dependencies]` exactly as the Contract lists; edition 2021; 4 members only (no binaries, per scope). ✅

**2. Placeholder scan** — No `TODO`/`TBD`/"add error handling"/"similar to Task N"/"handle edge cases" remain. Every code step shows complete compilable Rust. The three `⚠️ executor` notes are targeted verification pointers (vt100 `set_size`/`contents_formatted`, portable-pty `exit_code`/`kill` signal + `&self` clone-reader), each with a concrete fallback — not hand-waves. ✅

**3. Type consistency vs Contract** — Cross-checked names/signatures used across tasks against the Contract:
- `SessionId(pub u64)`, `Origin::{External,Hub}`, `SessionInfo` field set/order, all `ControlMsg` variant fields (`Open` has 8 fields incl. `origin`/`title`; `Closed` has `exit_code: Option<i32>`) — match. ✅
- `Frame::{Control(ControlMsg), Data{id:SessionId, bytes:Vec<u8>}}` — matches; used identically in `hub-transport` tests. ✅
- `PtyOutput.rx: Receiver<Vec<u8>>`, `PtyOutput.exit_rx: Receiver<Option<i32>>`; `spawn` returns `(Pty, PtyOutput)` — match. ✅
- `Screen::new(rows, cols, scrollback)` param order matches Contract (rows, cols) — note vt100's own `Parser::new` is also `(rows, cols, scrollback_len)`, consistent. ✅
- `bind_listener`/`connect` take `&std::path::Path`; `FramedConn::write_frame` takes pre-encoded `&[u8]` — match. ✅

**Fixes applied during review:** tightened `hub-term` scrollback assertion to avoid a false-negative on `contents_formatted` (renders only the visible grid), and standardized every Checkpoint on `cargo test -p <crate>` with an added workspace-wide run at the end of Task 6. No unresolved gaps.

**Task count:** 6 tasks.
