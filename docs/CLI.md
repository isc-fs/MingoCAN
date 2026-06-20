# The `can-flasher` CLI

Everything [the app](DESKTOP.md) does, scriptable — plus a few things it
doesn't: NVM key-value access, option bytes, session record/replay, and raw
frame injection.

Reach for the CLI when you are automating (CI, deploy scripts, bench
harnesses), working on a headless box, or want output you can pipe into `jq`.
Reach for the app when a human is doing the work.

> **The binary is still named `can-flasher`.** MingoCAN is the product; this is
> its engine, and renaming it would break every script that calls it.

Installing it: [INSTALL.md § The CLI](INSTALL.md#the-cli). The authoritative
spec — every flag, every opcode, every JSON schema field — is
[REQUIREMENTS.md](../REQUIREMENTS.md); this page is the 80 % path.

---

## Shape of a command

```
can-flasher [GLOBAL OPTIONS] <COMMAND> [ARGS]
```

| Command | Purpose |
|---|---|
| [`adapters`](#adapters--detect-can-hardware) | List detected CAN adapters on this machine |
| [`discover`](#discover--scan-the-bus) | Scan the bus, table every bootloader-mode device |
| [`flash`](#flash--program-firmware) | Program firmware end-to-end |
| [`verify`](#verify--compare-against-a-binary) | Compare the installed image against a file |
| [`diagnose`](#diagnose--dtcs-logs-live-data-health) | DTCs, log stream, live data, health, reset |
| [`config`](#config--nvm-and-option-bytes) | NVM key-value store + option bytes / WRP |
| [`provision`](#provision--assign-a-node-id-by-role) | Assign a board's node ID by role name |
| [`logs`](#logs--pull-microsd-car-data-logs) | List / seal / pull microSD car-data logs |
| [`pit-diag`](#pit-diag--telemetry-observer) | Observe AMS / ECU / uDV telemetry |
| [`send-raw`](#send-raw--one-raw-frame) | Send one raw CAN frame |
| [`replay`](#replay--record-and-read-sessions) | Record or replay a CAN session |
| `swd-flash` | First-boot a bare STM32 via ST-LINK *(opt-in: `--features swd`)* |

### Global options

| Option | Meaning |
|---|---|
| `-i, --interface <TYPE>` | `slcan` \| `socketcan` \| `pcan` \| `vector` \| `virtual` *(default `slcan`)* |
| `-c, --channel <CHANNEL>` | Adapter channel — format depends on backend and OS |
| `-b, --bitrate <BPS>` | Nominal CAN bitrate *(default `500000`)* |
| `--node-id <ID>` | Target node, hex `0x0A` or decimal `10`. See below. |
| `--timeout <MS>` | Reply timeout *(default `500`)* |
| `--json` | Machine-readable output on stdout |
| `--log <PATH>` | Append the session to a SQLite audit log |
| `--operator <NAME>` | Override the operator name recorded in that log |
| `--verbose` | Trace-level logging |

> `--timeout` is **per command**, covering a whole reassembled ISO-TP message —
> not per CAN frame. A command that moves a lot of data does not need a
> proportionally larger timeout.

### How `--node-id` resolves

It is global, but each subcommand treats an omitted value differently. There is
no single default.

| Command | If you omit `--node-id` |
|---|---|
| `flash` | **error** — the one command that will not guess which board to overwrite |
| `logs` | **error** — but the message is generic and the exit code is the catch-all **99**, not a targeted hint |
| `verify`, `diagnose`, `config` | defaults to `0x3` |
| `provision` | *target* defaults to `0x3`; the value *written* comes from the role argument |
| `discover`, `pit-diag`, `replay`, `adapters` | not used — broadcast or passive |

Roles: ECU `0x01`, AMS `0x02`, uDV `0x03`. A board that has never been
commissioned answers on `0xF`.

Every subcommand has its own `--help` with the complete flag list.

---

## First flash, end to end

Four steps, each checking the one before it, so you know which one broke:

```bash
# 1. Is the adapter visible to the OS?
can-flasher adapters

# 2. Is a bootloader listening on the bus?
can-flasher --interface slcan --channel /dev/ttyACM0 discover

# 3. Program it, verify each sector, jump to the app.
can-flasher --interface slcan --channel /dev/ttyACM0 --node-id 0x01 \
  flash build/firmware.elf

# 4. Optional post-deploy gate: exits 0 on match, 2 on mismatch.
can-flasher --interface slcan --channel /dev/ttyACM0 --node-id 0x01 \
  verify build/firmware.elf
```

Step 1 empty → adapter/OS setup ([INSTALL.md](INSTALL.md)). Step 2 times out →
wiring or bitrate. Step 3 exits 3 → the linker script targets the bootloader's
sector.

---

## `adapters` — detect CAN hardware

```bash
can-flasher adapters
can-flasher adapters --json | jq
```

Reports per family, and distinguishes "no adapter plugged in" from "the SDK for
this family is missing" — they are different problems.

## `discover` — scan the bus

Broadcasts and tables every device currently in bootloader mode: node ID,
bootloader version, chip ID.

```bash
can-flasher --interface pcan --channel PCAN_USBBUS1 discover
```

The fastest answer to *"is anything alive on this bus, and at what address?"*

## `flash` — program firmware

Loads an ELF / HEX / raw BIN, opens a session, then runs the sector-aware
diff → erase → write → verify pipeline and optionally jumps to the application.

```bash
# Typical
can-flasher --interface slcan --channel /dev/ttyACM0 --node-id 0x01 \
  flash build/firmware.elf

# Raw .bin needs an address — ELF and HEX carry their own
can-flasher … --node-id 0x01 flash build/firmware.bin --address 0x08020000

# CI: rehearse, emit JSON, leave the board in the bootloader
can-flasher --json --interface virtual \
  flash build/firmware.elf --dry-run --no-jump
```

| Flag | Effect |
|---|---|
| `--address <HEX>` | Load address for a raw `.bin` (ignored for ELF / HEX) |
| `--diff` *(default)* / `--no-diff` | Skip sectors whose device-side CRC already matches / force-write everything |
| `--verify-after` *(default)* / `--no-verify-after` | Re-read each written sector and CRC-match it |
| `--jump` *(default)* / `--no-jump` | Boot the application afterwards, or stay in the bootloader |
| `--dry-run` | Plan and validate, send no erase or write |
| `--enter-bootloader <auto\|always\|never>` *(default `auto`)* | See below |
| `--require-wrp` | Abort (exit 7) if the bootloader sector isn't write-protected |
| `--apply-wrp` | Latch WRP first if it isn't |
| `--keepalive-ms <MS>` | Session keepalive interval |
| `--profile` | Print per-stage timings |
| `--yes` | Skip interactive confirmations |

> **`--enter-bootloader`.** `flash` speaks the bootloader protocol, and a board
> running its *application* will not answer `CONNECT`. `auto` tries CONNECT and,
> only on timeout, sends the app-level reboot-to-bootloader trigger, waits, and
> retries. `always` sends it up front; `never` fails instead.
>
> **That trigger opens the board's HV relays and then resets it.** By hand:
> `can-flasher send-raw 0x002 B0 07 AD 11`.

Full guide, including what to do when it fails: [FLASHING.md](FLASHING.md).

## `verify` — compare against a binary

Re-reads the installed image and CRC-matches it against a file. Writes nothing.
Exit `0` on match, `2` on mismatch — designed to be a CI gate.

```bash
can-flasher … --node-id 0x01 verify build/firmware.elf
```

## `diagnose` — DTCs, logs, live data, health

```bash
can-flasher … diagnose read-dtc          # stored fault codes
can-flasher … diagnose clear-dtc --yes   # clear them
can-flasher … diagnose log               # stream the bootloader log ring
can-flasher … diagnose live-data         # stream the 32-byte live snapshot
can-flasher … diagnose health            # one-shot session health record
can-flasher … diagnose reset             # reset the device
```

`read-dtc`, `log`, `live-data` and `health` are read-only. `clear-dtc` and
`reset` change the board's state and prompt unless you pass `--yes`.

## `config` — NVM and option bytes

```bash
can-flasher … config nvm read <key>
can-flasher … config nvm write <key> <value>
can-flasher … config nvm erase <key>       # tombstone it
can-flasher … config nvm format --yes      # erase the ENTIRE sector

can-flasher … config ob read               # option-byte snapshot
can-flasher … config ob apply-wrp          # write-protect sectors; resets the device
```

> **`config nvm format` erases every key**, including the board's node ID, and
> `config ob apply-wrp` can — on recent H7 silicon — make sectors clearable only
> by a full chip erase. Both are covered in [SAFETY.md](SAFETY.md).

## `provision` — assign a node ID by role

Sugar over `config nvm write node-id` plus a reset: you type the role, the host
fills in the number.

```bash
can-flasher … --node-id 0xF provision ams     # explicit role
can-flasher … --node-id 0xF provision build/ams.elf   # role inferred from the filename
can-flasher … provision ecu --no-reset        # write only, don't reboot
```

The argument is a role name (`ecu`, `ams`, `udv`, case-insensitive) **or** a
path whose basename matches one — the file is never opened, only its name is
read.

| Flag | Effect |
|---|---|
| `--no-reset` | Skip the post-write reset. Only for chaining several writes; the last one should still reset. |
| `--yes` | Skip the confirmation. Required for scripts — a piped stdin otherwise auto-declines. |

A fresh board takes **two** steps, over two transports: the bootloader goes on
over SWD, then the node ID goes into NVM over CAN, written by the now-running
bootloader. `swd-flash --provision <role>` chains them, which is why it is
incompatible with `--no-reset`.

### `swd-flash --seed-node-id <role|0xN>` — commission with the probe alone

*Experimental; requires the `swd` feature and bootloader seed support.*

Writes the node ID over the **debug probe** while burning the bootloader — no
CAN adapter, no boot round-trip. Instead of asking the running bootloader to
write its NVM over CAN (`--provision`), it stages a small *provisioning seed*
(magic + node-id + complement) at a reserved flash address that the bootloader
adopts on its first boot, then reads it back to verify.

```bash
can-flasher swd-flash CAN_BL.elf --seed-node-id ams
```

- **Requires bootloader seed support**
  ([stm32-can-bootloader#183](https://github.com/isc-fs/stm32-can-bootloader/issues/183)).
  On a bootloader without it the seed is inert — use `--provision` (over CAN)
  instead.
- Mutually exclusive with `--provision` (the two write the node ID by different
  paths).
- Erases only the reserved seed sector, so the bootloader in sector 0 is left
  intact.

## `logs` — pull microSD car-data logs

Boards write car data to a microSD card; LOGFS pulls those files off over CAN.
**Read-only — there is no delete.** Served by the *application* firmware, so a
board sitting in its bootloader will not answer.

```bash
# Seal the log being written right now. An unsealed log does not list.
can-flasher … --node-id 0x02 logs finalize

can-flasher … --node-id 0x02 logs list
can-flasher … --node-id 0x02 logs pull --index 3 --out ./logs/
can-flasher … --node-id 0x02 logs pull --all --out ./logs/
```

| Flag | Meaning |
|---|---|
| `--index N` | Pull one file by its index from `list` |
| `--all` | Pull every file — opt-in on purpose |
| `--out DIR` | Where to write |
| `--no-verify` | Skip the closing CRC check (not recommended) |

**`--node-id` is mandatory here** with no default; omitting it fails with the
generic exit code **99**.

Throughput is ~10–20 kB/s, so a 4 MiB file is 3.5–7 minutes and `--all` on a
full card is 20–35. Commands retry up to three times and the internal timeout
floor is 2000 ms — a smaller `--timeout` is raised to it rather than honoured,
because a shorter deadline cannot outlast a FatFs read on a shared bus.

Full guide: [DATA_LOGS.md](DATA_LOGS.md).

## `pit-diag` — telemetry observer

Boards can be flipped into a diagnostic stream by the host. `pit-diag` is the
terminal-side driver: it sends the arm command, waits for the ACK, and decodes.

Three boards: `--profile ams` (arm `0x7F0`, ACK `0x7F1`, stream `0x680`–`0x6CA`),
`ecu` (`0x7E0` / `0x7E1`, `0x700`–`0x708`), `udv` (`0x7DE` / `0x7DF`,
`0x7A0`–`0x7A9`).

### `listen` — passive, never transmits

```bash
can-flasher --interface pcan --channel PCAN_USBBUS1 pit-diag listen
```

Decodes the frames boards broadcast *without* being asked — the ECU health frame
`0x704` and the AMS health frame `0x6CA` — so it answers *"is this board's app
alive?"* the moment the board powers up.

It never sends a frame. **This is the only pit-diag mode that is safe to point
at a live car.**

| Flag | Meaning |
|---|---|
| `--profile all\|ams\|ecu\|udv` | Which board's frames to decode *(default `all`)* |
| `--duration-ms <MS>` | Stop after N ms; omit to run until Ctrl-C |

### `enable` / `disable` / `stream` — these transmit

```bash
can-flasher … pit-diag enable  --profile ams     # arm
can-flasher … pit-diag disable --profile ams     # disarm
can-flasher … pit-diag stream  --profile ams     # arm + decode + disarm on exit

# Bounded, and as NDJSON for scripting
can-flasher … pit-diag stream --profile ecu --duration 10
can-flasher --json … pit-diag stream --profile ams --duration 5 \
  | jq -c 'select(.kind == "cellVoltage" and .firstCell == 0)'

# CI smoke check — non-zero if a 1 Hz window has the wrong frame count.
# Expected totals per profile: 58 AMS, 7 ECU, 4 uDV.
can-flasher … pit-diag stream --profile ams --duration 5 --strict-scan
```

> `stream` takes `--duration` in **seconds**; `listen` takes `--duration-ms` in
> **milliseconds**. Different flags, different units.

The arm payload is `DE AD BE EF`; disarm is all zeros. An ACK whose first byte is
anything other than `0x01` — including an empty payload — means **disabled**.
`stream` disarms on exit including on Ctrl-C, and a board clears the flag on
reboot if the tool dies without disarming.

Operator guide: [TELEMETRY.md](TELEMETRY.md).

## `send-raw` — one raw frame

The generic escape hatch: app-level commands, bench probes, anything the
protocol layer doesn't model.

```bash
can-flasher … send-raw 0x002 B0 07 AD 11    # app reboot-to-bootloader
```

## `replay` — record and read sessions

A passive bus recorder. Writes every frame in Linux `candump -l` format; `run`
reads one back and pretty-prints it, or emits JSON. Recording transmits nothing.

---

## Exit codes

Branch on the numeric code, not on stderr text. This table is canonical; the
other docs reference it rather than duplicating it.

| Code | Meaning |
|---|---|
| `0` | Success |
| `1` | Flash or write error |
| `2` | Verification mismatch |
| `3` | Protection violation — the image touches the bootloader or metadata sector |
| `4` | Device not found / timeout |
| `7` | WRP not applied (with `--require-wrp` and without `--apply-wrp`) |
| `8` | Input file error — bad format, missing `--address` on a `.bin`, file absent |
| `9` | Adapter not found, or its SDK is missing |
| `99` | Unclassified — read stderr |
| `130` | Interrupted (SIGINT / Ctrl-C) |

Codes `5` and `6` are reserved for a future security phase and are never
returned today.

## JSON output

`--json` is global. Output goes to stdout as a single object, or as one object
per line for streaming subcommands. Schemas live in
[REQUIREMENTS.md § Output and CI integration](../REQUIREMENTS.md#output-and-ci-integration),
which is authoritative if anything here disagrees.

## No hardware

`--interface virtual` spins up an in-process stub bootloader. Good for CI and
for trying commands out; not a substitute for a bench.

---

## See also

- [DESKTOP.md](DESKTOP.md) — the same capabilities with a UI
- [FLASHING.md](FLASHING.md) · [TELEMETRY.md](TELEMETRY.md) · [DATA_LOGS.md](DATA_LOGS.md)
- [SAFETY.md](SAFETY.md) — which of these commands write to a board
- [REQUIREMENTS.md](../REQUIREMENTS.md) — the authoritative spec
