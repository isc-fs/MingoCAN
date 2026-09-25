# v3.0.0

Node IDs are now set over **SWD, in the same run as the bootloader burn** — no
CAN adapter, no second step. Plus five new ECU telemetry frames that answer
"why is torque limited right now?"

This is a major version because commissioning changed for operators:
`swd-flash --provision` keeps its name but now works differently and needs a
newer bootloader. Read the first section before you burn a board.

## Changed: provisioning moves from CAN to SWD

Until now a fresh board took two steps over two transports: burn the bootloader
over SWD, then write its node ID over CAN once the bootloader was running. Now
the node ID rides along with the burn
([#336](https://github.com/isc-fs/MingoCAN/pull/336)).

- **App:** *Burn bootloader* → **Provision node-id** → pick the board's role.
  No CAN adapter, no "reset after flash" prerequisite.
- **CLI:** `can-flasher swd-flash CAN_BL.elf --provision <ecu|ams|udv|0xN>`
- **Running boards:** `can-flasher provision <role>` over CAN is unchanged, for
  renumbering a board that is already up.

The host programs a small *provisioning seed* next to the bootloader; on its
first boot the bootloader validates it and stores the node ID in its own NVM.

### What you need

- **Bootloader v1.7.0 or later**
  ([stm32-can-bootloader v1.7.0](https://github.com/isc-fs/stm32-can-bootloader/releases/tag/v1.7.0)).
  The app's **Fetch** in *Burn bootloader* pulls it. Anything older can't adopt
  the seed, so provisioning with it is **refused before the chip is touched** —
  never burned and left silently unprovisioned.
- **The `.elf`.** Seed support is checked from the image's symbol table, which
  `.hex` and `.bin` don't have, so those are refused for provisioning (burning
  them without a role still works).
- **A 1 MB STM32H72x/73x** — the boards we build.
- **The default chip-erase.** `--provision` refuses `--sector-erase`: without a
  full erase the old node ID stays in NVM and the bootloader keeps it.

### Removed

- The CAN step that `swd-flash --provision` used to chain after the burn.
- `swd-flash --seed-node-id` — folded into `--provision`.

### ⚠️ An unprovisioned board answers as the ECU

A board burned without a role answers as **`0x01`** — the stock bootloader's
compile-time default, which is the **ECU's** address — and collides with the ECU
on a shared bus until it gets an ID. Earlier docs said it answered on `0xF`;
that was wrong (`0xF` is broadcast, and the bootloader can't be built with it).
*Burn bootloader* now warns when **Provision node-id** is left on *Don't
provision*.

### How it's kept safe

The seed path shipped with three fixes found while reviewing it against the
bootloader, each of which would otherwise have left boards unprovisioned or
worse:

- **CRC layout.** The seed now matches the bootloader's record byte for byte,
  including the CRC32 it checks — pinned by test vectors computed independently
  of MingoCAN's own CRC.
- **One flash word, not one flash page.** The seed is programmed as a single
  256-bit flash word straight through the flash controller. Writing it through
  the probe's 1 KiB page writes would also have programmed the neighbouring
  app-metadata word, which the bootloader later rewrites in place — an ECC
  hazard on the next CAN app flash.
- **Refuse, don't guess.** Every reason provisioning could fail silently — old
  bootloader, `.hex`/`.bin`, wrong chip, `--sector-erase` — is checked before
  the probe is opened.

Bench-validated on an MLC with bootloader v1.7.0, including a CAN app flash
plus power cycle straight after SWD provisioning, and the v1.6.2 refusal.

## New: five ECU telemetry frames

The ECU streams five frames that MingoCAN used to drop on the floor
([#566](https://github.com/isc-fs/MingoCAN/issues/566)). They are now decoded
everywhere — the app's ECU telemetry tab, `pit-diag`'s text output and its
`--json` stream.

| Frame | What it tells you |
|---|---|
| `0x709` cell | The low-cell derate: raw vs estimated open-circuit cell voltage, the IR compensation, and the torque ceiling it sets |
| `0x70A` pack_temp | The pack thermal cap, and **which modules fed it** |
| `0x70B` inv_foc | Inverter Id / Iq, voltage modulus, and whether its feedback frames are fresh |
| `0x70C` inv_torque | Torque requested vs max-feasible vs estimated, and the Iq setpoint |
| `0x70D` power | Shaft, AC and DC power, and the accumulator current |

Three new cards on the ECU telemetry tab:

- **Torque derates** — the cell and pack caps side by side, amber when limiting.
  A module the pack cap silently excluded lights amber, and "no module usable"
  raises a red *temp unknown*.
- **Inverter limiting?** — requested vs estimated torque, and Iq against its
  setpoint (a limit vs a failure to follow). A voltage modulus near 100 % means
  no voltage headroom — the ordinary reason torque falls off at the top of the
  straight, not a fault.
- **Power & efficiency** — shaft, AC and DC power, with live **motor η**
  (shaft / AC) and **total η** (shaft / DC — the number `DrivetrainEffPct`
  should be set to). The ratios show only above 5 kW, where they mean something.

`pit-diag`'s ECU scan now expects **13** frames per 100 ms scan (was 8).

### Two values are deliberately shown raw

- **Max feasible torque** is shown as a raw number. The inverter vendor calls it
  "Ndm", not "Nm", and whether that means deci-Nm is unresolved — the ECU
  firmware forwards it unconverted for the same reason. Don't compare it to the
  Nm values until that is settled on the car.
- **The inverter's control mode / type / command source** are shown as numbers.
  No name table for them exists in the ECU firmware or the inverter vendor's
  DBC; naming them needs the inverter user manual.

## Known gaps

- The bootloader side still owes its flash-write-counter check and the
  protection-layer suite on the HIL testbench (see the v1.7.0 release notes);
  the provisioning path itself is bench-validated.
- The uDV's reason for an AS emergency is still not decodable — it needs a uDV
  firmware field ([#490](https://github.com/isc-fs/MingoCAN/issues/490)).

**Full changelog**: https://github.com/isc-fs/MingoCAN/compare/v2.14.0...v3.0.0
