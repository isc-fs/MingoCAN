# v2.13.0

Documentation, and the repo rename that preceded it. No behaviour changes.

## The repo is now `isc-fs/MingoCAN`

GitHub redirects the old URLs, so nothing breaks — but two references took an
explicit `--repo` and would have failed the *next* release rather than this one:
the release workflow's asset upload, and the iskApps mirror's download step.
Both are fixed, along with the Tauri updater's fallback endpoint, the VS Code
extension's `REPO` constant, and the in-app links.

If you have a clone:

```bash
git remote set-url origin https://github.com/isc-fs/MingoCAN.git
```

**The binary is still `can-flasher`.** Renaming it would break every script and
CI job that invokes the tool; the product name and the executable name are
allowed to differ.

## The documentation is rewritten

The docs were written when this repo was a CLI that happened to grow a GUI. It
is now a desktop app that happens to ship its engine as a CLI, and the
documentation said otherwise on nearly every page.

| Doc | |
|---|---|
| [README](https://github.com/isc-fs/MingoCAN/blob/main/README.md) | MingoCAN as the product, routing by task |
| [INSTALL.md](https://github.com/isc-fs/MingoCAN/blob/main/docs/INSTALL.md) | Three paths — app, CLI, source — plus all the per-OS adapter setup |
| [DESKTOP.md](https://github.com/isc-fs/MingoCAN/blob/main/docs/DESKTOP.md) | Every view, every setting |
| **[FLASHING.md](https://github.com/isc-fs/MingoCAN/blob/main/docs/FLASHING.md)** | **New.** Flashing is the tool's main job and had no guide of its own |
| **[CLI.md](https://github.com/isc-fs/MingoCAN/blob/main/docs/CLI.md)** | Was `USAGE.md`. Now one chapter — the `can-flasher` CLI — rather than the primary interface |
| [TELEMETRY.md](https://github.com/isc-fs/MingoCAN/blob/main/docs/TELEMETRY.md) | Leads with which modes transmit and which do not |
| [DATA_LOGS.md](https://github.com/isc-fs/MingoCAN/blob/main/docs/DATA_LOGS.md) | Leads with the two things that catch everyone |
| [SAFETY.md](https://github.com/isc-fs/MingoCAN/blob/main/docs/SAFETY.md) | Adds clear-DTC, device reset, and arming a telemetry stream |

## Fixed while writing them

Every page was written from a source audit rather than from the previous docs,
which turned up things the old text got wrong:

- **The Flash view's advanced toggles were documented as "all off by default
  unless noted". Five of the six default to ON** — skip unchanged sectors,
  verify each sector, confirm the whole image, start the app, and reboot a
  running board into the bootloader. Only *dry run* is off.
- **`--timeout` was described as a per-frame timeout. It is per command**,
  covering a whole reassembled ISO-TP message — which changes when you would
  ever want to raise it.
- **`can-flasher pit-diag --help` still called itself the "AMS pit-diag
  observer".** It has covered ECU and uDV for several releases. Help text is
  shipped documentation, so that string is fixed in the binary, not just in the
  docs.
- `--log` (the SQLite audit trail) and `--operator` appeared in no
  global-options table anywhere.
- Nothing documented the settings file, its per-OS location, or the 250 ms
  autosave — the first thing you want when the app is misbehaving.
- Nothing warned that the default build command is a generic CMake placeholder,
  wrong for most projects, and the most likely reason a first flash fails before
  it ever reaches the board.

Not rewritten: `REQUIREMENTS.md` and `ARCHITECTURE.md`. Those are spec and
internals, and rewriting a protocol spec during a documentation sweep is how
specs acquire errors. Their links were repointed and nothing else was touched.

**Full changelog**: https://github.com/isc-fs/MingoCAN/compare/v2.12.0...v2.13.0
