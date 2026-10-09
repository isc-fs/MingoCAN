# ISC MingoCAN

Desktop application for **flashing, monitoring, and debugging CAN messages** on
the ISC Racing Team's Formula Student ECUs. The CLI ([`can-flasher`](../../README.md))
covers power-user / CI workflows; the [VS Code extension](../../editor/vscode/)
covers in-editor flashing for developers; this app is the surface for everyone
else — mechanics at a workbench, hardware engineers at a test bench, race-day
operators in the pit.

Ships in lockstep with the CLI and VS Code extension (same version). Eight
views: Adapters; Program → Flash, Burn bootloader; Observe → Board health, Bus
monitor (Signals / By ID / Live frames), Telemetry, Data logs; Settings.
Operator docs: [docs/DESKTOP.md](../../docs/DESKTOP.md).

## Architecture

```mermaid
flowchart TB
    subgraph window["Tauri 2 native window — macOS / Linux / Windows"]
        direction TB
        fe["Svelte 5 frontend<br/>(TypeScript + Vite)<br/>src/, index.html"]
        be["Rust backend<br/>src-tauri/<br/>#[tauri::command] surface"]
        fe -- "tauri.invoke(…)" --> be
        be -- "emit(event, payload)" --> fe
    end
    be --> crate["can-flasher crate<br/>(path dependency)<br/>protocol · transport · flash · firmware · session"]
```

Same Rust on both sides of the IPC bridge — no shell-out tax, the bootloader
protocol code is reused directly. When a new adapter or a new opcode lands in
`can-flasher`, Studio picks it up by a Cargo bump.

## Why Tauri

- Reuses the existing `can-flasher` Rust crates **by path dependency** — no
  shell-out, no JSON parsing of CLI output. Same wire-format code in CLI and
  app, can't drift.
- Native binaries on Mac, Linux, and Windows from one codebase (~10 MB each).
- Web frontend (Svelte 5 + Vite) iterates UI fast without learning a separate
  GUI toolkit.

## Why Svelte 5

- Smallest runtime among modern frameworks. Tauri's own examples lean Svelte.
- Component model is simple enough that anyone on the team can learn it.
- Reactive runes (`$state`, `$derived`, `$effect`) compose cleanly without the
  hooks dance.

## Development

### Prerequisites

- **Node 20+** and **npm** (for the frontend toolchain)
- **Rust 1.95+** with the `rustup` standard target — same toolchain as
  `can-flasher`
- Platform native deps for Tauri (Webkit/GTK on Linux, Xcode CLT on macOS,
  WebView2 on Windows). See <https://tauri.app/start/prerequisites/>.

### Dev loop

```bash
cd apps/can-studio
npm install                # one-time
npm run tauri:dev          # opens the dev window, HMR for the frontend,
                           # cargo-watch for the Rust side
```

### Release build

```bash
npm run tauri:build        # produces a platform-native bundle in
                           # <repo root>/target/release/bundle/
```

Outputs:
- macOS: `bundle/macos/ISC MingoCAN.app` and `bundle/dmg/*.dmg`
- Linux: `bundle/deb/*.deb`, `bundle/appimage/*.AppImage`, `bundle/rpm/*.rpm`
- Windows: `bundle/nsis/*.exe`

### Icon generation

The committed `src-tauri/icons/icon.png` is the source. **The Tauri build
script requires `icons/icon.ico` on Windows even for `cargo check`** — so
run the icon generator once after your first `npm install`:

```bash
npx tauri icon src-tauri/icons/icon.png
```

That produces `icon.ico` / `icon.icns` / `32x32.png` / `128x128.png` /
`128x128@2x.png` and a handful of store-metadata PNGs alongside the source.
All of those are `.gitignore`d so dev machines and CI regenerate them on
demand.

CI runs this step automatically before `cargo check` so the workflow is
self-contained.

## macOS Gatekeeper note

The macOS bundles are **ad-hoc signed** (`bundle.macOS.signingIdentity: "-"` in
`tauri.conf.json`) but not notarised through Apple — the team isn't paying for
the Developer Program. On first launch macOS Gatekeeper shows
*"… developer cannot be verified"*; the operator opens the app in `Applications`
via **right-click → Open → confirm** and subsequent launches work normally.

If Gatekeeper instead says *"… is damaged and can't be opened"* (typically
caused by a stale download or by an older bundle that pre-dates the ad-hoc
signing), strip the quarantine attribute manually:

```bash
xattr -dr com.apple.quarantine "/Applications/ISC MingoCAN.app"
```

The proper long-term fix is signing with an Apple Developer ID + notarising;
that's deferred until the friction warrants the $99/year + setup time.

## Releasing

From v2.0.0 onward Studio ships in lockstep with the CLI and the VS Code
extension under a single unified release. One `v*` tag triggers the
consolidated [`release.yml`](../../.github/workflows/release.yml)
workflow which builds all three surfaces in parallel and attaches every
artefact to one GitHub Release page.

Studio's contribution to the release: a 3-platform matrix that produces
the native bundles per OS — `.dmg` + `.app.tar.gz` (macOS), `.deb` +
`.AppImage` + `.rpm` (Linux), `.exe` (NSIS, Windows). The verify-version
gate at the start of the workflow checks Studio's three version-of-truth
files (`src-tauri/Cargo.toml`, `package.json`, `src-tauri/tauri.conf.json`)
against the pushed tag alongside the CLI's `Cargo.toml` and the VS Code
extension's `package.json` — any mismatch fails the gate before any build
runs.

Manual dispatch (`Run workflow` button on the Actions UI) builds the bundles
as workflow artifacts without creating a Release — useful for testing a
build before tagging.

See [docs/CONTRIBUTING.md § Cutting a release](../../docs/CONTRIBUTING.md#cutting-a-release)
for the full step-by-step.

## Repository layout

```mermaid
mindmap
  root((apps/can-studio))
    Top-level
      README.md
      package.json · frontend tooling + tauri CLI
      tsconfig.json
      vite.config.ts
      svelte.config.js
      index.html · Vite entry
      public/icon.png · static asset
    src/ — Svelte 5 frontend
      main.ts · app bootstrap
      App.svelte · root layout + routing
      app.css · global styles
      lib/
        Sidebar.svelte
        AdaptersView.svelte
        FlashView.svelte
        SwdFlashView.svelte
        DiagnosticsView.svelte
        BusMonitorView.svelte
        PitDiagView.svelte
        DataLogsView.svelte
        SettingsView.svelte
        AdapterStatusBar.svelte
        NodeIdRolePicker.svelte
        UpdateBanner.svelte
        PlaceholderView.svelte
        settings.svelte.ts · persistent store + autosave
        flash.ts · flash command wrapper
        diagnose.ts · health / DTC wrappers
        bus_monitor.ts · bus monitor + capture wrappers
        dbc.ts · DBC load + status + signals
        pit_diag.ts · telemetry
        logs.ts · data logs
        swd.ts · SWD burn
        provision.ts · node-id roles
        updater.ts · self-update
        stores.ts · ViewId + VIEWS
        cli.ts / types.ts
    src-tauri/ — Rust backend (Tauri 2)
      Cargo.toml · workspace member
      tauri.conf.json · bundle config
      build.rs · tauri-build hook
      icons/ · generated from icon.png
      src/
        main.rs · binary entry
        lib.rs · plugin + state registration
        flash.rs · flash + build-only commands
        diagnose.rs · health + DTC commands
        pit_diag.rs · telemetry
        logs.rs · LOGFS pull + decode
        swd.rs · probe-rs burn/erase
        provision.rs · node-id provisioning
        bus_monitor.rs · promiscuous capture + candump
        dbc.rs · can-dbc parse + bit decoder
```

## License

MIT — declared in the crate manifests (`license = "MIT"`).
