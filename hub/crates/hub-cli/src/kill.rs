use crate::{daemon_client, paths};
use hub_proto::SessionId;
use std::path::Path;

pub async fn run(home: &Path, id: u64) -> anyhow::Result<()> {
    let sock = paths::daemon_sock_path(home);
    daemon_client::kill_session(&sock, SessionId(id)).await?;
    println!("killed session {id}");
    Ok(())
}
