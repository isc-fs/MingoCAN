### `cf provision <role|path>` and one-click provision in both GUIs

The team's three boards on the shared CAN bus each get a known 4-bit node-id (`ecu` → 1, `ams` → 2, `udv` → 3). Setting the right one used to mean remembering the magic number and typing:

```bash
cf --node-id 0xF config nvm write node-id 0x02 --reset
```

v2.4.4 wraps that as:

```bash
cf provision ams                       # explicit role
cf provision build/ams.elf             # role inferred from filename
cf provision ecu --no-reset            # write only, don't reboot
```

Path-shaped arguments (containing `/` `\`, or ending in `.elf` / `.hex` / `.bin`) are recognised by basename stem — `firmware/ams/main.elf` does **not** silently match `ams` because stems only come from the basename. Unknown inputs fail at parse time with the full expected-roles list.

### Provision-after-flash in MingoCAN and VS Code

Both GUI clients now offer to provision right after a successful CAN flash when the artifact filename matches a role:

- **MingoCAN Flash tab** — when the artifact path resolves to a role, a "Provision as `AMS` after flash" checkbox appears (default on). On submit, the host writes the node-id NVM key and fires `CMD_RESET[Bootloader]` after the flash ACK. Status line shows both outcomes.
- **VS Code** — after a successful `Build + Flash` (status bar or palette), a toast appears with **Provision as `AMS`** / **Skip**. Yes shells out to `can-flasher provision <role>` using the current global flags; result reports through the existing output channel.

Routine flashes with role-neutral names (`firmware.elf`, `main.bin`) stay silent — only filenames whose stem matches a role trigger the prompt.

---

**Compatibility** · CLI gains a subcommand · MingoCAN's Flash tab gains a toggle · VS Code's Build + Flash gains a post-success toast.

### Install

| Surface | Asset |
|---|---|
| CLI | `can-flasher-v2.4.4-<target>.{tar.gz,zip}` |
| VS Code extension | `vscode-stm32-can-2.4.4.vsix` |
| ISC MingoCAN — macOS (Apple Silicon) | `ISC.MingoCAN_2.4.4_aarch64.dmg` |
| ISC MingoCAN — Linux (x86_64) | `ISC.MingoCAN_2.4.4_amd64.deb` / `.AppImage` |
| ISC MingoCAN — Windows | `ISC.MingoCAN_2.4.4_x64_en-US.msi` |

Full notes: [`docs/RELEASE_NOTES_v2.4.4.md`](https://github.com/isc-fs/can-flasher/blob/v2.4.4/docs/RELEASE_NOTES_v2.4.4.md).
