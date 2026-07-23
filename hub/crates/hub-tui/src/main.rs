use hub_proto::{Frame, ControlMsg, SessionId};
use hub_relay::paths::HubPaths;
use hub_tui::ViewerClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut sock: Option<String> = None;
    let mut id: Option<u64> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--sock" => sock = args.next(),
            s => id = s.parse().ok(),
        }
    }
    let id = SessionId(id.expect("usage: hub-tui [--sock PATH] <session_id>"));
    let paths = HubPaths::from_env();
    let dsock = sock.map(std::path::PathBuf::from).unwrap_or_else(|| paths.daemon_sock());

    let mut vc = ViewerClient::connect(&dsock, id).await?;

    // Forward stdin -> Input.
    let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut stdin = tokio::io::stdin();
        let mut buf = vec![0u8; 8192];
        while let Ok(n) = stdin.read(&mut buf).await {
            if n == 0 { break; }
            if in_tx.send(buf[..n].to_vec()).is_err() { break; }
        }
    });

    let mut stdout = tokio::io::stdout();
    use tokio::io::AsyncWriteExt;
    loop {
        tokio::select! {
            frame = vc.recv() => match frame? {
                Some(Frame::Control(ControlMsg::Replay { screen, .. })) => { stdout.write_all(&screen).await?; stdout.flush().await?; }
                Some(Frame::Data { bytes, .. }) => { stdout.write_all(&bytes).await?; stdout.flush().await?; }
                Some(Frame::Control(ControlMsg::Closed { .. })) | None => break,
                _ => {}
            },
            Some(bytes) = in_rx.recv() => vc.send_input(&bytes).await,
        }
    }
    Ok(())
}
