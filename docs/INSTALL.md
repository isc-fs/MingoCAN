# Installing can-flasher

Per-OS CAN-adapter setup (canonical — [../REQUIREMENTS.md](../REQUIREMENTS.md)
points here), build-from-source options, and a quick no-hardware
smoke test so you can verify the binary works before plugging
anything into the car.

For the list of subcommands and their flags see
[USAGE.md](USAGE.md); for the protocol the flasher speaks, see
[../REQUIREMENTS.md](../REQUIREMENTS.md).

Most users want the [prebuilt binary path in the
README](../README.md#install) — no clone, no build directory.
What follows is for people who cloned the repo.

---

## Install from a clone

```bash
# One-time: install Rust (stable channel)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Linux only: install libudev + pkg-config for USB port enumeration
sudo apt-get install libudev-dev pkg-config

# Clone and pick one of the two options below.
git clone https://github.com/isc-fs/can-flasher.git
cd can-flasher
```

**Option A — Install `can-flasher` on your PATH:**

```bash
cargo install --path .
can-flasher --help        # works from any directory
```

**Option B — Just build in place:**

```bash
cargo build --release
./target/release/can-flasher --help
```

Contributors working on the tool itself want Option B — it's the
path `cargo test`, `cargo clippy`, and rust-analyzer all expect.
Dev-setup details (MSRV, release-profile knobs, test suite, CI
hooks) live in [CONTRIBUTING.md](CONTRIBUTING.md).

---

## Per-OS adapter setup

Pick the subsection that matches the `--interface` you plan to use.

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

### Vector XL Driver Library (Windows)

For VN1610 and other [VN16xx](https://www.vector.com/int/en/products/products-a-z/hardware/network-interfaces/vn16xx/)
series adapters. Download and install the **Vector XL Driver
Library** from
[vector.com](https://www.vector.com/int/en/products/products-a-z/software/xl-driver-library/) —
the installer drops `vxlapi64.dll` into `C:\Windows\System32`. The
flasher loads it at runtime, so machines without the SDK installed
won't see a link failure; instead `--interface vector` returns
`AdapterMissing` with the download URL.

To point at a non-default install location, set `VECTOR_LIB_PATH` to
the full path of `vxlapi64.dll` before running the flasher.

Linux support is not yet shipped — Vector's Linux driver doesn't
expose adapters as SocketCAN interfaces (the way PCAN does via
`peak_usb`), so a dedicated backend is needed and is on the roadmap.
macOS isn't supported by Vector at all.

---

## First command: enumerate adapters

Once the binary builds, list every CAN adapter the tool can detect
on the current machine:

```bash
can-flasher adapters

# Example output on a macOS workstation with nothing plugged in:
# SLCAN serial ports:
#   (none detected)
#
# PCAN devices:
#   (none detected — PCAN-Basic library may be missing)
#
# Vector XL devices:
#   (Vector XL Driver Library is currently Windows-only — Linux support planned)
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

Machine-readable output for CI pipelines:

```bash
can-flasher adapters --json | jq
# {
#   "slcan":     [ { "channel": "/dev/ttyACM0", "description": "CANable 2.0 (…)",
#                    "vid": "0x1d50", "pid": "0x606f" } ],
#   "socketcan": [],
#   "pcan":      [],
#   "vector":    []
# }
#
# On Windows with a VN1610 plugged in, "vector" would carry e.g.:
#   [ { "channel": "0", "name": "VN1610 1 Channel 1",
#       "transceiver": "CAN - TJA1041" },
#     { "channel": "1", "name": "VN1610 1 Channel 2",
#       "transceiver": "CAN - TJA1041" } ]
```

---

## No-hardware smoke test

Every subcommand accepts `--interface virtual`, which spins up an
in-process stub bootloader. Useful for CI or just trying things out
on a laptop with nothing plugged in:

```bash
# A complete flash-and-jump dry-run against the virtual stub,
# pointed at a small test binary:
dd if=/dev/urandom of=/tmp/fw.bin bs=1K count=128
can-flasher --interface virtual flash --dry-run --address 0x08020000 /tmp/fw.bin
```

Full details on what the virtual backend does (and doesn't) model
are in
[../REQUIREMENTS.md § Virtual / replay backend](../REQUIREMENTS.md#virtual--replay-backend).

---

## Next steps

- [USAGE.md](USAGE.md) — what every subcommand does, common flags, examples.
- [../REQUIREMENTS.md](../REQUIREMENTS.md) — authoritative CLI spec, opcode table, exit codes.
- [CONTRIBUTING.md](CONTRIBUTING.md) — developing the tool itself.
