use hub_cli::kill;
use hub_proto::{ControlMsg, Frame, SessionId};
use hub_transport::bind_listener;

#[tokio::test]
async fn kill_sends_kill_and_accepts_closed_ack() {
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
        // daemon_client::kill_session does Attach→Kill handshake (see daemon_client.rs)
        match conn.read_frame().await.unwrap() {
            Frame::Control(ControlMsg::Attach { id }) => assert_eq!(id, SessionId(9)),
            other => panic!("expected Attach, got {other:?}"),
        }
        match conn.read_frame().await.unwrap() {
            Frame::Control(ControlMsg::Kill { id }) => assert_eq!(id, SessionId(9)),
            other => panic!("expected Kill, got {other:?}"),
        }
        conn.write_frame(&hub_proto::encode_control(&ControlMsg::Closed {
            id: SessionId(9),
            exit_code: Some(0),
        }))
        .await
        .unwrap();
    });

    // kill::run resolves the sock from HOME; point HOME at our tempdir.
    std::env::set_var("HUB_SOCK", &sock);
    kill::run(dir.path(), 9).await.unwrap();
    std::env::remove_var("HUB_SOCK");
    server.await.unwrap();
}
