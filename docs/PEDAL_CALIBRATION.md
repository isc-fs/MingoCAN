# Pedal calibration

Sets the accelerator and brake endpoints on the ECU over CAN — no rebuild, no
reflash. It lives in the desktop app under **Telemetry → ECU tab**, below the
telemetry cards.

## Read this first

Calibration writes eight values into the ECU's non-volatile memory. Four of them
decide when the car is allowed to make torque:

| Value | What it gates |
|---|---|
| `brake_pressed` | the **EV.2.3 torque cut** |
| `brake_dv_hard` | broadcast to the uDV on **`0x505`** as this ECU's **EBS verdict** |
| `brake_arm` | ready-to-drive arming |
| `apps1/apps2` endpoints | the **T.11.8.9** plausibility cut |

A wrong calibration can cause unintended acceleration, or can defeat a cut that
exists to stop it. **Put the car on stands, wheels off the ground, before you
start.** The app shows that warning before the session opens, not after.

You only ever *capture* six of the eight values. **`brake_arm` and
`brake_dv_hard` are derived by the ECU** from the brake span. The app shows you
what the ECU derived — it never computes them itself, and neither should you.

## Before you start

**In the car**, the ECU refuses to open a session unless all four hold:

- tractive system inactive
- control FSM out of `Active`
- motor rpm zero
- commanded torque zero

Otherwise you get **`VehicleNotSafe`**.

> **These are re-checked on every capture and on commit, not just at the start.**
> If the car leaves the safe state mid-session, the ECU discards **everything
> you have staged** and you start over — it is not a matter of retrying the one
> point that failed.

**In the app**, the ECU telemetry stream must be armed. Calibration reads live
pedal values from it; without it the capture buttons stay dead because there is
nothing to capture. Arm it from the same ECU tab.

## Doing it

1. **Start the session.** The app reads and shows what is currently stored, so
   you can see what you are about to change.
2. **Capture five points**, in the order the wizard asks:
   accelerator released → accelerator full travel → accelerator half travel →
   brake released → brake pressed firmly.

   The **Capture** button stays disabled until the reading settles, and shows
   you the spread it is waiting on. That is deliberate: a released brake pedal
   drifts by around 150 counts, and capturing mid-wobble is how a car ends up
   arming ready-to-drive on an untouched pedal.

   The half-travel accelerator point is not an endpoint and is not optional. It
   is the **only** place the T.11.8.9 failure mode is actually measured — the
   two channels agree at rest and full travel by construction however badly
   they diverge in between.

3. **Review.** You get every value: currently stored, new, and the difference,
   with the two ECU-derived brake thresholds shown as the ECU computed them.
4. **Confirm and commit.** The checkbox — *"I have checked these values and the
   car is on stands"* — and a complete read-back are both required before
   **Commit** enables.
5. **Verify.** The app re-reads the stored values automatically and shows them.
   Then sweep both pedals and confirm they read 0 % and 100 %.

Afterwards, check **Firmware health → pedal cal** on the same tab. It should
read `loaded`. Anything else — `defaults`, `invalidFellBack`, `badVersionFellBack`
— means the ECU is running compile-time values, not yours.

## When it refuses

All five capture points are required (`MissingPoints` otherwise). Beyond that,
the ECU applies seven validation rules and **writes nothing if any fails** — the
car is unchanged:

- accelerator channel 1 span too small
- accelerator channel 2 span too small
- the two accelerator channels disagree
- an accelerator channel reads no higher at full travel than at rest
- brake span too small
- brake thresholds came out in the wrong order
- a value outside the valid 0–4095 ADC range

The app renders each of these as a sentence, never as a code or a hex mask.

Two results worth understanding:

- **`ValidationFailed` with no specific rule named** means the consistency CRC
  did not match — the tool and the ECU disagree about what was captured. Abort
  and capture again; do not retry the commit.
- **`NvmWriteFailed`** means the write or its read-back verify failed. **The
  previous calibration is still in force.** If it repeats, the storage region
  may be full and need compaction.

## Limitations you have to compensate for

**The ECU does not check that a sample was steady.** `SampleUnstable` exists in
the protocol but the firmware never sends it — the ECU accepts whatever it is
handed. The app's stability gate is the *only* such check anywhere in the
system, which is why it blocks rather than warns.

**Nothing here has been verified end-to-end on hardware.** The ECU's flash write
path has only been tested against a simulated flash sector, and the tool has
only been driven against a simulated ECU. Calibrate on stands and check the
`pedal cal` reading afterwards.

## Reset to defaults

Discards the stored calibration and restores the ECU's compile-time values. It
is behind a second confirmation because it is destructive, and because the
compile-time defaults may not match this car — two of the brake thresholds
shipped as placeholders inherited from an older board.

It commits, so it is a real write, and it is subject to the same `VehicleNotSafe`
gate as everything else.

---

## See also

- [DESKTOP.md](DESKTOP.md) — the rest of the app
- [SAFETY.md](SAFETY.md) — every operation that writes to a board
- Firmware side: [IFS08-CE-ECU#169](https://github.com/isc-fs/IFS08-CE-ECU/issues/169)
