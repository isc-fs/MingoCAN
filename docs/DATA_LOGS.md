# Pulling logs off a board

Boards write car data to a microSD card. LOGFS pulls those files off over CAN,
so you do not have to open anything up to get at them.

It is **read-only**: there is no delete. Files come off the card; nothing goes
onto it and nothing is removed.

## Seal the current log first

The log a board is writing right now does not appear in a listing until it is
sealed. Finalize first, then list:

```bash
can-flasher --interface pcan --channel PCAN_USBBUS1 --node-id 0x02 logs finalize
```

## In the app

**Data logs** in the sidebar. Pick the board, list, choose a file, pull. Progress
and cancellation are handled for you.

## On the CLI

```bash
can-flasher … --node-id 0x02 logs list
can-flasher … --node-id 0x02 logs pull --index 3 --out ./logs/
can-flasher … --node-id 0x02 logs pull --all --out ./logs/
```

| Flag | Meaning |
|---|---|
| `--index N` | pull one file by its index from `list` |
| `--all` | pull every file |
| `--out DIR` | where to write |
| `--no-verify` | skip the CRC check at the end (not recommended) |

> **`--node-id` is required for `logs`.** Unlike most subcommands it does not
> default to `0x3`, and omitting it fails without a specific exit-code hint —
> you get the generic **99**, not a targeted code. If a `logs` command exits 99
> and the message looks odd, check you passed a node ID.

Roles: ECU `0x01`, AMS `0x02`, uDV `0x03`.

## How long it takes

Throughput is roughly **10–20 kB/s**, so a 4 MiB file takes about **3.5 to 7
minutes**. `--all` on a full card is a coffee break, not a pause in
conversation. Plan accordingly rather than assuming it has hung.

Transfers retry up to three times, and the internal timeout floor is 2000 ms —
passing a `--timeout` smaller than that is raised to it rather than honoured,
because a shorter deadline cannot complete a read.

A pull that is interrupted by a busy bus recovers: the session is re-established
and the read resumes from the last acknowledged offset, with the final CRC still
gating the result.

## Troubleshooting

**Nothing lists.** Run `logs finalize` first — an unsealed log is invisible.

**It exits 99 with a confusing message.** Check `--node-id` is present.

**Nothing works while a board is in its bootloader.** LOGFS is served by the
*application* firmware, not the bootloader. A board sitting in its bootloader has
nothing listening for these commands.

---

## See also

- [DESKTOP.md](DESKTOP.md) · [USAGE.md](USAGE.md)
