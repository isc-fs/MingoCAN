# Installing can-flasher

Covers building from source, per-OS CAN-adapter setup, and a quick
no-hardware smoke test so you can verify the binary works before
plugging anything into the car.

For the list of subcommands and their flags see
[USAGE.md](USAGE.md); for the protocol the flasher speaks, see
[../REQUIREMENTS.md](../REQUIREMENTS.md).

---

## Build from source

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

The release profile uses `lto = "thin"`, `codegen-units = 1`, and
`strip = "symbols"`, so you end up with a single lean static binary
that you can copy to any machine of the same OS/arch without further
setup.

Toolchain versions: pinned to the stable channel via
`rust-toolchain.toml`. Current MSRV is **1.95**. If you contribute,
see [CONTRIBUTING.md](CONTRIBUTING.md) for the full dev setup.

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
#   "pcan":      []
# }
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
