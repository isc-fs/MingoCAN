### SWD safety net: independent flash readback + chip-erase default

Closes **#247** — silent BL corruption when burning over ST-Link.

v2.4.3's "verified ✓" line was a false positive on `.elf` inputs. probe-rs's `verify=true` returned OK on flash that was actually corrupt, and the CRC we displayed hashed raw `.elf` bytes (headers + symbol table + debug info), not the bytes that get flashed. End result: corrupt-but-"verified" burns boot fine, respond to short commands, then die under sustained CAN traffic at chunk 52–62 of an app flash. Re-flashing the same `.elf` with STM32CubeProgrammer fixed it every time.

**Two layered changes, both shipped here:**

**1. Host-side flash readback.** After probe-rs's download succeeds, the host reads the flashed range back over SWD via the Cortex-M AHB (`MemoryInterface::read`), CRCs those bytes independently, and compares to a CRC of the parsed image. Mismatch → `VerifyMismatch` error, refuse to declare the flash successful. Talks to flash via a different path than probe-rs's verify, so it can't be fooled the same way.

**2. Chip-erase default.** `SwdFlashRequest.sector_erase_only` defaults to `false`; `DownloadOptions::do_chip_erase = !sector_erase_only`. v2.4.3's sector-erase default was what reproduced #247. STM32CubeProgrammer chip-erases by default for the same `.elf` and produces a working BL every time. STM32H7 option bytes are in a separate bank — chip-erase of user flash leaves them alone.

**3. CRC source corrected.** We now hash the parsed `Image.data` (the bytes that actually land on chip), not the raw file bytes. The displayed CRC equals the on-chip CRC after a successful flash. Cross-board reconciliation finally works.

### CLI

New `--sector-erase` opt-out flag for operators with a specific reason (e.g. preserving NVM in an adjacent sector). Default behaviour is chip-erase.

### MingoCAN

The Burn-bootloader tab's success status now reports the real on-chip CRC. New optional `sectorEraseOnly` field on the Tauri arg (defaults to false). No UI toggle yet — chip-erase is the safe path for the team's hardware, the toggle wiring is a follow-up if anyone needs it.

### Operator note

If you've burned a BL via MingoCAN v2.4.3 or v2.4.4 and the chip behaves oddly under sustained CAN traffic, re-burn it with v2.4.5 (or with STM32CubeProgrammer as a fallback). The new safety net won't retroactively fix a previously-corrupt burn — but it'll catch the corruption next time.

---

**Compatibility** · CLI gains `--sector-erase` opt-out · MingoCAN's Tauri command gains an optional field · VS Code extension unchanged.

### Install

| Surface | Asset |
|---|---|
| CLI | `can-flasher-v2.4.5-<target>.{tar.gz,zip}` |
| VS Code extension | `vscode-stm32-can-2.4.5.vsix` |
| ISC MingoCAN — macOS (Apple Silicon) | `ISC.MingoCAN_2.4.5_aarch64.dmg` |
| ISC MingoCAN — Linux (x86_64) | `ISC.MingoCAN_2.4.5_amd64.deb` / `.AppImage` |
| ISC MingoCAN — Windows | `ISC.MingoCAN_2.4.5_x64_en-US.msi` |

Full notes: [`docs/RELEASE_NOTES_v2.4.5.md`](https://github.com/isc-fs/can-flasher/blob/v2.4.5/docs/RELEASE_NOTES_v2.4.5.md).
