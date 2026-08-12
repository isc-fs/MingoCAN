# v2.12.0

**Pedal calibration is removed.** The rest of this release is documentation.

## Removed: pedal calibration

The calibration wizard shipped in v2.10.0 and was never successfully used to
calibrate a car. It is gone rather than left in place as a feature that looks
available and is not.

Removed: the calibration panel on the Telemetry → ECU tab, the `pit_cal_command`
backend command, the `0x7E2`–`0x7E5` session codec, and the runbook that
documented it. The uDV **steering** end-stop calibration (`0x7A6` / `0x7DF`) is
a different feature and is untouched.

**What is kept:** the read-only `pedal cal` field on the ECU Firmware health
card. It comes from the ungated `0x704` health frame and reports whether the
ECU is running a stored calibration (`loaded`) or fell back to compile-time
values (`defaults`, `invalidFellBack`, `badVersionFellBack`). That is still
worth seeing — it is now a fact about the firmware on the board rather than
confirmation of anything this tool did.

**Consequence:** a calibration already stored in an ECU's NVM stays there and
keeps working. This tool can no longer write, change, or reset one. An NVM
format still erases it, and there is now no way to put one back from here.

Anyone who needs the code back: it is in the `v2.11.0` tag, and the firmware
side is [IFS08-CE-ECU#169](https://github.com/isc-fs/IFS08-CE-ECU/issues/169).

## Documentation

The desktop app is the primary way people use this tool and had no
documentation at all. Four new guides:

| Guide | For |
|---|---|
| [DESKTOP.md](https://github.com/isc-fs/can-flasher/blob/main/docs/DESKTOP.md) | The app, view by view |
| [TELEMETRY.md](https://github.com/isc-fs/can-flasher/blob/main/docs/TELEMETRY.md) | Watching a live car — which modes transmit and which do not |
| [DATA_LOGS.md](https://github.com/isc-fs/can-flasher/blob/main/docs/DATA_LOGS.md) | Pulling microSD logs |
| [SAFETY.md](https://github.com/isc-fs/can-flasher/blob/main/docs/SAFETY.md) | Every operation that writes to a board, and what guards it |

The existing reference docs had drifted. Corrected:

- The README advertised **v1.2.0** as the current release, and its subcommand
  table was missing `logs`, `pit-diag` and `provision`.
- `USAGE.md` described `pit-diag` as AMS-only with a hardcoded profile. ECU and
  uDV have shipped for several releases, and **`listen` — the only send-silent
  mode, the one that is safe on a live car — was undocumented entirely.**
  `logs` had no section at all.
- Nothing anywhere said how `--node-id` resolves. It is an error on `flash` and
  `logs`, defaults to `0x3` on four commands, and is ignored by the rest. There
  is now a table.
- `INSTALL.md` had no path for someone who only wants the app, and the
  unsigned-build prompts were documented only in `UPDATES.md`, where nobody
  installing for the first time would look.
- The installer asset names were wrong — Windows ships an NSIS `-setup.exe`,
  not an `.msi`.
- `CONTRIBUTING.md` conflated the six version files you must bump with the five
  that CI actually checks. `Cargo.lock` is the ungated one, and the one that
  drifts.

## Also in this cycle

[#558](https://github.com/isc-fs/can-flasher/issues/558) — the ECU DBC snapshot
was behind upstream and the decoder with it: `0x700` lost `ev_2_3` (EV.2.3 was
deleted in FS-Rules 2024) and gained `power_capped`; `0x704` gained
`stub_torque_cap`; `0x706` grew from DLC 4 to 7; `0x708` from 6 to 7.

**Full changelog**: https://github.com/isc-fs/can-flasher/compare/v2.11.0...v2.12.0
