# v3.2.0

**Burn the bootloader without an ST-LINK.** A new board can now get its CAN
bootloader, and its node ID, straight over its own USB port, using the STM32's
built-in USB DFU bootloader.

## Burn bootloader over USB

**Burn bootloader** has a new **Connection** switch: **ST-LINK (SWD)**, as
before, or **USB (DFU)**.

1. Hold **BOOT0** high (jumper or button).
2. Connect the board's **USB** to the laptop.
3. Reset or power-cycle the board, then press **⟳**. The **Board in DFU mode**
   card shows it.
4. Pick the bootloader (**Fetch** pulls the latest), set **Provision node-id**
   to the board's role, and press **Burn bootloader**.
5. Set **BOOT0** back low afterwards, or the next reset re-enters DFU.

The burn erases the whole flash, writes the bootloader, reads it back over USB
and checks its CRC. It then writes the node ID if you asked for one and starts
the bootloader. The node-ID checks are the same as over SWD, and they run
before anything is erased: a bootloader `.elf` v1.7.0 or newer, and a 1 MB
H72x/73x chip.

From the command line (CLI built with `--features swd`, like `swd-flash`):

```bash
can-flasher dfu-list
can-flasher dfu-flash --release --provision ams
```

## ⚠️ Worth knowing

- **Windows:** bind the WinUSB driver to the board once, while it's in DFU mode.
  Run Zadig, pick **STM32 BOOTLOADER**, choose **WinUSB** and click **Replace
  Driver**. **Linux:** add the udev rule from the install guide. **macOS:**
  nothing to install.
- **A DFU burn always erases the whole chip:** application, node ID and stored
  calibration. That's why there's no separate **Erase chip** button in USB mode.
- **New in this release, and not yet run on a car's board.** If DFU doesn't find
  the board, or a burn fails, use the ST-LINK path, which is unchanged, and open
  an issue.

**Full changelog**: https://github.com/isc-fs/MingoCAN/compare/v3.1.2...v3.2.0
