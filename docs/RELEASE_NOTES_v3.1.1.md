# v3.1.1

**The AMS's new binary logs come off the card as spreadsheets.** Every pulled
`.BIN` file now also lands as a `.csv` beside it, with nothing to click. Data
logs also gains checkboxes for downloading several files at once.

## Binary logs decode themselves

Newer AMS firmware writes three binary files next to each `LOGnnnn.CSV`:

| File | What's in it |
|---|---|
| `IMUnnnn.BIN` | IMU, 100 Hz |
| `CELnnnn.BIN` | every cell-voltage read and current sample, 5 Hz |
| `ELEnnnn.BIN` | oversampled pack current and DC-bus voltage, 100 Hz |

When MingoCAN saves one, it also writes `<name>.csv` beside it. For example,
`CEL0003.BIN` comes with `CEL0003.csv`. The CSV matches the AMS team's own
`tools/log_decode.py` byte for byte, and all files from one window share the
`tick_ms` clock, so they line up with `LOGnnnn.CSV`.

- **The `.BIN` is kept.** It is the CRC-checked original.
- **A `.BIN` that won't decode is still saved.** Only its `.csv` is missing,
  and you get a warning.
- **A file cut short by a power-off** loses at most its last partial record.
- **New streams need no MingoCAN update.** Each `.BIN` describes its own layout,
  so the AMS can add one and it will still decode.

### In the app

- **Data logs has LOG, IMU, CEL and ELE tabs.**
- **"Saved" names the CSV**, and **Show in folder** opens it.
- **Checkboxes on New on the card** let you select files to download, including
  a select-all box and shift-click to select a range. **Download selected (N)**
  downloads them newest first.

### From the CLI

- **`logs pull` decodes by default** and prints `decoded -> CEL0003.csv (N records)`.
  `--no-decode` keeps only the `.BIN`.
- **`logs list` prints indices in hex**, and `--index` accepts them. The top two
  bits give the file kind: `0x0…` LOG, `0x8…` IMU, `0x4…` CEL, `0xC…` ELE. For
  example, `--index 0x4003` pulls `CEL0003.BIN`.
- **New: `can-flasher logs decode <files>`** for `.BIN` files copied off the card
  by hand. It needs no adapter.

## ⚠️ Worth knowing

- **Update before pulling from AMS firmware with binary logs.** 3.1.0 read the
  file kind from one bit only, so it put `CEL` files on the LOG tab and showed
  one as the *newest log*.
- **Copies get a new name.** A second copy of a file is now saved as
  `LOG0003_2.CSV`. The CLI used to name it `LOG0003.CSV.1`, which spreadsheets
  wouldn't open.

**Full changelog**: https://github.com/isc-fs/MingoCAN/compare/v3.1.0...v3.1.1
