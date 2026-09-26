# v3.0.2

**A small desktop-app cleanup.** The *Data logs* tab no longer has a
**Seal active log** button
([#595](https://github.com/isc-fs/MingoCAN/pull/595)).

## What it did

The button asked the AMS to close the log file it was writing, so the current
run showed up in the list straight away. The AMS already closes that file
when it shuts down, so the button only saved you one power cycle.

## What to do instead

- **Normal use:** power the car down. The run that was being written appears
  in *Data logs* the next time you connect.
- **You need it sealed without a power cycle:** the CLI still has it —
  `can-flasher logs finalize`.

Nothing else changed. The CLI and VS Code extension are the same code as
3.0.1 apart from the version number.

**Full changelog**: https://github.com/isc-fs/MingoCAN/compare/v3.0.1...v3.0.2
