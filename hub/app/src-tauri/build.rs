fn main() {
    // Task 3: register the viewer command surface with the ACL so
    // `capabilities/default.json` can grant them to the webview. Without
    // this, `tauri_build::build()`'s default (empty) `AppManifest` means no
    // permission identifiers exist for our own `#[tauri::command]`s, and
    // referencing e.g. "allow-list-sessions" in a capability would fail
    // build-time capability validation.
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "list_sessions",
            "attach",
            "detach",
            "send_input",
            "resize",
            "claim_size",
            "kill",
            "spawn_session",
            "reconcile_sessions",
            "get_buffer_size",
            "set_buffer_size",
            "get_setup_declined",
            "set_setup_declined",
            "hub_is_installed",
            "hub_do_install",
            "hub_do_uninstall",
        ])),
    )
    .expect("failed to run tauri-build");
}
