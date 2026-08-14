# Pulling logs off a board

Boards write car data to a microSD card. MingoCAN pulls those files off over
CAN, so you do not have to open anything up or pull the card.

It is **read-only**: files come off, nothing goes on, and nothing is deleted.

---

## Two things that catch everyone

**1. Seal the current log first.** The log a board is writing *right now* does
not appear in a listing until it is sealed. Finalize, then list. If a listing
comes back empty or missing today's run, this is why.

**2. It takes minutes, not seconds.** Throughput is roughly **10–20 kB/s**, so a
4 MiB file is **3.5 to 7 minutes**, and pulling a full card is a 20–35 minute
job. Plan for it rather than assuming the tool has hung.

## In the app

**Observe → Data logs.**

1. **Finalize** to seal the log currently being written.
2. **List** the files on the card.
3. Pick one and pull it, or pull them all.

Progress and cancellation are handled for you. The transfer survives a busy bus:
if the session drops, it re-establishes and resumes from the last acknowledged
offset, with a final CRC gating the result.

## From the CLI

```bash
can-flasher --interface pcan --channel PCAN_USBBUS1 --node-id 0x02 logs finalize
can-flasher … --node-id 0x02 logs list
can-flasher … --node-id 0x02 logs pull --index 3 --out ./logs/
can-flasher … --node-id 0x02 logs pull --all --out ./logs/
```

| Flag | Meaning |
|---|---|
| `--index N` | Pull one file by its index from `list` |
| `--all` | Pull every file — opt-in on purpose, given the timings above |
| `--out DIR` | Where to write |
| `--no-verify` | Skip the closing CRC check (not recommended) |

> **`--node-id` is mandatory for `logs`** and has no default. Omitting it fails
> with the generic exit code **99** rather than a targeted hint — so if a `logs`
> command exits 99 with a message that reads oddly, check the node ID first.

Roles: ECU `0x01`, AMS `0x02`, uDV `0x03`.

Commands retry up to three times, and the internal timeout floor is 2000 ms — a
`--timeout` smaller than that is raised to it rather than honoured, because a
shorter deadline cannot outlast a FatFs read on a shared bus.

---

## Troubleshooting

**Nothing lists.** Finalize first. An unsealed log is invisible.

**It exits 99 with a confusing message.** Check `--node-id` is present.

**Nothing works at all.** LOGFS is served by the **application** firmware, not
the bootloader. A board sitting in its bootloader has nothing listening for
these commands — which also means you cannot pull logs from a board you just
flashed with *Start the app after flashing* turned off.

**A pull died partway.** Just start it again. Transfers resume from the last
acknowledged offset and the closing CRC still gates the result.

---

## See also

- [DESKTOP.md](DESKTOP.md) · [TELEMETRY.md](TELEMETRY.md) · [CLI.md](CLI.md)
