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

mod bus_monitor;
mod dbc;
mod diagnose;
mod flash;
mod live_data;
mod swd;

use can_flasher::cli::adapters::{collect_report, AdapterReport};

// ---- Tauri commands ----

/// Returns the version of the bundled `can-flasher` crate. Useful
/// for the UI's About panel and as a sanity check that the path
/// dependency resolved correctly.
#[tauri::command]
fn can_flasher_version() -> &'static str {
    // Studio + can-flasher ship in lockstep — `release.yml`'s
    // verify-version gate proves the workspace's Cargo.toml and
    // the studio's Cargo.toml carry the same version on any
    // tagged build. Returning our own `CARGO_PKG_VERSION`
    // therefore matches the bundled library version without a
    // hardcoded string that goes stale (the previous "1.2.0"
    // outlived three releases).
    env!("CARGO_PKG_VERSION")
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
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .manage(live_data::LiveDataState::default())
        .manage(bus_monitor::BusMonitorState::default())
        .manage(dbc::DbcState::default())
        .invoke_handler(tauri::generate_handler![
            can_flasher_version,
            discover_adapters,
            flash::flash,
            flash::build_only,
            flash::read_cmake_presets,
            diagnose::health,
            diagnose::read_dtcs,
            diagnose::clear_dtcs,
            live_data::live_data_start,
            live_data::live_data_stop,
            bus_monitor::bus_monitor_start,
            bus_monitor::bus_monitor_stop,
            bus_monitor::bus_monitor_capture_start,
            bus_monitor::bus_monitor_capture_stop,
            dbc::dbc_load,
            dbc::dbc_unload,
            dbc::dbc_status,
            dbc::dbc_signals,
            swd::swd_list_probes,
            swd::swd_flash,
            swd::swd_fetch_bootloader,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the ISC CAN Studio Tauri app");
}
