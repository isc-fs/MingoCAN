# Changelog

All notable changes to the ISC STM32 CAN Flasher VS Code extension.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] — 2026-05-12

### Added
- Team-supplied extension icon (yellow ISC shield + key + subtle circuit motif).
- `LICENSE` (MIT) inside the extension package — silences the `vsce` warning
  and surfaces a License tab on the Marketplace listing.
- This `CHANGELOG.md`.

### Changed
- README rewritten for the Marketplace listing surface: consumer-facing intro,
  install + first-run, command and settings references, with contributor /
  development notes moved toward the bottom.
- Manifest metadata expanded (description, categories, keywords, `bugs` /
  `homepage` / `qna` URLs) for better Marketplace discoverability.

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
