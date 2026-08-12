![ISC Logo](http://iscracingteam.com/wp-content/uploads/2022/03/Picture5.jpg)

# IFS08 · can-flasher

Host-side CAN flasher for the [isc-fs/stm32-can-bootloader](https://github.com/isc-fs/stm32-can-bootloader).
Single static Rust binary that runs on Linux, macOS and Windows;
speaks the bootloader's classic-CAN protocol through four hardware
adapter families plus an in-process virtual loopback: SLCAN
(CANable, all OSes), SocketCAN (Linux), PCAN-Basic (Windows /
macOS), Vector XL Driver Library (VN1610 and the rest of the
[VN16xx](https://www.vector.com/int/en/products/products-a-z/hardware/network-interfaces/vn16xx/)
series on Windows), plus a `virtual` backend for hardware-less
CI + integration tests.

**[Latest release](https://github.com/isc-fs/can-flasher/releases/latest)** —
the desktop app, the CLI and the VS Code extension all ship together from a
single tag.

### Start here

| You want to… | Go to |
|---|---|
| Flash a board, watch telemetry, pull logs, calibrate pedals | **[docs/DESKTOP.md](docs/DESKTOP.md)** — the desktop app is the primary surface |
| Script it, or work at the bench | [docs/USAGE.md](docs/USAGE.md) — the CLI |
| Understand what can write to a car | [docs/SAFETY.md](docs/SAFETY.md) |
| Work on the tool itself | [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) |

| Subcommand | Purpose |
|---|---|
| `adapters` | List detected CAN adapters |
| `discover` | Scan the bus, table every bootloader-mode device |
| `diagnose` | DTC / log / live-data / health / reset |
| `config` | NVM read/write + option bytes + WRP apply |
| `verify` | Compare installed image against a binary |
| `replay` | Record / replay CAN sessions for testing |
| `flash` | Program firmware end-to-end |
| `send-raw` | Send one raw CAN frame (app-level reboot-to-BL, bench probes) |
| `logs` | Pull microSD car-data logs off a board over CAN |
| `pit-diag` | Observe AMS / ECU / uDV telemetry — `listen` is send-silent and safe on a live car |
| `provision` | Write a board's node ID into bootloader NVM |
| `swd-flash` *(opt-in: `--features swd`)* | First-boot a bare STM32 via ST-LINK — covers the chicken-and-egg case where the CAN bootloader isn't on the chip yet |

## Supported adapters

| Family | Platforms | Channel example | Notes |
|---|---|---|---|
| **SLCAN** | Linux / macOS / Windows | `/dev/ttyACM0`, `COM3` | CANable, CANtact, any SLCAN-compatible USB adapter |
| **SocketCAN** | Linux | `can0`, `vcan0` | Native kernel sockets; also handles PCAN on Linux via the `peak_usb` module |
| **PCAN-Basic** | Windows / macOS | `PCAN_USBBUS1` | PEAK adapters via `libloading` — SDK loaded at runtime |
| **Vector XL** | Windows | `0`, `1` (XL channel index) | VN1610 / VN16xx via `vxlapi64.dll` — SDK loaded at runtime |
| **Virtual** | all | (ignored) | In-process bus for testing without hardware |

---

## Documentation

| Doc | Read when |
|---|---|
| [docs/INSTALL.md](docs/INSTALL.md) | Building the binary + per-OS adapter setup |
| [docs/USAGE.md](docs/USAGE.md) | Day-to-day subcommand reference + examples + exit codes |
| [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) | Developing the tool itself — toolchain, tests, CI, branch conventions |
| [docs/DESKTOP.md](docs/DESKTOP.md) | The desktop app: adapters, the eight views, settings |
| [docs/TELEMETRY.md](docs/TELEMETRY.md) | Watching a live car safely — which modes transmit and which do not |
| [docs/PEDAL_CALIBRATION.md](docs/PEDAL_CALIBRATION.md) | Calibrating APPS + brake over CAN — safety-critical runbook |
| [docs/DATA_LOGS.md](docs/DATA_LOGS.md) | Pulling microSD logs off a board |
| [docs/SAFETY.md](docs/SAFETY.md) | Every operation that writes to a board, and what guards it |
| [docs/UPDATES.md](docs/UPDATES.md) | The desktop app's auto-updater |
| [docs/PERFORMANCE.md](docs/PERFORMANCE.md) | Flash-throughput measurements (historical, v1.2.0-era) |
| [REQUIREMENTS.md](REQUIREMENTS.md) | Authoritative CLI spec, opcode table, frame format, JSON schemas |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Code layout, module tree, design rationale |
| [ROADMAP.md](ROADMAP.md) | Phase-by-phase delivery history (auto-generated from `.github/roadmap.yaml`) |

---

## Install

Need Rust once (any OS):
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Linux also needs:
```bash
sudo apt-get install libudev-dev pkg-config
```

Then install `can-flasher` to your PATH, straight from GitHub — no
clone, no build directory to manage:

```bash
cargo install --git https://github.com/isc-fs/can-flasher.git
```

After this, `can-flasher --help` works from anywhere. Docs refer
to it as `cf` for brevity; `alias cf=can-flasher` in your shell
rc file if you like.

Sanity check — should list your CAN adapter if one is plugged in,
or print an empty list if not:
```bash
can-flasher adapters
```

Full per-OS adapter setup (CANable, SocketCAN, PCAN):
[docs/INSTALL.md](docs/INSTALL.md).

### Build from source (contributors only)

```bash
git clone https://github.com/isc-fs/can-flasher.git
cd can-flasher
cargo build --release
./target/release/can-flasher --help
```

See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) for toolchain
details, test suite, and branch conventions.

---

## Editor + desktop integrations

| Path | Status | What it does |
|---|---|---|
| [editor/vscode/](editor/vscode/) | ✅ live | VS Code extension that wraps `cmake --build` + `can-flasher flash` into one command, plus device discovery, adapter picker, live-data + DTC panels. Shells out to `can-flasher --json` — never speaks the protocol directly. Distributed as a `.vsix` attached to each `v*` release; internal ISC consumption. |
| [apps/can-studio/](apps/can-studio/) | ✅ live — Tier 2 | Tauri 2 desktop app for flashing + diagnostics + generic CAN bus monitor + DBC-decoded Signals view. Reuses the `can-flasher` crate by path — same Rust on both sides of the IPC bridge. macOS / Linux / Windows native bundles attached to each `v*` release. |

Both surfaces ship in lockstep with the CLI from a single `v*` tag — see [docs/CONTRIBUTING.md § Cutting a release](docs/CONTRIBUTING.md#cutting-a-release).

---

## Licence

MIT, see [`Cargo.toml`](Cargo.toml).

---

*ISC Racing Team*
