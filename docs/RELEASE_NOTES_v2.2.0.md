# v2.2.0 — Visible progress + Erase chip + simpler SWD UI

A polish-and-power-tools release. Both flash paths now show a real
progress bar instead of an opaque "Flashing…" spinner, and the
Burn-bootloader tab gains a destructive-but-handy chip-erase
action for commissioning workflows.

## Highlights

### Live progress bars

**SWD path** (Burn bootloader tab + `can-flasher swd-flash`)

probe-rs's flashing pipeline already knows when it's erasing,
programming, verifying, or filling — we now surface those
phases. The Studio shows an accent-coloured bar with the current
op label + percent; the CLI uses an `indicatif` bar for the
same data.

**CAN path** (Flash tab)

The over-CAN flash pipeline already emits per-sector events; v2.2
turns them into a continuous overall progress bar driven by
`sectors_written / sectors_planned`, smoothed by the current
sector's bytes-of-total so the bar slides instead of stepping.

### Erase chip

The Burn-bootloader tab now has a red **Erase chip** button next
to the burn action. Wipes the whole flash via probe-rs's
`erase_all`. Useful when:

- Commissioning a fresh ECU from a clean state
- Recovering from a half-written flash
- Resetting a chip whose option bytes need re-applying

A confirmation step keeps a slip from wiping a working chip.

### Burn-bootloader tab cleanup

Two opinionated UI changes:

- **New header copy** explains *why* the bootloader matters
  ("Burn it once when commissioning a new ECU; from then on,
  every app update uses the Flash tab over CAN.") instead of
  the abstract "first-boot a bare STM32 via ST-LINK" line.
- **Dropped the Target card.** Chip + base address were always
  `STM32H733ZGTx` / `0x08000000` for the team's ECU and the
  inputs were just footguns. The CLI keeps the full surface
  (`--chip`, `--base`) for operators flashing other STM32
  families.

### Sidebar shows live app version

The sidebar's "Tier 2 · DBC + Signals live" tagline is replaced
with a live `v2.2.0` label read from `tauri.conf.json`. The same
field `release.yml`'s verify-version gate compares the tag against,
so what operators see in the sidebar always matches the asset they
downloaded.

## Install

CLI binaries, the VSIX, and Studio bundles are attached below. The
`swd-flash` CLI subcommand still requires `--features swd` at
build time; Studio bundles already include it.

## Closes

- #198 — SWD/CAN progress bars + Erase chip + simpler SWD view
