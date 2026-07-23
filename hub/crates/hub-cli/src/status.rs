use crate::{daemon_client, paths, reconcile};
use std::path::Path;

pub async fn run(home: &Path) -> anyhow::Result<()> {
    let sock = paths::daemon_sock_path(home);
    let live = match daemon_client::list_sessions(&sock).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("hub: daemon unreachable ({e:#}); showing records only");
            Vec::new()
        }
    };
    let records = reconcile::scan_records(&paths::sessions_dir(home));
    let sock_alive = |p: &Path| std::os::unix::net::UnixStream::connect(p).is_ok();
    let b = reconcile::reconcile(&live, &records, &sock_alive);

    println!("HEALTHY ({}):", b.healthy.len());
    for s in &b.healthy {
        println!("  {:>4}  {:?}  {}", s.id.0, s.origin, s.title);
    }
    println!("GHOST (relay crashed, cleanup) ({}):", b.ghost.len());
    for r in &b.ghost {
        println!("  {:>4}  {:?}  {}", r.info.id.0, r.info.origin, r.info.title);
    }
    println!("ORPHAN (live, no record; adopt/kill) ({}):", b.orphan.len());
    for s in &b.orphan {
        println!("  {:>4}  {:?}  {}", s.id.0, s.origin, s.title);
    }
    Ok(())
}
