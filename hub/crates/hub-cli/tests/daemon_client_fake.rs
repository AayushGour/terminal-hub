use hub_cli::daemon_client::list_sessions;
use hub_proto::{ControlMsg, Frame, Origin, SessionId, SessionInfo};
use hub_transport::bind_listener;

#[tokio::test]
async fn list_sessions_roundtrips_against_fake_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("hubd.sock");

    let listener = bind_listener(&sock).await.unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut conn = hub_transport::FramedConn::new(stream);
        // F1: the client sends `Hello { token }` as its first frame; consume it.
        match conn.read_frame().await.unwrap() {
            Frame::Control(ControlMsg::Hello { .. }) => {}
            other => panic!("expected Hello first, got {other:?}"),
        }
        // Then expect a List frame.
        match conn.read_frame().await.unwrap() {
            Frame::Control(ControlMsg::List) => {}
            other => panic!("expected List, got {other:?}"),
        }
        let sessions = vec![SessionInfo {
            id: SessionId(7),
            origin: Origin::External,
            title: "vscode".into(),
            pid: 4321,
            started_unix: 1,
            cols: 100,
            rows: 30,
        }];
        conn.write_frame(&hub_proto::encode_control(&ControlMsg::Sessions { sessions }))
            .await
            .unwrap();
    });

    let got = list_sessions(&sock).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, SessionId(7));
    server.await.unwrap();
}
