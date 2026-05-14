# CAN Flasher — Project Requirements

Host-side CAN flasher (Rust CLI) for programming the STM32 CAN
bootloader shipped at [`isc-fs/stm32-can-bootloader`](https://github.com/isc-fs/stm32-can-bootloader)
**v1.0.0** (`v1.0.0` = Phase-1..4 feature-complete; Phase 5 security is
deferred — see [§ Deferred scope](#deferred-scope-v2-tied-to-bootloader-phase-5)
at the end of this file).

The bootloader is the source of truth for wire formats and addresses;
this document tracks what the host tool must implement to speak to
it. Any drift between this file and `bl_proto.h` / `bl_memmap.h` in
the bootloader repo is a bug in this file — fix this file first.

---

## Target hardware (bootloader side)

- **MCU**: STM32H733ZGT6
- **Flash**: 1 MB, 8 × 128 KB sectors, Bank 1 only on this variant
- **Bootloader sector**: Sector 0 — `0x08000000`–`0x0801FFFF` (128 KB),
  WRP-protectable once `OB_APPLY_WRP` has been issued
- **CAN peripheral on the device**: **FDCAN2 only**, classic CAN
  framing (ISO 11898-1). **CAN FD is not used** by the bootloader
  protocol at this stage — the host tool can open an FD-capable
  adapter for other bus traffic, but the flasher itself speaks
  classic CAN at whatever nominal bitrate the bus is running.
- **Default bitrate**: 500 kbps (host tool default; actual bus rate
  is site-dependent)

---

## Supported host adapters

Four hardware adapter families are supported, all first-class with
no fallback hierarchy, plus a `virtual` in-process loopback for
hardware-less CI and integration tests. The CLI picks between them
with `--interface`.

### CANable (SLCAN firmware)

CANable adapters ship with SLCAN firmware, which exposes the device
as a virtual serial port on every OS with no driver installation.
Recommended adapter for development workstations and CI runners.

| Model | Notes |
|---|---|
| CANable 2.0 | STM32G431, SLCAN at classic CAN rates |
| CANable Pro / MKS CANable Pro | Adds galvanic isolation — use on the car |
| CANtact | Original SLCAN reference design |
| Any SLCAN-compatible USB adapter | Generic serial-line CAN protocol |

**Firmware recommendation**: flash the Elmue 2.5 firmware instead of
the stock canable.io Candlelight build. The stock Candlelight firmware
for CANable 2.0 has a broken Windows MS OS descriptor implementation;
the Elmue firmware fixes this and improves USB throughput. SLCAN mode
is unaffected by this choice — both firmwares expose identical SLCAN
behaviour.

**Per-OS setup**:

| OS | What to do |
|---|---|
| Linux | Add user to `dialout` group: `sudo usermod -aG dialout $USER`. Device appears as `/dev/ttyACM0` or `/dev/ttyUSB0`. |
| macOS | No driver needed. Device appears as `/dev/cu.usbmodem*`. |
| Windows | No driver needed (CDC ACM). Device appears as `COM3`, `COM4`, etc. Check Device Manager. |

### PCAN (PEAK System)

PEAK adapters are recommended when hardware timestamps, high bus
loads, or multi-channel operation are required. PEAK provides the
PCAN-Basic SDK (shared library) for Windows and macOS, and a kernel
module (`peak_usb`) for Linux that exposes a native SocketCAN
interface.

| Model | Notes |
|---|---|
| PCAN-USB | Classic, single channel |
| PCAN-USB Pro | Dual channel, galvanic isolation |
| PCAN-USB FD | Single channel, HW timestamps (FD unused by the flasher) |
| PCAN-USB Pro FD | Dual channel, HW timestamps (FD unused by the flasher) |

**Per-OS setup**:

| OS | What to do |
|---|---|
| Linux | Install `peak_usb` kernel module via `sudo apt install libpcan-dev peak-linux-driver`. Device appears as a SocketCAN interface (`can0`, `can1`). |
| macOS | Install PCAN-Basic for Mac from peak-system.com. The `.dylib` lands at `/usr/local/lib/libPCBUSB.dylib`. |
| Windows | Install PCAN-Basic from peak-system.com. `PCANBasic.dll` lands in `System32`. |

**Channel naming on PCAN** (used with `--channel`):

| OS | Format | Example |
|---|---|---|
| Linux | SocketCAN interface name | `can0` |
| Windows | PCAN channel constant | `PCAN_USBBUS1` |
| macOS | PCAN channel constant | `PCAN_USBBUS1` |

On Linux, PCAN routes through SocketCAN — the `SocketCanBackend`
handles it transparently once `peak_usb` is loaded. On Windows and
macOS the `PcanBackend` calls PCAN-Basic directly via `libloading`.

### Vector (XL Driver Library)

Vector adapters are recommended when working with existing
[CANoe](https://www.vector.com/int/en/products/products-a-z/software/canoe/)
/ [CANalyzer](https://www.vector.com/int/en/products/products-a-z/software/canalyzer/)
toolchains, when the lab already has the XL Driver Library installed
across machines, or when a vehicle-grade adapter with multi-bus
capability is preferred for the car. Vector provides the **XL Driver
Library** (`vxlapi64.dll`) for Windows; Linux support is on the
roadmap but not in v1.

| Model | Notes |
|---|---|
| VN1610 | 2 × CAN / CAN FD, USB 2.0, D-SUB — the typical dev adapter |
| VN1611 | VN1610 variant |
| VN1630A | 4 × CAN / CAN FD, exchangeable transceivers |
| VN1640A | 4 × CAN / CAN FD + SENT |
| VN1670 | Up to 15 channels via piggyback slots |
| Any XL-API-compatible Vector device | Detected automatically by `xlGetDriverConfig` |

CAN FD is not used by the bootloader, but the host tool opens the
channel in classic-CAN mode regardless of the adapter's FD
capability.

**Per-OS setup**:

| OS | What to do |
|---|---|
| Windows | Install the Vector XL Driver Library from [vector.com](https://www.vector.com/int/en/products/products-a-z/software/xl-driver-library/). `vxlapi64.dll` lands in `System32`. |
| Linux | _Not yet supported by `--interface vector`. Roadmap item — Vector's Linux driver does not expose adapters as SocketCAN interfaces, so a dedicated code path is needed._ |
| macOS | _Not supported. Vector does not ship an XL Driver Library for macOS._ |

**Channel naming on Vector** (used with `--channel`): the 0-based XL
channel index as reported by `xlGetDriverConfig`. Run `can-flasher
adapters` to see the index for each physical channel — a VN1610
typically appears as channels `0` and `1` if it's the only Vector
device plugged in.

`VectorBackend` calls XL-API directly via `libloading` against a
runtime-resolved `vxlapi64.dll`. Missing library →
`TransportError::AdapterMissing` with the Vector download URL, the
same fail-soft contract as PCAN.

---

## Language and toolchain

### Rust

- **Single static binary** — no runtime to install on any host OS.
  Distributed via GitHub Releases as pre-built binaries per target.
- **Memory safety** — a flash utility writing to production hardware
  must not have buffer overruns or use-after-free bugs corrupting
  flash operations.
- **Async from day one** — Tokio gives async CAN frame I/O,
  concurrent multi-node flashing, and a clean timeout model.
- **Prior art** — Rust is already in use elsewhere in this project;
  sharing the language keeps context switching low.

### Crate dependencies (v1 scope)

| Crate | Purpose |
|---|---|
| `clap` v4 (derive) | CLI argument parsing |
| `tokio` | Async runtime |
| `socketcan` | Linux SocketCAN backend (CANable candlelight + PCAN via peak_usb) |
| `serialport` | SLCAN serial backend — all platforms |
| `libloading` | Runtime loading of PCAN-Basic DLL on Windows/macOS |
| `object` | ELF parsing (section headers, symbol tables) |
| `ihex` | Intel HEX parsing |
| `crc` | CRC32 for flash verification (ISO-HDLC / IEEE 802.3 polynomial — match the bootloader's HAL CRC unit) |
| `serde` + `serde_json` | Structured JSON output |
| `rusqlite` | Audit log (SQLite) |
| `tracing` + `tracing-subscriber` | Structured logging |
| `indicatif` | Progress bars for flash operations |
| `anyhow` + `thiserror` | Error handling |
| `tabled` | Terminal table rendering for device reports |

Crates deferred to v2 (security scope — see end of file):
`ed25519-dalek`, `aes`, `ctr`, `blake2`.

---

## Transport layer design

### `CanBackend` trait

All adapter backends implement this trait. The rest of the
application is adapter-agnostic.

```rust
#[async_trait]
pub trait CanBackend: Send + Sync {
    /// Send a single CAN frame. Blocks until the frame is accepted
    /// by the adapter.
    async fn send(&self, frame: CanFrame) -> Result<()>;

    /// Receive a single frame, returning Err on timeout.
    async fn recv(&self, timeout: Duration) -> Result<CanFrame>;

    /// Change the bus bitrate. Requires the bus to be in a stopped state.
    async fn set_bitrate(&self, nominal_bps: u32) -> Result<()>;

    /// Instantaneous bus load as a fraction 0.0–1.0. Returns 0.0 if
    /// unsupported.
    fn bus_load(&self) -> f32;

    /// Whether this backend supports hardware timestamps.
    fn has_hw_timestamps(&self) -> bool;

    /// Human-readable adapter description for display and audit log.
    fn description(&self) -> String;
}
```

Classic CAN only in v1 — no `data_bps` argument. If a v2 bootloader
ever adopts CAN FD on the wire, `set_bitrate` grows a second phase
rate.

### Backend implementations

#### `SlcanBackend` — all platforms

Speaks the SLCAN ASCII protocol over a serial port. Used with
CANable (all models) and any other SLCAN-compatible adapter.

- Opens the serial port with the `serialport` crate at 2 Mbaud
  (CANable default USB CDC baud; the rate of the serial link itself
  is irrelevant to CAN bitrate).
- Sets CAN bitrate with the `S` command (standard rates).
- Sends frames with `t` (standard 11-bit). 29-bit / extended frames
  are unused by the bootloader protocol; the backend rejects them
  upstream with a clear error.
- Receives frames by reading and parsing ASCII lines from the serial
  stream.
- Runs the read loop in a dedicated Tokio task feeding a bounded
  channel.
- `has_hw_timestamps()` returns `false` — SLCAN does not expose
  timestamps.
- `bus_load()` returns `0.0` — not available over SLCAN.

```rust
pub struct SlcanBackend {
    port:  Arc<Mutex<Box<dyn SerialPort>>>,
    rx:    Receiver<CanFrame>,
    _task: JoinHandle<()>,
}

impl SlcanBackend {
    pub fn open(port_name: &str, bitrate: u32) -> Result<Self>;
}
```

Channel string format: `/dev/ttyACM0`, `/dev/cu.usbmodem14201`,
`COM3`.

#### `SocketCanBackend` — Linux only

Uses the `socketcan` crate to open a native kernel CAN socket.
Handles:

- CANable with Candlelight firmware (`gs_usb` module)
- PCAN with `peak_usb` kernel module
- Any other SocketCAN-compatible adapter
- Virtual `vcan0` interfaces for testing

```rust
#[cfg(target_os = "linux")]
pub struct SocketCanBackend {
    socket: CanSocket,
}

#[cfg(target_os = "linux")]
impl SocketCanBackend {
    pub fn open(iface: &str) -> Result<Self>;
}
```

Channel string format: `can0`, `can1`, `vcan0`.

`has_hw_timestamps()` returns `true` when the socket is opened with
`SO_TIMESTAMPING` and the adapter reports HW timestamp capability
(PCAN-USB FD, PCAN-USB Pro FD).

#### `PcanBackend` — Windows and macOS

Loads `PCANBasic.dll` (Windows) or `libPCBUSB.dylib` (macOS) at
runtime via `libloading`. Library path resolution order:

1. `PCAN_LIB_PATH` environment variable
2. System default path (`System32` on Windows, `/usr/local/lib` on
   macOS)
3. Executable directory

If the library cannot be found, the error message must include:
- The paths searched
- Download URL: `https://www.peak-system.com/Software-APIs.305.0.html`
- The `PCAN_LIB_PATH` override variable

Initialisation sequence:

1. `CAN_Initialize(channel, btr0btr1, 0, 0, 0)` for classic CAN.
2. Spawn a read thread calling `CAN_Read` in a tight loop, feeding
   frames into a Tokio channel.

```rust
pub struct PcanBackend {
    lib:     Library,
    channel: u16,               // PCAN channel constant e.g. 0x51 = PCAN_USBBUS1
    rx:      Receiver<CanFrame>,
    _thread: JoinHandle<()>,
}

impl PcanBackend {
    pub fn open(channel: &str, bitrate: u32) -> Result<Self>;
}
```

Channel string parsing: `PCAN_USBBUS1` through `PCAN_USBBUS16` map
to numeric constants `0x51`–`0x60`. Unrecognised strings return a
descriptive error listing valid values.

`has_hw_timestamps()` is determined by calling
`CAN_GetValue(PCAN_CHANNEL_FEATURES)` and checking the
`FEATURE_HW_TIME_NANOSECONDS` flag.

`bus_load()` is populated by periodically calling
`CAN_GetValue(channel, PCAN_BUSSPEED_NOMINAL)`.

#### On Linux, `--interface pcan` delegates to `SocketCanBackend`

```rust
#[cfg(target_os = "linux")]
pub mod pcan_linux {
    pub fn open(channel: &str) -> Result<Box<dyn CanBackend>> {
        // On Linux PCAN devices appear as SocketCAN interfaces
        // after peak_usb loads. `channel` is the interface name
        // e.g. "can0".
        Ok(Box::new(SocketCanBackend::open(channel)?))
    }
}
```

The user always uses `--channel can0` on Linux regardless of whether
the adapter is a CANable (candlelight) or a PCAN — the kernel hides
the difference.

#### `VectorBackend` — Windows

Talks to the Vector XL Driver Library via `libloading` against a
runtime-resolved `vxlapi64.dll`. Same fail-soft contract as PCAN: a
missing library surfaces as `TransportError::AdapterMissing` with the
SDK download URL, not a link error.

```rust
#[cfg(target_os = "windows")]
pub struct VectorBackend {
    api: Arc<VectorApi>,        // libloading::Library + resolved fn ptrs
    port_handle: XLportHandle,
    access_mask: XLaccess,      // channel bitmask from xlGetDriverConfig
    rx: Arc<TokioMutex<mpsc::Receiver<CanFrame>>>,
    shutdown: Arc<AtomicBool>,
    reader_handle: StdMutex<Option<JoinHandle<()>>>,
    description: String,
}

#[cfg(target_os = "windows")]
impl VectorBackend {
    /// Open the 0-based XL channel index from `xlGetDriverConfig`,
    /// initialise the channel at `bitrate` if init access is granted,
    /// activate the bus, spawn the polling reader thread.
    pub fn open(channel: &str, bitrate: u32) -> Result<Self>;
}
```

- Bitrate is set via `xlCanSetChannelBitrate(port, mask, bps)` when the
  open call obtains init permission. If another process already
  initialised the bus at a different rate, the flasher logs a warning
  and continues at the existing rate.
- TX is via `xlCanTransmit` from a `spawn_blocking` task; RX is a
  dedicated thread polling `xlReceive` (returns
  `XL_ERR_QUEUE_IS_EMPTY` when no frame, so 1 ms sleep between empty
  reads keeps the core idle).
- Reader filters out TX echoes (`XL_CAN_MSG_FLAG_TX_COMPLETED` /
  `_TX_REQUEST`) and error / remote frames; only 11-bit standard data
  frames make it to the host session.
- `description()` reports the XL channel name (e.g. `"Vector VN1610 1
  Channel 1 (channel 0, 500000 bps)"`) for the audit log.
- `has_hw_timestamps()` returns `false` today even though the XL API
  provides nanosecond timestamps — the flasher doesn't currently
  consume them.

`XLchannelConfig` struct layout has drifted across SDK releases, so
the backend treats `xlGetDriverConfig`'s output as an opaque byte
buffer and reads only the fields it needs (name, channel mask,
channel index, bus capabilities, transceiver name) at documented
offsets. `XL_CHANNEL_CONFIG_SIZE` is the single constant to bump if a
future SDK shifts the slot stride.

#### `VirtualBackend` — all platforms

In-process loopback for testing and CI. Two `VirtualBackend`
instances created from the same `VirtualBus` share a pair of
`tokio::sync::broadcast` channels.

```rust
pub struct VirtualBus {
    host_tx:   Sender<CanFrame>,
    device_tx: Sender<CanFrame>,
}

impl VirtualBus {
    pub fn new() -> Self;
    pub fn host_backend(&self)   -> VirtualBackend;
    pub fn device_backend(&self) -> VirtualBackend;
}
```

### Backend selection at runtime

```rust
#[derive(clap::ValueEnum, Clone, Debug)]
pub enum InterfaceType {
    /// SLCAN serial — CANable and compatible adapters, all platforms
    Slcan,
    /// Native SocketCAN kernel socket — Linux only
    Socketcan,
    /// PEAK PCAN — SocketCAN on Linux, PCAN-Basic SDK on Win/macOS
    Pcan,
    /// Vector XL Driver Library — VN-series, Windows (Linux planned)
    Vector,
    /// In-process virtual bus for testing
    Virtual,
}

pub fn open_backend(
    iface:   InterfaceType,
    channel: Option<&str>,
    bitrate: u32,
) -> Result<Box<dyn CanBackend>> {
    match iface {
        InterfaceType::Slcan => {
            Ok(Box::new(SlcanBackend::open(channel?, bitrate)?))
        }
        #[cfg(target_os = "linux")]
        InterfaceType::Socketcan | InterfaceType::Pcan => {
            Ok(Box::new(SocketCanBackend::open(channel?)?))
        }
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        InterfaceType::Pcan => {
            Ok(Box::new(PcanBackend::open(channel?, bitrate)?))
        }
        #[cfg(target_os = "windows")]
        InterfaceType::Vector => {
            Ok(Box::new(VectorBackend::open(channel?, bitrate)?))
        }
        InterfaceType::Virtual => {
            Ok(Box::new(VirtualBackend::new()))
        }
        // Unsupported (interface, OS) combinations return
        // TransportError::AdapterMissing with a clear message — see
        // src/transport/mod.rs for the full router. Sketch above
        // omits those for readability.
        _ => bail_adapter_missing(iface),
    }
}
```

### Adapter capability matrix

| Backend | Platform | HW timestamps | Bus load | Notes |
|---|---|---|---|---|
| `SlcanBackend` | All | No | No | Zero driver install |
| `SocketCanBackend` | Linux | PCAN FD models | No | Native kernel socket |
| `PcanBackend` | Win / macOS | PCAN FD models | Yes | Requires PEAK SDK loaded at runtime via `libloading` |
| `VectorBackend` | Win | Yes | Yes | XL Driver Library; VN1610 / VN16xx series. Loaded at runtime via `libloading`; Linux support deferred |
| `VirtualBackend` | All | No | No | In-process loopback for CI and integration tests |

---

## CLI interface

### Top-level commands

```
can-flasher <COMMAND>

Commands:
  flash       Flash firmware to a device
  verify      Verify flash contents against a binary without writing
  discover    Scan the bus and list all bootloader-mode devices
  diagnose    Read/clear DTCs, stream logs, stream live data, session health
  config      Read/write device configuration (NVM) and option bytes (WRP)
  replay      Record or replay a CAN session (testing)
  adapters    List detected CAN adapters on this machine
  send-raw    Send one raw CAN frame (app-level reboot-to-BL, bench probes)
```

No `debug` subcommand in v1: the bootloader does not expose
`CMD_MEM_READ`/`CMD_MEM_WRITE`. The runtime-inspection surface that
actually exists is covered by `diagnose log`, `diagnose live-data`,
and `diagnose health`.

No `sign` / `keygen` subcommand in v1: Ed25519 signing is part of
the deferred security scope. See [§ Deferred scope](#deferred-scope-v2-tied-to-bootloader-phase-5).

### Global flags

```
  -i, --interface <TYPE>    CAN backend: slcan | socketcan | pcan | vector | virtual
  -c, --channel <CHANNEL>   Adapter channel (see format table below)
  -b, --bitrate <BPS>       Nominal CAN bitrate [default: 500000]
      --node-id <ID>        Target node ID hex or decimal [default: broadcast]
      --timeout <MS>        Per-frame timeout in ms [default: 500]
      --json                Machine-readable JSON output on stdout
      --log <PATH>          Append session to audit log (SQLite)
      --verbose             Trace-level logging
      --operator <NAME>     Override operator name in audit log
```

#### `--channel` format by adapter and OS

| Adapter | OS | Example |
|---|---|---|
| CANable / SLCAN | Linux | `/dev/ttyACM0` |
| CANable / SLCAN | macOS | `/dev/cu.usbmodem14201` |
| CANable / SLCAN | Windows | `COM3` |
| PCAN via SocketCAN | Linux | `can0` |
| PCAN-Basic | Windows | `PCAN_USBBUS1` |
| PCAN-Basic | macOS | `PCAN_USBBUS1` |
| Vector XL | Windows | `0`, `1` (XL channel index, zero-based) |
| Virtual | All | `vbus0` (ignored internally) |

### `adapters` subcommand

Lists detectable CAN adapters on the current machine:

```
$ can-flasher adapters

SLCAN serial ports:
  /dev/ttyACM0   CANable 2.0  (USB 1d50:606f)

PCAN devices:
  PCAN_USBBUS1   PCAN-USB Pro FD  hw_timestamps=yes  (serial: 0x0041)

SocketCAN interfaces (Linux):
  can0   (up, 500000 bps)
  vcan0  (up, virtual)
```

Detection logic:
- SLCAN: enumerate serial ports and filter by known USB VID/PID list
  (CANable: `1d50:606f`, CANtact: `0403:6015`, etc.)
- PCAN: attempt to load PCAN-Basic; if found, call `CAN_GetValue` over
  all channel constants and collect those that return
  `PCAN_ERROR_OK`.
- SocketCAN (Linux only): enumerate `/sys/class/net/` entries with
  `type == 280`.

### `flash` subcommand

```
can-flasher flash [OPTIONS] <FIRMWARE>

Arguments:
  <FIRMWARE>    Path to .elf, .bin, or .hex firmware file

Options:
  --address <ADDR>        Override load address (required for raw .bin only)
  --require-wrp           Abort if bootloader sector not write-protected [default: false]
  --apply-wrp             Apply WRP if missing, then continue
  --diff                  Only flash sectors that differ from device contents [default: true]
  --no-diff               Force-write every sector regardless of device CRC
  --dry-run               Validate and simulate without sending erase/write commands
  --verify-after          Readback CRC verification after flash [default: true]
  --no-verify-after       Skip post-flash verification
  --jump                  Jump to application after successful flash [default: true]
  --no-jump               Stay in bootloader mode after flash
  --keepalive-ms <MS>     Session keepalive interval [default: 5000]
```

Removed relative to the earlier draft:
- `--slot <A|B>` — the v1.0.0 bootloader is single-slot. Every flash
  targets the app region at `0x08020000..0x080DFFFF`.
- `--sign-key <PATH>` — deferred to v2.

The bootloader's default `--require-wrp` is **false** because dev
boards ship without WRP latched. Production flashing scripts should
override to `--require-wrp --apply-wrp` so a host-side check enforces
it and falls back to applying WRP on first run.

### `verify` subcommand

```
can-flasher verify <FIRMWARE>
  -- issues CMD_FLASH_VERIFY against the installed image.
  -- computes (CRC32, size, version) from the provided binary and
     compares against the metadata FLASHWORD at 0x080FFFE0.
  -- exits 0 if match, 2 if mismatch.
```

`verify` does not read flash back byte-for-byte — the bootloader's
`FLASH_VERIFY` already does a CRC check and commits the metadata, so
this subcommand piggybacks on that to avoid `0xC0000` bytes of
round-trip traffic. For a full byte-for-byte comparison, use
`diagnose live-data` during development or issue repeated
`CMD_FLASH_READ_CRC` calls over the range.

### `discover` subcommand

```
can-flasher discover [--timeout-ms <MS>]
  -- broadcasts CMD_DISCOVER to dst=0xF.
  -- each bootloader on the bus replies with
     [opcode, node_id, proto_major, proto_minor] as a TYPE=DISCOVER
     message back to the host (dst=0x0).
  -- for every responder, optionally follows up with CMD_GET_FW_INFO
     to populate the table; failures degrade gracefully ("no app
     installed" for nodes that NACK with NO_VALID_APP).
  -- prints a table:
       Node ID | Proto | FW Version | Git Hash | Product | WRP Status | Reset Cause
```

WRP status and reset cause come from a follow-up `CMD_GET_HEALTH`
(not session-gated, so no `CONNECT` required).

### `diagnose` subcommand

```
can-flasher diagnose

Subcommands:
  read-dtc                              Read stored fault codes
  clear-dtc                             Clear stored fault codes (prompts for confirmation unless --yes)
  log       [--severity <N>]            Stream bootloader log ring (CMD_LOG_STREAM_START + NOTIFY_LOG)
  live-data [--rate-hz <HZ>]            Stream the 32-byte snapshot (CMD_LIVE_DATA_START + NOTIFY_LIVE_DATA)
  health                                One-shot session health report (CMD_GET_HEALTH, 32-byte record)
  reset    [--mode <hard|soft|bootloader|app>]
```

- `--severity <N>` takes a numeric `BL_LOG_SEV_*` value (`0`=info,
  `1`=warn, `2`=error, `3`=fatal). The bootloader filters below this
  at the drain boundary.
- `--rate-hz <HZ>` must land in `[1, 50]` — out-of-range earns
  `NACK(UNSUPPORTED)`.
- Live-data is emitted as a **fixed 32-byte packed struct** (see
  § CAN protocol specification below). Field interpretation is a
  host-side concern; the flasher ships a signal-definition TOML
  checked in under `signals/bl_live_v1.toml`.
- Reset modes: `hard` (0), `soft` (1 — same as 0 on H7), `bootloader`
  (2, sets RTC BKP0R magic then resets), `app` (3, direct jump
  without reset; validated against `Bootloader_CheckApplication`).

### `config` subcommand

```
can-flasher config

Subcommands:
  ob read                    Read option-byte snapshot (16-byte record)
  ob apply-wrp [--sector-mask <HEX>]
                             Apply WRP; requires an active session and
                             sends the brick-safety token automatically
  nvm read  <KEY>            Read a NVM parameter by key
  nvm write <KEY> <VALUE>    Write a NVM parameter
  nvm erase <KEY>            Tombstone a NVM parameter (value-length = 0)
```

- `ob apply-wrp` prompts for explicit y/N confirmation unless `--yes`
  is passed. The tool builds the command args as
  `[0x00505257, sector_mask]` automatically — the user never sees the
  `"WRP\0"` token. Default `--sector-mask 0x01` (protect sector 0
  only); accepts a hex mask for other layouts.
- After `ob apply-wrp` the device resets; the tool waits
  `--reset-wait-ms` (default 2000 ms), reconnects, and verifies the
  mask took effect via `ob read`. Exit code 7 if the latch didn't
  stick.
- `nvm write` takes a `KEY` as a 16-bit hex / decimal value and a
  `VALUE` as either a quoted UTF-8 string or a `0x`-prefixed hex
  blob. Max value length is 20 bytes (`BL_NVM_MAX_VALUE_LEN`).
- Reserved keys: `0x0001` `BL_NVM_KEY_NODE_ID`, `0x0002`
  `BL_NVM_KEY_CAN_BITRATE`. `0x1000+` is the user / app range.

### `replay` subcommand

```
can-flasher replay record --out <FILE>   Record a live session to file
can-flasher replay run    <FILE>         Replay against virtual backend
```

---

## Memory map (STM32H733ZGT6, bootloader v1.0.0)

| Region | Start | End | Size | Purpose |
|---|---|---|---|---|
| Bootloader | `0x08000000` | `0x0801FFFF` | 128 KB | Sector 0, WRP-protectable |
| Application | `0x08020000` | `0x080DFFFF` | 768 KB | Sectors 1–6, single slot |
| NVM | `0x080E0000` | `0x080FFFDF` | ~128 KB | Sector 7, log-structured KV store |
| Metadata | `0x080FFFE0` | `0x080FFFFF` | 32 B | App metadata FLASHWORD (last word of sector 7) |

- **No A/B rollback.** The bootloader ships with a single 768 KB app
  slot at `0x08020000`. Flashing always targets this region.
- **No dedicated audit/DTC flash region.** The DTC table and log
  ring live in **Backup SRAM** at `0x38800000`, not in flash. They
  survive soft resets and watchdog fires but not power loss (unless
  the board has a coin cell on V_BAT).
- **Metadata FLASHWORD** at `0x080FFFE0` carries
  `[magic, crc32, size, version, reserved…]` for the installed
  image. `CMD_FLASH_VERIFY` rewrites it on success.

The flash manager must **never** erase or write `0x08000000`–`0x0801FFFF`
under any circumstances. The bootloader enforces this independently
(`BL_NACK_PROTECTED_ADDR`), but the host tool checks first so a
typo produces a clearer error than a NACK.

Writable range (host-enforced):
`0x08020000 ≤ addr AND addr + length ≤ 0x080E0000`. The bootloader
uses the same bound (`BL_APP_END + 1`).

---

## CAN protocol specification

Source of truth: `Core/Inc/bl_proto.h` and `Core/Inc/bl_memmap.h` in
the bootloader repo. Any field below must match those files exactly.
Ping ARCHITECTURE.md for prose.

### Frame ID layout (11-bit standard CAN, fix/12)

```
Bits [10:5]  — always zero (keeps every valid ID ≤ 0x01F for
                 CAN arbitration priority vs any non-BL traffic)
Bit    4     — direction   (0 = host→node, 1 = node→host)
Bits [3:0]   — other-end node ID
                 host→node: destination (0x0..0xE unicast, 0xF broadcast)
                 node→host: source      (0x1..0xE; 0x0, 0xF reserved)
```

Concrete ID ranges:

| Range           | Purpose                                              |
|-----------------|------------------------------------------------------|
| `0x000..=0x00E` | host → node CMD + host-side ISO-TP CFs               |
| `0x00F`         | host → broadcast (DISCOVER, broadcast APP_CTRL)      |
| `0x010..=0x01E` | node → host ACK/NACK/NOTIFY/DISCOVER-reply/CF/FC     |
| `0x01F`         | reserved                                             |
| `0x020..=0x7FF` | reserved for future protocol extensions              |

The message TYPE is **not** encoded in the ID — it lives in payload
byte 1 (see below). This keeps the ID space small (5 bits in use),
which maximises CAN arbitration priority on a shared bus, and lets
every frame of an ISO-TP multi-frame transfer share a single ID
(FF and CFs both ride on the `direction | node` pair; the PCI byte
distinguishes them).

### Message-type byte

Carried as **payload byte 1** of every SF and FF (byte 0 is the
ISO-TP PCI, byte 2 is the opcode). CFs and FCs don't carry the
message-type byte — they're unambiguous by PCI, and they inherit
semantic type from their parent FF.

| Value | Name              | Direction       | Description                                            |
|-------|-------------------|-----------------|--------------------------------------------------------|
| `0x00`| `CMD`             | Host → Device   | Bootloader command                                     |
| `0x01`| `ACK`             | Device → Host   | Positive acknowledgement                               |
| `0x02`| `NACK`            | Device → Host   | Negative acknowledgement with error code               |
| `0x03`| `NOTIFY`          | Device → Host   | Unsolicited event (heartbeat, DTC, log, live data)     |
| `0x04`| `DISCOVER_REQUEST`| Host → Broadcast| Discovery ping (sent with dst = 0xF)                   |
| `0x05`| `DISCOVER_REPLY`  | Device → Host   | Discovery reply                                        |
| `0x06`| `APP_CTRL`        | Host → Node     | App-level command; BL silently drops, app handles it   |
| `0x07..0xFF`     | *reserved*       |                                                        |

`APP_CTRL` is the escape hatch for app-controlled workflows (e.g.
"reboot this running app back to bootloader so the next flash can
proceed"). The BL filter accepts these frames — they share the
host→node ID space — but the BL dispatcher silently drops anything
whose msg_type isn't one of its own. Application firmware (see
`demo/MAIN_IFS08_DEMO/`) installs its own filter using the same ID
masks and interprets `APP_CTRL` payloads however it likes. The
reference convention is:

```
APP_CTRL payload (after msg_type):
  opcode   1 B  (0x01 = ENTER_BOOTLOADER, rest app-defined)
  args     0+ B (opcode-specific)

APP_CTRL ENTER_BOOTLOADER reply:
  ID       0x01<node>
  payload  [PCI_SF|3, APP_CTRL, ENTER_BOOTLOADER, 0x00 (status)]
  then the app writes RTC->BKP0R = 0xB00710AD and NVIC_SystemResets.
```

### Multi-frame (ISO-TP)

Byte 0 nibble:

| PCI | Nibble | Description |
|---|---|---|
| Single frame (SF) | `0x0` | Payload length in low nibble (≤ 7 B), rest of frame = payload |
| First frame (FF) | `0x1` | Low nibble + byte 1 encode a 12-bit total length; bytes 2–7 = initial payload |
| Consecutive frame (CF) | `0x2` | Low nibble = sequence index (1, 2, 3, …, wraps mod 16); bytes 1–7 = payload |
| Flow control (FC) | `0x3` | Low nibble = flag (0 CTS, 1 WAIT, 2 OVFL); bytes 1–2 = block size + separation time |

Reassembly timeout: `BL_ISOTP_TIMEOUT_MS` (see bootloader header). On
timeout the bootloader sends `NACK(BL_NACK_TRANSPORT_TIMEOUT)` and
resets its reassembler. Host timeouts should match or slightly
exceed.

Max declared length: 1024 bytes per message. Anything larger earns
`NACK(BL_NACK_TRANSPORT_ERROR)`.

### Command opcodes (source: `bl_proto.h`)

| Opcode | Name | Session | Direction | Payload | Response |
|---|---|:-:|---|---|---|
| `0x01` | `CMD_CONNECT` | – | H→D | `[major, minor]` | ACK `[opcode, major, minor]` or `NACK(PROTOCOL_VERSION)` |
| `0x02` | `CMD_DISCONNECT` | – | H→D | – | ACK `[opcode]` |
| `0x03` | `CMD_DISCOVER` | – | Broadcast | – (host sends `msg_type=DISCOVER_REQUEST`, ID=0x00F) | `[opcode, node_id, major, minor]` with `msg_type=DISCOVER_REPLY` on ID `0x01<node>` |
| `0x04` | `CMD_GET_FW_INFO` | – | H→D | – | ACK `[opcode, <64-byte __firmware_info record>]` or `NACK(NO_VALID_APP)` / `NACK(UNSUPPORTED)` |
| `0x05` | `CMD_GET_HEALTH` | – | H→D | – | ACK `[opcode, <32-byte health record>]` |
| `0x10` | `CMD_FLASH_ERASE` | ✔ | H→D | `[start_le32, length_le32]` | ACK `[opcode]` |
| `0x11` | `CMD_FLASH_WRITE` | ✔ | H→D | `[addr_le32, data…]` (≤ 256 B data) | ACK `[opcode]` |
| `0x12` | `CMD_FLASH_READ_CRC` | ✔ | H→D | `[addr_le32, length_le32]` | ACK `[opcode, crc32_le32]` |
| `0x13` | `CMD_FLASH_VERIFY` | ✔ | H→D | `[expected_crc_le32, expected_size_le32, expected_version_le32]` | ACK `[opcode]` — writes the metadata FLASHWORD on success |
| `0x30` | `CMD_LOG_STREAM_START` | ✔ | H→D | `[min_severity]` | ACK `[opcode]`; `NOTIFY_LOG` starts flowing |
| `0x31` | `CMD_LOG_STREAM_STOP` | ✔ | H→D | – | ACK `[opcode]`; ring contents preserved |
| `0x32` | `CMD_LIVE_DATA_START` | ✔ | H→D | `[rate_hz]` (1–50) | ACK `[opcode]`; `NOTIFY_LIVE_DATA` starts flowing |
| `0x33` | `CMD_LIVE_DATA_STOP` | ✔ | H→D | – | ACK `[opcode]` |
| `0x40` | `CMD_DTC_READ` | – | H→D | – | ACK `[opcode, count_le16, entry_0, entry_1, …]` — 20 B per entry, ≤ 32 entries |
| `0x41` | `CMD_DTC_CLEAR` | ✔ | H→D | – | ACK `[opcode]` |
| `0x50` | `CMD_OB_READ` | – | H→D | – | ACK `[opcode, <16-byte OB status>]` |
| `0x51` | `CMD_OB_APPLY_WRP` | ✔ | H→D | `[token_le32, sector_bitmap_le32?]` | ACK `[opcode]` **before** reset, then MCU resets |
| `0x60` | `CMD_RESET` | – | H→D | `[mode]` (0..3) | ACK `[opcode]` emitted **before** reset |
| `0x61` | `CMD_JUMP` | – | H→D | `[addr_le32]` | ACK `[opcode]` emitted **before** jump |
| `0x80` | `CMD_NVM_READ` | ✔ | H→D | `[key_le16]` | ACK `[opcode, len, value…]` or `NACK(NVM_NOT_FOUND)` |
| `0x81` | `CMD_NVM_WRITE` | ✔ | H→D | `[key_le16, value…]` (≤ 20 B value) | ACK `[opcode]`; `value_len == 0` is a tombstone |

Session-gated opcodes require a preceding successful `CMD_CONNECT`.
The session is cleared by `CMD_DISCONNECT`, a watchdog timeout, or
MCU reset.

`CMD_OB_APPLY_WRP` token: first 4 args bytes **must** equal
`BL_OB_APPLY_TOKEN = 0x00505257` ("WRP\0", LE) or the request earns
`NACK(OB_WRONG_TOKEN)`. Optional bytes 4..7 carry a little-endian
`sector_bitmap`; default `0x01` (protect sector 0 only). On recent
H7 silicon WRP is only cleared by a full chip erase via external
debugger — the token is a deliberate brick-safety belt.

### Unsolicited notifications (`TYPE = NOTIFY`, dst = host)

| Opcode | Name | Payload |
|---|---|---|
| `0xF0` | `NOTIFY_HEARTBEAT` | `[opcode, node_id, reset_cause, flags_low_byte, uptime_le24]` — 7 B; 1 Hz while session active |
| `0xF1` | `NOTIFY_DTC` | `[opcode, dtc_entry_20B]` — single 20-byte DTC entry; emitted only on genuinely new codes (dedupes are silent) |
| `0xF2` | `NOTIFY_LOG` | `[opcode, entries…]` where each entry is `[text_len_u8, severity_u8, uptime_le32, text_bytes…]` — multiple entries packed back-to-back per notification |
| `0xF3` | `NOTIFY_LIVE_DATA` | `[opcode, <32-byte snapshot>]` — at `rate_hz` while live stream active |

The host must subscribe `NOTIFY_HEARTBEAT` implicitly for every open
session — the device emits it at 1 Hz once `CONNECT` succeeds and
the flasher uses it to detect a dropped / crashed device.

### Fixed-layout records

**`__firmware_info`** — 64 B, emitted by the **application** at
`0x08020400`, consumed by `CMD_GET_FW_INFO`:

```
offset  size  field
  0      4    magic                = 0xF14F1B00
  4      4    record_version       = 0x00010000  (major.minor)
  8      4    fw_version_major
 12      4    fw_version_minor
 16      4    fw_version_patch
 20      4    mcu_id               = DBGMCU IDCODE (e.g. 0x00000450 for STM32H7x3)
 24      8    git_hash             = first 8 bytes of SHA-1
 32      8    build_timestamp      = unix seconds, LE
 40     16    product_name         = ASCII, NUL-padded
 56      8    reserved             = zero
```

**Health record** — 32 B, returned by `CMD_GET_HEALTH`:

```
offset  size  field                 notes
  0      4    uptime_seconds        since boot
  4      4    reset_cause           BL_RESET_* (POWER_ON / PIN / SOFTWARE / IWDG / WWDG / LOW_POWER / BROWNOUT)
  8      4    flags                 bitmask; see below
 12      4    flash_write_count     reserved until Phase 4 NVM-backed counter
 16      4    dtc_count
 20      4    last_dtc_code
 24      8    reserved              zero
```

Health flags bitmask (bits 0–1 and 4 live today):

```
bit 0   BL_HEALTH_FLAG_SESSION_ACTIVE
bit 1   BL_HEALTH_FLAG_VALID_APP_PRESENT
bit 4   BL_HEALTH_FLAG_WRP_PROTECTED    — sector 0 is WRP-latched
```

**Live-data snapshot** — 32 B, emitted via `NOTIFY_LIVE_DATA`:

```
offset  size  field
  0      4    uptime_ms
  4      2    frames_rx        (saturates at 0xFFFF)
  6      2    frames_tx
  8      2    nacks_sent
 10      2    dtc_count
 12      2    last_dtc_code
 14      1    flags            — SESSION_ACTIVE(0) VALID_APP_PRESENT(1) LOG_STREAMING(2) LIVEDATA_STREAMING(3) WRP_PROTECTED(4)
 15      1    last_opcode      — most recent CMD opcode received
 16      4    last_flash_addr
 20      4    isotp_rx_progress
 24      4    session_age_ms
 28      4    reserved
```

**DTC entry** — 20 B, packed into `CMD_DTC_READ` response:

```
offset  size  field
  0      2    code
  2      1    severity         — INFO(0) WARN(1) ERROR(2) FATAL(3)
  3      1    occurrence_count — saturates at 255
  4      4    first_seen_uptime_seconds
  8      4    last_seen_uptime_seconds
 12      4    context_data
 16      4    reserved
```

**Option-byte status** — 16 B, returned by `CMD_OB_READ`:

```
offset  size  field
  0      4    wrp_sector_mask  — bit N set = sector N WRP-protected (HAL convention, bit-sense
                                 inverted from the underlying FLASH_WPSN_CUR1R)
  4      4    user_config      — raw H7 user-config word
  8      1    rdp_level        — raw OB_RDP_LEVEL_* byte
  9      1    bor_level        — raw OB_BOR_LEVEL_* byte
 10      2    reserved
 12      4    reserved_ext
```

**NVM entry** — 32 B, one FLASHWORD; the bootloader's on-disk
format. The host only needs this if it decodes NVM dumps offline;
normal `CMD_NVM_READ` / `CMD_NVM_WRITE` traffic deals in
`[key_le16, value…]`.

### NACK error codes (source: `bl_proto.h`)

| Code | Name | Meaning |
|---|---|---|
| `0x01` | `BL_NACK_PROTECTED_ADDR` | Write/erase range touches sector 0 or the metadata FLASHWORD |
| `0x02` | `BL_NACK_OUT_OF_BOUNDS` | Address outside the writable app region |
| `0x03` | `BL_NACK_CRC_MISMATCH` | `FLASH_VERIFY`: computed CRC != expected |
| `0x06` | `BL_NACK_BAD_SESSION` | Session-gated opcode issued without a prior `CONNECT` |
| `0x07` | `BL_NACK_FLASH_HW` | HAL flash erase / program returned non-OK |
| `0x08` | `BL_NACK_BUSY` | Previous op not complete (reserved) |
| `0x09` | `BL_NACK_TRANSPORT_TIMEOUT` | ISO-TP reassembly ran past `BL_ISOTP_TIMEOUT_MS` |
| `0x0A` | `BL_NACK_TRANSPORT_ERROR` | ISO-TP PCI / seq / overflow |
| `0x0B` | `BL_NACK_PROTOCOL_VERSION` | Host/device major version disagree |
| `0x0C` | `BL_NACK_NO_VALID_APP` | Jump / reset-to-app with no valid image |
| `0x0D` | `BL_NACK_NVM_NOT_FOUND` | `NVM_READ` for a key with no live value |
| `0x0E` | `BL_NACK_NVM_FULL` | `NVM_WRITE` can't fit even after compaction |
| `0x0F` | `BL_NACK_OB_WRONG_TOKEN` | `OB_APPLY_WRP` missing / wrong confirmation token |
| `0xFE` | `BL_NACK_UNSUPPORTED` | Unknown opcode, bad arg length, or unaligned address — the generic "bad request" |

Codes `0x04` (signature invalid) and `0x05` (replay counter low) are
reserved for v2 / Phase-5 reactivation and never emitted by v1.0.0.

---

## Flash manager requirements

- **Sector-aware erase**: map the write region to sectors (128 KB
  each); erase only touched sectors. The bootloader already does
  this on its side; the host tool shouldn't issue per-byte erases.
- **Diff flash**: compute CRC32 per sector on the device via
  `CMD_FLASH_READ_CRC`; skip matching sectors. Enabled by default.
  Idempotent re-flash of an unchanged image should issue zero erases
  and zero writes.
- **Write chunk size**: 256 B per `CMD_FLASH_WRITE` on classic CAN.
  ISO-TP segments into SF + CFs; a 256 B write becomes one FF + ~37
  CFs at 8-byte frames.
- **Alignment**: `CMD_FLASH_WRITE` address must be FLASHWORD-aligned
  (32 B on H7). The tool pads partial tail FLASHWORDs with `0xFF`
  to match the bootloader's behaviour.
- **Verification**: after each sector write, issue `CMD_FLASH_READ_CRC`
  and compare. Hard-fail on mismatch.
- **Final commit**: once all writes land, issue `CMD_FLASH_VERIFY`
  with the computed `(crc32, size, version)` triple. The bootloader
  re-reads the range, re-computes CRC, and on match programs the
  metadata FLASHWORD at `0x080FFFE0`. Mismatch → `NACK(CRC_MISMATCH)`,
  no metadata commit.
- **Dry-run**: all validation and CRC computation runs; no erase or
  write commands are transmitted.
- **No rollback slots**: single-slot bootloader; no A/B semantics to
  target.

---

## Firmware loader requirements

- **ELF**: parse section headers to extract load addresses
  automatically. Accept any segment entirely inside the writable app
  region; reject segments that touch sector 0 or go past `0x080E0000`
  before sending any frame.
- **Intel HEX**: full support including extended linear address
  records.
- **Raw binary**: requires explicit `--address`. Default behaviour
  when no address is given is to reject with exit code 8.
- **Build metadata**: read the `__firmware_info` symbol or section
  at `BL_APP_BASE + 0x400 = 0x08020400`. Display on connect, embed
  in the audit-log row.
- **Address validation**: validate **all** segments before sending
  any frame. Fail immediately (exit code 3) if any segment overlaps
  `0x08000000`–`0x0801FFFF` or goes past `0x080DFFFF`.
- **CRC / size / version computation**: the three fields passed to
  `CMD_FLASH_VERIFY` are computed from the **final** image layout
  (post-pad to sector boundary, post-`0xFF` fill of gaps between
  segments). Version is taken from `__firmware_info.fw_version_*`
  and packed as `(major << 16) | (minor << 8) | patch`.

---

## Multi-node orchestration

- `--node-id` accepts a comma-separated list or `all`.
- Flash operations for multiple nodes run concurrently, interleaved
  by node ID.
- Each node has independent progress tracking and error state.
- One failing node is isolated — others continue.
- Final output includes a per-node result table: `OK | FAIL | SKIPPED`
  with a reason column.

---

## Diagnostics requirements

- **Read DTC**: multi-frame response containing
  `[count_le16, entry_0, entry_1, …]`. Max 32 entries, 20 B each,
  so the worst-case payload is `2 + 32 * 20 = 642 B` — one FF plus
  ~92 CFs at 8-byte frames.
- **Clear DTC**: session-gated. Confirmation prompt unless `--yes`
  is passed.
- **Log stream**: `CMD_LOG_STREAM_START(min_severity)` begins flowing
  `NOTIFY_LOG` frames from the BKPSRAM log ring. The tool reassembles
  multi-frame chunks, displays with host-clock timestamps alongside
  the device-reported ones, and filters below `--severity` on the
  host too (defensive — bootloader already filters at the drain).
- **Live data**: `CMD_LIVE_DATA_START(rate_hz)` begins flowing 32-byte
  snapshots via `NOTIFY_LIVE_DATA` at 1–50 Hz. Signal definitions
  live in `signals/bl_live_v1.toml`, checked into this repo; hosts
  decode by offset + type + scale.
- **Health report**: one-shot `CMD_GET_HEALTH` returning the 32-byte
  record. Decoded to a human-readable table that includes reset
  cause, uptime, flags (session active, valid app, WRP protected),
  DTC count, and last DTC code.
- **Reset**: `CMD_RESET(mode)` with modes 0..3 as documented in the
  opcode table.

---

## Session timeout and watchdog

- Bootloader exits session mode after `BL_SESSION_TIMEOUT_MS`
  (**30 s** default) without a valid frame from the host. The
  watchdog bumps on any RX, so any opcode — including a repeated
  `CMD_CONNECT` — keeps it alive.
- The tool sends a keepalive every `--keepalive-ms` (default 5000 ms)
  during long operations. Keepalive opcode: `CMD_GET_HEALTH` (cheap,
  session-agnostic, and incidentally refreshes the local view of
  device state).
- On `BL_NACK_BAD_SESSION` from a session-gated opcode, the tool
  re-issues `CMD_CONNECT` and retries the failed op once before
  escalating.

---

## Output and CI integration

### Exit codes

Canonical table lives in
[docs/USAGE.md § Exit codes](docs/USAGE.md#exit-codes) — the
operator-facing doc owns it so pit-wall users find the table in
the first doc they'd reach for. Codes `5` (signature failed) and
`6` (replay rejection) remain reserved for Phase-5 reactivation.

### JSON output (`--json`)

```json
{
  "operation": "flash",
  "status": "ok",
  "adapter": {
    "type": "slcan",
    "channel": "/dev/ttyACM0",
    "hw_timestamps": false,
    "description": "CANable 2.0 (USB 1d50:606f)"
  },
  "node_id": "0x03",
  "bootloader": {
    "proto_major": 0,
    "proto_minor": 1,
    "reset_cause": "POWER_ON",
    "wrp_protected": false
  },
  "firmware": {
    "path": "build/firmware.elf",
    "git_hash": "a1b2c3d4",
    "version": "1.4.2",
    "size_bytes": 98304,
    "crc32": "0xDEADBEEF",
    "product_name": "IFS08-CE-ECU"
  },
  "sectors_erased": [1, 2],
  "sectors_written": [1, 2],
  "sectors_skipped": [3, 4, 5, 6],
  "duration_ms": 4312,
  "error": null
}
```

No `device_uid` field in v1 — the bootloader doesn't expose a UID
read opcode. Identity is established by node ID + `__firmware_info`
content.

### Audit log (SQLite)

```sql
CREATE TABLE sessions (
  id            INTEGER PRIMARY KEY,
  timestamp     TEXT NOT NULL,
  operation     TEXT NOT NULL,
  adapter_type  TEXT,
  adapter_chan  TEXT,
  node_id       TEXT,
  fw_hash       TEXT,
  fw_version    TEXT,
  result        TEXT NOT NULL,
  error         TEXT,
  operator      TEXT,
  git_user      TEXT
);
```

### GitHub Actions — CANable

```yaml
- name: Flash firmware via CAN (CANable)
  run: |
    can-flasher flash build/firmware.elf \
      --interface slcan \
      --channel /dev/ttyACM0 \
      --bitrate 500000 \
      --require-wrp \
      --apply-wrp \
      --json \
      --log flash_audit.sqlite
  env:
    CAN_FLASHER_OPERATOR: ${{ github.actor }}
```

### GitHub Actions — PCAN (Linux runner with peak_usb)

```yaml
- name: Install PCAN kernel module
  run: |
    sudo apt-get install -y libpcan-dev
    sudo modprobe peak_usb
    sudo ip link set can0 up type can bitrate 500000

- name: Flash firmware via CAN (PCAN)
  run: |
    can-flasher flash build/firmware.elf \
      --interface pcan \
      --channel can0 \
      --bitrate 500000 \
      --json \
      --log flash_audit.sqlite
  env:
    CAN_FLASHER_OPERATOR: ${{ github.actor }}
```

When `GITHUB_STEP_SUMMARY` is set, a markdown flash report is
written automatically: adapter type + channel, node ID, firmware
version, git hash, product name, sectors touched, WRP status,
duration, result.

---

## Virtual / replay backend

- **Record**: `can-flasher replay record --out session.candump` —
  captures all frames from any live backend in Linux `candump`
  format (compatible with `can-utils`).
- **Replay**: `can-flasher replay run session.candump` — replays
  against the virtual backend, asserting identical frame sequences.
- **Virtual loopback**: `--interface virtual` spins up an in-process
  stub bootloader (separate Tokio task). Full flash pipeline runs
  in CI without hardware.
- Tests to cover: address validation, NACK handling (at least the
  12 codes implemented in v1), multi-frame reassembly, WRP
  enforcement + token gating, `CMD_OB_APPLY_WRP` token rejection,
  session timeout, concurrent multi-node flash, keepalive behaviour,
  reconnect-after-`BAD_SESSION`.

---

## Project layout

```mermaid
mindmap
  root((can-flasher repo))
    Top-level docs
      Cargo.toml · manifest + target-gated deps
      README.md
      ARCHITECTURE.md · module-level notes
      REQUIREMENTS.md · this file
      ROADMAP.md · auto-generated from YAML
    docs/
      INSTALL.md · toolchain + per-OS adapter setup
      USAGE.md · subcommand reference + examples
      CONTRIBUTING.md · contributor guide
      PERFORMANCE.md · flash-speed baseline + --profile
    src/
      lib.rs · pub mod declarations
      main.rs · clap entry point
      logging.rs · tracing-subscriber bootstrap
      cli/
        mod.rs · Cli + Command + GlobalFlags
        adapters · enumerate detected adapters
        flash · end-to-end programming
        verify · readback CRC comparison
        discover · bus scan + device table
        diagnose · DTC / log / live-data / health
        config · NVM + option bytes + WRP
        replay · candump record/play
        send_raw · single raw CAN frame
      protocol/ · pure wire-format, no I/O
        mod.rs · CanFrame + ParseError
        ids.rs · FrameId + MessageType + node consts
        opcodes.rs · Command/Notify/Nack codes
        isotp.rs · ISO-TP segment + reassemble
        records.rs · FirmwareInfo + Health + Live + Dtc + ObStatus
        commands.rs · typed command builders
        responses.rs · Response parser
      transport/ · adapter I/O behind CanBackend trait
        mod.rs · CanBackend + open_backend router
        virtual_bus.rs · in-process loopback
        stub_device.rs · bootloader simulator
        slcan.rs · all OSes
        socketcan.rs · Linux only
        pcan.rs · Windows + macOS
        vector.rs · Windows XL Driver Library
      session/
        mod.rs · handshake + keepalive + reconnect
      firmware/
        mod.rs · Image + address validation
        loader.rs · ELF / Intel HEX / raw .bin
      flash/
        mod.rs · FlashManager sector map + diff + verify
    tests/ · against VirtualBus + StubDevice
      virtual_pipeline.rs · end-to-end session round-trip
      flash_manager.rs · FlashManager state-machine harness
      *_subcommand.rs · one per CLI subcommand
    demo/
      MAIN_IFS08_DEMO · reference STM32H733 app
    apps/
      can-studio · Tauri 2 desktop app
    editor/
      vscode · TypeScript extension
    .github/
      roadmap.yaml · source of truth for ROADMAP.md
      scripts/render_roadmap.py
      workflows/
        ci.yml · fmt/clippy/build/test matrix
        release.yml · v* CLI binaries
        editor-release.yml · editor-v* VSIX
        can-studio-release.yml · can-studio-v* native bundles
        sync-dev-after-release.yml · dispatch recovery handle
        branch-issue.yml · auto-create tracking issue on branch push
        close-on-dev-merge.yml · auto-close on dev merge
        roadmap.yml · regenerate ROADMAP.md from YAML
```

The `apps/can-studio/` and `editor/vscode/` integrations have their
own READMEs detailing the dependency model — Studio links the
`can-flasher` crate by path; the VS Code extension shells out to
an installed `can-flasher` binary on `PATH`.

No `src/security/` directory in v1 (Phase 5 security work — Ed25519
signing, challenge-response, replay counter — is deferred); no
`src/debug/` (no `CMD_MEM_READ` / `CMD_MEM_WRITE` to wrap). The
`audit` / `output / summary` / `protection / wrp` boxes from earlier
planning drafts never materialised as separate modules — that
functionality is inlined into the relevant subcommand instead.

---

## Cross-platform build matrix

```yaml
strategy:
  matrix:
    include:
      - os: ubuntu-latest
        target: x86_64-unknown-linux-gnu
      - os: ubuntu-latest
        target: aarch64-unknown-linux-gnu    # Raspberry Pi / ARM SBCs on the car
      - os: macos-latest
        target: aarch64-apple-darwin         # Apple Silicon
      - os: macos-latest
        target: x86_64-apple-darwin          # Intel Mac
      - os: windows-latest
        target: x86_64-pc-windows-msvc
```

Release artifacts: pre-built binaries for all five targets on GitHub
Releases. Optional: Homebrew tap for macOS/Linux, winget manifest
for Windows.

`PcanBackend` compiles on all targets but is a runtime no-op on
Linux (replaced by `SocketCanBackend`). `SocketCanBackend` is
excluded from Windows/macOS builds via `#[cfg(target_os = "linux")]`.
The PCAN SDK is never a compile-time dependency — it is loaded at
runtime via `libloading`, so the binary works on machines without
the SDK installed as long as `--interface pcan` is not used.

---

## Non-functional requirements

- **Flash throughput**: ≥ 30 KB/s at 500 kbps on SLCAN. PCAN and
  SocketCAN backends should meet or exceed this due to lower
  host-side latency. A full 768 KB application flash must complete
  in under 30 s on classic CAN at 500 kbps.
- **Latency**: discovery broadcast response rendered within 100 ms
  of the first device reply on all backends.
- **Reliability**: a single dropped or NACK-`TRANSPORT_*` frame
  triggers a retransmit, not a failed flash. Maximum 3 retries per
  frame before aborting with exit code 1.
- **Idempotency**: re-flashing an already-current image completes
  in < 2 s in diff mode (no sectors changed → no erases, no writes,
  only CRC reads).
- **No root on Linux**: `dialout` group for SLCAN serial, `can`
  group or a udev rule for SocketCAN. The utility must not request
  elevated privileges.
- **No admin on Windows**: PCAN driver installation is a one-time
  administrator setup. The utility itself runs as a normal user.
- **SDK optional at compile time**: the binary must compile and run
  on a machine without the PCAN SDK. Missing SDK is a runtime
  error with a clear message; it is never a build failure.

---

## Bootloader firmware interface expectations

The utility assumes the v1.0.0 bootloader exposes the following over
the CAN protocol:

- **`__firmware_info` record** — 64 bytes at `0x08020400`
  (`BL_APP_BASE + 0x400`), emitted by the **application**. Magic
  `0xF14F1B00`. Layout as documented above. Consumed via
  `CMD_GET_FW_INFO`.
- **Metadata FLASHWORD** — 32 bytes at `0x080FFFE0`, written by the
  bootloader on `CMD_FLASH_VERIFY` success. Magic `0xB007C0DE`.
  Carries `(crc32, size, version)` for the currently installed
  image.
- **Log ring** — in **Backup SRAM** at `0x38800000`, drained via
  `CMD_LOG_STREAM_START` + `NOTIFY_LOG`. Not directly memory-readable
  — host must go through the opcode.
- **DTC table** — in **Backup SRAM**, 32 entries × 20 B each + 16 B
  header. Drained via `CMD_DTC_READ`, cleared via `CMD_DTC_CLEAR`,
  new entries announced via `NOTIFY_DTC`.
- **Live-data snapshot** — 32-byte packed struct composed on demand
  in `bl_live_fill_snapshot`. Emitted via `CMD_LIVE_DATA_START` +
  `NOTIFY_LIVE_DATA`.
- **NVM store** — log-structured KV in sector 7
  (`0x080E0000..0x080FFFDF`), accessed exclusively via
  `CMD_NVM_READ` / `CMD_NVM_WRITE`. Reserved keys `0x0001`
  `BL_NVM_KEY_NODE_ID`, `0x0002` `BL_NVM_KEY_CAN_BITRATE`.

---

## Deferred scope (v2, tied to bootloader Phase 5)

The following host-side features are **intentionally out of scope
for v1** and will be picked up if / when the bootloader reactivates
Phase 5. Until then the flasher rejects the corresponding flags with
a clear "not supported by this bootloader version" message — never a
compile error, never a silent no-op.

- **`sign` / `keygen` subcommands** — Ed25519 offline signing (tied
  to bootloader `feat/17-ed25519-sign`).
- **`--sign-key <PATH>`** global flag — embeds an Ed25519 signature
  into the outgoing image (same dependency).
- **Replay counter / monotonic version reject** — tied to bootloader
  `feat/18-replay-counter`. Flasher would read the stored counter
  via `CMD_NVM_READ` key `0x0003` and refuse to transmit an image
  whose version is ≤ the stored counter.
- **Challenge-response session auth** — tied to bootloader
  `feat/19-challenge-response`. `CMD_CONNECT` ACK would carry a
  16-byte nonce; the flasher would issue `CMD_AUTH` (opcode TBD)
  with `Blake2b-MAC(preshared_key, nonce)` before any flash op is
  accepted.
- **`--encrypt` flag + AES-128-CTR DATA-frame encryption** — tied
  to bootloader `feat/20-encrypted-transport`, itself also deferred
  at Phase 5 close.
- **Exit codes `5` (signature failed) and `6` (replay rejected)** —
  allocated but never emitted in v1.
- **Adapter-side device UID read** — the bootloader has no opcode
  for reading the MCU UID. If Phase 5 adds one (e.g. for
  challenge-response key derivation) the tool's `discover` table
  grows a `UID` column at that time.

The `signals/bl_live_v1.toml` file and the `bl_live_v1` signal set
name are versioned with `_v1` so that a future snapshot layout
change (inevitable if Phase 5 adds authenticated-session flags or
a signature-verify-result counter) can ship a `_v2` file side-by-
side without breaking old hosts.
