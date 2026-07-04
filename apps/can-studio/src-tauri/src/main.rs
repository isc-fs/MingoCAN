// ISC MingoCAN — Tauri 2 application entry point.
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
    // If launched as the hidden out-of-process CAN backend helper
    // (`__can-host`), run the stdio bridge and exit before doing any
    // GUI/tracing setup. The desktop binary spawns *itself* as the
    // helper (`current_exe()` is `can-studio`, not `can-flasher`), so
    // this guard must live here too — otherwise a driver crash on the
    // helper side would take the whole app down, defeating isolation.
    if can_flasher::transport::isolation::maybe_run_as_host() {
        return;
    }

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
