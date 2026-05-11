# Flash performance — v1.1.x baseline and v1.2.0 outcome

Investigation doc for why `cf flash` takes ~52 s to program a
26 KB application on real hardware, and what we learned about how
to (and how *not* to) speed it up. Builds on the `--profile` flag
from PR #82.

Take every number here with a grain of "run it on your own
machine" — macOS scheduler granularity varies, and the hardware
numbers were collected on a single bench (STM32H733 + Protofusion
Labs CANable 2.0 @ 500 kbps). Trends are robust; absolute
milliseconds aren't.

**TL;DR** — v1.2.0 ships `DEFAULT_WRITE_CHUNK` 128 → 256 B (fix/20),
taking the hardware flash from 52 s → ~49.6 s (~4.5 %). The bigger
wins predicted by the §3 back-of-envelope (5-15 s flash from precise
pacing / no pacing) turned out **not** to exist at 500 kbps on this
adapter — see [§ hardware bench](#what-the-hardware-bench-actually-said-fix19--fix20)
for why. We dropped fix/19 (precise pacing) after measurement.

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

## What the hardware bench actually said (fix/19 + fix/20)

When we actually tried the changes above on the IFS08 bench
(STM32H733 + Protofusion Labs CANable 2.0 @ 500 kbps), the
numbers told a different story than §3 predicted. Sharing them so
the next person doesn't chase the same ghosts.

### Precise pacing is a dead end here

| Variant | Per-chunk p50 | Wall | Result |
|---|---:|---:|---|
| dev baseline (`tokio::time::sleep(1ms)` → ~2.3 ms actual) | 47 ms | **52 s** | ✅ completes 1023/1023 chunks |
| `spin_loop` busy-wait at 1 ms precise | 40 ms | — | ❌ fails chunk 48, `TRANSPORT_TIMEOUT` from BL |
| `std::thread::sleep(1ms)` (yields to OS) | 40 ms | — | ❌ fails chunk 48, same failure |
| `std::thread::sleep(2ms)` precise | 53 ms | — | ❌ fails chunk 572, same failure |
| `std::thread::sleep(3ms)` precise | 79 ms | **84 s** | ✅ completes, but **slower than baseline** |

The "~40 s of sleep is wasted" diagnosis in §3 was wrong. The
CANable's **sustainable end-to-end TX rate is the binding
constraint**, not tokio's timer granularity. dev works because
`tokio::time::sleep` has 2.3 ms median *and* occasional 10 ms
spikes (tokio timer wheel jitter); the spikes give the adapter's
internal buffer recovery time. Remove the variance — by any
mechanism, busy-wait or yielding sleep — and the steady-state rate
alone walks the buffer over the cliff, first after ~960 frames at
1 ms, at ~11 400 frames at 2 ms, never at 3 ms.

No BEL bytes come back from the CANable when it drops — it just
silently loses CFs, which the BL sees as reassembly timeout or
bad-sequence error.

**Takeaway**: precise pacing alone cannot speed up hardware flash
on this adapter. We've dropped the fix/19 precise-pacing branch.

### 256 B chunks: small but real win

PR #57 had dropped `DEFAULT_WRITE_CHUNK` from 256 → 128 with a
note that 256 B bursts overflowed the CANable. On remeasurement
(fix/20) the 256 B chunks now complete cleanly — likely because
#57 also split the SLCAN reader/writer ports in the same commit,
and the actual root cause of the reported "overflow" was mutex
contention, not buffer size.

| Variant | Chunks | Per-chunk p50 | Wall |
|---|---:|---:|---:|
| 128 B chunks (v1.1.x) | 1023 | 47 ms | 51.9 s |
| **256 B chunks (v1.2.0)** | **511** | **90 ms** | **49.6 s** |

A ~4.5 % speedup — much less than the "halving" you might expect,
because the per-chunk wall time roughly **doubles** with chunk size
(37 frames × pacing vs 19 frames × pacing). The savings come only
from eliminating ~512 per-chunk BL-RTT overheads (program cycle,
ACK). Modest, but reliable, aligned with REQUIREMENTS.md, and
removes the v1.1.x code/docs inconsistency for free.

This is what actually lands in v1.2.0.

### Where the bigger wins would have to come from

At 500 kbps classic CAN with this adapter, a ~2.3 ms/frame floor
× 20 000 frames per full-sector rewrite puts a theoretical minimum
somewhere near **~45 s** (essentially: dev's baseline). We can't
meaningfully beat that at the current bitrate without changing the
topology. Real levers:

1. **Adaptive pacing**. Probe the CANable's actual limit; back off
   only when BEL fires (counter already exists per fix/17) or on a
   BL retry. Complex, and the CANable doesn't BEL on silent drops,
   so the signal to back off has to come from BL retries, not the
   adapter. Untried.
2. **Pipelining the write loop**. Issue `FLASH_WRITE(n+1)` while
   BL is still programming `n`. Amortises per-chunk BL-RTT (~10 ms
   × 511 chunks = ~5 s potential saving). Engine refactor.
3. **Higher bus bitrate (1 Mbps)**. Halves wire time per frame,
   but only helps if the BL and car harness can take it — not a
   host-only change.
4. **CAN-FD** (much more data per frame). Requires FD-capable
   adapter and BL support. Out of scope for v1.x.

None of these look worth the complexity for a saving of 5-10 s on
a 50 s operation that you run maybe ten times per deploy. Ship the
256 B chunk win, update the expectation in this doc, revisit if
someone really needs sub-30-s flash.

---

## Why we paced in the first place (fix/10 context)

The CANable's TX buffer is finite. When host + CANable get ahead
of the CAN wire — which happens on multi-frame bursts — the
CANable silently drops frames, which manifests on the BL side as
ISO-TP reassembly timeouts. fix/10 added `tokio::time::sleep(1ms)`
between frames as a conservative throttle. It worked; the fix/19
bench just revealed that the *real* pacing we were getting was
~2.3 ms, not 1 ms, and the adapter actually needs ~2.3 ms to
survive a continuous burst.

Any pacing rework has to preserve fix/10's invariant: never burst
faster than the CANable can sustain onto the wire. The safest
knob today is `PACING_INTERVAL` in `src/transport/slcan.rs` — if
someone later adopts a different adapter with a larger buffer,
they can measure and lower it.

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
