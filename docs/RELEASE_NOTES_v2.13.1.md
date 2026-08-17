# v2.13.1

A LOGFS fix confirmed on the bench, plus the specification and architecture
documents.

## Fixed: large log pulls could not finish on a busy bus

Pulling a big log off a board would fail partway with
`did not hold after 5 reconnect(s)` — a 525 KB file died at 26 % against a real
ECU flooding the bus, and at 41 % on a light one.

The transfer was **not stuck**. It was recovering and advancing each time; it
simply ran out of a budget that should never have been a total. `MAX_RESYNCS`
counted *every* reconnect over the life of a pull, so a 293 KB file fit inside
five and a 525 KB one legitimately did not — a longer file drops the session
more often, for entirely healthy reasons.

The budget now counts **consecutive** reconnects that made no progress, and
resets on every byte the transfer advances. A pull that keeps moving may
reconnect as many times as its length demands; only a genuinely stuck one — this
many reconnects in a row with zero new bytes — fails.

**The "dead bus fails cleanly" guarantee is intact.** A bus that lets nothing
through still trips the cap in five tries. Verified by a stub that drops the
session roughly ten times across a twenty-read file, far past the old budget,
each drop separated by progress — with a negative control confirming the test
fails if the reset-on-progress is removed.

Bench-confirmed against a real ECU (IFS08_HIL#94, following #527).

## Documentation: the spec and the architecture

Both documents described a CLI-only tool with eight subcommands. There are
twelve, the desktop app is the primary surface, and three subsystems had shipped
without ever reaching either file.

**[ARCHITECTURE.md](https://github.com/isc-fs/MingoCAN/blob/main/ARCHITECTURE.md)**

- Module tree rebuilt from the filesystem. It was missing `pit_diag/`,
  `logfs_client.rs`, `app_control.rs`, `swd/`, `transport/isolation.rs`,
  `protocol/logfs.rs` and four `cli/` modules.
- Leads with the structural fact that was buried: **MingoCAN consumes the engine
  as a path dependency**, so there is one protocol implementation and both the
  app and the CLI are thin skins over it.
- New section on `transport/isolation.rs` — running a crash-prone native backend
  out-of-process, so a `libPCBUSB` SIGBUS on adapter unplug kills a helper
  process instead of the whole app. Previously undocumented anywhere.
- New section on the DBC problem: why the telemetry decoders are hardcoded, why
  conformance tests alone cannot catch upstream drift, and why the drift watch
  has to be scheduled from the default branch.

**[REQUIREMENTS.md](https://github.com/isc-fs/MingoCAN/blob/main/REQUIREMENTS.md)**

- Reframed from a CLI spec to an **engine** spec, stating explicitly that
  requirements written against the CLI bind the app equally, because they are
  the same code.
- Specs added for `provision`, `logs`, `pit-diag` and `swd-flash` — all four
  shipped without ever being specified. `logs` and `pit-diag` are marked as
  separate contracts: LOGFS is served by the *application* firmware, and
  pit-diag is broadcast telemetry with no session at all.
- New **Telemetry observers** section — the arm/disarm contract, the
  passive/armed split, and decoder conformance as a standing requirement.
- Corrections: `--node-id` was documented as `[default: broadcast]` when there is
  no single default and two subcommands hard-error; `--timeout` was described as
  per-frame when it is per command.

**Left byte-for-byte alone:** the memory map, the CAN protocol specification, the
fixed-layout records, and the NACK table. Those mirror the bootloader's
`bl_proto.h` and `bl_memmap.h`, and rewriting a wire contract during a
documentation pass is how wire contracts acquire errors. The doctrine at the top
of the file now says so.

## Also

Two links in `.github/roadmap.yaml` used `../apps/…`, but `ROADMAP.md` sits at
the repo root, so they escaped the repository. Fixed at the source.

**Full changelog**: https://github.com/isc-fs/MingoCAN/compare/v2.13.0...v2.13.1
