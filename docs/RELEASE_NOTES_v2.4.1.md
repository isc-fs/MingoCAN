# v2.4.1 — Adapter-presence detection

A VS Code-extension-led patch release. CLI binaries and Studio
bundles ship functionally unchanged.

## Highlights

### Pill flips to `(disconnected)` when the probe is unplugged

Before v2.4.1 the status-bar pill and the Tools-sidebar pill kept
showing the last-saved `iscFs.interface` + `iscFs.channel`
regardless of whether the hardware was actually on the bus.
Unplugging the probe between runs left the UI looking live — and
the next click was about to talk to nothing.

A new presence service shells `can-flasher --json adapters` and
compares the configured adapter against the detected list. When
the configured probe isn't on the bus, both pills flip to a
warning state:

```
$(debug-disconnect) pcan: PCAN_USBBUS1 (disconnected)
```

…with a tooltip explaining what to do (plug it back in or pick a
different adapter).

The check runs:

- on activation (cold-start truth-up),
- on `iscFs.*` settings changes,
- on window-focus regained (canonical "I just plugged it back in"),
- every 8 s while the window is focused.

Polling pauses when the window loses focus so a probe-less laptop
doesn't get pinged in the background.

## Install

CLI binaries, the VSIX, and Studio bundles attached below. Only
the VSIX has functional changes; CLI and Studio move with the
lockstep version bump.

## Closes

- #228 — adapter-presence detection
