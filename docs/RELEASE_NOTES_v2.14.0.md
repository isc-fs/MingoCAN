# v2.14.0

Five new ECU telemetry fields, decoded and surfaced everywhere.

## New: the ECU's autonomous-system mirror, and a refused reboot

The ECU repo's DBC moved
([`ce5107a1`](https://github.com/isc-fs/IFS08-CE-ECU/commit/ce5107a1b1976126eb8d9ca5faec07b754b8316e)),
adding five signals to frames MingoCAN already decodes. The daily drift watch
caught it — [#573](https://github.com/isc-fs/MingoCAN/issues/573) — and this
release reconciles the decoder.

| Frame | Signal | Meaning |
|---|---|---|
| `0x704` health | `boot_refused` | The ECU declined a reboot-to-bootloader trigger because the car was in the drive ladder |
| `0x707` dv | `as_status` | Autonomous-system state, mirrored from the uDV |
| `0x707` dv | `as_fresh` | The mirrored state is live |
| `0x707` dv | `as_from_stale` | The mirrored state came from a stale uDV frame |
| `0x707` dv | `as_emergency` | The uDV signalled an emergency stop |

All five appear on the app's ECU telemetry tab, in `pit-diag`'s text output, and
in its `--json` stream.

`0x707`'s minimum length grows from 4 bytes to 5, since `as_status` sits at byte
4. A board sending the old 4-byte frame is no longer decoded on that ID.

### Two of these are worth reading carefully

**`boot_refused` does not present as a telemetry curiosity — it presents as a
failed flash.** If Connect times out and this flag is set, the tool did its job
and the ECU said no: the car was in the drive ladder, so it declined to drop out
of drive underneath you. Leave drive or power-cycle before retrying. Forcing
`--enter-bootloader always` will not help, because the refusal is a decision
rather than a missed trigger. It is sticky since boot.

**The `as` row is mirrored, not owned.** The ECU is echoing a byte the uDV
publishes, which is what makes the freshness flags load-bearing rather than
decoration: `fresh` clear, or a `(stale)` marker, means you are reading the last
value the ECU heard and not the current one.

Both are now documented in
[TELEMETRY.md](https://github.com/isc-fs/MingoCAN/blob/main/docs/TELEMETRY.md)
and [FLASHING.md](https://github.com/isc-fs/MingoCAN/blob/main/docs/FLASHING.md),
including in the connect-timeout troubleshooting list where someone hitting the
refusal will actually be looking.

## The conformance suite gained the test it was missing

`as_status` is the one decoder enum whose `VAL_` table **does not live on the
message that carries it**. `PitDiag_dv` declares the signal with no table of its
own; the names are published once, on the uDV's `UDV_as_status` (`0x50A`).

That indirection is a silent-failure shape: nothing about `PitDiag_dv` changes
if the uDV renumbers the enum, so the layout assertions would keep passing while
every label meant something else. There is now a value-table test pinning the
decoder to the uDV's table, verified with a negative control — renaming `Ready`
to `"armed"` in the decoder fails it.

Every other decoder enum already had such a test. This one needed it most.

## Known gap

The same upstream commit adds `UDV_as_status` (`0x50A`) as a new **uDV bus
message**, which this release does not decode. The ECU conformance test asserts
only the `PitDiag_*` messages, so it is out of that test's scope; decoding it
belongs with the uDV telemetry work in
[#490](https://github.com/isc-fs/MingoCAN/issues/490).

**Full changelog**: https://github.com/isc-fs/MingoCAN/compare/v2.13.1...v2.14.0
