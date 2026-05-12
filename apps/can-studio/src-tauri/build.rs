// Tauri build script. Generates the runtime asset manifest and wires
// the bundler resources from `tauri.conf.json` into the compiled
// binary. Required for Tauri 2; the framework looks for the
// generated assets at runtime via `tauri::generate_context!()`.

fn main() {
    tauri_build::build();
}
