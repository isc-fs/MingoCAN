# v2.3.3 — One-click Build + Flash for STM32CubeMX projects

A patch release that finishes the *STM32CubeIDE-equivalent Flash
button* arc. CLI binaries and Studio bundles are functionally
unchanged from v2.3.2; the lockstep version moves with the VSIX.

## Highlights

### `$(zap) Build + Flash` shortcut, lightning glyph

The status-bar Flash button gets a lightning bolt icon and an
explicit `Build + Flash` label so its two-stage behaviour is
visually unambiguous. Same `iscFs.flash` command underneath.

### One-click build + flash on a fresh clone

Three layered changes mean the Flash button now Just Works
end-to-end on a fresh STM32CubeMX-generated CMake project:

1. **`iscFs.buildCommand`** default is now
   `cmake -B build -S . && cmake --build build`. The configure
   step is a no-op when `build/` already exists, so subsequent
   flashes pay no time penalty.
2. **`CMakePresets.json` auto-detection.** When a presets file is
   present at the workspace root *and* the operator is still on
   the default, the extension synthesises
   `cmake --preset <X> && cmake --build --preset <Y>` from the
   first build / configure preset — picks up the
   arm-none-eabi toolchain pin that STM32CubeMX writes there.
   The artifact glob narrows to that preset's `binaryDir` so
   multi-preset projects don't pick up the wrong build's `.elf`.
3. **`iscFs.firmwareArtifact`** default is now
   `**/build/**/*.{elf,hex,bin}`. Auto-discovers the firmware
   after a build without operator intervention.

Both substitutions log to **View → Output → ISC MingoCAN** so
the operator can see exactly which preset / command was used.

### Better failure UX

- When `iscFs.buildCommand` is empty, the Flash button now
  surfaces a warning toast (**Set build command** / **Continue
  (flash only)**) instead of silently skipping the build.
- When no artifact matches the glob, the toast offers two
  buttons: **Build first** (jumps to `iscFs.buildCommand`) or
  **Set artifact path** (jumps to `iscFs.firmwareArtifact`).
- When the build step fails, the output channel pops to the
  front automatically and the toast offers **Change build
  command** to jump straight to the setting.

### Finished the v2.3.1 rebrand

The v2.3.1 rebrand to **ISC MingoCAN** caught `package.json`,
the output-channel name, and the command-palette categories but
missed every runtime toast string. v2.3.3 sweeps the remaining
`"ISC CAN:"` prefixes in `flash.ts` / `diagnose.ts` /
`picker.ts` / `extension.ts` to `"ISC MingoCAN:"` so operators
see one consistent brand everywhere.

## Install

CLI binaries, the VSIX, and Studio bundles attached below.
Only the VSIX has functional changes; CLI and Studio move with
the lockstep version bump.

## Closes

- #216 — status-bar Flash button: lightning icon + Build + Flash label + empty-buildCommand prompt
- #218 — Flash button works out of the box: CMake configure default, firmware glob, friendlier no-match UX, finished MingoCAN sweep
- #220 — `CMakePresets.json` auto-detection
