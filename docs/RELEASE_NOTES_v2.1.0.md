# v2.1.0 — SWD flashing across all surfaces

Three months of work landed in this release closes the long-standing
"first boot" gap on the host side: a bare STM32 that's never run
the CAN bootloader can now be programmed end-to-end from the same
tool operators use for every subsequent app update.

## Highlights

### SWD flashing via ST-LINK (`--features swd`)

The new `swd-flash` subcommand and Studio **Burn bootloader** tab
drive an ST-LINK V2 / V3 through [probe-rs](https://probe.rs):

- `.elf` / `.hex` / `.bin` artifacts; auto-pick on single probe,
  `--probe-serial` selector when several are wired in.
- Defaults tuned for the team's STM32H733; `--chip` accepts any
  probe-rs target string for other STM32 families.
- Erase + write + verify + optional reset, all in one invocation.

```bash
# Lay the bootloader onto a fresh chip
can-flasher swd-flash bootloader.elf

# Then switch to the over-CAN path for every app update afterwards
can-flasher flash --interface slcan --channel /dev/ttyACM0 app.elf
```

### Auto-download from BL release page

Pair `--release` with `swd-flash` and the matching `CAN_BL.elf`
is fetched from [`isc-fs/stm32-can-bootloader`](https://github.com/isc-fs/stm32-can-bootloader/releases)
and cached locally. Tagged releases are immutable, so a repeat
flash never hits the network again:

```bash
can-flasher swd-flash --release             # latest
can-flasher swd-flash --release v1.2.0      # pinned
can-flasher swd-flash --release --release-format hex
```

Cache location: `<platform-cache>/can-flasher/bootloaders/<tag>/CAN_BL.<ext>`.

The Studio's **Burn bootloader** tab gets a matching "fetch from
releases" row inside the Firmware card — blank tag pulls latest;
the artifact field auto-fills and the existing flash button just
works.

### Bug fixes

- Studio's `can_flasher_version()` now reads `CARGO_PKG_VERSION`
  instead of a hardcoded string that had drifted three releases
  behind (`"1.2.0"` while the binary was actually 2.0.0).

## Install

CLI binaries, the VS Code extension VSIX, and Studio bundles for
all three OSes are attached below. Pick the one that matches your
platform.

For the operator who only wants the SWD ergonomics:

```bash
cargo install --git https://github.com/isc-fs/can-flasher.git --features swd
```

Per-OS prerequisites for the libusb / WinUSB stack live in
[docs/INSTALL.md § ST-LINK + SWD](https://github.com/isc-fs/can-flasher/blob/main/docs/INSTALL.md#st-link--swd-optional-feature-swd).
The default operator binary (no `swd`) is unchanged.

## Compatibility

- Bootloader contract: unchanged from v1.2.0; flasher v2.0.0
  protocol parity is preserved.
- Operators on the old `editor-v*` / `can-studio-v*` tag streams
  should switch to this unified `v*` namespace — that's been the
  rule since v2.0.0; we mention it again here for anyone still on
  v1.x.

## Closes

- #190 — feasibility spike: `swd-flash` CLI subcommand
- #192 — Studio Burn bootloader tab
- #194 — auto-download bootloader from BL release page
