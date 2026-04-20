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
  adapters    List detected CAN adapters on this machine

Global Options:
  -i, --interface <TYPE>    CAN backend: slcan | socketcan | pcan | virtual
  -c, --channel <CHANNEL>   Adapter channel (format depends on OS and backend)
  -b, --bitrate <BPS>       Nominal CAN bitrate [default: 500000]
      --node-id <ID>        Target node ID hex or decimal [default: broadcast]
      --timeout <MS>        Per-frame timeout in ms [default: 500]
      --json                Machine-readable JSON output on stdout
      --log <PATH>          Append session to audit log (SQLite)
      --verbose             Trace-level logging
      --operator <NAME>     Override operator name in audit log
```

Every subcommand has its own `--help` with the full flag list —
treat the snippets below as the 80 % path.

> The audit-log plumbing (`--log`) is stubbed: the flag parses but
> the SQLite sink is deferred to post-v1 per
> [../ROADMAP.md](../ROADMAP.md).

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

Most-used flags:

| Flag | Effect |
|---|---|
| `--address <HEX>` | Load address for raw `.bin` (ignored for ELF / HEX) |
| `--require-wrp` | Abort with exit 7 if sector 0 isn't WRP-latched |
| `--apply-wrp` | Latch WRP before flashing when it isn't |
| `--no-diff` | Force-write every sector even if the CRC already matches |
| `--dry-run` | Validate + plan but send no erase / write / verify commands |
| `--no-verify-after` | Skip the post-write per-sector CRC check |
| `--no-jump` | Stay in bootloader mode after a successful flash |

The default behaviour (`--diff`, `--verify-after`, `--jump`,
no WRP enforcement) is the happy path for dev iteration. A
production deploy script typically adds
`--require-wrp --apply-wrp`.

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

can-flasher --interface slcan --channel /dev/ttyACM0 config nvm read  --key 0x0001
can-flasher --interface slcan --channel /dev/ttyACM0 config nvm write --key 0x0002 --value 0xDEADBEEF
can-flasher --interface slcan --channel /dev/ttyACM0 config nvm erase --key 0x0002
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

## Exit codes

CI pipelines should branch on the numeric exit code, not the stderr
text. The table below mirrors
[../REQUIREMENTS.md § Exit codes](../REQUIREMENTS.md#exit-codes).

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
| `99` | Unclassified error — check stderr |

Codes `5` (signature failed) and `6` (replay rejection) are reserved
for the post-v1 security phase and never returned by v1.0.0.

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
