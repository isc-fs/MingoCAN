# ISC STM32 CAN Flasher — VS Code extension

VS Code wrapper around the [`can-flasher`](../../README.md) CLI: build
the current STM32 firmware project and flash it to a CAN-connected
node from inside the editor.

**Status: every roadmap surface is live.** Build / flash, adapter
detection + the device tree + status-bar selector, the three
one-shot diagnostics commands (DTC read / clear / session-health),
*and* the streaming live-data webview all work against a real
`can-flasher` binary. See [Roadmap](#roadmap) for the
command-by-command summary.

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

### Tier A — Build + Flash (v0.1, ✅ live)

| Command | What it does |
|---|---|
| `iscFs.flash` | Runs `iscFs.buildCommand` via the user's shell, then `can-flasher flash <iscFs.firmwareArtifact>` with the configured adapter. Progress notification shows the live flash phase (erasing / writing N% / verifying / committing); the **ISC CAN** output channel carries the full argv + stdout/stderr; success / failure toasts include duration and an exit-code label that maps to REQUIREMENTS.md § Output and CI integration. |
| `iscFs.flashWithoutBuild` | Same as above, build step skipped — useful when iterating on flash parameters with a pre-built artifact. |

Glob patterns are accepted in `iscFs.firmwareArtifact`; multiple matches trigger a Quick Pick.

### Tier B — Device awareness (v0.2, ✅ live)

| Command / View | What it does |
|---|---|
| `iscFs.devices` (tree, Explorer panel) | Hierarchical view: every detected adapter, with the active adapter expanded to show its bootloader-mode devices. Each device row shows product name, firmware version, WRP status, reset cause. Tooltip carries the full record. |
| `iscFs.refreshDevices` (⟳ on the view title bar) | Re-runs `adapters --json` + `discover --json` against the active adapter. No background polling — refresh is always operator-initiated so no frame goes on the bus uninvited. |
| `iscFs.discover` (palette) | Same code path as the refresh button; reveals the ISC CAN output channel so keyboard-driven operators get a textual summary alongside the tree update. |
| `iscFs.selectAdapter` (palette + status-bar click) | Quick-pick across every detected adapter plus the in-process virtual loopback. Selection prompts for **Workspace** (default — moves with the project) or **User** (global) settings scope. |
| `iscFs.flashThisDevice` (right-click in tree) | Sets `iscFs.nodeId` to the clicked device's ID for one flash, then restores the previous value. Lets the operator target a specific node from a multi-node bus without editing settings by hand. |
| **Status-bar item** (bottom-left) | `🔌 vector: 0 → 0x3` — current adapter + channel + node. Click to open the adapter picker. Goes warning-yellow with `⊘ ISC CAN: no adapter` when no channel is configured. |

The tree populates lazily — opening the view triggers the first `adapters` + `discover` round-trip, after which it caches until the operator hits ⟳. Inactive adapters are listed but collapsed (no `discover` is run against them); to inspect another adapter's devices, select it first.

### Tier C — Diagnostics (v0.3, ✅ commands live)

| Command | What it does |
|---|---|
| `iscFs.health` | Runs `diagnose health --json`. Pretty-prints uptime, reset cause, session/app/WRP state, flash-write count, last DTC code, raw flags into the ISC CAN output channel. |
| `iscFs.readDtcs` | Runs `diagnose read-dtc --json`. Formats entries into a column-aligned table in the output channel. Severity-aware toast: info on empty, warning on `WARN`, error on `ERROR`/`FATAL`. |
| `iscFs.clearDtcs` | Modal confirmation ("This cannot be undone."), then runs `diagnose clear-dtc --yes --json`. Success / failure toast. |

DTC display lives in the **output channel** rather than the Problems panel. DTCs are hardware fault codes that don't map to source-file ranges, so VS Code's diagnostic-collection plumbing doesn't fit them well; structured text in the dedicated channel keeps copy-paste, scrollback, and a deterministic record at hardware-test time.

### Tier C.2 — Live-data webview (v0.4, ✅ live)

| Command | What it does |
|---|---|
| `iscFs.liveData` | Opens (or focuses) a webview panel titled **ISC CAN — Live data (`interface`·`channel`)**. **Start** spawns `can-flasher diagnose live-data --rate-hz N --json`; each snapshot updates a sliding-window Chart.js line chart (frames/sec RX + TX), a row of state-pill indicators (`session active`, `valid app`, `WRP`, `log stream`, `live-data stream`), and a grid of numeric counters (uptime, session age, DTC count, NACK count, last opcode, last flash addr, ISO-TP RX progress). **Stop** kills the child process and freezes the chart. **Clear chart** wipes accumulated points without restarting. |

**Multi-panel**: one panel per (interface, channel) pair. Each panel captures its adapter identity at creation time, so a running stream stays locked to its adapter even if the operator later switches `iscFs.interface`/`iscFs.channel` to a different board. Open the command, switch adapters, open it again — two panels, two boards, side-by-side, independent streams.

**Theme reactivity**: chart axis ticks / grid / legend colours re-read from `var(--vscode-*)` whenever the body class changes (light ↔ dark ↔ high-contrast). No need to close and re-open the panel after a theme switch.

Bundled assets ship under `editor/vscode/media/`:

- `chart.umd.min.js` — Chart.js v4.4.7 (vendored, ~200 KB) so the `.vsix` is fully self-contained and works offline
- `live-data.css` — VS Code-theme-aware styles (uses `var(--vscode-*)` colour tokens; light / dark / high-contrast carry through)
- `live-data.js` — webview-side renderer: chart init, message handling, rate computation (snapshot deltas → frames/sec), sliding-window pruning

Two new settings:

| Setting | Default | Range |
|---|---|---|
| `iscFs.liveDataRateHz` | `10` | `1`–`50` (CLI constraint) |
| `iscFs.liveDataWindowSeconds` | `60` | `5`–`600` |

Each panel owns one host-side `LiveDataController`. When the panel closes, the controller disposes and kills the in-flight child — no orphan `can-flasher` processes. When the extension deactivates the same dispose chain fires via `context.subscriptions` for every panel in the `byKey` map.

Webview is configured with a strict CSP: `default-src 'none'`, `script-src` gated on a nonce regenerated per panel, `connect-src 'none'` so the chart can never phone home. All data flows in through `postMessage` from the host.

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
# → vscode-stm32-can-0.1.0.vsix
```

Then in the target VS Code: **Extensions → … menu → Install from VSIX**.

## Releasing

Official `.vsix` builds are produced by the [`Editor release`](../../.github/workflows/editor-release.yml) GitHub Actions workflow. To cut a new release:

1. Bump `version` in [package.json](package.json) (and commit + merge through the normal feat-branch + PR flow).
2. Push a tag of the form `editor-vX.Y.Z` matching the bumped version:
   ```bash
   git tag editor-v0.1.0
   git push origin editor-v0.1.0
   ```
3. The workflow's `verify-version` gate cross-checks the tag against `package.json`. On match it compiles, packages with `vsce`, and creates a GitHub Release with the `.vsix` attached as the only asset.
4. Team members install from the Release page (Extensions → ⋯ menu → Install from VSIX…).

Manual dispatch (`Run workflow` button) on the Actions UI produces a `.vsix` as a workflow artifact (14-day retention) without creating a Release — useful for testing a build before tagging.

The Rust CLI and the extension have **independent release cadences and tag spaces**: `v*` triggers the binary build, `editor-v*` triggers the extension build, neither interferes with the other.

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
