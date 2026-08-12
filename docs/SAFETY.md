# What writes to a board

Most of this tool only listens. This page lists everything that does not, what
each one changes, and what actually stands between you and it.

The desktop app enforces the same split visually: the **Program** sidebar group
writes, the **Observe** group does not. Calibration is the exception — it lives
inside a view in Observe because it needs that view's live data, so it carries
its own confirmations instead of inheriting the group's posture.

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

## Pedal calibration

**Telemetry → ECU tab.** Writes eight values to ECU NVM that gate the torque
cut, ready-to-drive arming, and the EBS verdict sent to the uDV.

**Guards:** the ECU refuses to open a session unless the tractive system is
inactive, the FSM is out of `Active`, motor rpm is zero and commanded torque is
zero — re-checked on every capture and on commit, discarding the staged set if
it ever fails. Seven validation rules run before anything is written, and a
consistency CRC ensures the tool and the ECU agree on what was captured. The app
adds a wheels-off-ground warning, a full before/after review, an explicit
confirmation checkbox, and an automatic read-back afterwards.

**Not guarded:** sample stability. The ECU accepts whatever it is handed — the
app's stability gate is the only such check in the system.

Full runbook: [PEDAL_CALIBRATION.md](PEDAL_CALIBRATION.md).

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

> A formatted board has lost its node ID as well as its calibration. Recovery
> means re-provisioning *and* re-calibrating, in that order. Bear this in mind
> when reading "calibration survives a reflash" — it survives an **application**
> reflash, not an NVM format and not a full chip erase.

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
2. **On stands for anything that writes.** Especially calibration.
3. **`--yes` removes the only guard some of these have.** Use it in scripts,
   not at the car.
4. **Check afterwards, not just before.** Calibration has a read-back;
   `pedal cal` on the Firmware health card is the independent confirmation that
   what you committed is actually in force.

---

## See also

- [PEDAL_CALIBRATION.md](PEDAL_CALIBRATION.md) · [DESKTOP.md](DESKTOP.md) · [USAGE.md](USAGE.md)
