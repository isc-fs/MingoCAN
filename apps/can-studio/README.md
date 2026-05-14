# ISC CAN Studio

Desktop application for **flashing, monitoring, and debugging CAN messages** on
the ISC Racing Team's Formula Student ECUs. The CLI ([`can-flasher`](../../README.md))
covers power-user / CI workflows; the [VS Code extension](../../editor/vscode/)
covers in-editor flashing for developers; this app is the surface for everyone
else — mechanics at a workbench, hardware engineers at a test bench, race-day
operators in the pit.

**Status: v0.1 — Phase 0 complete.** All four workflow surfaces (Adapters,
Flash, Diagnostics, Live data) plus a dedicated Settings view drive real
`can-flasher` functionality. Selection + per-view config persists across
restarts. Native file pickers wired in.

## Architecture

```
┌───────────────────────────────────────────────┐
│  Tauri 2 native window (Mac / Linux / Win)    │
│  ┌─────────────────────┐                      │
│  │ Svelte 5 frontend   │  ← src/, index.html  │
│  │ (TypeScript + Vite) │                      │
│  └──────────┬──────────┘                      │
│             │ tauri.invoke(…)                 │
│  ┌──────────▼────────────────────────────┐    │
│  │ Rust backend (src-tauri/)             │    │
│  │ ├─ #[tauri::command] surface          │    │
│  │ └─ embeds can-flasher crate by path   │    │
│  │    → protocol/, transport/, flash/    │    │
│  └───────────────────────────────────────┘    │
└───────────────────────────────────────────────┘
```

Same Rust on both sides of the IPC bridge — no shell-out tax, the bootloader
protocol code is reused directly. When a new adapter or a new opcode lands in
`can-flasher`, the Studio picks it up by a Cargo bump.

## Tier roadmap

Same shape as the VS Code extension's evolution.

| Tier | Surface | Status |
|---|---|---|
| **0** | Adapters / Flash / Diagnostics / Live-data, persistent settings, native file pickers, Settings view | ✅ live (v0.1.0) |
| **1** | Generic CAN bus monitor — live frame list, filter by ID, per-ID rate, pause | ✅ live (v0.2.0); capture-to-file deferred to v0.2.1 |
| **2** | DBC file support — decoded signal column, dedicated Signals view, signal-trigger expressions | 🔜 |
| **3** | Frame transmitter — single-shot + cyclic + signal-triggered sends | 🔜 |
| **4** | Record / replay sessions (candump format), multi-channel scope-style charts | 🔜 |

Tier 0 wraps existing CLI capability. Tier 1 is the inflection point where this
becomes a real CAN tool — it needs a generic bus-monitor module in the Rust
core (new crate or new module under `can-flasher`).

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
                           # src-tauri/target/release/bundle/
```

Outputs:
- macOS: `bundle/macos/ISC CAN Studio.app` and `bundle/dmg/*.dmg`
- Linux: `bundle/deb/*.deb`, `bundle/appimage/*.AppImage`, `bundle/rpm/*.rpm`
- Windows: `bundle/msi/*.msi`, `bundle/nsis/*.exe`

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
xattr -dr com.apple.quarantine "/Applications/ISC CAN Studio.app"
```

The proper long-term fix is signing with an Apple Developer ID + notarising;
that's deferred until the friction warrants the $99/year + setup time.

## Releasing

Official native bundles are produced by the [`ISC CAN Studio release`](../../.github/workflows/can-studio-release.yml)
GitHub Actions workflow. To cut a new release:

1. Bump `version` in all three places (kept in lockstep by the
   `verify-version` gate):
   - [`apps/can-studio/src-tauri/Cargo.toml`](src-tauri/Cargo.toml)
   - [`apps/can-studio/package.json`](package.json)
   - [`apps/can-studio/src-tauri/tauri.conf.json`](src-tauri/tauri.conf.json)
2. Land the bump through the normal feat-branch + PR flow.
3. Push a tag of the form `can-studio-vX.Y.Z` matching the bumped version:
   ```bash
   git tag can-studio-v0.1.0
   git push origin can-studio-v0.1.0
   ```
4. The workflow's three matrix jobs build natively on Ubuntu, macOS, and
   Windows, then each one attaches its platform-specific bundles to a single
   GitHub Release tagged `can-studio-v…`. Bundles produced:
   - macOS: `.dmg` + `.app` (inside the dmg)
   - Linux: `.deb` + `.AppImage`
   - Windows: `.msi` (preferred) + `.exe` installer
5. Team members install from the Release page.

Manual dispatch (`Run workflow` button on the Actions UI) builds the bundles
as workflow artifacts without creating a Release — useful for testing a
build before tagging.

### Tag-space separation

| Tag pattern | Workflow | Produces |
|---|---|---|
| `v*` | [release.yml](../../.github/workflows/release.yml) | Rust `can-flasher` binaries |
| `editor-v*` | [editor-release.yml](../../.github/workflows/editor-release.yml) | VS Code extension `.vsix` |
| `can-studio-v*` | [can-studio-release.yml](../../.github/workflows/can-studio-release.yml) | ISC CAN Studio native bundles |

None of the three trigger the others. A CLI release, an extension release,
and a Studio release can all ship on the same day without interfering.

## Repository layout

```
apps/can-studio/
├── README.md                  ← you are here
├── package.json               ← frontend tooling + tauri CLI
├── tsconfig.json
├── vite.config.ts
├── svelte.config.js
├── index.html                 ← Vite entry
├── public/icon.png            ← static asset served by Vite
├── src/                       ← Svelte 5 frontend
│   ├── main.ts
│   ├── App.svelte
│   └── app.css
└── src-tauri/                 ← Rust backend (Tauri 2)
    ├── Cargo.toml             ← member of the root workspace
    ├── tauri.conf.json
    ├── build.rs
    ├── icons/icon.png
    └── src/
        ├── main.rs            ← binary entry; calls into lib.rs
        └── lib.rs             ← #[tauri::command] surface
```

## License

MIT — see [LICENSE](../../LICENSE) at the repo root.
