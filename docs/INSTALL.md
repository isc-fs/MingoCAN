# Installing MingoCAN

Three paths, depending on what you need:

1. **[The app](#the-app)** — what almost everyone wants.
2. **[The CLI](#the-cli)** — for scripting, CI, and headless bench boxes.
3. **[From source](#build-from-source)** — for working on MingoCAN itself.

Then, whichever you picked, do the **[per-OS adapter setup](#per-os-adapter-setup)**.
That part is not optional and not something any installer can do for you: the
PCAN and Vector SDKs are loaded at runtime and cannot legally be bundled.

---

## The app

Download the installer for your platform from the
**[latest release](https://github.com/isc-fs/MingoCAN/releases/latest)**:

| Platform | Asset |
|---|---|
| macOS (Apple Silicon) | `ISC.MingoCAN_<version>_aarch64.dmg` |
| Windows | `ISC.MingoCAN_<version>_x64-setup.exe` |
| Linux | `ISC.MingoCAN_<version>_amd64.AppImage`, `_amd64.deb`, or `-1.x86_64.rpm` |

The app bundles the flashing engine — there is no separate CLI to install
alongside it — and it keeps itself up to date. See [UPDATES.md](UPDATES.md).

### The builds are not code-signed

You will get a scary dialog the first time. This is expected; the team does not
currently pay for an Apple Developer ID or an EV code-signing certificate.

| OS | What you see | What to do |
|---|---|---|
| macOS | *"…can't be opened because it is from an unidentified developer"* | Right-click the app → **Open** → **Open**. Once per install. Or `xattr -dr com.apple.quarantine /Applications/ISC\ MingoCAN.app` |
| Windows | SmartScreen: *"Windows protected your PC"* | **More info** → **Run anyway** |
| Linux | AppImage won't launch | `chmod +x ISC.MingoCAN_*.AppImage` |

On macOS this recurs after an auto-update, because the relaunched bundle is
freshly quarantined. [UPDATES.md](UPDATES.md) covers that case.

**Next:** [DESKTOP.md](DESKTOP.md) to learn the app, or
[FLASHING.md](FLASHING.md) to go straight to flashing a board.

---

## The CLI

The same release carries standalone `can-flasher` binaries:

```
can-flasher-<version>-aarch64-apple-darwin.tar.gz
can-flasher-<version>-x86_64-unknown-linux-gnu.tar.gz
can-flasher-<version>-aarch64-unknown-linux-gnu.tar.gz
can-flasher-<version>-x86_64-pc-windows-msvc.zip
```

Extract, put the binary on your `PATH`, and check it runs:

```bash
can-flasher --version
```

The VS Code extension ships in the same release as
`vscode-stm32-can-<version>.vsix`.

**Next:** [CLI.md](CLI.md).

---

## Build from source

```bash
# One-time: install Rust (stable channel; rustup picks the pinned version)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Linux only: USB port enumeration needs libudev
sudo apt-get install libudev-dev pkg-config

git clone https://github.com/isc-fs/MingoCAN.git
cd MingoCAN
```

### The CLI

**Option A — install it on your `PATH`:**

```bash
cargo install --path .
can-flasher --help
```

**Option B — build in place:**

```bash
cargo build --release
./target/release/can-flasher --help
```

Contributors want Option B — it's the path `cargo test`, `cargo clippy`, and
rust-analyzer all expect. Toolchain details (MSRV, release-profile knobs, test
layout, CI hooks) are in [CONTRIBUTING.md](CONTRIBUTING.md).

### The app

```bash
cd apps/can-studio
npm install
npm run tauri dev        # hot-reloading dev build
npm run tauri build      # production bundle
```

On Linux the Tauri build needs GTK and WebKit development packages:

```bash
sudo apt-get install libwebkit2gtk-4.1-dev libgtk-3-dev \
                     libayatana-appindicator3-dev librsvg2-dev
```

---

## Per-OS adapter setup

Pick the subsection matching the adapter you plan to use. This applies equally
to the app and the CLI — both load the same libraries at runtime.

### CANable / SLCAN (all OSes)

| OS | Setup |
|---|---|
| Linux | `sudo usermod -aG dialout $USER` (log out and back in). Device appears as `/dev/ttyACM0` or `/dev/ttyUSB0`. |
| macOS | No driver needed. Device appears as `/dev/cu.usbmodemNNN`. |
| Windows | No driver needed (CDC ACM). Device appears as `COM3`, `COM4`, … |

### SocketCAN (Linux only)

```bash
# Bring up a real CAN interface
sudo ip link set can0 up type can bitrate 500000

# Or a virtual one, to test without hardware
sudo modprobe vcan
sudo ip link add dev vcan0 type vcan
sudo ip link set up vcan0
```

### PCAN (Windows / macOS)

The PCAN shared library is loaded at runtime, and where you get it differs per
OS.

**macOS.** PEAK does not ship a macOS driver themselves; macOS support for
PCAN-USB is the free third-party **MacCAN `libPCBUSB`** library, which PEAK
officially points to. It is a pure user-space driver — no kext, no SIP changes,
universal arm64 + Intel, macOS 10.13+.

1. Download the latest universal build from the
   [MacCAN releases](https://github.com/mac-can/PCBUSB-Library/releases)
   (e.g. `macOS_Library_for_PCANUSB_vX.Y.tar.gz`). Background and install notes:
   [mac-can.github.io/drivers/libPCBUSB.html](https://mac-can.github.io/drivers/libPCBUSB.html).
2. Extract and run the bundled `install.sh` with `sudo` — it installs
   `libPCBUSB.dylib` into `/usr/local/lib`, exactly where MingoCAN looks by
   default. Manual fallback: `sudo cp libPCBUSB.*.dylib /usr/local/lib/`.
3. Gatekeeper quarantines unsigned downloads. If the adapter still doesn't
   appear after installing, clear the attribute:
   `sudo xattr -dr com.apple.quarantine /usr/local/lib/libPCBUSB.dylib`
   (or approve it under System Settings → Privacy & Security).

**Windows.** Install **PCAN-Basic** from
[PEAK](https://www.peak-system.com/products/software/development-packages/pcan-basic/).
The installer drops `PCANBasic.dll` onto the system DLL path.

**Linux.** Nothing to install — PCAN adapters appear under SocketCAN via the
`peak_usb` kernel module. Use the SocketCAN backend (the `pcan` backend
delegates to it on Linux anyway).

If the library lives somewhere non-standard, set `PCAN_LIB_PATH` to its full
path.

### Vector XL Driver Library (Windows)

For VN1610 and other
[VN16xx](https://www.vector.com/int/en/products/products-a-z/hardware/network-interfaces/vn16xx/)
adapters. Install the
[Vector XL Driver Library](https://www.vector.com/int/en/products/products-a-z/software/xl-driver-library/);
the installer drops `vxlapi64.dll` into `C:\Windows\System32`. It is loaded at
runtime, so a machine without the SDK doesn't fail to start — it reports
`AdapterMissing` with the download URL instead.

Set `VECTOR_LIB_PATH` to point at a non-default install location.

Linux is not supported yet: Vector's Linux driver doesn't expose adapters as
SocketCAN interfaces the way PCAN does, so a dedicated backend is needed and is
on the roadmap. macOS isn't supported by Vector at all.

### ST-LINK + SWD (optional)

Only needed for **Burn bootloader** / `swd-flash`, which is how a
never-programmed board gets its first firmware. Driven through
[probe-rs](https://probe.rs). In the CLI this is behind the `swd` Cargo feature.

| OS | Setup |
|---|---|
| Linux | `sudo apt-get install libusb-1.0-0` (plus `libudev-dev` to build). Install the [ST-LINK udev rule](https://github.com/stlink-org/stlink/blob/master/config/udev/rules.d/49-stlinkv2.rules) into `/etc/udev/rules.d/`, then `sudo udevadm control --reload-rules && sudo udevadm trigger`, so you don't need `sudo` to reach the probe. |
| macOS | No driver needed — probe-rs claims the ST-LINK through IOKit/libusb. |
| Windows | Run [Zadig](https://zadig.akeo.ie/) once: pick the **ST-Link Debug** interface, set the target driver to **WinUSB**, click **Replace Driver**. |

---

## Check it worked

### In the app

Open MingoCAN. It lands on **Adapters**. Your adapter should be in the list,
tagged with its kind. If it isn't, press **⟳** — discovery runs when the view
opens and when you refresh, but it does not poll in the background.

### On the CLI

```bash
can-flasher adapters
```

With a CANable plugged in:

```
SLCAN serial ports:
  /dev/ttyACM0   CANable 2.0 (USB 1d50:606f)
```

With nothing plugged in, each family reports why it found nothing — a missing
PCAN library and an absent adapter are different messages, on purpose:

```
SLCAN serial ports:
  (none detected)

PCAN devices:
  (none detected — PCAN-Basic library may be missing)

Vector XL devices:
  (Vector XL Driver Library is currently Windows-only — Linux support planned)

SocketCAN interfaces:
  (SocketCAN is Linux-only)
```

Machine-readable, for CI:

```bash
can-flasher adapters --json | jq
```

---

## No hardware? Use the virtual bus

Every part of the tool accepts a `virtual` backend: an in-process stub
bootloader with nothing behind it. It is for smoke-testing the tool itself, not
for talking to a car.

In the app it's the **`virtual`** entry in the Adapters list, always present.
On the CLI:

```bash
dd if=/dev/urandom of=/tmp/fw.bin bs=1K count=128
can-flasher --interface virtual flash --dry-run --address 0x08020000 /tmp/fw.bin
```

What the virtual backend does and doesn't model is documented in
[REQUIREMENTS.md § Virtual / replay backend](../REQUIREMENTS.md#virtual--replay-backend).

---

## Next

- [DESKTOP.md](DESKTOP.md) — the app, view by view
- [FLASHING.md](FLASHING.md) — flash your first board
- [SAFETY.md](SAFETY.md) — what writes to a car, and what stops it
- [CLI.md](CLI.md) — the `can-flasher` command line
- [CONTRIBUTING.md](CONTRIBUTING.md) — developing MingoCAN itself
