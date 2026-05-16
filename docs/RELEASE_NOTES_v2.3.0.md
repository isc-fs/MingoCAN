# v2.3.0 — Rebranded as ISC MingoCAN + PCAN on macOS

## Highlights

### The Studio app is now **ISC MingoCAN**

Renamed in every place an operator sees: sidebar header, window
title, dock / start-menu name, About panel, README headings,
Tauri bundle metadata. The macOS `.dmg`, Linux `.deb` /
`.AppImage` / `.rpm`, and Windows `.msi` / `.exe` are produced
under the new name from this release onward.

The internal identifiers stay where they are — folder
(`apps/can-studio/`), Cargo crate (`can-studio`), npm package
(`isc-can-studio`), bundle identifier
(`com.iscracingteam.can-studio`). Renaming the bundle identifier
would break auto-update for already-installed apps; the rest are
just paths.

### PCAN-USB adapters now appear on macOS

The v2.2.0 bundle silently failed to detect PCAN adapters on
macOS even with the MacCAN driver installed and the device
plugged in. Root cause: the bundle ships with hardened runtime
enabled by default and no entitlements, so macOS library
validation refused to load `libPCBUSB.dylib` (signed by UV
Software, not by us or Apple).

v2.3.0 adds the
`com.apple.security.cs.disable-library-validation` entitlement
to the macOS bundle — Apple's documented escape hatch for
plugin systems and apps that load third-party SDKs. PCAN-USB and
PCAN-USB FD now show up in the **Adapters** view the same way
SLCAN and SocketCAN already did.

To use PCAN on macOS:

1. Install MacCAN's PCBUSB library from
   <https://www.mac-can.com/PCBUSB-Library.html> — drops
   `libPCBUSB.dylib` into `/usr/local/lib/`.
2. Plug the PCAN-USB device in.
3. Open ISC MingoCAN's **Adapters** view; refresh.

Library validation is only enforced on macOS, so Windows
(`vxlapi64.dll` for Vector) and Linux paths are unchanged.

## Install

CLI binaries, the VSIX, and the renamed Studio bundles
(`ISC.MingoCAN_2.3.0_*`) are attached below.

## Closes

- #202 — rename Studio app to ISC MingoCAN
- #204 — macOS hardened-runtime library-validation entitlement
