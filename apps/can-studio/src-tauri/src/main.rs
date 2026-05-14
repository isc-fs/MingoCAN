// ISC CAN Studio — Tauri 2 application entry point.
//
// This is the scaffold: window comes up, frontend loads, one trivial
// `#[tauri::command]` proves the Rust ↔ JS bridge works end-to-end.
// Tier 0 (flash + DTC + health) wraps the existing `can-flasher`
// crate paths; Tier 1 (bus monitor) adds a new module here.

// On Windows the framework expects a Windows GUI subsystem binary;
// the cfg-attr below suppresses the console window in release
// builds. macOS / Linux ignore it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // tracing-subscriber bootstrap matches the convention used by
    // the can-flasher CLI. RUST_LOG=can_studio=debug works as
    // expected. Defaults to INFO when no env var is set.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    can_studio::run();
}
