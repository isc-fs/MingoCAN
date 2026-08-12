# MingoCAN desktop app

The desktop app is the main way to use this tool. If you are flashing a board,
watching telemetry, pulling logs, or calibrating pedals, you want the app — the
[CLI](USAGE.md) exists for scripting and bench automation.

Install it from the [latest release](https://github.com/isc-fs/can-flasher/releases/latest):
grab the `.dmg` (macOS, Apple Silicon), `-setup.exe` (Windows) or the
`.AppImage` / `.deb` / `.rpm` (Linux). Full per-platform steps, including the
one-time "unidentified developer" prompt, are in [INSTALL.md](INSTALL.md).

---

## First run: pick an adapter

The app opens on **Adapters**. Nothing else works until you choose one, so start
here.

The list shows every CAN backend found on this machine, tagged by kind —
`slcan`, `socketcan`, `pcan`, `vector` — plus a **`virtual`** entry that is
always present. Virtual is an in-process loopback with no hardware behind it;
it is for smoke-testing the tool itself against the bootloader stub, not for
talking to a car.

Discovery runs when the view opens and when you press **⟳**. It does not poll in
the background, so if you plug an adapter in while the app is running, press
refresh — the list will not update on its own.

## The one precondition everything shares

Every other view needs **an interface selected, and that interface to have a
channel**. The only exception is `virtual`, which is channel-less by nature.

Views that do not have this show a banner with a **Choose adapter →** link
rather than failing when you press a button.

> **The status bar shows the adapter you selected, not a live connection.**
> The app does not hold the bus open between actions — it opens the adapter for
> each operation and releases it. A green-looking status bar is not evidence
> that a board is powered, connected, or answering. This is the single most
> common source of "is it connected?" confusion.
>
> On startup the app re-checks that your saved adapter is still present, and
> swaps to a **No adapter detected → Select adapter →** strip if it has gone.

## The views

The sidebar has two groups. **Program** writes to a board; **Observe** only
reads. That split is deliberate — it is what makes it safe to open anything in
Observe on a live car.

```
Adapters                 ← pinned top, start here

PROGRAM  (writes to the car)
  Flash                  firmware over CAN
  Burn bootloader        first-boot bootloader via SWD
OBSERVE  (read-only)
  Board health           DTCs & session health
  Bus monitor            live frames & DBC-decoded signals
  Telemetry              live AMS / ECU / uDV telemetry
  Data logs              pull microSD logs over CAN

Settings                 ← pinned bottom
```

### Flash

Builds and flashes firmware over CAN. Choose Release or Debug, point it at a
build directory, pick the target board by role, and go.

Six advanced toggles, all off by default unless noted: **diff** (send only
changed pages), **verify after**, **final commit**, **jump** (start the app when
done), **enter bootloader** (trigger the target into its bootloader first), and
**dry run**.

Two errors you will meet if you skip a step, quoted so they are searchable:

- *"Pick an adapter in the Adapters view first."*
- *"Set a firmware artifact path in Settings first."*

### Burn bootloader

First-boot only: puts the bootloader onto a blank board over SWD, not CAN. A
board that has never been programmed cannot be reached by Flash, because there
is nothing on it listening to the bus yet.

### Board health

DTCs and session health for a connected board.

### Bus monitor

Every frame on the bus, with DBC decoding when you load a `.dbc`. The DBC you
choose is remembered **per adapter** — keyed on interface plus channel — so a
DBC picked for the powertrain bus does not follow you to a different adapter.

### Telemetry

Live telemetry from the AMS, ECU and uDV, with a tab per board and an **All**
cockpit that shows the three side by side. See [TELEMETRY.md](TELEMETRY.md).

**Pedal calibration lives inside this view**, on the dedicated ECU tab — it is
the one thing here that writes to the car. It has its own runbook:
[PEDAL_CALIBRATION.md](PEDAL_CALIBRATION.md).

### Data logs

Pulls microSD logs off a board over CAN. See [DATA_LOGS.md](DATA_LOGS.md) —
in particular the note that a full card takes minutes per file, not seconds.

## Node IDs and roles

Boards are addressed by node ID. The app lets you pick by role:

| Role | Node ID |
|---|---|
| ECU | `0x01` |
| AMS | `0x02` |
| uDV | `0x03` |

## Where your settings live

Settings are written to `settings.json` in the OS application-config directory,
saved automatically 250 ms after your last change. Defaults: bitrate **500000**,
node ID **0x3**, timeout **500 ms**; Bus monitor keeps **5000** rows and opens on
the **signals** tab.

---

## See also

- [PEDAL_CALIBRATION.md](PEDAL_CALIBRATION.md) — the safety-critical one
- [TELEMETRY.md](TELEMETRY.md) — watching a live car
- [DATA_LOGS.md](DATA_LOGS.md) — pulling logs
- [USAGE.md](USAGE.md) — the CLI
- [SAFETY.md](SAFETY.md) — everything that writes to a board
