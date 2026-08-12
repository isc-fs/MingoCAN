# What writes to a board

Most of this tool only listens. This page lists everything that does not, what
each one changes, and what actually stands between you and it.

The desktop app enforces the same split visually: the **Program** sidebar group
writes, the **Observe** group does not. There are no exceptions — nothing in
Observe transmits.

---

## Firmware flash

`can-flasher flash` · **Flash** view

Writes the application firmware. On a board that was mid-flash when something
went wrong, the app region may be partially written and the device will not boot
the app until you flash again — diff mode re-sends only what changed.

**Guard:** none beyond your intent. This is the routine operation the tool
exists for.

## Bootloader burn (SWD)

`can-flasher swd` · **Burn bootloader** view

First-boot only, over SWD rather than CAN. A blank board has nothing listening
on the bus, so this is the only way in.

## Node-ID provisioning

`can-flasher provision <role>`

Writes the board's node ID into bootloader NVM and resets it so the new ID takes
effect. Get this wrong and the board answers on a different address than you
expect — recoverable, but confusing.

**Guard:** an interactive confirmation, skippable with `--yes` for scripting.
`--no-reset` writes without rebooting, for chaining several writes; the last one
should still reset.

## NVM write

`can-flasher config nvm write <key> <value>`

Writes a single key into bootloader NVM — node ID, CAN bitrate, and similar.

## NVM format

`can-flasher config nvm format`

**Erases the entire NVM sector — every key.** That includes the board's node ID
and its stored pedal calibration.

**Guard:** an interactive confirmation naming the node, skippable with `--yes`.

> A formatted board has lost its node ID as well as its stored pedal
> calibration. Re-provisioning brings the node ID back; the calibration cannot
> be restored with this tool, which no longer writes one. Note the distinction:
> a stored calibration survives an **application** reflash, but not an NVM
> format and not a full chip erase.

## Option bytes / write protection

`can-flasher config ob apply-wrp`

Applies write protection to flash sectors and resets the device. Session-gated,
and the tool auto-fills the brick-safety token so you do not have to handle it.

> **On recent H7 silicon, write protection is clearable only by a full chip
> erase.** Applying WRP to the wrong sectors can therefore cost you everything
> on the part, including the bootloader. `--allow-app-sectors` exists because
> protecting application sectors is a deliberate, unusual act — if you find
> yourself reaching for it, be sure.

---

## Rules of thumb

1. **Anything in Observe is safe on a live car.** `pit-diag listen` is
   send-silent by design. `enable` and `stream` transmit — they arm a stream —
   so they are not in that category.
2. **On stands for anything that writes.**
3. **`--yes` removes the only guard some of these have.** Use it in scripts,
   not at the car.
4. **Check afterwards, not just before.** `pedal cal` on the Firmware health
   card tells you whether the ECU is running a stored calibration or its
   compile-time defaults — read-only, but the fastest way to see what a board
   is actually running.

---

## See also

- [DESKTOP.md](DESKTOP.md) · [TELEMETRY.md](TELEMETRY.md) · [USAGE.md](USAGE.md)
