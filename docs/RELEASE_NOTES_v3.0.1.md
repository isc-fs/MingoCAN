# v3.0.1

**The desktop app for 3.0.** v3.0.0 shipped the command-line tool and the VS
Code extension, but its desktop builds failed, so it has no installers. This
release is the same code with the build fixed — install this one.

Everything new in 3.0 is described in the
[v3.0.0 release notes](https://github.com/isc-fs/MingoCAN/releases/tag/v3.0.0):
node IDs set over SWD at the bootloader burn (needs bootloader v1.7.0), and
five new ECU telemetry frames with three new cards on the ECU telemetry tab.

## What went wrong

The Tauri build refuses to run when a plugin's npm package and its Rust crate
are on different minor versions. The app's npm dependencies floated on `^2`, and
upstream released `@tauri-apps/plugin-updater` 2.12 while the Rust side was
still pinned at 2.10 — so every desktop build stopped at that check. Nothing in
MingoCAN changed; any release cut after that upstream bump would have hit it.

The app's `@tauri-apps` npm packages are now pinned to the same minor versions
as their Rust crates
([#591](https://github.com/isc-fs/MingoCAN/pull/591)), so an upstream release
can't break a build this way again.

## If you already installed 3.0.0

- **CLI / VS Code extension:** 3.0.0 and 3.0.1 are the same code; updating is
  optional.
- **Desktop app:** there was no 3.0.0 desktop build. The app's updater offers
  3.0.1 directly from 2.14.0.

**Full changelog**: https://github.com/isc-fs/MingoCAN/compare/v3.0.0...v3.0.1
