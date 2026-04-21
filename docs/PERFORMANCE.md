# Flash performance — v1.1.x baseline and v1.2.0 targets

Investigation doc for why `cf flash` takes ~52 s to program a
26 KB application on real hardware, and what we can do about it.
Builds on the `--profile` flag landed alongside this doc.

Take every number here with a grain of "run it on your own
machine" — macOS scheduler granularity varies, and the hardware
numbers were collected on a single bench (STM32H733 + Protofusion
Labs CANable @ 500 kbps). Trends are robust; absolute milliseconds
aren't.

---

## The question

Hardware-observed end-to-end wall time for a full `cf flash
--verify-after --no-jump --no-diff` of the 26 KB `MAIN_IFS08_DEMO`
binary: **~52 seconds**. That works out to ~500 bytes/second on a
500 kbps bus with a theoretical ~60 KB/s ceiling. Bus utilisation:
under 1 %. Something upstream of the wire is holding us back.

---

## What we measured

### 1. Virtual backend (floor)

`cf flash --interface virtual … --profile` runs the full flash
engine against an in-process stub — no serial, no USB, no adapter,
no firmware, no CAN wire. Everything from the CLI layer through
ISO-TP segmentation to the flash manager still executes. Just the
downstream transport is stubbed to a tokio mpsc channel.

```
total flash engine:         21 ms
               startup        0 ms
             plan/diff        0 ms
                 erase       20 ms
  write (sector complete)     0 ms
                verify        0 ms
            pre-commit        0 ms

per-chunk write interval — sector 0x01, n=1023:
  min 0 ms   p50 0 ms   p95 0 ms   max 0 ms
```

**Takeaway**: the host + protocol logic processes 1024 write
chunks in sub-millisecond each. Whatever accounts for the 52 s on
hardware happens entirely downstream of the flash engine itself.

### 2. `tokio::time::sleep` granularity on macOS

`SlcanBackend::send()` ends each frame with
`tokio::time::sleep(PACING_INTERVAL)` where
`PACING_INTERVAL = Duration::from_millis(1)` (added in fix/10 to
stop the CANable TX buffer overflowing on ISO-TP bursts).

Microbench (`tokio::time::sleep` → measured elapsed, n=500 per
request value, `rt-multi-thread`):

| Requested | min | p50 | p95 | max |
|----------:|----:|----:|----:|----:|
|      0 µs |  12 µs | 1173 µs | 1665 µs | 4427 µs |
|    100 µs | 583 µs | 1183 µs | 2691 µs | 7886 µs |
|    500 µs | 1086 µs | 1646 µs | 3114 µs | 10219 µs |
| **1000 µs** | **1161 µs** | **2310 µs** | **3329 µs** | **10594 µs** |
|   2000 µs | 2295 µs | 3441 µs | 4068 µs | 5855 µs |
|   5000 µs | 5444 µs | 6810 µs | 7356 µs | 10122 µs |
|  10000 µs | 10061 µs | 12065 µs | 12462 µs | 15062 µs |

A request of 1 ms becomes a wait of **~2.3 ms median**, **3.3 ms
p95**, up to **10.6 ms**. Below about 2 ms, the scheduler floor
dominates; above, the requested duration wins.

### 3. Back-of-envelope from the hardware measurement

A 26 KB flash at the current 128-byte chunk size:

- 26172 B ÷ 128 B/chunk = **204 write chunks** over the app region,
  plus the diff-check READ_CRC per sector.
- 128 B payload + 5 B address/opcode/msg_type overhead = 133 B per
  write command. ISO-TP segmentation: 1 FF (6 B data) + 19 CFs
  (7 B data each) = **20 frames per write** if the message is
  exactly chunk-aligned, 19 when it's smaller.
- Plus 1 FC from BL + 1 ACK SF from BL = **~22 bus frames per
  chunk** round-trip.
- 204 chunks × ~20 TX frames = **~4080 paced TX frames**. At the
  measured 2.3 ms median sleep per frame: **~9.4 s of pacing on
  the write path**.
- Erase: one 128 KB sector → ~2–4 s on H7.
- Read-CRC: BL software CRC over 128 KB → ~10–50 ms.
- Per-chunk ACK round-trip: host TX, BL receive + program + TX,
  host RX. With the BL's main loop doing `HAL_Delay(1)` each
  iteration the per-command ACK floor is ~3–5 ms.
- 204 × 4 ms ACK latency ≈ **~0.8 s**.
- Final VERIFY: another software CRC over the app region,
  ~20–50 ms.

Adding those up: ~12 s of explainable time against a measured 52 s.
The rest — **~40 s** — is unexplained by mechanics alone.

### 4. Where the missing 40 s probably is

The chunk estimate in (3) above used 204 chunks. But the virtual
profile for a **full sector** rewrite (`--no-diff`) emitted
**1023 chunks**, not 204 — the flash manager walks the entire
sector (128 KB) even when the image is only 26 KB, padding the
tail with 0xFF. That's a correctness property (the BL's post-write
CRC check covers the whole sector, so everything past the image
gets written as 0xFF or preserved by the erase). So:

- **Real chunk count** at `--no-diff` full-sector rewrite: 1024
  chunks × 20 frames = **20,480 paced TX frames**.
- 20,480 × 2.3 ms median pacing = **~47 s of sleep time**.

That lines up with the 52 s total: ~47 s sleep + ~3 s erase + ~1 s
ACK latency + ~1 s CRC + handshake overhead ≈ **~52 s**.

**The pacing sleep is the dominant cost.** Everything else is
noise by comparison.

---

## What fixes buy us

Rough theoretical savings if we eliminate the pacing sleep's
scheduler overhead but keep its protective function (1 ms between
frames, precise):

| Fix | Expected new wall time |
|---|---|
| **Do nothing** | 52 s |
| `spin_sleep` for sub-ms (or a hybrid sleep that busy-waits the tail) | ~30 s |
| Drop pacing entirely (measure if CANable copes) | ~8–12 s |
| 256 B chunks instead of 128 B (halves chunk count) | ~26 s at current pacing |
| Chunks + precise pacing | ~15 s |
| All three (256 B chunks + no pacing + fully pipelined) | ~5–8 s |

A 10× improvement (52 s → ~5 s) is plausible. A 4× improvement
(52 s → ~13 s) is almost certain with the precise-sleep fix alone.

---

## Why we paced in the first place (fix/10 context)

The CANable's TX buffer is finite. When host + CANable get ahead
of the CAN wire — which happens on multi-frame bursts — the
CANable silently drops frames, which manifests on the BL side as
ISO-TP reassembly timeouts. fix/10 added the 1 ms sleep as a
conservative throttle. It worked; it just turns out to be much
more expensive than intended.

Any pacing rework needs to preserve the property fix/10 enforced:
never burst faster than the CANable can sustain onto the wire.

Options (in rough order of implementation cost):

1. **Precise 1 ms sleep** (`spin_sleep` crate or custom
   busy-loop-with-yield hybrid). Minimal risk — same throttle,
   less overhead.
2. **Adaptive pacing**: start at 0 ms; if a BEL byte arrives from
   the CANable (we already count these per fix/17), back off.
   Requires passing the BEL counter into `send()`, which is a
   minor refactor.
3. **No pacing, rely on ISO-TP FC(Wait) from the BL**: the BL
   already implements flow control. If we trust the BL to throttle
   us via FC frames between FF and CFs, we don't need host-side
   pacing at all. Requires BL-side FC(Wait) to actually fire on
   TX-buffer-full conditions — untested.
4. **Larger chunks** (256 B or even 512 B): halves or quarters the
   chunk count. Has to be validated against the CANable's burst
   capacity regardless of pacing.
5. **Pipelining**: issue the next `FLASH_WRITE` before the
   previous ACK lands. Amortises the per-chunk ACK RTT. Larger
   refactor of the flash engine.

---

## v1.2.0 plan

1. Land fix #1 (precise pacing). Remeasure — expect ~30 s.
2. Land fix #4 (256 B chunks). Remeasure — expect ~15 s.
3. If hardware still under-performs, attempt fix #2 (adaptive
   pacing off BEL) or #5 (pipelining). Validate against the same
   CANable that surfaced the original fix/10 overflow — if 0 ms
   pacing with 256 B chunks works cleanly there, no further
   complexity needed.
4. Re-run `--profile` on every step and paste the numbers into the
   PR. This doc is the baseline to diff against.

Not all three fixes need to land — measure after each, stop when
the wall time is good enough. Every change that complicates the
pacing story costs maintainability later, so keep the simplest
thing that hits the target.

---

## How to reproduce

```sh
# Virtual floor (host + engine overhead only):
cf --interface virtual --node-id 0x3 \
   flash demo/MAIN_IFS08_DEMO.bin --address 0x08020000 \
   --verify-after --no-jump --no-diff --profile

# Hardware (adapter + BL + wire):
cf --interface slcan --channel /dev/ttyACM0 --bitrate 500000 --node-id 0x1 \
   flash demo/MAIN_IFS08_DEMO.bin --address 0x08020000 \
   --verify-after --no-jump --no-diff --timeout 10000 --profile
```

`--profile` prints a per-phase breakdown to stderr at flash
completion. Per-chunk interval stats surface TX pacing directly
(p95 near 0 = no bottleneck; p95 in the milliseconds = pacing).

For the sleep microbench:

```sh
# In a temporary cargo project with `tokio = { version = "1", features = ["full"] }`:
for requested_us in [0, 100, 500, 1_000, 2_000, 5_000, 10_000]:
    sleep(requested_us) × 500 iterations → min/p50/p95/max
```
