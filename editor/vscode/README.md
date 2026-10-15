# ISC MingoCAN Flasher

> One-button **build + flash + diagnose** of STM32 firmware over CAN, from inside VS Code.

Wraps the [`can-flasher`](https://github.com/isc-fs/MingoCAN) Rust CLI in a VS Code
surface: a single command builds your firmware project, flashes it through whichever CAN
adapter you have plugged in, and surfaces live telemetry and fault codes in panels next
to your code. Built for the [ISC Racing Team](https://iscracingteam.com)'s Formula
Student development inner loop and distributed internally via GitHub Releases — not
listed on the VS Code Marketplace.

## Install

The team distributes the extension as a `.vsix` attached to each GitHub Release. The latest release is always at:

<https://github.com/isc-fs/MingoCAN/releases/latest>

Then, in VS Code:

1. Download `vscode-stm32-can-<version>.vsix` from the release assets.
2. **Extensions panel → ⋯ menu → Install from VSIX…** → pick the downloaded file.

Or from the command line:

```bash
code --install-extension vscode-stm32-can-<version>.vsix
```

The extension activates on the next reload.

### Staying up to date

VS Code does not auto-update sideloaded extensions. To pick up a new release, repeat the install steps with the newer `.vsix` — VS Code overwrites the previous install.

If you want a hands-off updater, a small shell script that polls the [GitHub Releases API](https://docs.github.com/en/rest/releases/releases) and reinstalls when a newer version appears is the usual approach; it's a follow-up if/when the team finds the manual reinstall friction.

## Features

- **Build & Flash** — `ISC MingoCAN: Build & Flash firmware` runs your configured CMake (or
  any) build command, resolves the firmware artifact, and flashes it via `can-flasher`
  with a phase-aware progress notification (`erased sector 3` → `writing sector 3: 67 %`
  → `verified sector 3` → `committing` → `done in 51 234 ms`).
- **Adapter coverage** — supports **SLCAN** (CANable, all OSes), **SocketCAN** (Linux),
  **PCAN-Basic** (Windows / macOS), and **Vector XL Driver Library** (VN1610 / VN16xx on
  Windows), plus an in-process **virtual** loopback for hardware-free tests.
- **Device tree** — the **Devices** view in the ISC MingoCAN activity-bar panel lists
  every CAN adapter the host can see, with the active adapter expanded to show its
  bootloader-mode devices (firmware version, product name, WRP status, reset cause);
  boards running their application (heard via a passive `pit-diag listen`) appear too,
  and drive a status-bar health light.
- **Status-bar adapter picker** — `🔌 vector: 0 → 0x3` in the status bar; click to swap
  adapters, with a Workspace / User settings-scope sub-prompt.
- **DTC viewer** — `Read DTCs` produces a severity-aware table in the output channel;
  `Clear DTCs` gates a destructive clear behind a modal confirmation.
- **Live-data webview** — `Open live-data panel` opens a Chart.js streaming chart with
  frames/sec RX + TX, plus state-pill indicators and a counter grid. One panel per
  (interface, channel) pair so you can watch two boards side-by-side. Theme-reactive —
  axis colours rebind on light ↔ dark switches.
- **Per-device "Flash this device…"** — right-click a node in the device tree to target
  that node for one flash and restore the previous setting afterward.

Every action shells out to `can-flasher --json` — the wire protocol lives in one place
(the CLI), and the extension is a thin orchestration layer that can never drift from it.

## Requirements

- **VS Code 1.85** or later.
- **[`can-flasher`](https://github.com/isc-fs/MingoCAN) CLI** — downloaded automatically
  at the extension's own version (`iscFs.cliAutoDownload`). Offline, or with that setting
  off, put the same-version binary on `PATH` or set `iscFs.canFlasherPath`. From source:
  `cargo install --git https://github.com/isc-fs/MingoCAN.git can-flasher`.
- A CAN adapter — CANable / CANtact (SLCAN), PEAK PCAN, Vector VN1610, or any kernel
  CAN interface on Linux. See
  [adapter setup](https://github.com/isc-fs/MingoCAN/blob/main/docs/INSTALL.md) in
  the CLI repo.

## First-run setup

After install, open VS Code's Settings UI and search for **ISC MingoCAN**:

1. **`iscFs.canFlasherPath`** — leave as `can-flasher` if the binary is on PATH;
   otherwise point at it.
2. **`iscFs.interface`** + **`iscFs.channel`** — pick your adapter. Easier route: open
   the Command Palette and run **`ISC MingoCAN: Select CAN adapter…`** — the extension
   enumerates everything the host can see and writes the choice into your workspace's
   `.vscode/settings.json`.
3. **`iscFs.firmwareArtifact`** — path or glob to the firmware binary, relative to the
   workspace root. Examples: `build/firmware.elf`, `build/*.elf`. Multi-match globs
   trigger a Quick Pick at flash time.
4. **`iscFs.buildCommand`** — defaults to `cmake -B build -S . && cmake --build build`.
   Set to an empty string to skip the build step entirely (or use
   `ISC MingoCAN: Flash firmware (skip build)`).

For a no-hardware smoke test set `iscFs.interface` to `virtual` — the extension drives
an in-process bootloader stub built into the CLI.

## Commands

All commands are available from the Command Palette under the **ISC MingoCAN** category.

| Command | Purpose |
|---|---|
| `ISC MingoCAN: Build & Flash firmware` | Build, then flash the configured artifact. |
| `ISC MingoCAN: Flash firmware (skip build)` | Flash the existing artifact without rebuilding. |
| `ISC MingoCAN: Discover devices on bus` | Refresh the device tree + scroll the output channel. |
| `ISC MingoCAN: Refresh device list` | Same as ⟳ in the device-tree view. |
| `ISC MingoCAN: Select CAN adapter…` | Quick Pick across detected adapters. |
| `ISC MingoCAN: Show session health` | `diagnose health --json` summary. |
| `ISC MingoCAN: Read DTCs` | Column-aligned DTC table, severity-aware toast. |
| `ISC MingoCAN: Clear DTCs` | Modal confirmation, then clear. |
| `ISC MingoCAN: Open live-data panel` | Streaming chart + state pills + counters. |
| `ISC MingoCAN: Re-flash last (skip build)` | Flash the last artifact again without rebuilding. |
| `ISC MingoCAN: Open tools panel` | Open the tools panel — every action as a button. |
| `ISC MingoCAN: Doctor: check flashing environment` | Check CLI, adapter and config before flashing. |
| `ISC MingoCAN: Use CLI on PATH (ignore managed download)` | Use the `can-flasher` on PATH instead of the version-matched download. |

The **ISC MingoCAN** activity-bar panel carries a **Tools** view and the **Devices**
tree. Right-click a node row for **Flash this device…**.

## Settings reference

| Setting | Default | Purpose |
|---|---|---|
| `iscFs.canFlasherPath` | `can-flasher` | Path / binary name. |
| `iscFs.cliAutoDownload` | `true` | Download and use a `can-flasher` matching the extension's version. |
| `iscFs.interface` | `slcan` | `slcan` / `socketcan` / `pcan` / `vector` / `virtual`. |
| `iscFs.channel` | _(empty)_ | Adapter channel string — format depends on backend. |
| `iscFs.bitrate` | `500000` | Nominal CAN bitrate, bps. |
| `iscFs.nodeId` | _(empty)_ | Target node (`0x1` ECU, `0x2` AMS, `0x3` uDV). Required to flash — prompted for and saved if empty. |
| `iscFs.buildCommand` | `cmake -B build -S . && cmake --build build` | Pre-flash shell command. |
| `iscFs.firmwareArtifact` | `**/build/**/*.{elf,hex,bin}` | Path or glob to `.elf` / `.hex` / `.bin`. |
| `iscFs.timeoutMs` | `500` | Per-command reply timeout (`--timeout`). |
| `iscFs.requireWrp` / `iscFs.applyWrp` | `false` / `false` | WRP policy on `flash`. |
| `iscFs.profile` | `false` | Pass `--profile` to `flash` for per-phase timing. |
| `iscFs.jumpAfterFlash` | `true` | Jump to the application after a successful flash. |
| `iscFs.liveDataRateHz` | `10` | Snapshot rate for the live-data webview (1–50). |
| `iscFs.liveDataWindowSeconds` | `60` | Sliding-window size on the live-data chart (5–600). |

## Logs

Every shell-out to `can-flasher` writes its argv plus the raw stdout/stderr to a
dedicated **ISC MingoCAN** output channel. Open it via **View → Output → ISC MingoCAN**. Useful
when something misbehaves at a bench: deterministic record of exactly what was run.

## Repository

The extension lives inside the [`can-flasher`](https://github.com/isc-fs/MingoCAN)
monorepo under [`editor/vscode/`](https://github.com/isc-fs/MingoCAN/tree/main/editor/vscode).
Contributor / development notes:
[CONTRIBUTING.md](https://github.com/isc-fs/MingoCAN/blob/main/docs/CONTRIBUTING.md).

- Bugs and feature requests:
  [github.com/isc-fs/MingoCAN/issues](https://github.com/isc-fs/MingoCAN/issues)

## Release notes

See [CHANGELOG.md](https://github.com/isc-fs/MingoCAN/blob/main/editor/vscode/CHANGELOG.md).

## Licence

MIT — see [LICENSE](https://github.com/isc-fs/MingoCAN/blob/main/editor/vscode/LICENSE).
