# Watching a live car

Everything in this document is read-only except where it says otherwise. The
tool's **Observe** views and the `pit-diag listen` mode never transmit, which is
what makes them safe to point at a running car.

## The safe mode: `pit-diag listen`

```bash
can-flasher --interface pcan --channel PCAN_USBBUS1 pit-diag listen
```

`listen` is **send-silent**. It never arms anything and never writes a frame, so
it is the mode to reach for when the car is live. It decodes the frames boards
broadcast without being asked — the ECU health frame `0x704` and the AMS health
frame `0x6CA` — which means it answers *"is this board's app alive?"* the moment
the board powers up.

| Flag | Meaning |
|---|---|
| `--profile all\|ecu\|ams` | which board's frames to decode (default `all`) |
| `--duration-ms` | stop after N ms; omit to run until Ctrl-C |

Add `--json` for NDJSON, one object per line, for piping into `jq`.

## The arming modes

`enable`, `disable` and `stream` **arm a diagnostic stream on the target** —
they transmit. The board starts emitting a much larger set of frames.

| Board | Arm ID | ACK ID | Stream |
|---|---|---|---|
| AMS | `0x7F0` | `0x7F1` | `0x680`–`0x6CA` |
| ECU | `0x7E0` | `0x7E1` | `0x700`–`0x708` |

The arm payload is `DE AD BE EF`; disarm is all zeros. An ACK whose first byte
is anything other than `0x01` — including an empty payload — means **disabled**.

```bash
can-flasher … pit-diag stream --profile ecu          # arm, print, disarm on exit
can-flasher … pit-diag stream --profile ams --json   # same, as NDJSON
```

`stream` disarms on exit, including on Ctrl-C. If a tool crashes mid-session the
board also clears the flag on reboot.

## In the app

**Telemetry** has a tab per board plus an **All** cockpit showing the three side
by side. The dedicated tabs show more than the cockpit does — firmware health,
the inverter fault layers, and (on the ECU tab) pedal calibration, which is the
one thing in this view that writes. See
[PEDAL_CALIBRATION.md](PEDAL_CALIBRATION.md).

**Board health** shows DTCs and session health.

**Bus monitor** shows raw frames, and decodes them into named signals when you
load a `.dbc`. The DBC is remembered per adapter, keyed on interface + channel.

## Things worth knowing when reading the ECU tab

- **`pedal cal`** on the Firmware health card says whether a *stored* pedal
  calibration is in force. `loaded` is the only value that means yes;
  `defaults`, `invalidFellBack` and `badVersionFellBack` all mean the ECU is
  running compile-time values.
- **The stubs row** only appears on a bench build. Any of `no-AMS`,
  `no-inverter`, `start`, `brake` or `torque-cap` means the ECU is faking an
  input or clamping torque. You should never see it on the car.
- **Inverter fault layers** show the two layers *underneath* the DEM code. If an
  L1 condition is live — `HVIL_Open` especially — **no CAN command will clear
  the fault**, because the root cause is still present. That is the difference
  between a firmware problem and a wiring or interlock problem, and the panel
  says so rather than letting you retry a reset that cannot work.
- **`Power cap`** means the EV 2.2.1 power envelope is limiting torque this
  tick. It replaced an older `EV 2.3` flag — that rule was deleted in FS-Rules
  2024, and the tool showed it for a while after the ECU stopped implementing
  it.
- **`TX dropped`** is sticky since boot: the ECU dropped at least one CAN frame
  from a full transmit queue. The heartbeat the AMS watchdogs to keep the AIRs
  closed rides that same queue, so it is worth taking seriously.

---

## See also

- [DESKTOP.md](DESKTOP.md) · [DATA_LOGS.md](DATA_LOGS.md) · [USAGE.md](USAGE.md)
