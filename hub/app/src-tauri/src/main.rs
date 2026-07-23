// Prevents an extra console window on Windows in release (phase-2 seam).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    hub_app_lib::run();
}
