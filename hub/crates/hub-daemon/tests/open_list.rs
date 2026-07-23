use hub_proto::{encode_control, ControlMsg, Frame, Origin};
use hub_relay::conn::write_frame;
use hub_relay::paths::HubPaths;
use std::path::PathBuf;

fn tmp_hub_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("hubtest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[tokio::test]
async fn open_assigns_id_and_list_returns_session() {
    let paths = HubPaths::new(tmp_hub_dir());
    paths.ensure_dirs().unwrap();
    let p2 = paths.clone();
    tokio::spawn(async move { hub_daemon::server::run(p2).await.unwrap() });

    // Wait for the socket to exist.
    for _ in 0..100 {
        if paths.daemon_sock().exists() { break; }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Relay side: connect (F1 Hello first), send Open, expect Opened{id}.
    let (mut fr, mut wr) = hub_relay::conn::dial_hello(&paths.daemon_sock(), paths.base()).await.unwrap();
    let open = ControlMsg::Open {
        shell: "/bin/cat".into(), cwd: "/".into(),
        cols: 80, rows: 24, term: "xterm-256color".into(),
        origin: Origin::External, title: "t".into(),
    };
    write_frame(&mut wr, &encode_control(&open)).await.unwrap();
    let id = match fr.next().await.unwrap() {
        Some(Frame::Control(ControlMsg::Opened { id })) => id,
        other => panic!("expected Opened, got {other:?}"),
    };
    assert_eq!(id.0, 1);

    // Viewer side: connect (F1 Hello first), send List, expect the one session.
    let (mut vfr, mut vwr) = hub_relay::conn::dial_hello(&paths.daemon_sock(), paths.base()).await.unwrap();
    write_frame(&mut vwr, &encode_control(&ControlMsg::List)).await.unwrap();
    match vfr.next().await.unwrap() {
        Some(Frame::Control(ControlMsg::Sessions { sessions })) => {
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].id, id);
            assert_eq!(sessions[0].title, "t");
        }
        other => panic!("expected Sessions, got {other:?}"),
    }
}
