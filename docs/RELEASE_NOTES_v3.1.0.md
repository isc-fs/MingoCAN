# v3.1.0

**A new Data logs page.** The newest log is now at the top of the page with one
button. Downloads save without a folder dialog. Logs this laptop already has
fold out of the way.

## Data logs, redesigned

**Observe → Data logs** now checks the card as soon as you open it.

- **Newest file first.** The top card shows the newest log. Press
  **Download newest**, or just Enter. **Download all new** takes everything
  you don't have yet, newest first, so stopping halfway still keeps the most
  recent data.
- **No folder dialog.** Files save to
  `Documents/MingoCAN Logs/<AMS|ECU|UDV>/<date>/`, where the date is the day
  you downloaded them, since the card has no clock. **Change…** picks another
  folder once and the app remembers it. **Open folder** shows where files went.
  *One folder per day* turns the date folders off.
- **LOG and IMU files have separate tabs**, each showing how many are new.
- **Logs from** picks the board. It defaults to the AMS and is separate from
  the node ID on the Adapters page, so a first visit no longer sends you to
  Adapters.
- **While a file downloads**, the top card shows progress, speed and time left,
  with **Cancel now** and, for several files, **Stop after this file**. Leaving
  the page asks before it cancels the download.

### Files you already have fold away

Nothing can be deleted from the card, so each laptop keeps its own list of what
it has downloaded. Every check sorts the card into four groups:

- **New on the card**
- **Missing on disk**, shown in amber: downloaded before, but the copy is gone
  or changed
- **Already downloaded**, collapsed
- **Hidden**, collapsed: files you dismissed with **Hide** or **Hide all**

A file only counts as downloaded while its CRC-checked copy is still on this
laptop at the right size. If you delete or move the copy, the file comes back
as *missing*. It is never quietly hidden. If the card was reformatted or
swapped, nothing folds away and everything shows as new. Hiding never deletes
anything; **Unhide** brings files back.

## ⚠️ Worth knowing

- **The last few minutes need a power-cycle.** The AMS starts a new file every
  5 minutes, and the one it is writing only appears after the AMS boots again.
  The docs used to say "after shutdown". That was wrong.
- **A failed or cancelled download starts over.** Short drops during a download
  are recovered on their own. Starting a download again begins at 0.
- **`can-flasher logs finalize` is gone.** The CLI no longer seals the active
  log, matching the desktop app since 3.0.2. Power-cycle the car instead.

## Also in this release

- Primary buttons across the desktop app are filled amber again. A CSS rule had
  been overriding them, so every main action looked like an ordinary outlined
  button.
- CI now builds cleanly with Rust 1.99: `async-trait` was updated to 0.1.92.

**Full changelog**: https://github.com/isc-fs/MingoCAN/compare/v3.0.2...v3.1.0
