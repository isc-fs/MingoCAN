# Pulling logs off a board

Boards write car data to a microSD card. MingoCAN pulls those files off over
CAN, so you do not have to open anything up or pull the card.

It is **read-only**: files come off, nothing goes on, and nothing is deleted.

---

## Two things that catch everyone

**1. The last few minutes aren't listed yet.** The AMS starts a new file every
5 minutes (or 4 MiB), and the file it is writing *right now* only appears once
it is sealed — which happens the next time the AMS boots. Power-cycle the car,
then check again. If the newest file seems to be missing the end of a run, this
is why.

**2. It takes minutes, not seconds.** Throughput is roughly **10–20 kB/s**, so a
4 MiB file is **3.5 to 7 minutes**, and pulling a full card is a 20–35 minute
job. Plan for it rather than assuming the tool has hung.

## In the app

**Observe → Data logs.** The tab checks the card as soon as you open it.

- **Newest file first.** The top card shows the newest file. Press
  **Download newest** (or Enter) and it lands on disk. **Download all new**
  takes everything you don't have yet, newest first, so stopping halfway still
  keeps the most recent data.
- **LOG and IMU files are separate.** Switch with the *LOG files* / *IMU files*
  tabs; each shows how many are new.
- **No folder dialog.** Files go to `Documents/MingoCAN Logs/<AMS|ECU|UDV>/<date>/`,
  where the date is the *download* date (the card has no clock). The path is
  shown at the top; **Change…** picks another folder once and remembers it,
  **Open folder** shows it, and *one folder per day* turns the date level off.
- **Logs from** picks the board. It defaults to the AMS and is separate from the
  node ID on the Adapters page.

While a file downloads, the top card shows progress, speed and time left, with
**Cancel now** and — for several files — **Stop after this file**. Stay on the
page: leaving it asks first, then cancels the download.

### Already-downloaded files fold away

Nothing can be deleted from the card, so this laptop keeps its own list of what
it has downloaded (`logs-ledger.jsonl` in the app's data folder). Each check
sorts the card into:

| Group | What's in it |
|---|---|
| **New on the card** | Not downloaded on this laptop. Open. |
| **Missing on disk** | Downloaded before, but the copy is gone or a different size. Always open, in amber — **Download again**, or **Forget** it. |
| **Already downloaded** | A copy is on this laptop at exactly the listed size. Collapsed. |
| **Hidden** | Files you dismissed with **Hide** (or **Hide all**). Collapsed; **Unhide** brings them back. Hiding never deletes anything. |

A file only counts as downloaded after its CRC check passed and it was saved,
and only while that copy is still there — moving or deleting it brings the file
back as *missing*. If a downloaded file's CRC on the card no longer matches (a
reformatted or swapped card), nothing is folded and everything shows as new.
The list is per laptop; another laptop has its own.

The transfer survives a busy bus: if the session drops mid-pull, it
re-establishes and carries on from the last acknowledged offset, with a final
CRC gating the result.

## From the CLI

```bash
can-flasher --interface pcan --channel PCAN_USBBUS1 --node-id 0x02 logs list
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

**Nothing lists, or the end of a run is missing.** The file being written is
invisible until it is sealed at the next boot. Power-cycle the car, then check
again.

**It exits 99 with a confusing message.** Check `--node-id` is present.

**Nothing works at all.** LOGFS is served by the **application** firmware, not
the bootloader. A board sitting in its bootloader has nothing listening for
these commands — which also means you cannot pull logs from a board you just
flashed with *Start the app after flashing* turned off.

**A pull died partway.** Just start it again. A pull rides out short drops on
its own, but one that fails or is cancelled saves nothing, and the next attempt
starts from the beginning of the file.

---

## See also

- [DESKTOP.md](DESKTOP.md) · [TELEMETRY.md](TELEMETRY.md) · [CLI.md](CLI.md)
