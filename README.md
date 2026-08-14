![ISC Logo](http://iscracingteam.com/wp-content/uploads/2022/03/Picture5.jpg)

# ISC MingoCAN

**The pit-lane tool for the IFS08.** One window that flashes firmware over CAN,
watches the car's boards live, pulls the data logs off them, and tells you why a
board is unhappy — for the ISC Racing Team's Formula Student ECUs.

It talks to the [isc-fs/stm32-can-bootloader](https://github.com/isc-fs/stm32-can-bootloader)
over classic CAN through five adapter families, and runs natively on macOS,
Windows and Linux.

**[⬇ Download the latest release](https://github.com/isc-fs/MingoCAN/releases/latest)**
· [Install guide](docs/INSTALL.md) · [App guide](docs/DESKTOP.md)

---

## Start here

| You want to… | Go to |
|---|---|
| **Install it and flash your first board** | [docs/INSTALL.md](docs/INSTALL.md) → [docs/FLASHING.md](docs/FLASHING.md) |
| Learn the app, view by view | [docs/DESKTOP.md](docs/DESKTOP.md) |
| Watch a live car without touching it | [docs/TELEMETRY.md](docs/TELEMETRY.md) |
| Get the logs off a board | [docs/DATA_LOGS.md](docs/DATA_LOGS.md) |
| Know what can write to a car, and what stops it | [docs/SAFETY.md](docs/SAFETY.md) |
| Script it, or work headless at the bench | [docs/CLI.md](docs/CLI.md) |
| Work on MingoCAN itself | [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) |

## What's in the app

The sidebar splits in two, and the split is the point: **Program** writes to a
board, **Observe** only reads. Anything in Observe is safe to open on a live car.

```
Adapters              ← pinned top; nothing else works until you pick one

PROGRAM   (writes to the car)
  Flash                 Build & flash firmware over CAN
  Burn bootloader       First-boot bootloader via SWD

OBSERVE   (read-only)
  Board health          DTCs & session health
  Bus monitor           Live CAN frames & DBC-decoded signals
  Telemetry             Live AMS / ECU / uDV telemetry
  Data logs             Pull microSD car-data logs over CAN

Settings              ← pinned bottom
```

## Three surfaces, one release

Every tagged release ships all three together, at the same version:

| Surface | What it's for | Docs |
|---|---|---|
| **MingoCAN** (desktop app) | The primary tool. Everything below, with a UI. | [DESKTOP.md](docs/DESKTOP.md) |
| **`can-flasher`** (CLI) | Scripting, CI, bench automation, headless boxes. | [CLI.md](docs/CLI.md) |
| **VS Code extension** | Flash from the editor while developing firmware. | [editor/vscode](editor/vscode/README.md) |

> The executable is still called `can-flasher`. Renaming it would break every
> script and CI job that invokes the tool, so the product name and the binary
> name are deliberately allowed to differ.

## Supported adapters

| Family | Platforms | Channel example | Notes |
|---|---|---|---|
| **SLCAN** | Linux / macOS / Windows | `/dev/ttyACM0`, `COM3` | CANable, CANtact, any SLCAN-compatible USB adapter |
| **SocketCAN** | Linux | `can0`, `vcan0` | Native kernel sockets; also serves PEAK hardware via `peak_usb` |
| **PCAN-Basic** | Windows / macOS | `PCAN_USBBUS1` | PEAK adapters; SDK loaded at runtime |
| **Vector XL** | Windows | `0`, `1` (XL channel index) | VN1610 / [VN16xx](https://www.vector.com/int/en/products/products-a-z/hardware/network-interfaces/vn16xx/); SDK loaded at runtime |
| **Virtual** | all | (ignored) | In-process loopback for testing without hardware |

The PCAN and Vector SDKs are loaded at runtime and cannot be bundled — see
[INSTALL.md § Per-OS adapter setup](docs/INSTALL.md#per-os-adapter-setup).

## The boards

| Role | Node ID | What it is |
|---|---|---|
| ECU | `0x01` | Vehicle control unit |
| AMS | `0x02` | Accumulator management system |
| uDV | `0x03` | Driverless supervisor |

A board that has never been commissioned answers on `0xF` until you
[provision](docs/CLI.md#provision--assign-a-node-id-by-role) it.

## Documentation

| Doc | Read when |
|---|---|
| [docs/INSTALL.md](docs/INSTALL.md) | Installing the app or the CLI, and per-OS adapter setup |
| [docs/DESKTOP.md](docs/DESKTOP.md) | Learning the app — every view, every setting |
| [docs/FLASHING.md](docs/FLASHING.md) | Flashing firmware, and what to do when it fails |
| [docs/TELEMETRY.md](docs/TELEMETRY.md) | Watching a live car — which modes transmit and which don't |
| [docs/DATA_LOGS.md](docs/DATA_LOGS.md) | Pulling microSD logs off a board |
| [docs/SAFETY.md](docs/SAFETY.md) | Every operation that writes to a board, and what guards it |
| [docs/CLI.md](docs/CLI.md) | The `can-flasher` CLI — subcommands, flags, exit codes |
| [docs/UPDATES.md](docs/UPDATES.md) | How the app updates itself |
| [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) | Developing MingoCAN — toolchain, tests, CI, release flow |
| [docs/PERFORMANCE.md](docs/PERFORMANCE.md) | Flash-throughput measurements (historical, v1.2.0-era) |
| [REQUIREMENTS.md](REQUIREMENTS.md) | The authoritative spec — protocol, opcodes, every flag |
| [ARCHITECTURE.md](ARCHITECTURE.md) | How the code is laid out |
| [ROADMAP.md](ROADMAP.md) | What's next (auto-generated) |

## Building from source

```bash
git clone https://github.com/isc-fs/MingoCAN.git
cd MingoCAN
cargo build --release          # the CLI
cd apps/can-studio && npm install && npm run tauri build   # the app
```

Full details, including the Linux system packages the app needs, are in
[INSTALL.md](docs/INSTALL.md#build-from-source).

---

<sub>Built by the [ISC Racing Team](http://iscracingteam.com). MingoCAN is the
desktop companion to the `can-flasher` engine — same code, same release, one
window.</sub>
