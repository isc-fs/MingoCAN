# ISC STM32 CAN Flasher — VS Code extension

VS Code wrapper around the [`can-flasher`](../../README.md) CLI: build
the current STM32 firmware project and flash it to a CAN-connected
node from inside the editor.

**Status: v0 sketch.** The extension manifest is complete (commands,
settings, view container), but every command currently surfaces a
"not implemented" toast. Real handlers land in subsequent PRs — see
[Roadmap](#roadmap) below.

## What it's for

Right now the ISC racing-team workflow is:

1. Build firmware (toolchain-specific — CMake invocation, `west build`, IDE-specific button, …)
2. Drop to a terminal: `can-flasher --interface … --channel … flash build/firmware.elf`
3. Read the result, decide what to do next

This extension collapses (1) and (2) into a single command — and adds
the small ergonomic wins that make a development inner-loop pleasant:
device discovery in a sidebar, adapter selection from a quick-pick,
DTC viewer, live-data panel.

## How it talks to hardware

The extension **shells out to `can-flasher`** for every action that
touches the bus. It never speaks the bootloader protocol directly.
That means:

- One canonical implementation of the wire format (`can-flasher`'s
  `protocol/` module). The extension can't drift.
- The CLI's `--json` mode (already exhaustive on `adapters`,
  `discover`, `flash`, `diagnose`) is the API surface — stable and
  documented in [REQUIREMENTS.md](../../REQUIREMENTS.md).
- Adding a new adapter (e.g. when Vector Linux support lands) is
  purely a `can-flasher` change; the extension picks it up
  automatically the next time it runs `can-flasher adapters --json`.

The build side runs whatever shell command is in
`iscFs.buildCommand` (defaults to `cmake --build build`). It's not
locked to any particular STM32 build system — anything that produces
an ELF/HEX/BIN at the configured path works.

## Roadmap

The sketch lays the package surface for all three tiers; each tier
lands as its own PR.

### Tier A — Build + Flash (v0.1, MVP)

| Command | What it does |
|---|---|
| `iscFs.flash` | Run `iscFs.buildCommand`, then `can-flasher flash <iscFs.firmwareArtifact>` with the configured adapter |
| `iscFs.flashWithoutBuild` | Skip the build step; flash the existing artifact |

This is the minimum useful surface — one keybinding, one CAN flash.

### Tier B — Device awareness (v0.2)

| Command / View | What it does |
|---|---|
| `iscFs.devices` (tree) | Live list of detected bootloader-mode nodes; refreshed via `discover --json` |
| `iscFs.refreshDevices` | Force a fresh `adapters` + `discover` poll |
| `iscFs.discover` | One-shot bus scan, results in output channel |
| `iscFs.selectAdapter` | Quick-pick across `slcan`/`socketcan`/`pcan`/`vector`/`virtual` populated from `adapters --json` |

### Tier C — Diagnostics (v0.3, post-MVP)

| Command / View | What it does |
|---|---|
| `iscFs.readDtcs` | Pretty-print active DTCs in the Problems panel |
| `iscFs.clearDtcs` | Issue `CMD_DTC_CLEAR` (with confirmation) |
| `iscFs.health` | Session-health output panel |
| Live data webview | Periodically poll `diagnose live-data --json` and chart it |

### Out of scope (for now)

- Build-system integration tighter than "shell out and parse exit
  code" — no `gcc` diagnostic squiggles, no `tasks.json` provider
- Marketplace publication — distribution is `.vsix` files shared
  within ISC
- Multi-workspace support beyond "the active folder" — single-board
  flashing only, no fleet management

## Settings

All extension settings live under `iscFs.*` in VS Code's settings.
See `package.json`'s `contributes.configuration.properties` for the
canonical list. Highlights:

| Setting | Default | Purpose |
|---|---|---|
| `iscFs.canFlasherPath` | `can-flasher` | Path / binary name |
| `iscFs.interface` | `slcan` | `--interface` |
| `iscFs.channel` | _(empty)_ | `--channel` (format depends on backend) |
| `iscFs.bitrate` | `500000` | `--bitrate` |
| `iscFs.nodeId` | _(empty)_ | `--node-id`, omit for broadcast |
| `iscFs.buildCommand` | `cmake --build build` | Run before flash (empty = skip) |
| `iscFs.firmwareArtifact` | _(empty)_ | Path / glob to .elf/.hex/.bin |
| `iscFs.timeoutMs` | `500` | `--timeout` |
| `iscFs.requireWrp` / `iscFs.applyWrp` | `false` / `false` | WRP gating policy on `flash` |
| `iscFs.profile` | `false` | Pass `--profile` for timing diagnostics |
| `iscFs.jumpAfterFlash` | `true` | Pass `--jump` (false → stays in BL) |

## Development

This sketch doesn't ship `node_modules` — install before first
compile.

```bash
cd editor/vscode
npm install
npm run compile     # one-shot
npm run watch       # tsc -watch
```

In VS Code, open the `editor/vscode/` folder as a separate window and
press `F5` to launch a development host with the extension loaded.
Every command should appear in the palette as `ISC CAN: …` and pop
a "not implemented" toast.

To produce a `.vsix` for sideload installation:

```bash
npm run package
# → vscode-stm32-can-0.0.1.vsix
```

Then in the target VS Code: **Extensions → … menu → Install from VSIX**.

## Design notes

### Why in-repo (vs. its own repo)?

The extension's API surface is `can-flasher`'s `--json` output. Two
people changing both in lockstep is much easier when they're in the
same PR — schema changes can ship as a single atomic update. If the
extension ever needs to be released independently (different cadence,
different versioning, different lifecycle), splitting it out later is
cheap.

### Command-ID prefix

`iscFs.*` — short, recognisable, doesn't collide with anything else
likely to appear in a VS Code workspace. The marketplace publisher
field uses the same `isc-fs` identifier.

### Tree provider over webview

The "Detected devices" sidebar is a `TreeView`, not a webview. Tree
views render natively in the Explorer panel, get keyboard navigation
and command-bar buttons for free, and are cheap to refresh. Webviews
are reserved for the Tier C live-data panel where a chart is needed.

### Output channel as logging

Every shell-out to `can-flasher` writes both its argv and its
stdout/stderr to a dedicated `ISC CAN` Output channel. Operators
clicking a button in the UI get a deterministic record of what was
actually run, which is essential when something goes wrong on a
hardware test bench.
