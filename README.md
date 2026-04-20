![ISC Logo](http://iscracingteam.com/wp-content/uploads/2022/03/Picture5.jpg)

# IFS08 · can-flasher

Host-side CAN flasher for the [isc-fs/stm32-can-bootloader](https://github.com/isc-fs/stm32-can-bootloader).
Single static Rust binary that runs on Linux, macOS and Windows;
speaks the bootloader's classic-CAN protocol through SLCAN adapters
(CANable), SocketCAN (Linux) or PCAN-Basic (Windows / macOS).

**v1.0.0** — all 7 subcommands are live and the full host-side
flasher is feature-complete against the v1.0.0 bootloader contract.

| Subcommand | Purpose |
|---|---|
| `adapters` | List detected CAN adapters |
| `discover` | Scan the bus, table every bootloader-mode device |
| `diagnose` | DTC / log / live-data / health / reset |
| `config` | NVM read/write + option bytes + WRP apply |
| `verify` | Compare installed image against a binary |
| `replay` | Record / replay CAN sessions for testing |
| `flash` | Program firmware end-to-end |

---

## Documentation

| Doc | Read when |
|---|---|
| [docs/INSTALL.md](docs/INSTALL.md) | Building the binary + per-OS adapter setup |
| [docs/USAGE.md](docs/USAGE.md) | Day-to-day subcommand reference + examples + exit codes |
| [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) | Developing the tool itself — toolchain, tests, CI, branch conventions |
| [REQUIREMENTS.md](REQUIREMENTS.md) | Authoritative CLI spec, opcode table, frame format, JSON schemas |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Code layout, module tree, design rationale |
| [ROADMAP.md](ROADMAP.md) | Phase-by-phase delivery history (auto-generated from `.github/roadmap.yaml`) |

---

## 60-second quick start

```bash
# One-time: install Rust (stable channel)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Linux only: install libudev + pkg-config for USB port enumeration
sudo apt-get install libudev-dev pkg-config

# Build + run
git clone https://github.com/isc-fs/can-flasher.git
cd can-flasher
cargo build --release
./target/release/can-flasher --help

# Smoke test against the in-process stub (no hardware required)
./target/release/can-flasher --interface virtual adapters
```

Full install + adapter setup: [docs/INSTALL.md](docs/INSTALL.md).

---

## Licence

MIT, see [`Cargo.toml`](Cargo.toml).

---

*ISC Racing Team*
