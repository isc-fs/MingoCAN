# v2.4.0 — VS Code: PlatformIO-style activity-bar sidebar

A VS Code-extension-led minor release. CLI binaries and Studio
bundles ship functionally unchanged; the lockstep version moves
with the VSIX.

## Highlights

### Activity-bar sidebar

A new **ISC MingoCAN** icon on the activity bar (the leftmost
vertical strip in VS Code) reveals a sidebar with two views
side-by-side:

- **Tools** — one-click access to every operator action:
  Build + Flash, Flash without build, Discover devices, Read /
  Clear DTCs, Session health, Live data, plus an adapter pill
  with a quick-switch button.
- **Devices** — the live discovery tree (was previously a
  section in the Explorer view).

Same shape as PlatformIO's left-rail panel: click the icon,
sidebar slides out, every action one click away. No more
hunting in the command palette unless you want to.

The bottom-bar `$(zap) Build + Flash` and `$(tools) Tools`
buttons stay where they are; the Tools button now reveals the
sidebar instead of opening an editor-tab panel.

### Backwards compat

- Existing operators with the Devices tree in the Explorer view
  see it in the new activity-bar container instead — same data,
  just a different location.
- The old editor-tab dashboard is reachable as **ISC MingoCAN:
  Open tools panel** in the command palette, for operators who
  liked having it side-by-side with code.

## Install

CLI binaries, the VSIX, and Studio bundles attached below.
Only the VSIX has functional changes; CLI and Studio move with
the lockstep version bump.

## Closes

- #224 — activity-bar sidebar for Tools + Devices (PlatformIO-style)
