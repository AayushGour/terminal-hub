//! Focus-follows-size debounce (spec §7): coalesce a burst of size claims,
//! wait ~50ms of quiescence, then apply the newest ONLY if it changed.

use crate::relay::RelayEvent;
use std::time::Duration;
use tokio::sync::mpsc;

pub fn spawn_coalescer(cur_cols: u16, cur_rows: u16, out: mpsc::UnboundedSender<RelayEvent>) -> mpsc::UnboundedSender<(u16, u16)> {
    let (tx, mut rx) = mpsc::unbounded_channel::<(u16, u16)>();
    let (mut ccols, mut crows) = (cur_cols, cur_rows);
    tokio::spawn(async move {
        while let Some(mut latest) = rx.recv().await {
            // Keep collecting for 50ms of quiescence.
            loop {
                match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
                    Ok(Some(newer)) => latest = newer,
                    Ok(None) | Err(_) => break,
                }
            }
            if latest.0 != ccols || latest.1 != crows {
                ccols = latest.0; crows = latest.1;
                if out.send(RelayEvent::ApplyResize(latest.0, latest.1)).is_err() { break; }
            }
        }
    });
    tx
}
