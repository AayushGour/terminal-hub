pub mod daemon; // Task 2 (reworked: per-tile ConnManager); pub for integration test
mod commands; // Task 3+
mod config; // Task 10
pub mod lifecycle; // feat: app-lifecycle install/uninstall (pub for integration test)
mod reconcile; // Task 4

use std::sync::Arc;

use commands::AppState;
use daemon::{AppSink, ConnManager};
use tauri::Manager;

// `$HUB_DIR` if set, else `$HOME/.hub`. Task 4 fix: this used to hard-code
// `$HOME/.hub` and ignore `$HUB_DIR`, while `daemon.rs`'s own (independently
// maintained) `hub_base_dir` -- used for the token lookup on the very same
// connection -- already honored it, same as `hub_relay::paths::HubPaths::
// from_env` and `hub_cli::paths::hub_dir`. Left un-fixed, a daemon/relay
// started under `HUB_DIR=<tmp>` (test harnesses, isolated dev runs) would
// have the app dial the wrong socket AND `reconcile_sessions` (Task 4) scan
// the wrong (or a real, unrelated) `sessions/` dir.
pub(crate) fn hub_home() -> std::path::PathBuf {
    if let Ok(d) = std::env::var("HUB_DIR") {
        return std::path::PathBuf::from(d);
    }
    let home = std::env::var("HOME").expect("HOME not set");
    std::path::Path::new(&home).join(".hub")
}

pub fn hub_home_sessions() -> std::path::PathBuf {
    hub_home().join("sessions")
}

pub fn run() {
    tauri::Builder::default()
        .manage(AppState { mgr: std::sync::Mutex::new(None) })
        .setup(|app| {
            let handle = app.handle().clone();
            let sock = hub_home().join("hubd.sock");
            // Construct the per-tile connection manager. No I/O here: it opens
            // one dedicated connection per tile lazily on `attach`, and the
            // daemon need not even be running yet — a tile's `attach` reports a
            // clean error and can retry once the daemon is up (Approach A: no
            // single app-wide connection to establish or lose).
            let sink: Arc<dyn daemon::EventSink> = Arc::new(AppSink(handle.clone()));
            let mgr = Arc::new(ConnManager::new(sock, sink));
            let state: tauri::State<AppState> = handle.state();
            *state.mgr.lock().unwrap() = Some(mgr);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_sessions,
            commands::attach,
            commands::detach,
            commands::send_input,
            commands::resize,
            commands::claim_size,
            commands::kill,
            commands::spawn_session,
            reconcile::reconcile_sessions,
            config::get_buffer_size,
            config::set_buffer_size,
            config::get_setup_declined,
            config::set_setup_declined,
            lifecycle::hub_is_installed,
            lifecycle::hub_do_install,
            lifecycle::hub_do_uninstall,
        ])
        .run(tauri::generate_context!())
        .expect("error while running hub app");
}
