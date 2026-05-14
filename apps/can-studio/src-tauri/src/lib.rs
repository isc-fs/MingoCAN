// ISC CAN Studio — Tauri command surface.
//
// `main.rs` is the binary entry point; the actual app body lives
// here so that integration tests in `tests/` can reach the same
// module tree without going through the Tauri runtime's
// `generate_context!` macro (which only works once per process).
//
// Tier 0a-d (current): `discover_adapters` + `flash` (streamed) +
// `health` / `read_dtcs` / `clear_dtcs` + `live_data_start` /
// `live_data_stop` (streamed snapshots).

mod diagnose;
mod flash;
mod live_data;

use can_flasher::cli::adapters::{collect_report, AdapterReport};

// ---- Tauri commands ----

/// Returns the version of the bundled `can-flasher` crate. Useful
/// for the UI's About panel and as a sanity check that the path
/// dependency resolved correctly.
#[tauri::command]
fn can_flasher_version() -> &'static str {
    // Inlined to match the workspace's Cargo.toml; the library
    // crate doesn't currently re-export this. Move into
    // `can_flasher` proper once we add an About panel.
    "1.2.0"
}

/// Enumerate every CAN adapter the host can see — same data the
/// CLI's `can-flasher adapters --json` produces. `AdapterReport`
/// derives `Serialize`, so Tauri carries it across the IPC bridge
/// without any custom wrapping; the frontend types in
/// `src/lib/types.ts` mirror it field-for-field.
#[tauri::command]
fn discover_adapters() -> AdapterReport {
    collect_report()
}

// ---- Public app entry ----

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .manage(live_data::LiveDataState::default())
        .invoke_handler(tauri::generate_handler![
            can_flasher_version,
            discover_adapters,
            flash::flash,
            diagnose::health,
            diagnose::read_dtcs,
            diagnose::clear_dtcs,
            live_data::live_data_start,
            live_data::live_data_stop
        ])
        .run(tauri::generate_context!())
        .expect("error while running the ISC CAN Studio Tauri app");
}
