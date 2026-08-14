# The MingoCAN app

Every view, every setting, and the handful of things that are not obvious from
looking at the window.

New here? [INSTALL.md](INSTALL.md) first, then this.

---

## The shape of it

A left rail with eight entries, and a split that carries meaning:

```
Adapters              ← pinned top; start here

PROGRAM   (writes to the car)
  Flash                 Build & flash firmware over CAN
  Burn bootloader       First-boot bootloader via SWD

OBSERVE   (read-only)
  Board health          DTCs & session health
  Bus monitor           Live CAN frames & DBC signals
  Telemetry             Live AMS / ECU / uDV telemetry
  Data logs             Pull microSD car-data logs over CAN

Settings              ← pinned bottom
```

**Everything under Observe is safe to open on a live car.** Nothing in that
group transmits. That is the whole reason the sidebar is grouped this way.

The sidebar also shows the running version under the product name. It reads the
same field the release workflow's version gate compares the git tag against, so
what you see there is genuinely the release you installed.

---

## Adapters — start here

The app opens on this view, and nothing else works until you pick something.

The list shows every CAN backend found on this machine, tagged by kind —
`slcan`, `socketcan`, `pcan`, `vector` — plus a **`virtual`** entry that is
always present. Virtual is an in-process loopback with no hardware behind it:
for smoke-testing the tool, not for talking to a car.

**Discovery does not poll.** It runs when the view opens and when you press
**⟳**. Plug an adapter in while the app is running and the list will not notice
until you refresh.

> ### The status bar shows your *selection*, not a *connection*
>
> The app does not hold the bus open between actions. It opens the adapter for
> each operation and releases it afterwards. A populated status bar means "you
> picked this adapter", not "a board is powered and answering".
>
> This is the single most common source of *"is it connected?"* confusion, and
> it is deliberate — holding the port open would lock out every other tool on a
> shared bench.
>
> On startup the app re-checks that your saved adapter is still present, and
> swaps to a **No adapter detected → Select adapter →** strip if it has gone.

Every other view needs an interface **and a channel**. The only exception is
`virtual`, which is channel-less by nature. Views without one show a banner with
a **Choose adapter →** link rather than failing when you press a button.

---

## Program

### Flash

Builds and flashes firmware over CAN. This is the tool's main job and has its
own guide: **[FLASHING.md](FLASHING.md)**.

The short version: pick **Build profile** (Release or Debug), point at a
**Build directory**, choose the **Target board** by role, press Flash. The build
command and artifact path live in Settings, not here, because you set them once
per project and then stop thinking about them.

Under **Advanced options**, six toggles. Five are **on** by default — the
defaults are the safe, fast, normal path:

| Option | Default | What it does |
|---|---|---|
| Skip unchanged sectors | **on** | Only rewrite sectors whose contents changed. Much faster reflashes. |
| Verify each sector after writing | **on** | Read each sector's CRC back and compare it. |
| Confirm the whole image at the end | **on** | Final whole-image CRC, so the bootloader marks the new app valid. |
| Start the app after flashing | **on** | Boot straight into what you just flashed. |
| Reboot a running board into the bootloader | **on** | If the board is running its application, send the reboot trigger so you don't have to reach for a reset button. |
| Dry run — no erases or writes | off | Walk the whole pipeline and send nothing. A rehearsal. |

If the **Target board** you picked is a known role, you'll be asked after a
successful flash whether to commission the board with that role's node ID. It's
skippable — routine reflashes don't need it.

### Burn bootloader

First-boot only, over **SWD** rather than CAN. A board that has never been
programmed has nothing listening on the bus, so this is the only way in.

Three sections: **Probe** (the ST-LINK), **Firmware** (the bootloader image),
and **Options**. Needs the ST-LINK setup from
[INSTALL.md](INSTALL.md#st-link--swd-optional).

Afterwards the board answers on CAN at the unprovisioned address `0xF`, and
Flash works normally.

---

## Observe

### Board health

Two cards.

**Session health** is a one-shot snapshot from the bootloader: uptime, buffer
state, and how the current session is doing.

**Diagnostic Trouble Codes** lists what the board has stored and lets you clear
them. Reading is free; clearing is the one thing in this view that changes
anything on the board, and it asks first.

### Bus monitor

Every frame on the bus, three ways:

| Tab | Shows |
|---|---|
| **Signals** | DBC-decoded named signals. The default. |
| **By ID** | One row per CAN ID with counts — the fastest way to see what is actually talking. |
| **Live frames** | The raw stream. |

The Signals tab needs a `.dbc`. **The DBC you load is remembered per adapter**,
keyed on interface *plus* channel — so a DBC chosen for the powertrain bus does
not follow you to a different adapter, or to a different channel on the same
one.

Live frames keeps 5000 rows by default and drops the oldest beyond that, which
bounds memory on a busy bus. Raise it in Settings if you're chasing something
rare, and expect the window to get heavier.

### Telemetry

Live telemetry from the AMS, ECU and uDV: a tab per board, plus an **All**
cockpit showing the three side by side. The dedicated tabs show more than the
cockpit does — firmware health, inverter fault layers, per-cell detail.

This view has modes that transmit and modes that do not, and the difference
matters on a live car. **[TELEMETRY.md](TELEMETRY.md)** is the guide.

### Data logs

Pulls the car-data logs off a board's microSD card over CAN, so you don't have
to open anything up. Read-only — there is no delete.

Two things trip people up: an unsealed log doesn't appear in a listing, and a
full card is a coffee break rather than a pause. **[DATA_LOGS.md](DATA_LOGS.md)**
covers both.

---

## Settings

Five sections. Changes save automatically 250 ms after you stop typing — there
is no Save button, and that is not an oversight.

| Section | What's in it |
|---|---|
| **Selected adapter** | Which adapter is active, mirrored from the Adapters view |
| **Bus parameters** | Bitrate, default node ID, reply timeout |
| **Firmware build** | Build command, build directory, artifact path, build profile |
| **DBC files (per-adapter)** | The interface+channel → `.dbc` associations |
| **About** | Version, links, update check |

### Defaults worth knowing

| Setting | Default | Note |
|---|---|---|
| Bitrate | `500000` | |
| Node ID | `0x3` | uDV. The fallback where a view doesn't ask you explicitly. |
| Reply timeout | `500` ms | Per command, covering a whole reassembled message — not per CAN frame. |
| Artifact path | `build/{profile}/firmware.elf` | `{profile}` is substituted with the chosen build profile |
| Build command | `cmake --build build --config {profile}` | Same substitution |
| Build profile | `release` | |
| Bus monitor rows | `5000` | |
| Bus monitor tab | `signals` | Which tab you land on after a restart |

> **The default build command is a generic CMake invocation and is wrong for
> most projects.** It is a placeholder, not a recommendation. Set it once per
> firmware repo — see [FLASHING.md](FLASHING.md#build-configuration).

### Where settings live

A `settings.json` in the OS application-config directory:

| OS | Path |
|---|---|
| macOS | `~/Library/Application Support/com.iscracingteam.can-studio/` |
| Windows | `%APPDATA%/com.iscracingteam.can-studio/` |
| Linux | `~/.config/com.iscracingteam.can-studio/` |

Deleting that file resets everything to the defaults above. It is a reasonable
first move when the app is behaving strangely.

---

## Node IDs and roles

Boards are addressed by a 4-bit node ID. The app lets you pick by role:

| Role | Node ID |
|---|---|
| ECU | `0x01` |
| AMS | `0x02` |
| uDV | `0x03` |

A board that has never been commissioned answers on `0xF`. Give it a real ID
with the CLI's [`provision`](CLI.md#provision--assign-a-node-id-by-role)
command, or accept the prompt the Flash view offers after a successful flash.

---

## Updates

The app checks for updates and can install them itself. See
[UPDATES.md](UPDATES.md) — including the macOS quirk where an updated build
re-triggers the unidentified-developer prompt.

---

## See also

- [FLASHING.md](FLASHING.md) — the main job, end to end
- [TELEMETRY.md](TELEMETRY.md) — watching a live car
- [DATA_LOGS.md](DATA_LOGS.md) — pulling logs
- [SAFETY.md](SAFETY.md) — everything that writes to a board
- [CLI.md](CLI.md) — the same capabilities, scriptable
