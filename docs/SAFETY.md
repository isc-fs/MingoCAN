# What writes to a board

Most of MingoCAN only listens. This page lists everything that does not, what
each one changes, and what actually stands between you and it.

The app makes the split visible: the **Program** sidebar group writes, the
**Observe** group does not. There are no exceptions — nothing in Observe
transmits.

---

## Rules of thumb

1. **Anything in Observe is safe on a live car.** On the CLI, the equivalent is
   `pit-diag listen`, which is send-silent by design. `enable` and `stream` arm
   a stream, so they transmit and are not in that category.
2. **On stands for anything that writes.**
3. **`--yes` removes the only guard some CLI commands have.** Use it in scripts,
   not at the car.
4. **The reboot-to-bootloader trigger is a real state change.** It opens the
   board's HV relays and resets it. It is on by default in Flash, correctly —
   but it is a reason to be on stands.

---

## Firmware flash

**Flash** view · `can-flasher flash`

Writes the application firmware. On a board interrupted mid-flash, the app
region may be partially written; the board stays in the bootloader rather than
running a corrupt app, and flashing again fixes it.

**Guard:** none beyond your intent. This is the routine operation the tool
exists for. The bootloader lives in a protected sector and an application flash
cannot touch it — an image whose linker script tries is rejected with exit 3.

## Bootloader burn (SWD)

**Burn bootloader** view · `can-flasher swd-flash`

First-boot only, over SWD rather than CAN. A blank board has nothing listening
on the bus, so this is the only way in.

## Node-ID provisioning

`can-flasher provision <role>` — also offered by the app after a successful
flash to a board with a known role.

Writes the board's node ID into bootloader NVM and resets it so the new ID takes
effect. Get it wrong and the board answers on a different address than you
expect — recoverable, but confusing.

**Guard:** an interactive confirmation, skippable with `--yes` for scripting.
`--no-reset` writes without rebooting, for chaining several writes; the last one
should still reset.

## Clearing DTCs

**Board health** view · `can-flasher diagnose clear-dtc`

Discards the board's stored fault codes. They are evidence — clear them after
you have read them, not before.

**Guard:** a confirmation, skippable with `--yes`.

## Device reset

`can-flasher diagnose reset`

Resets the board. Fine on a bench, a real event on a car.

## NVM write

`can-flasher config nvm write <key> <value>`

Writes a single key into bootloader NVM — node ID, CAN bitrate, and similar.

## NVM format

`can-flasher config nvm format`

**Erases the entire NVM sector — every key.** That includes the board's node ID
and its stored pedal calibration.

**Guard:** an interactive confirmation naming the node, skippable with `--yes`.

> A formatted board has lost its node ID as well as its stored pedal
> calibration. Re-provisioning brings the node ID back; **the calibration cannot
> be restored with this tool**, which no longer writes one. Note the
> distinction: a stored calibration survives an *application* reflash, but not
> an NVM format and not a full chip erase.

## Option bytes / write protection

`can-flasher config ob apply-wrp`

Applies write protection to flash sectors and resets the device. Session-gated,
and the tool auto-fills the brick-safety token so you do not have to handle it.

> **On recent H7 silicon, write protection is clearable only by a full chip
> erase.** Applying WRP to the wrong sectors can therefore cost you everything
> on the part, including the bootloader. `--allow-app-sectors` exists because
> protecting application sectors is a deliberate, unusual act — if you find
> yourself reaching for it, be sure.

## Arming a telemetry stream

**Telemetry** view (the arm control) · `can-flasher pit-diag enable` / `stream`

Does not write to storage, but **does transmit**: it tells a board to start
emitting its full diagnostic frame set, which adds real load to the bus.

**Guard:** none. It is reversible — disarm, or reboot the board — but it is not
a passive act. Use `listen` when the car is live.

---

## What MingoCAN can no longer do

**Pedal calibration was removed in v2.12.0.** The wizard shipped in v2.10.0 and
was never used to successfully calibrate a car, so it was pulled rather than
left looking available.

A calibration already stored in an ECU's NVM stays there and keeps working. This
tool can no longer write, change or reset one — and since `config nvm format`
still erases it, there is now no way to put one back from here.

The read-only `pedal cal` field on the ECU telemetry tab remains, and reports
whether the ECU is running a stored calibration or its compile-time defaults.

---

## See also

- [FLASHING.md](FLASHING.md) · [TELEMETRY.md](TELEMETRY.md) · [DESKTOP.md](DESKTOP.md) · [CLI.md](CLI.md)
