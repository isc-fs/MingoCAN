![ISC Logo](http://iscracingteam.com/wp-content/uploads/2022/03/Picture5.jpg)

# IFS08 · can-flasher

Host-side CAN flasher for the [isc-fs/stm32-can-bootloader](https://github.com/isc-fs/stm32-can-bootloader).
Single static Rust binary that runs on Linux, macOS and Windows; speaks
the bootloader's classic-CAN protocol through SLCAN adapters
(CANable), SocketCAN (Linux) or PCAN-Basic (Windows / macOS).

- **Spec**: [REQUIREMENTS.md](REQUIREMENTS.md)
- **Architecture**: [ARCHITECTURE.md](ARCHITECTURE.md)
- **Delivery roadmap**: [ROADMAP.md](ROADMAP.md)

---

## Status

In active development — targeting **v1.0.0** alongside the bootloader.
Six of seven subcommands are live; the flash pipeline (`feat/15`–
`feat/17`) is the last stretch per [ROADMAP.md](ROADMAP.md).

| Subcommand | Status |
|-----------|:------:|
| `adapters` — list detected CAN adapters | ✅ live |
| `discover` — scan the bus, print table of bootloader-mode devices | ✅ live |
| `diagnose` — DTC / log / live-data / health / reset | ✅ live |
| `config` — NVM read/write + option bytes + WRP apply | ✅ live |
| `verify` — compare installed image against a binary | ✅ live |
| `replay` — record / replay CAN sessions for testing | ✅ live |
| `flash` — program firmware end-to-end | 🔜 `feat/15`–`feat/17` |

---

## Quick start

### From source

```bash
# One-time: install Rust (stable channel)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Linux only: install libudev + pkg-config for USB port enumeration
sudo apt-get install libudev-dev pkg-config

# Build
git clone https://github.com/isc-fs/can-flasher.git
cd can-flasher
cargo build --release

# Run — the binary ends up at target/release/can-flasher
./target/release/can-flasher --help
```

### First command

```bash
# Enumerate detectable adapters
can-flasher adapters

# Example output on a macOS workstation with nothing plugged in:
# SLCAN serial ports:
#   (none detected)
#
# PCAN devices:
#   (none detected — PCAN-Basic library may be missing)
#
# SocketCAN interfaces:
#   (SocketCAN is Linux-only)
```

With a CANable plugged in:

```bash
can-flasher adapters
# SLCAN serial ports:
#   /dev/ttyACM0   CANable 2.0 (USB 1d50:606f)
```

Machine-readable output:

```bash
can-flasher adapters --json | jq
# {
#   "slcan":     [ { "channel": "/dev/ttyACM0", "description": "CANable 2.0 (…)",
#                    "vid": "0x1d50", "pid": "0x606f" } ],
#   "socketcan": [],
#   "pcan":      []
# }
```

### No-hardware smoke test

Every subcommand can be pointed at an in-process stub bootloader via
`--interface virtual`. Useful for CI or trying things out on a laptop
with no CANable plugged in. Details in
[REQUIREMENTS.md § Virtual / replay backend](REQUIREMENTS.md#virtual--replay-backend).

---

## Per-OS adapter setup

Depending on which `--interface` you plan to use:

### CANable / SLCAN (all OSes)

| OS | Setup |
|---|---|
| Linux | `sudo usermod -aG dialout $USER` (log out + back in). Device appears as `/dev/ttyACM0` or `/dev/ttyUSB0`. |
| macOS | No driver needed. Device appears as `/dev/cu.usbmodemNNN`. |
| Windows | No driver needed (CDC ACM). Device appears as `COM3`, `COM4`, etc. |

### SocketCAN (Linux only)

```bash
# Bring up a real CAN interface
sudo ip link set can0 up type can bitrate 500000

# Or a virtual one for testing without hardware
sudo modprobe vcan
sudo ip link add dev vcan0 type vcan
sudo ip link set up vcan0
```

### PCAN-Basic (Windows / macOS)

Download and install the PCAN-Basic SDK from
[peak-system.com/Software-APIs.305.0.html](https://www.peak-system.com/Software-APIs.305.0.html).
The flasher loads the shared library at runtime; on Linux PCAN
adapters appear under SocketCAN via the `peak_usb` kernel module so
the SDK isn't needed there.

---

## Command reference

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

Each subcommand has its own `--help` with detailed arguments. `flash`
is the remaining stub and returns a "not implemented" message naming
the feat branch that will implement it — so you can always see what's
still blocking.

Full flag reference + JSON schemas + exit code table:
[REQUIREMENTS.md § CLI interface](REQUIREMENTS.md#cli-interface) and
[§ Output and CI integration](REQUIREMENTS.md#output-and-ci-integration).

---

## Development

### Toolchain

Pinned to the stable channel via `rust-toolchain.toml`; rustup auto-
installs the right version on first `cargo` invocation. Current MSRV
is **1.95**. `rustfmt` and `clippy` ship in the default profile.

### Common commands

```bash
cargo build                              # debug build
cargo build --release                    # optimised build (LTO, strip)
cargo test                               # full suite (lib + integration + doc)
cargo fmt                                # auto-format
cargo clippy --all-targets -- -D warnings  # lints as errors
```

### Test coverage

Three test flavours all run under `cargo test`:

- **Unit tests** in each module's `#[cfg(test)] mod tests { … }` — the
  bulk of the coverage (~90 % of tests). Pure functions, parsers,
  encoders.
- **Integration test** in `tests/virtual_pipeline.rs` — spins up the
  `VirtualBus` + `StubDevice` + `Session` and round-trips commands
  through the full stack.
- **Doc tests** in `///` blocks — currently one example in
  `protocol::commands`.

Hardware-in-the-loop (real CANable / SocketCAN / PCAN adapters) is
not part of CI; it's covered by the manual smoke-test workflow.

### CI

`.github/workflows/ci.yml` runs on every push to `dev` / `main` and
every PR into them:

- `rustfmt --check`
- `clippy --all-targets --all-features -- -D warnings`
- `build + test` matrix: Linux / macOS / Windows

Docs-only changes (README / REQUIREMENTS / ARCHITECTURE / ROADMAP)
skip CI via path filters — no runner minutes for comment tweaks.

---

## How we work with this repository

### Main branches

```
main  ──────────────────●──────────────────────●──▶  validated releases only
                        ↑                      ↑
dev   ──────●───●───●───●───●───●───●───●───●──●──▶  continuous integration
            ↑   ↑       ↑   ↑   ↑       ↑   ↑
          feat/1 fix/1 feat/2 fix/2   feat/3 fix/3
```

`main` carries validated, tagged releases (`v0.x.0-…`, culminating
at `v1.0.0`). `dev` is where feat / fix branches integrate. Nobody
commits directly to either.

### Branch naming

```
feat/<n>-<short-title>   new functionality  (feat/9-session-lifecycle, …)
fix/<n>-<short-title>    bug or doc fix      (fix/1-workflow-titled-branches, …)
```

`feat` and `fix` have independent counters — `feat/2` and `fix/2`
can coexist. The short kebab-case title is mandatory so the purpose
is visible at a glance.

### Tracking issues

Every branch auto-creates a GitHub Issue on its first push (via
`.github/workflows/branch-issue.yml`):

- Title: `[feat/N-short-title]` or `[fix/N-short-title]`
- Label: `feat` or `fix`
- Body: populated from the first commit's message

The issue closes automatically when the PR merges into `dev` (via
`.github/workflows/close-on-dev-merge.yml`). Closed issues form the
permanent history of the project — grepping them is how future
contributors see what's been done.

### Roadmap

[`ROADMAP.md`](ROADMAP.md) is **auto-generated** from
`.github/roadmap.yaml` by `.github/scripts/render_roadmap.py`. The
workflow runs on every push to `dev` and commits the regenerated
file if anything changed. Branch status badges come from the
tracking-issue state, so closed issues flip `🔜 planned` →
`✅ done` automatically.

Don't hand-edit `ROADMAP.md` — update the YAML instead.

### Typical workflow

```bash
# 1. Make sure dev is current
git checkout dev && git pull origin dev

# 2. Cut a branch (use the next feat/fix number + a short kebab title)
git checkout -b feat/10-discover-subcommand

# 3. Work, commit, push
git commit -m "short description"
git push origin feat/10-discover-subcommand

# 4. Open PR against dev (use `Closes #<issue>` in the body so the
#    tracking issue auto-closes on merge)
gh pr create --base dev --title "..." --body "Closes #NN …"

# 5. Squash-merge after review; the tracking issue closes itself
```

Phase boundaries (every few merged branches) trigger a `dev → main`
**merge commit** (not squash) + a milestone tag + a GitHub Release.
The roadmap table tracks which tag closes each phase.

---

## Licence

MIT, see [`Cargo.toml`](Cargo.toml).

---

*ISC Racing Team — IFS08 Driverless*
