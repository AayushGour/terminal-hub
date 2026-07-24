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
    /// Shell integration (OSC 7): the shell's current working directory.
    /// Empty until the relay's `ShellIntegration` scanner sees its first OSC
    /// 7 sequence (design spec 2026-07-23-shell-integration-design.md §5) --
    /// a shell that doesn't source the hook simply never updates this.
    pub cwd: String,
    /// Shell integration (OSC 133;D;<code>): the exit code of the most
    /// recently finished command. `None` until the first one is observed.
    pub last_exit_code: Option<i32>,
    /// Bumped on every `CommandFinished` shell-integration event (starts at
    /// 0). NOT bumped by a bare cwd change -- consumers diff `cwd` itself for
    /// that (spec §5/§6).
    pub activity_seq: u64,
}

/// Control-plane messages (serialized as JSON in a control frame).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ControlMsg {
    /// Local-IPC authentication handshake (F1). MUST be the FIRST frame a
    /// client sends on ANY hub socket (daemon `hubd.sock` OR a relay's
    /// per-session `<id>.sock`) before any session operation. Carries the
    /// per-install secret token from `<HUB_DIR>/token` (readable only by the
    /// owning uid, 0600). The server also verifies the peer's uid via
    /// SO_PEERCRED / getpeereid. A connection whose first frame is not a valid
    /// `Hello` with the correct token — or whose peer uid differs — is closed
    /// without processing anything. The token is NEVER logged.
    Hello {
        token: String,
    },
    // relay -> daemon
    // F5: deliberately NO `env` field. The daemon owns no pty and never reads
    // it, so shipping the shell's full environment (which can carry secrets
    // /tokens) over this socket would be pure exposure with no benefit. The
    // relay spawns its pty with the real process environment directly
    // (`RelayState::spawn_pty` / `Pty::spawn`), never via this message.
    Open {
        shell: String,
        cwd: String,
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
    /// relay -> daemon: pushed whenever the relay's `ShellIntegration`
    /// scanner (OSC 7 / OSC 133;D) reports a new event, over the relay's
    /// existing persistent control connection -- no new connection/socket.
    /// The daemon's `Registry::update_activity` mutates the in-memory
    /// `SessionInfo` for `id` (design spec 2026-07-23-shell-integration-design.md
    /// §5).
    SessionActivity {
        id: SessionId,
        cwd: String,
        last_exit_code: Option<i32>,
        activity_seq: u64,
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
    /// Graceful daemon-process stop (hub/CLI -> daemon). The daemon detaches
    /// from relays and exits the process; it MUST NOT kill relays (relays
    /// own the ptys and are the SPOF-surviving component by design).
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_msg_json_round_trips() {
        let msg = ControlMsg::Open {
            shell: "/bin/zsh".to_string(),
            cwd: "/home/u".to_string(),
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
            cwd: "/home/u/build".to_string(),
            last_exit_code: Some(0),
            activity_seq: 3,
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, back);

        let mut set = HashSet::new();
        set.insert(SessionId(7));
        assert!(set.contains(&SessionId(7)));
        assert!(!set.contains(&SessionId(8)));
    }

    #[test]
    fn session_activity_msg_json_round_trips() {
        let msg = ControlMsg::SessionActivity {
            id: SessionId(3),
            cwd: "/srv/app".to_string(),
            last_exit_code: Some(127),
            activity_seq: 5,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ControlMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }
}
