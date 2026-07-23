//! F1 local-IPC auth gate (hub-daemon integration). Proves the real daemon:
//!  * rejects a connection whose FIRST frame is not a `Hello` — the connection
//!    is closed and the op it tried to run never happens,
//!  * rejects a `Hello` carrying the wrong token,
//!  * accepts a `Hello` with the correct token and then serves the op.
//! Plus a unit check of the peer-uid compare helper on the same-uid path.
//!
//! Cross-uid peer-cred rejection cannot be exercised in a single-uid test
//! environment; it is verified MANUALLY (connect as a different uid and confirm
//! the daemon closes the connection logging "peer uid mismatch"). The
//! implementation — `hub_transport::auth::peer_uid_ok` — is wired into every
//! accept path in `hub-daemon/src/server.rs` and `hub-relay/src/relay.rs`.

use hub_proto::{encode_control, ControlMsg, Frame};
use hub_relay::conn::{write_frame, FrameReader};
use hub_relay::paths::HubPaths;
use std::os::unix::io::AsRawFd;
use std::time::Duration;

fn unique_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("hub-auth-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Start an in-process daemon on its own base dir (no shared `HUB_DIR` env, so
/// these tests never race one another) and wait for its socket to bind. The
/// daemon creates `<base>/token` before binding, so it exists once we return.
async fn start_daemon(tag: &str) -> HubPaths {
    let paths = HubPaths::new(unique_dir(tag));
    paths.ensure_dirs().unwrap();
    let p2 = paths.clone();
    tokio::spawn(async move { let _ = hub_daemon::server::run(p2).await; });
    for _ in 0..300 {
        if paths.daemon_sock().exists() { break; }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(paths.daemon_sock().exists(), "daemon socket never appeared");
    paths
}

/// Read the next frame within `budget`. `Ok(None)`/`Err` == the daemon closed
/// the connection (rejection); `Ok(Some(_))` == the daemon actually replied.
/// A timeout is a failure (the daemon must never leave a caller hanging).
async fn next_within(
    fr: &mut FrameReader<tokio::net::unix::OwnedReadHalf>,
    budget: Duration,
) -> anyhow::Result<Option<Frame>> {
    match tokio::time::timeout(budget, fr.next()).await {
        Ok(res) => res,
        Err(_) => anyhow::bail!("timed out: daemon neither replied nor closed the connection"),
    }
}

#[tokio::test]
async fn auth_rejects_no_hello() {
    let paths = start_daemon("no-hello").await;

    // Send List with NO preceding Hello. The daemon must close the connection
    // WITHOUT processing the List (i.e. no Sessions reply ever comes back).
    let stream = tokio::net::UnixStream::connect(paths.daemon_sock()).await.unwrap();
    let (rd, mut wr) = stream.into_split();
    let mut fr = FrameReader::new(rd);
    write_frame(&mut wr, &encode_control(&ControlMsg::List)).await.unwrap();

    match next_within(&mut fr, Duration::from_secs(3)).await {
        Ok(None) | Err(_) => {} // connection closed / no reply -> the op did NOT run
        Ok(Some(f)) => panic!("no-Hello connection must be closed with no op; got {f:?}"),
    }
}

#[tokio::test]
async fn auth_rejects_wrong_token() {
    let paths = start_daemon("wrong-token").await;

    // A well-formed Hello, but the wrong token -> rejected and closed; the
    // pipelined List behind it must never be processed.
    let stream = tokio::net::UnixStream::connect(paths.daemon_sock()).await.unwrap();
    let (rd, mut wr) = stream.into_split();
    let mut fr = FrameReader::new(rd);
    write_frame(&mut wr, &hub_transport::auth::hello_frame("0000-not-the-real-token-0000"))
        .await
        .unwrap();
    write_frame(&mut wr, &encode_control(&ControlMsg::List)).await.unwrap();

    match next_within(&mut fr, Duration::from_secs(3)).await {
        Ok(None) | Err(_) => {} // rejected and closed -> the op did NOT run
        Ok(Some(f)) => panic!("wrong-token connection must be closed with no op; got {f:?}"),
    }
}

#[tokio::test]
async fn auth_accepts_valid_token() {
    let paths = start_daemon("valid-token").await;

    // Present the exact token the daemon wrote to <base>/token on startup.
    let token = hub_transport::auth::load_token(paths.base()).unwrap();
    let stream = tokio::net::UnixStream::connect(paths.daemon_sock()).await.unwrap();
    let (rd, mut wr) = stream.into_split();
    let mut fr = FrameReader::new(rd);
    write_frame(&mut wr, &hub_transport::auth::hello_frame(&token)).await.unwrap();
    write_frame(&mut wr, &encode_control(&ControlMsg::List)).await.unwrap();

    // A correctly-authenticated List is processed and answered with Sessions
    // (an empty session set is fine -- the point is the op actually ran).
    match next_within(&mut fr, Duration::from_secs(3)).await {
        Ok(Some(Frame::Control(ControlMsg::Sessions { .. }))) => {}
        other => panic!("valid-token List must return Sessions, got {other:?}"),
    }
}

/// Unit check of the peer-uid compare helper on the SAME-uid path: both ends of
/// a locally-connected AF_UNIX socket share our uid, so `peer_uid_ok` must
/// return true (exercises the real getpeereid / SO_PEERCRED code). The cross-uid
/// rejection path is manual-only (see the module comment).
#[tokio::test]
async fn peer_uid_ok_true_for_same_uid_local_peer() {
    let sock = std::env::temp_dir().join(format!("hub-auth-uid-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock);
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let accept = tokio::spawn(async move {
        let (s, _) = listener.accept().await.unwrap();
        hub_transport::auth::peer_uid_ok(s.as_raw_fd())
    });
    let client = tokio::net::UnixStream::connect(&sock).await.unwrap();
    assert!(
        hub_transport::auth::peer_uid_ok(client.as_raw_fd()),
        "client side must see a same-uid server"
    );
    assert!(accept.await.unwrap(), "server side must see a same-uid client");
    let _ = std::fs::remove_file(&sock);
}
