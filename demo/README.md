# `demo/` — reference application firmware for the isc-fs CAN bootloader

This directory holds the reference target that our bench flow flashes,
verifies, and jumps to end-to-end. Two artifacts live here:

- **`MAIN_IFS08_DEMO/`** — full STM32CubeMX + CMake project source.
  Compiles down to the app binary that `can-flasher flash` deploys to the
  H733. Includes the FDCAN filter setup, the protocol-compliant
  `APP_HandleCanFrame` that accepts `send-raw`'s reboot-to-BL frame,
  and the minimal LED-toggling main loop that gives a visible "app
  is running" signal.
- **`MAIN_IFS08_DEMO.bin`** — pre-built flat binary of the app, kept
  alongside the source so a cold clone of `MingoCAN` can exercise
  the flash pipeline without the full STM32 toolchain. Rebuilt
  whenever the source changes; regenerate via the CMake flow below.

## Why the app source lives in the flasher repo

The BL, the flasher, and the app all speak the same wire protocol;
they have to stay in lockstep. Keeping the reference app here lets one
PR touch the wire format, the flasher and the reference app together.

The real product application (private, hardware-specific) is not
this one — it's in `IFS08_PRIVATE`. This demo is deliberately
minimal: a blinky with enough CAN plumbing to accept the
reboot-to-BL frame and identify itself over the bus. Anything fancier
goes on the private side.

## Prerequisite: bootloader already flashed

This demo is the *application*. Before it can run, sector 0 needs
the bootloader programmed via SWD and (ideally) WRP-latched. If
that hasn't been done yet, follow [the bootloader's
PROVISIONING.md](https://github.com/isc-fs/stm32-can-bootloader/blob/main/PROVISIONING.md)
first — it covers a fresh board from factory state all the way to
shipping-ready.

## App-side protocol summary

`MAIN_IFS08_DEMO` installs an FDCAN2 filter that accepts host→node
traffic for its own node ID and broadcasts. Incoming frames are
decoded as ISO-TP SF (we only handle single-frame app-ctrl today;
multi-frame app commands can be added if needed) using the wire
layout documented in [../REQUIREMENTS.md § Message type
byte](../REQUIREMENTS.md#message-type-byte):

```
payload[0] = PCI  (0x02..0x07 for SF with len=2..7)
payload[1] = msg_type  (0x06 = APP_CTRL)
payload[2] = opcode  (0x01 = APP_CMD_ENTER_BOOTLOADER)
```

On receipt of `APP_CTRL / ENTER_BOOTLOADER`, the app writes the
boot-request magic `0xB00710AD` to `RTC->BKP0R` and issues
`NVIC_SystemReset()`. The bootloader sees the magic at power-on and
stays in BL mode (auto-jump gated off), so the next `can-flasher flash` just
works without physical contact with the board.

Any `msg_type` the app doesn't understand is silently dropped — the
BL protocol's own traffic (`msg_type` 0x00..0x05) transits through
the same filter but lands in this branch and never provokes an app
reply. That's deliberate: when both the app *and* another BL on the
bus happen to be looking at the same frame ID (e.g. during a
broadcast discover), only the BL should speak up.

## Building

Requires STM32CubeIDE toolchain or a standalone ARM GCC + CMake.

```shell
cd demo/MAIN_IFS08_DEMO
cmake -B build -S . -G Ninja
cmake --build build
# → build/MAIN_IFS08_DEMO.elf + .bin
cp build/MAIN_IFS08_DEMO.bin ../MAIN_IFS08_DEMO.bin  # update the checked-in pre-built
```

## Flashing

From the repo root:

```shell
CHAN=$(ls /dev/cu.usbmodem* | head -1)
can-flasher --interface slcan --channel "$CHAN" --bitrate 500000 \
   --node-id 0x1 --timeout 10000 \
   flash demo/MAIN_IFS08_DEMO.bin --address 0x08020000 --verify-after --jump
```

## Returning to bootloader (from a running app)

The demo app implements the original ISO-TP `APP_CTRL` convention on
`0x001`, not the `0x002` + per-node magic used by the production
AMS/ECU apps ([docs/CLI.md § send-raw](../docs/CLI.md#send-raw--one-raw-frame)).
`can-flasher flash`'s automatic reboot uses the latter — for node 0x1
it sends `0x002` + `B0 07 AD 12` (`--enter-bootloader auto`), which
the demo ignores — so with the demo app send this frame by hand
before flashing:

```shell
can-flasher --interface slcan --channel "$CHAN" --bitrate 500000 \
   send-raw 0x001 03 06 01
# → frame: ID 0x001, PCI SF len=3, APP_CTRL(0x06), ENTER_BOOTLOADER(0x01)
# → app ACKs on 0x011, resets, BL holds.
```
