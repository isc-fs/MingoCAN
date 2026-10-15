# v3.1.2

**Flashing the uDV no longer reboots the AMS.** MingoCAN now sends the uDV its
own reboot-to-bootloader trigger, and refuses to guess a trigger for a board it
doesn't know.

## The uDV gets its own reboot trigger

Before flashing, MingoCAN can reboot a running board into its bootloader
(`--enter-bootloader auto|always` in the CLI, **Reboot a running board into the
bootloader** in the app). It does this by sending `0x002` with a payload that
picks the board. 3.1.1 had no payload for the uDV (node `0x3`) and sent the
AMS's instead, so on the shared ACU bus **flashing the uDV rebooted the AMS**.

| Board | Node | Trigger |
|---|---|---|
| ECU | `0x1` | `B0 07 AD 12` |
| AMS | `0x2` | `B0 07 AD 11` |
| uDV | `0x3` | `B0 07 AD 13` (honoured only in AS OFF) |

## ⚠️ Unknown boards are refused

Any other node-id now fails **before anything is sent**, instead of falling back
to the AMS payload. The error lists the known boards. To flash such a board:

1. Put it in its bootloader yourself, e.g. `can-flasher send-raw 0x002 <payload>`.
2. Flash with `--enter-bootloader never`, or untick the reboot toggle in the app.

`--enter-bootloader auto` still connects to an unknown board that is already in
its bootloader.

## Other fixes

- **Board health → Read DTCs showed garbage** whenever a DTC was stored: every
  row was shifted by two bytes. The app and the CLI now read the list correctly.
- **Bus monitor → Signals values froze** until you switched tabs. They update
  live again.
- **The app's Flash tab refuses to run without a target node-id** instead of
  silently picking the uDV.
- **Copy fixes** in Burn bootloader and Settings ("msand", a pointer to a field
  that doesn't exist, and a hint that misdescribed the target-board setting).
- **Docs** match what the app and CLI actually do, including the per-board
  reboot payloads and the AMS-only HV relays.
- **Builds are reproducible.** The app and extension lockfiles are committed and
  CI and releases build with `npm ci`. MingoCAN now ships an MIT `LICENSE`.

**Full changelog**: https://github.com/isc-fs/MingoCAN/compare/v3.1.1...v3.1.2
