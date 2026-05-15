# Changelog

All notable changes to the ISC STM32 CAN Flasher VS Code extension.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
