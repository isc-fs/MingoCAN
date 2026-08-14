# Watching a live car

MingoCAN's **Telemetry** view shows what the AMS, ECU and uDV are doing, right
now. Most of it is read-only. The part that isn't is worth understanding before
you point it at a running car.

---

## The one thing to know

There are two ways to get telemetry, and they are not equally safe:

| | Transmits? | Safe on a live car? |
|---|---|---|
| **Listening** — decode what boards broadcast anyway | No | **Yes** |
| **Arming a stream** — ask a board to emit its full frame set | **Yes** | On stands |

Boards broadcast a health frame without being asked — the ECU's `0x704` and the
AMS's `0x6CA`. Reading those tells you whether a board's application is alive
the moment it powers up, and costs the bus nothing.

Everything else — every cell voltage, every NTC, the FSM state, the inverter
fault layers — only appears once you **arm** the stream, which means sending a
frame to the car.

## In the app

**Observe → Telemetry.** A tab per board — AMS, ECU, uDV — plus an **All**
cockpit showing the three side by side.

The dedicated tabs show more than the cockpit does: firmware health, the
inverter fault layers, per-cell detail. The cockpit is for watching; the tabs
are for diagnosing.

Arming is an explicit action on each tab. Until you arm, you're seeing the
ungated health frame and nothing else — which is often all you need.

## Reading the ECU tab

A handful of fields that mean more than they appear to:

- **`pedal cal`** reports whether the ECU is running a *stored* pedal
  calibration or fell back to compile-time values. `loaded` is the only value
  meaning a stored one is in force; `defaults`, `invalidFellBack` and
  `badVersionFellBack` all mean compile-time. MingoCAN cannot write a
  calibration — as of v2.12.0 that feature is gone — so treat this as a fact
  about the firmware on the board.

- **The stubs row** only appears on a bench build. Any of `no-AMS`,
  `no-inverter`, `start`, `brake` or `torque-cap` means the ECU is faking an
  input or clamping torque. **You should never see this on the car.**

- **Inverter fault layers** show the two layers *underneath* the DEM code. If an
  L1 condition is live — `HVIL_Open` especially — **no CAN command will clear
  the fault**, because the root cause is still physically present. That is the
  difference between a firmware problem and a wiring or interlock problem, and
  the panel says so rather than letting you retry a reset that cannot work.

- **`Power cap`** means the EV 2.2.1 power envelope is limiting torque this
  tick. It replaced an older `EV 2.3` flag — that rule was deleted in FS-Rules
  2024, and the tool kept showing it for a while after the ECU stopped
  implementing it.

- **`TX dropped`** is sticky since boot: the ECU dropped at least one CAN frame
  from a full transmit queue. The heartbeat the AMS watchdogs to keep the AIRs
  closed rides that same queue, so it is worth taking seriously.

## The other Observe views

**Board health** shows the bootloader's session health and stored DTCs. Reading
is free; clearing DTCs is the one write, and it asks first.

**Bus monitor** shows raw frames and decodes them into named signals when you
load a `.dbc`. The DBC is remembered per adapter, keyed on interface + channel.
Neither transmits.

---

## From the CLI

```bash
# Safe on a live car — never sends a frame.
can-flasher --interface pcan --channel PCAN_USBBUS1 pit-diag listen

# Arms the stream on the target. Not for a live car.
can-flasher … pit-diag stream --profile ecu
```

`listen` is send-silent by design. `enable`, `disable` and `stream` all
transmit. Full flag reference:
[CLI.md § pit-diag](CLI.md#pit-diag--telemetry-observer).

| Board | Arm ID | ACK ID | Stream range |
|---|---|---|---|
| AMS | `0x7F0` | `0x7F1` | `0x680`–`0x6CA` |
| ECU | `0x7E0` | `0x7E1` | `0x700`–`0x708` |
| uDV | `0x7DE` | `0x7DF` | `0x7A0`–`0x7A9` |

The arm payload is `DE AD BE EF`; disarm is all zeros. An ACK whose first byte
is anything other than `0x01` — including an empty payload — means **disabled**.
`stream` disarms on exit including on Ctrl-C, and a board clears the flag on
reboot if the tool dies first.

---

## See also

- [DESKTOP.md](DESKTOP.md) · [DATA_LOGS.md](DATA_LOGS.md) · [SAFETY.md](SAFETY.md) · [CLI.md](CLI.md)
