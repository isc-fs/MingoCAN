# v2.3.2 — VS Code status-bar shortcuts + tools panel

Another patch focused on the VS Code extension. CLI binaries and
Studio bundles are functionally unchanged from v2.3.1; the
version string moves in lockstep.

## Highlights

### Status-bar Flash + Tools shortcuts

Two new one-click buttons sit next to the existing adapter pill
at the bottom of the window:

- **`$(rocket) Flash`** — runs build + flash via CAN, same as the
  `iscFs.flash` palette command. The fastest path from "edited
  some firmware" to "running on the bench".
- **`$(tools) Tools`** — opens the new dashboard panel (below).

No more `Cmd+Shift+P` round-trip for the two most-used actions.

### Tools dashboard panel

A dedicated webview that puts every operator-facing action on one
screen, grouped by phase of the workflow:

| Section | Buttons |
|---|---|
| **Adapter** | live pill + *Change…* |
| **Flash** | *Build + flash*, *Flash without build* |
| **Devices** | *Discover devices*, *Refresh device list* |
| **Diagnostics** | *Session health*, *Read DTCs*, *Clear DTCs*, *Live data…* |

Reachable from the status-bar **Tools** button, or via the
command palette as **ISC MingoCAN: Open tools panel**. The
adapter pill stays in sync with the status-bar picker, so
switching adapters while the panel's open updates the panel
without re-opening it.

Aimed at operators who'd rather point and click than memorise
palette command names — the palette path still works for the
keyboard crowd.

## Install

CLI binaries, the VSIX, and Studio bundles attached below. Same
shape as v2.3.1; only the VSIX has functional changes.

## Closes

- #212 — status-bar Flash + Tools shortcuts and dedicated tools panel
