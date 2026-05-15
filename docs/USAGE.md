# Using can-flasher

Day-to-day reference: what each subcommand does, the flags you'll
actually reach for, and a short example per command. For the
authoritative spec (every flag, every JSON schema field, every
opcode) see [../REQUIREMENTS.md](../REQUIREMENTS.md).

For install / first-run / adapter setup, see
[INSTALL.md](INSTALL.md).

---

## Top-level

```
can-flasher [OPTIONS] <COMMAND>

Commands:
  flash       Flash firmware to a device
  verify      Verify flash contents against a binary without writing
  discover    Scan the bus and list all bootloader-mode devices
  diagnose    Read/clear DTCs, stream logs, stream live data, session health
  config      Read/write device configuration (NVM) and option bytes (WRP)
  replay      Record or replay a CAN session (testing)
  send-raw    Send one raw CAN frame (app-level reboot-to-BL, bench probes)
  adapters    List detected CAN adapters on this machine

Global Options:
  -i, --interface <TYPE>    CAN backend: slcan | socketcan | pcan | vector | virtual
  -c, --channel <CHANNEL>   Adapter channel (format depends on OS and backend)
  -b, --bitrate <BPS>       Nominal CAN bitrate [default: 500000]
      --node-id <ID>        Target node ID hex or decimal [default: broadcast]
      --timeout <MS>        Per-frame timeout in ms [default: 500]
      --json                Machine-readable JSON output on stdout
      --verbose             Trace-level logging
      --operator <NAME>     Override operator name in audit log
```

Every subcommand has its own `--help` with the full flag list —
treat the snippets below as the 80 % path.

---

## Typical first flash: stitched command sequence

Four-step happy path for a new board. Each step checks the one
before it so you know which step broke when something does.

```bash
# 1. Adapter plugged in, visible to the OS?
can-flasher adapters

# 2. Bootloader on the target, listening for CAN?
can-flasher --interface slcan --channel /dev/ttyACM0 discover

# 3. Program the firmware, verify the CRC, jump to it.
can-flasher --interface slcan --channel /dev/ttyACM0 --node-id 0x3 \
  flash build/firmware.elf --verify-after --jump

# 4. (Optional) Re-read + CRC-match the installed image,
#    e.g. in a CI post-deploy gate. Exits 0 on match, 2 on mismatch.
can-flasher --interface slcan --channel /dev/ttyACM0 --node-id 0x3 \
  verify build/firmware.elf
```

If step 1 returns `(none detected)`, fix the OS/adapter setup
(see [INSTALL.md](INSTALL.md)). If step 2 times out, the adapter's
probably not wired to the board or the bitrate is wrong. If step 3
fails with exit 3 (ProtectionViolation), the binary's linker targets
the bootloader's sector — see [../demo/README.md](../demo/README.md)
for a known-good linker script.

---

## `adapters` — detect CAN hardware

Lists every adapter the current machine exposes to this binary.
First thing to run on a new machine — if the adapter you want doesn't
show up here, none of the other subcommands will find it.

```bash
can-flasher adapters              # human-readable
can-flasher adapters --json       # for CI pipelines
```

See [INSTALL.md § First command](INSTALL.md#first-command-enumerate-adapters)
for example output on each platform.

---

## `discover` — scan the bus

Broadcasts `CMD_DISCOVER` to every node and tabulates every
bootloader that replies, with its firmware info and health record.
Run this before `flash` / `verify` to confirm the target is
listening.

```bash
can-flasher --interface slcan --channel /dev/ttyACM0 discover
#  NODE   PROTO   RESET CAUSE    WRP    APP VALID    VERSION    PRODUCT
#  0x03    0.1     POWER_ON     no        yes       1.4.2      IFS08-CE-ECU

can-flasher --interface slcan --channel /dev/ttyACM0 discover --json | jq
```

---

## `flash` — program firmware

The main event. Loads an ELF / HEX / raw .bin, opens a session, runs
the sector-aware diff/erase/write/verify pipeline, optionally jumps
to the application.

```bash
# Typical: flash an ELF, WRP-latch if missing, jump on success
can-flasher --interface slcan --channel /dev/ttyACM0 \
  flash build/firmware.elf --apply-wrp

# Raw .bin needs --address (ELF / HEX carry it themselves)
can-flasher --interface slcan --channel /dev/ttyACM0 \
  flash build/firmware.bin --address 0x08020000

# CI: dry-run + JSON report, no JUMP, no WRP side-effects
can-flasher --json --interface virtual \
  flash build/firmware.elf --dry-run --no-jump
```

Most-used flags. The default behaviour is the happy path for dev
iteration; production deploy scripts typically add
`--require-wrp --apply-wrp`.

| Flag | Effect |
|---|---|
| `--address <HEX>` | Load address for raw `.bin` (ignored for ELF / HEX) |
| `--require-wrp` | Abort with exit 7 if sector 0 isn't WRP-latched |
| `--apply-wrp` | Latch WRP before flashing when it isn't |
| `--diff` *[default]* | Skip sectors whose device-side CRC already matches |
| `--no-diff` | Force-write every sector regardless of CRC |
| `--dry-run` | Validate + plan but send no erase / write / verify commands |
| `--verify-after` *[default]* | Re-read each written sector and CRC-match before ACKing success |
| `--no-verify-after` | Skip the post-write per-sector CRC check |
| `--jump` *[default]* | Issue `CMD_JUMP` to the installed app after a successful flash |
| `--no-jump` | Stay in bootloader mode after a successful flash |

Full argument reference: `can-flasher flash --help`.

---

## `verify` — compare installed image against a binary

Computes `(CRC32, size, version)` from the local binary, fires
`CMD_FLASH_VERIFY`, exits 0 on match or 2 on mismatch. No bytes get
written; ideal for a post-deploy check.

```bash
can-flasher --interface slcan --channel /dev/ttyACM0 \
  verify build/firmware.elf

# Exit 0 → installed image matches. Exit 2 → mismatch (CI can
# then re-flash, alert, whatever.)
```

---

## `diagnose` — read state + live telemetry

Six sub-actions in one subcommand. All are session-gated except
`health`.

```bash
can-flasher --interface slcan --channel /dev/ttyACM0 diagnose read-dtc
can-flasher --interface slcan --channel /dev/ttyACM0 diagnose clear-dtc --yes
can-flasher --interface slcan --channel /dev/ttyACM0 diagnose health
can-flasher --interface slcan --channel /dev/ttyACM0 diagnose log       # Ctrl-C to stop
can-flasher --interface slcan --channel /dev/ttyACM0 diagnose live-data # Ctrl-C to stop
can-flasher --interface slcan --channel /dev/ttyACM0 diagnose reset --mode soft
```

---

## `config` — NVM + option bytes

Inspect or mutate the NVM key/value store and the bootloader's
option bytes (read-only for option bytes except `apply-wrp`).

```bash
can-flasher --interface slcan --channel /dev/ttyACM0 config ob read
can-flasher --interface slcan --channel /dev/ttyACM0 config ob apply-wrp \
  --sector-mask 0x01

can-flasher --interface slcan --channel /dev/ttyACM0 config nvm read   --key 0x0001
can-flasher --interface slcan --channel /dev/ttyACM0 config nvm write  --key 0x0002 --value 0xDEADBEEF
can-flasher --interface slcan --channel /dev/ttyACM0 config nvm erase  --key 0x0002

# bootloader 0.2+ — wipe the entire NVM sector (every key + metadata)
can-flasher --interface slcan --channel /dev/ttyACM0 config nvm format --yes
```

---

## `replay` — record + read CAN sessions

Passive bus monitor. Writes every frame to a file in
Linux `candump -l` format; `run` reads that file back and
pretty-prints it (or emits JSON).

```bash
# Record whatever happens on the bus for 10 seconds
can-flasher --interface slcan --channel /dev/ttyACM0 \
  replay record --out flash.candump --duration-ms 10000

# Read the file back
can-flasher replay run flash.candump
can-flasher --json replay run flash.candump | jq
```

Files are compatible with `canplayer` and `cantools` — you can
replay them against a `vcan` interface externally if the need arises.

---

## `send-raw` — single raw CAN frame

Bypass the bootloader protocol entirely and transmit one classic-CAN
frame with the operator-supplied 11-bit ID and 0–8 payload bytes.
Used for app-level conventions that the BL doesn't know about — most
commonly a "reboot to bootloader" escape sent to a running
application before the next flash.

```bash
# Send one frame with ID 0x010, payload 0x01, listen 100 ms for replies (default)
can-flasher --interface slcan --channel /dev/ttyACM0 \
  send-raw 0x010 01

# No listen window — fire and exit
can-flasher --interface slcan --channel /dev/ttyACM0 \
  send-raw 0x010 01 --listen-ms 0

# Longer listen window to catch a slow ACK
can-flasher --interface slcan --channel /dev/ttyACM0 \
  send-raw 0x010 DE AD BE EF --listen-ms 500
```

The specific ID and payload convention for "reboot to BL" or any
other app-level signal lives in the **application firmware** and the
matching REQUIREMENTS.md update, not here — `send-raw` is the
generic primitive, the convention is app-defined.

---

## `swd-flash` — first-boot via ST-LINK (opt-in build)

A bare STM32 can't speak the CAN bootloader's wire protocol until
the bootloader is itself on the chip. `swd-flash` covers that
first-boot problem by driving an ST-LINK V2 / V3 through
[probe-rs](https://probe.rs), so the same binary handles both the
initial SWD flash and every subsequent over-CAN app update.

The subcommand is **only present in builds with the `swd` Cargo
feature enabled** (off by default, since the dependency pulls in a
libusb stack that not every operator wants). To enable:

```bash
cargo install --path . --features swd
# or, from a clone:
cargo build --release --features swd
```

Examples:

```bash
# Flash the bootloader .elf to a single attached ST-LINK + STM32H733
can-flasher swd-flash bootloader.elf

# Same, but download the latest bootloader from the BL release page
# instead of pointing at a local file. Cached under your platform
# cache dir (~/Library/Caches on macOS, $XDG_CACHE_HOME on Linux,
# %LOCALAPPDATA% on Windows) so repeat runs skip the network.
can-flasher swd-flash --release

# Pin to a specific BL release tag
can-flasher swd-flash --release v1.2.0

# Fetch the .hex or .bin instead of .elf
can-flasher swd-flash --release --release-format hex

# Pin to a specific probe when several ST-LINKs are wired in
can-flasher swd-flash bootloader.elf --probe-serial 0670FF555654846687204023

# Different STM32 family (any probe-rs target string works)
can-flasher swd-flash app.elf --chip STM32G431RBTx

# Raw .bin needs --base; the default is 0x08000000 (main flash start)
can-flasher swd-flash blob.bin --base 0x08020000

# Skip the readback-and-compare to save ~1s on bench loops
can-flasher swd-flash bootloader.elf --no-verify

# Leave the chip halted after the write (e.g. for a debugger attach)
can-flasher swd-flash bootloader.elf --no-reset
```

Platform prerequisites (libusb stack for the ST-LINK USB endpoint)
are listed in [INSTALL.md § ST-LINK + SWD](INSTALL.md#st-link--swd-optional-feature-swd).

The feature is currently a **feasibility spike**: ST-LINK only, no
auto-download of the bootloader artifact, no GDB/RTT pass-through.
See [REQUIREMENTS.md](../REQUIREMENTS.md) once the spike graduates.

---

## Exit codes

CI pipelines should branch on the numeric exit code, not the stderr
text. This table is the canonical list; other docs (REQUIREMENTS.md,
CONTRIBUTING.md) reference it rather than duplicating.

| Code | Meaning |
|---|---|
| `0` | Success |
| `1` | Flash or write error |
| `2` | Verification mismatch (the `verify` subcommand, or `flash`'s final `CMD_FLASH_VERIFY`) |
| `3` | Protection violation — firmware touches sector 0 or the metadata sector |
| `4` | Device not found / timeout |
| `7` | WRP not applied (when `--require-wrp` is set and `--apply-wrp` isn't) |
| `8` | Input file error — bad format, missing `--address` on `.bin`, file doesn't exist |
| `9` | Adapter not found or SDK missing |
| `130` | Interrupted by user (SIGINT / Ctrl-C) |
| `99` | Unclassified error — check stderr |

Codes `5` (signature failed) and `6` (replay rejection) are reserved
for the post-v1 security phase and not returned today.

---

## JSON output

Every subcommand accepts `--json` as a global flag; output goes to
`stdout` as a single JSON object (or a stream of line-JSON events,
depending on the subcommand). Schemas live in
[../REQUIREMENTS.md § Output and CI integration](../REQUIREMENTS.md#output-and-ci-integration)
— treat that section as authoritative if anything here diverges.

---

## Where next

- Programming the bootloader protocol yourself? See
  [../REQUIREMENTS.md](../REQUIREMENTS.md) for the opcode table and
  frame-format details.
- Contributing to the tool itself? See
  [CONTRIBUTING.md](CONTRIBUTING.md).
