# v2.11.1

A documentation release with one real bug fix behind it.

## Fixed

**`Reset to defaults` did not reset anything.** The pedal-calibration
panel sent `resetDefaults` and stopped there. That command only clears
the *staged* set — without a following `commit`, the ECU's NVM was
never touched. The button reported success and changed nothing, which
is the worst possible failure mode for a control whose entire job is to
put a known-good calibration back. It now stages, reads back, commits,
and re-reads what is stored.

**`ValidationFailed` with no rule named** is the consistency-CRC case:
the tool and the ECU disagree about what was captured. The message did
not say so, leaving the one failure that means *abort and re-capture*
indistinguishable from an ordinary rule violation you could retry.

Two labels in the calibration panel now match what the ECU calls those
values.

## Documentation

The desktop app is the primary way people use this tool and had no
documentation at all. Five new guides:

| Guide | For |
|---|---|
| [DESKTOP.md](https://github.com/isc-fs/can-flasher/blob/main/docs/DESKTOP.md) | The app, view by view |
| [TELEMETRY.md](https://github.com/isc-fs/can-flasher/blob/main/docs/TELEMETRY.md) | Watching a live car — which modes transmit and which do not |
| [PEDAL_CALIBRATION.md](https://github.com/isc-fs/can-flasher/blob/main/docs/PEDAL_CALIBRATION.md) | The safety-critical runbook |
| [DATA_LOGS.md](https://github.com/isc-fs/can-flasher/blob/main/docs/DATA_LOGS.md) | Pulling microSD logs |
| [SAFETY.md](https://github.com/isc-fs/can-flasher/blob/main/docs/SAFETY.md) | Every operation that writes to a board, and what guards it |

The existing reference docs had drifted. Corrected:

- The README advertised **v1.2.0** as the current release, and its
  subcommand table was missing `logs`, `pit-diag` and `provision`.
- `USAGE.md` described `pit-diag` as AMS-only with a hardcoded profile.
  ECU and uDV have shipped for several releases, and **`listen` — the
  only send-silent mode, the one that is safe on a live car — was
  undocumented entirely.** `logs` had no section at all.
- Nothing anywhere said how `--node-id` resolves. It is an error on
  `flash` and `logs`, defaults to `0x3` on four commands, and is ignored
  by the rest. There is now a table.
- `INSTALL.md` had no path for someone who only wants the app, and the
  unsigned-build prompts were documented only in `UPDATES.md`, where
  nobody installing for the first time would look.
- The installer asset names were wrong in one place — Windows ships an
  NSIS `-setup.exe`, not an `.msi`.
- `CONTRIBUTING.md` conflated the six version files you must bump with
  the five that CI actually checks. `Cargo.lock` is the ungated one.

## Also in this cycle

[#558](https://github.com/isc-fs/can-flasher/issues/558) — the ECU DBC
snapshot was behind upstream and the decoder with it. Shipped in
v2.11.0's line but closed now: `0x700` lost `ev_2_3` (EV.2.3 was deleted
in FS-Rules 2024) and gained `power_capped`; `0x704` gained
`stub_torque_cap`; `0x706` grew from DLC 4 to 7; `0x708` from 6 to 7.

**Full changelog**: https://github.com/isc-fs/can-flasher/compare/v2.11.0...v2.11.1
