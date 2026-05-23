### SWD flash: verifiable post-flash fingerprint (#238 closes #236)

Every successful SWD burn now surfaces a CRC32 + verify status + VTref voltage. probe-rs's readback-compare was already default-on; this release just makes the proof visible so operators reconciling a suspected-bad ECU against a known-good one have a CRC to compare instead of guessing.

**CLI**
```
✓ flashed bootloader.elf to STM32H733ZGTx via SWD
    ↳ 52.1 KB · CRC32=0xDEADBEEF · verified ✓ · VTref=3.27 V
```

**MingoCAN — Burn bootloader tab**

The success status grows a small key/value grid: Size · CRC32 · Verify · VTref. Paste-friendly straight into bench notes.

`--no-verify` paths now log a loud warning at start + an `eprintln` after — the foot-gun stays available but never silent.

### CLI: `node-id` alias for `cf config nvm write` (#240 closes #231 task 2)

The well-known NVM keys gain operator-friendly names. Today's registry is one entry: `node-id` → `0x0001` (BL_NVM_KEY_NODE_ID).

```bash
# Bench-friendly shorthand — same write as 0x0001 0x02:
cf --node-id 0x1 config nvm write node-id 0x02 --reset
cf --node-id 0x1 config nvm read  node-id
```

Aliases work in every position a key is accepted (read / write / erase). Hex / decimal literals still parse — additive surface, no breaking change.

Typos (`node_id` instead of `node-id`) fail at parse time rather than turning into a mystery NACK on the bus.

---

**Compatibility** · CLI gains visible CRC + one alias · MingoCAN's Burn-bootloader tab shows the new fingerprint · VS Code extension unchanged from v2.4.2.

### Install

| Surface | Asset |
|---|---|
| CLI | `can-flasher-v2.4.3-<target>.{tar.gz,zip}` |
| VS Code extension | `vscode-stm32-can-2.4.3.vsix` |
| ISC MingoCAN — macOS (Apple Silicon) | `ISC.MingoCAN_2.4.3_aarch64.dmg` |
| ISC MingoCAN — Linux (x86_64) | `ISC.MingoCAN_2.4.3_amd64.deb` / `.AppImage` |
| ISC MingoCAN — Windows | `ISC.MingoCAN_2.4.3_x64_en-US.msi` |

Full notes: [`docs/RELEASE_NOTES_v2.4.3.md`](https://github.com/isc-fs/can-flasher/blob/v2.4.3/docs/RELEASE_NOTES_v2.4.3.md) · Closes #236 + #231.
