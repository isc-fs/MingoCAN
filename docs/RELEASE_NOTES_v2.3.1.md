# v2.3.1 — VS Code extension fixes + rename

A patch focused on the VS Code extension. The CLI binaries and
the Studio bundles ship unchanged from v2.3.0 (lockstep versions
just bump along).

## Highlights

### VS Code extension: auto-discover the `can-flasher` binary

The previous release silently failed to detect adapters when VS
Code was launched from Finder / Dock / launchd on macOS — the
operator's per-user PATH (`~/.local/bin`, `~/.cargo/bin`,
`/opt/homebrew/bin`) isn't visible to GUI-launched apps, and the
default `iscFs.canFlasherPath` setting was the bare string
`"can-flasher"`. `child_process.spawn` ENOENTed, the Adapters
tree came back empty.

v2.3.1 probes a curated list of well-known install dirs whenever
the setting is at its default:

- **macOS / Linux**: `/usr/local/bin` → `/opt/homebrew/bin` →
  `~/.local/bin` → `~/.cargo/bin`
- **Windows**: `%LOCALAPPDATA%\Programs\can-flasher` →
  `C:\Program Files\can-flasher` → `~\.cargo\bin`

First hit wins. Operator-customised paths are still honoured
verbatim. The discovery is session-cached and invalidated on
settings change, so a settings edit doesn't need a window reload.

When the binary still can't be found, a one-shot notification
surfaces with two actions:

- **Download CLI** — opens the latest GitHub release in your browser.
- **Open settings** — jumps to `iscFs.canFlasherPath`.

### VS Code extension renamed to **ISC MingoCAN Flasher**

Matches the desktop app rebrand from v2.3.0. Touches operator-
facing strings only — the internal extension ID
(`isc-fs.vscode-stm32-can`) is unchanged, so existing installs
pick up the rename in place without re-installation.

What you'll see in VS Code:

- Extensions panel: **ISC MingoCAN Flasher** (was *ISC STM32 CAN Flasher*)
- Command palette: **ISC MingoCAN: Flash**, **ISC MingoCAN: Discover devices**, … (was *ISC CAN: Flash*, …)
- Activity bar tree: **ISC MingoCAN Devices** (was *ISC CAN Devices*)
- Output panel: **View → Output → ISC MingoCAN** (was *ISC CAN*)

## Install

CLI binaries, the VSIX, and Studio bundles attached below. Same
shape as v2.3.0; only the VSIX has functional changes.

## Closes

- #208 — auto-discover `can-flasher` binary + rename to ISC MingoCAN Flasher
