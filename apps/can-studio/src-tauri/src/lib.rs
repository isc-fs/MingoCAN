// ISC CAN Studio — Tauri command surface.
//
// `main.rs` is the binary entry point; the actual app body lives
// here so that integration tests in `tests/` can reach the same
// module tree without going through the Tauri runtime's
// `generate_context!` macro (which only works once per process).
//
// Tier 0 will add `flash`, `discover_adapters`, `health`, `dtc_read`,
// `dtc_clear` — each one a thin `#[tauri::command]` that calls into
// `can_flasher`'s existing modules. Tier 1 adds a `bus_monitor` event
// stream backed by `transport::CanBackend` in promiscuous mode.

// ---- Tauri commands ----

/// Smoke-test command — proves the Rust ↔ JS bridge is wired up.
/// Removed once Tier 0's real commands land.
#[tauri::command]
fn studio_version() -> String {
    format!("ISC CAN Studio v{} (scaffold)", env!("CARGO_PKG_VERSION"))
}

/// Returns the version of the bundled `can-flasher` crate. Useful
/// for the UI's About panel and as a sanity check that the path
/// dependency resolved correctly.
#[tauri::command]
fn can_flasher_version() -> &'static str {
    // The library crate doesn't currently re-export this, so we
    // inline a constant matching the workspace's Cargo.toml. When
    // Tier 0 lands we'll move this into `can_flasher` itself.
    "1.2.0"
}

// ---- Public app entry ----

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            studio_version,
            can_flasher_version
        ])
        .run(tauri::generate_context!())
        .expect("error while running the ISC CAN Studio Tauri app");
}
