# Changelog

All notable changes to the ISC MingoCAN Flasher VS Code extension.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **`iscFs.buildCommand` default now configures CMake too** —
  `cmake -B build -S . && cmake --build build` instead of just
  `cmake --build build`. Matches STM32CubeIDE's *build then
  flash* button: clicking Flash on a fresh clone now Just Works
  without manually running CMake configure first. The configure
  step is a no-op on subsequent runs when `build/` already
  exists, so there's no time penalty.
- **`iscFs.firmwareArtifact` now defaults to `**/build/**/*.{elf,hex,bin}`.**
  New installs no longer need to set it manually for the common
  CMake / Make project layout — the extension auto-discovers the
  firmware after a build. Operators with a non-standard build
  output still set the path explicitly; the existing
  multi-match quick-pick prompt is unchanged.

### Changed
- **No-firmware-artifact error is more actionable.** When the
  glob matches nothing, the warning now offers two buttons:
  *Build first* (jumps to `iscFs.buildCommand` so you can
  confirm it before re-running Flash) and *Set artifact path*
  (jumps to `iscFs.firmwareArtifact`). Previously the only
  action was *Open settings*.
- **All operator-facing strings now say "ISC MingoCAN:".** The
  rebrand from v2.3.1 missed many of the runtime toasts /
  progress titles / quick-pick prompts inside the extension
  source. Swept across `flash.ts`, `diagnose.ts`, `picker.ts`,
  `extension.ts` so what operators see matches the Extensions
  panel name.

- **Status-bar Flash button: lightning-bolt icon + "Build + Flash"
  label.** The button now reads `$(zap) Build + Flash` instead of
  `$(rocket) Flash`, making it visually unambiguous what the click
  does. Same `iscFs.flash` command (build → flash via CAN); only
  the surface changed.
- The tools dashboard's primary button gets the matching ⚡ glyph.

### Fixed
- **"Flash button didn't build" UX.** When `iscFs.buildCommand` is
  empty, the build step used to be silently skipped — operators
  saw the click as flash-only and assumed it wasn't building.
  Now the extension surfaces a warning toast with two actions:
  *Set build command* (jumps to settings) or *Continue (flash
  only)* (the previous behaviour). `iscFs.flashWithoutBuild` is
  unaffected.

## [2.3.2] — 2026-05-16

### Added
- **Status-bar shortcuts** — a `$(rocket) Flash` button and a
  `$(tools) Tools` button render next to the existing adapter
  pill at the bottom of the window. One click runs the same
  `iscFs.flash` / `iscFs.openTools` commands the palette
  exposes — no more `Cmd+Shift+P` round-trip for the two
  most-used actions.
- **Tools dashboard** — a dedicated webview panel
  (`iscFs.openTools`, also reachable from the status bar)
  groups every operator-facing action into one screen:
  Flash, Devices, Diagnostics, plus a live adapter pill. The
  command palette still works; the panel is the
  point-and-click alternative for operators who'd rather not
  memorise command names.

## [2.3.1] — 2026-05-16

### Changed
- **Renamed to "ISC MingoCAN Flasher"** to match the desktop app
  rebrand from v2.3.0. The internal extension ID
  (`isc-fs.vscode-stm32-can`) stays unchanged so existing installs
  pick up the rename in place. Command-palette category is
  shortened to `ISC MingoCAN`, the device tree view becomes
  `ISC MingoCAN Devices`, and the output channel becomes
  `View → Output → ISC MingoCAN`.

### Fixed
- **Adapters never populated when VS Code was launched from
  Finder / Dock / launchd on macOS.** The default
  `iscFs.canFlasherPath` setting was the bare string
  `"can-flasher"`, which `child_process.spawn` resolves through
  launchd's reduced `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`) — so
  binaries installed under `~/.local/bin`, `~/.cargo/bin`, or
  `/opt/homebrew/bin` weren't found. The extension now probes a
  curated list of well-known install directories whenever the
  setting is at its default, and falls back to the bare name
  only if nothing's found. Operator-customised paths are still
  honoured verbatim.

### Added
- One-shot notification when the CLI binary can't be spawned
  (typically because it isn't installed). Buttons jump straight
  to the latest release page or open the `iscFs.canFlasherPath`
  setting.

## [2.0.0] — 2026-05-15 — Unified release model

### Changed
- **Versioning unified across all three product surfaces.** The
  CLI, this extension, and ISC CAN Studio now ship at the same
  version from a single `v*` tag (e.g. `v2.0.0`). The previous
  `editor-v*` and `can-studio-v*` tag namespaces are retired —
  see [`docs/CONTRIBUTING.md § Cutting a release`](https://github.com/isc-fs/can-flasher/blob/v2.0.0/docs/CONTRIBUTING.md#cutting-a-release).
  Jumps the extension's SemVer from 0.1.x straight to 2.0.0 to
  match the CLI; functionally equivalent to 0.1.4. Zero code
  changes in the extension itself for this release.
- Distribution channel unchanged: GitHub Releases (not the VS
  Code Marketplace).

## [0.1.4] — 2026-05-15

### Changed
- Co-release with `can-flasher` v1.3.1, which adds a per-frame
  SocketCAN `trace!` log (and a permanent ISO-TP regression suite
  against bootloader v1.2.0 strictness). The extension still shells
  out to the CLI binary, so the new trace is available end-to-end
  the moment operators upgrade their installed CLI to v1.3.1+.
  No extension-side code changes were needed.

## [0.1.3] — 2026-05-15

### Changed
- Co-release with `can-flasher` v1.3.0, which adds the
  `BL_CMD_NVM_FORMAT` opcode to track bootloader protocol v0.2.
  The extension still shells out to the CLI binary, so the new
  `config nvm format --yes` subcommand is available end-to-end
  the moment operators upgrade their installed CLI to v1.3.0+.
  No extension-side code changes were needed.

## [0.1.2] — 2026-05-14

### Changed
- Refreshed brand logo (extension icon + media assets) to match the
  ISC team's updated mark. Same shield silhouette, cleaner type and
  contrast. Synchronised with `apps/can-studio` so all surfaces
  share the icon.

## [0.1.1] — 2026-05-12

### Added
- Team-supplied extension icon (yellow ISC shield + key + subtle circuit motif).
- `LICENSE` (MIT) inside the extension package — silences the `vsce` warning
  on package build.
- This `CHANGELOG.md`.

### Changed
- README rewritten: consumer-facing intro, install + first-run, command and
  settings references, with contributor / development notes moved to the
  parent-repo `docs/CONTRIBUTING.md`. The text reads well both inside VS
  Code's Extensions pane and on the GitHub Release page.
- Manifest metadata expanded (description, categories, keywords, `bugs` /
  `homepage` / `qna` URLs). Most of these only matter for the Marketplace
  surface but are harmless for sideload too.

### Decided not to do
- **VS Code Marketplace publish.** The Azure DevOps account + PAT + scope
  dance is more setup than the team-internal use case warrants. Distribution
  stays on GitHub Releases. Manifest still carries Marketplace-friendly
  metadata so the decision is reversible without changes; just add a
  `VSCE_PAT` secret and the publish step back to the release workflow.

## [0.1.0] — 2026-05-12 — First internal release

First .vsix shipped via GitHub Releases (`editor-v0.1.0`). Feature-complete
v0 roadmap:

### Added
- **Tier A — Build + Flash**
  - `ISC CAN: Build & Flash firmware` — runs `iscFs.buildCommand`, resolves
    the firmware artifact, spawns `can-flasher flash --json`, parses the
    per-line event stream into a phase-aware progress notification.
  - `ISC CAN: Flash firmware (skip build)` — same, minus the build step.
  - Exit-code-aware error toasts mapping to the `can-flasher` REQUIREMENTS.md
    exit-code table.
- **Tier B — Device awareness**
  - `ISC CAN Devices` tree view in the Explorer pane: hierarchical
    Adapter → Devices listing, lazy first-refresh, manual refresh
    only (no background polling).
  - `ISC CAN: Discover devices on bus`, `ISC CAN: Refresh device list`.
  - `ISC CAN: Select CAN adapter…` quick-pick with a Workspace / User
    settings-scope sub-prompt.
  - Status-bar item showing current adapter selection; click to re-pick.
  - `ISC CAN: Flash this device…` right-click action that targets the
    selected node ID for one flash and restores the previous setting.
- **Tier C.1 — One-shot diagnostics**
  - `ISC CAN: Show session health` — column-aligned health record in
    the output channel.
  - `ISC CAN: Read DTCs` — severity-aware DTC table; toast severity
    matches the worst entry.
  - `ISC CAN: Clear DTCs` — modal confirmation then `clear-dtc --yes`.
- **Tier C.2 — Streaming live data**
  - `ISC CAN: Open live-data panel` — webview with a Chart.js sliding-window
    line chart (frames/sec RX + TX, computed as snapshot deltas) plus a
    row of state-pill indicators and a counter grid.
  - One panel per (interface, channel) pair; captures the adapter at
    creation time so a running stream stays locked to its board
    independent of later settings changes.
  - Theme reactivity: chart axis colours rebind on body-class change.

## [0.0.1] — 2026-05-12 — Sketch

Initial commit. Manifest complete, every command surfaced a
"not implemented" toast. Not released.
