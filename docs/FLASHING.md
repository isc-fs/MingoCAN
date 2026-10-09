# Flashing firmware

The main job. Put new firmware on a board over CAN, in about ten seconds, with
no cables to move.

---

## Before your first flash

Three things have to be true:

1. **An adapter is selected** — the Adapters view, with a channel.
2. **The bootloader is on the board.** Flash speaks the *bootloader* protocol.
   A never-programmed board has nothing listening on the bus; it needs
   **Burn bootloader** over SWD first, exactly once in its life. The burn
   always chip-erases: the application, the node-id and anything else stored
   on the chip (e.g. pedal calibration) are wiped.
3. **The build settings point at your firmware.** See
   [Build configuration](#build-configuration) below.

## Doing it

In the sidebar: **Program → Flash**.

1. Pick a **Build profile** — Release or Debug. This substitutes into both the
   build command and the artifact path, so one control switches the whole build.
2. Point at your **Build directory** — the firmware repo.
3. Pick the **Target board** by role: ECU, AMS or uDV.
4. Press **Build & Flash**. To reflash the artifact already on disk without
   rebuilding, use **Flash (skip build)**.

The app builds first, then flashes. Progress is per sector, and the log below
the button is the same output the CLI prints.

On success, with the default options, the board is already running the new
firmware — **Start the app after flashing** is on by default.

## What actually happens

| Stage | What it does |
|---|---|
| **Build** | Runs your build command with `{profile}` substituted |
| **Load** | Parses the ELF/HEX/BIN and works out which sectors it lands in |
| **Connect** | Opens a bootloader session with the target node |
| **Diff** | Asks the board for each sector's CRC and skips the ones that already match |
| **Erase + write** | Only the sectors that changed |
| **Verify** | Re-reads each written sector's CRC |
| **Commit** | A whole-image CRC, so the bootloader marks the app valid |
| **Jump** | Boots the application |

The diff stage is why a re-flash after a one-line change takes a fraction of the
time of a first flash — most sectors are untouched and get skipped.

## Getting a running board into the bootloader

A board running its *application* does not answer the bootloader's `CONNECT`.

**Reboot a running board into the bootloader** is on by default and handles
this: on a connect timeout, the host sends the app-level reboot trigger, waits
for the bootloader to come up, and retries.

> **That trigger resets the board into its bootloader — and the AMS opens its
> HV relays first.** On a car, that is a real state change, not just a software
> reset. It is the correct
> thing to do before flashing — but it is a reason to be on stands.

**The ECU can refuse.** If the car is in the drive ladder it declines the
trigger rather than dropping out of drive underneath you. Connect then fails,
and the refusal shows up on the ECU telemetry tab as
`reboot-to-BL refused (in drive)` — sticky since boot. Leave drive or
power-cycle; forcing `--enter-bootloader always` will not help, because the
refusal is the ECU's decision, not a missed trigger.

To do it by hand from the CLI, the payload selects the board (AMS and ECU share
the ID): `can-flasher send-raw 0x002 B0 07 AD 11` for the AMS,
`can-flasher send-raw 0x002 B0 07 AD 12` for the ECU.

---

## Build configuration

Set these once per firmware repo, in **Settings → Firmware build**.

| Setting | Default | Notes |
|---|---|---|
| Build command | `cmake --build build --config {profile}` | Runs before flashing |
| Build working directory (Flash tab: Build directory) | *(empty → the artifact's folder)* | Where the build command runs |
| Built firmware path | `build/{profile}/firmware.elf` | Use an **absolute** path. A relative path is not resolved against the build directory (only a repo's committed `iscFs.firmwareArtifact` is) |
| Build profile (Flash tab) | Release | Substituted as `Release`/`Debug` for `{profile}` in both fields above |

`{profile}` is substituted in **both** the command and the artifact path, so
switching Release ↔ Debug on the Flash tab switches the whole build without
touching Settings.

> **The defaults are placeholders and are wrong for most projects.** They are a
> generic CMake invocation, not a recommendation. If Flash fails immediately
> with a build error, this is almost always why.

> **A repo's committed config wins.** If the Build directory contains
> `.vscode/settings.json` with `iscFs.buildCommand` and/or
> `iscFs.firmwareArtifact` (the keys the VS Code extension uses), Flash uses
> those instead of the Settings fields. A relative `iscFs.firmwareArtifact` is
> resolved against that directory and is not `{profile}`-substituted. Settings →
> Firmware build then shows *✓ Using committed config from …*. The target board
> is never taken from the repo.

### Firmware formats

| Format | Load address |
|---|---|
| `.elf` | Carried in the file |
| `.hex` | Carried in the file |
| `.bin` | **CLI only** (`flash --address 0x08020000`). The app has no address field and rejects a `.bin` artifact |

---

## When it goes wrong

### "Pick an adapter in the Adapters view first."

No adapter selected. The Adapters view is the fix.

### "Set a firmware artifact path in Settings first."

Settings → Firmware build → Built firmware path.

### The build fails before anything reaches the board

Your build command or build directory is wrong. The defaults are placeholders.
Run the same command in a terminal from the same directory and see what happens.

### Connect times out

In order of likelihood:

1. The board is running its application and **Reboot a running board into the
   bootloader** is off.
2. The adapter isn't wired to the board, or is on the wrong bus.
3. The bitrate is wrong — check Settings → Bus parameters against the car.
4. The board has no bootloader yet. Use **Burn bootloader**.
5. Wrong node ID. `discover` on the CLI lists every board in bootloader mode.
6. **The ECU refused the reboot trigger** because the car is in the drive
   ladder. Check the ECU telemetry tab for `reboot-to-BL refused (in drive)`.

### Protection violation

The firmware's linker script targets the bootloader's own sector. The tool
refuses rather than bricking the board — this is the guard working. Fix the
linker script; the application starts at `0x08020000`.

### It flashed, but the board doesn't run the app

Check whether **Start the app after flashing** was on. If it was off, the board
is sitting in the bootloader, which is a valid state — it just needs a jump or a
power cycle.

If it *was* on and the app still isn't running, the image may be invalid. Flash
again with **Confirm the whole image at the end** on; that is the step that
marks the app valid.

### A flash was interrupted

The app region may be partially written and the board will stay in the
bootloader rather than run a corrupt app. Just flash again. Nothing is bricked
— the bootloader lives in a protected sector and is not touched by an
application flash.

---

## Commissioning a new board

A board fresh off the bench is commissioned in **one SWD step**: the bootloader
and its node ID go on together.

- **App:** *Burn bootloader*, with **Provision node-id** set to the board's role.
- **CLI:** `can-flasher swd-flash CAN_BL.elf --provision <role>` (needs a CLI
  built with `--features swd`) — see
  [`swd-flash --provision`](CLI.md#swd-flash---provision-role0xn--burn-and-provision-in-one-step).

The node ID is written as a provisioning seed that the bootloader adopts into
its NVM on first boot. This needs a bootloader built with seed support
(stm32-can-bootloader#183); an older one is refused before the chip is erased.
Burned without a role, the board answers as **`0x01`** — the stock bootloader's
compile-time default, which is the **ECU's** address — so on a shared bus it
collides with the ECU until it is provisioned. Provision at burn time.

To change the node ID of a board that is already running, use
[`provision`](CLI.md#provision--assign-a-node-id-by-role) over CAN.

---

## Doing it from the CLI

Everything above is [`can-flasher flash`](CLI.md#flash--program-firmware). The
short form:

```bash
can-flasher --interface pcan --channel PCAN_USBBUS1 --node-id 0x01 \
  flash build/release/firmware.elf
```

`--node-id` is **mandatory** for `flash` — it is the one command that will not
guess which board to overwrite.

---

## See also

- [DESKTOP.md](DESKTOP.md) — the rest of the app
- [SAFETY.md](SAFETY.md) — everything that writes to a board
- [CLI.md](CLI.md) — `flash`, `verify`, `provision`, `swd-flash`
- [PERFORMANCE.md](PERFORMANCE.md) — throughput measurements
