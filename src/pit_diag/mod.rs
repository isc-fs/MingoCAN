//! AMS pit-diag observer protocol.
//!
//! When armed, the AMS emits 53 frames at 1 Hz carrying the
//! diagnostic picture: every cell voltage, every NTC temperature,
//! FSM state, poll-timing telemetry, and per-IC PEC error counts.
//! This module handles the arm/disarm handshake and decodes the
//! wire frames into typed records the rest of the crate (and the
//! Studio app) can consume.
//!
//! ## Wire protocol
//!
//! Source of truth: `docs/CAN_MAP.md` in the AMS repo (IFS08-CE-AMS).
//! The #252 issue body in this repo also reserves `0x6C2..=0x6C6`
//! for balance / boot / crash / firmware-ID frames — those are
//! forward-looking; the firmware hasn't shipped them yet.
//!
//! - **Enable**:  emit `0x7F0` with payload `DE AD BE EF`.
//! - **Disable**: emit `0x7F0` with payload `00 00 00 00`.
//! - **ACK**:     AMS replies on `0x7F1` with 1 byte —
//!   `0x01` = enabled, `0x00` = disabled.
//! - **Stream IDs once armed (53 frames / scan total)**:
//!   - `0x680..=0x697` (24 frames) — cell voltages, 4 cells/frame,
//!     big-endian `u16` millivolts. The last frame's 4th slot is a
//!     `0xFFFF` sentinel because 95 cells don't divide evenly by 4.
//!   - `0x6A0..=0x6B8` (25 frames) — NTC temperatures, 8 NTCs/frame,
//!     signed `i8` °C. Exact (25 × 8 = 200).
//!   - `0x6C0` — FSM extended status (state, mode_locked, TSMS,
//!     DASH_CHG, AMS_OK, PEC error total, plus the #276 fault-reason
//!     / fault-detail bytes).
//!   - `0x6C1` — Poll timing (last V-poll ms, worst V-poll ms,
//!     T-sweep failure mask).
//!   - `0x6C7` / `0x6C8` — per-IC PEC error counts (#258), one
//!     saturating `u8` per monitor IC (10 ICs = 2 × 5 modules).
//!
//! ## Scope
//!
//! Decoders are hand-coded against the AMS doc; the slice 6 plan in
//! #252 swaps them for DBC consumption. [`AMS_EXPECTED_FRAMES_PER_SCAN`]
//! lets the Studio + CLI flag a *frame-count* drift before silent
//! miscalibration — but note it can't catch an in-frame byte
//! repurposing (e.g. #276 moving `0x6C0[6..7]` from reserved to
//! fault-reason); those are caught by tracking the AMS CAN_MAP
//! per-frame, which is why the host carries an explicit per-frame
//! layout here.
//!
//! VCU + UDV will get equivalent streams in their own ID ranges
//! (TBD per team). The plugin/profile abstraction lands in a later
//! slice; until then the symbol names here are AMS-prefixed
//! (`AMS_*`) so the eventual refactor is obvious.

use crate::protocol::CanFrame;

// ---- Wire-level constants ---------------------------------------

/// CAN ID the AMS listens on for arm/disarm commands.
pub const AMS_ARM_ID: u16 = 0x7F0;
/// CAN ID the AMS uses to ACK arm/disarm commands.
pub const AMS_ACK_ID: u16 = 0x7F1;
/// Arm payload — the literal `DE AD BE EF` sentinel.
pub const AMS_ARM_ENABLE_PAYLOAD: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];
/// Disarm payload — all zeros.
pub const AMS_ARM_DISABLE_PAYLOAD: [u8; 4] = [0x00, 0x00, 0x00, 0x00];

// ---- Pack geometry -----------------------------------------------

/// Number of modules in the AMS pack.
pub const AMS_NUM_MODULES: usize = 5;
/// Cells per module.
pub const AMS_CELLS_PER_MODULE: usize = 19;
/// Total cells = modules × cells/module = 95.
pub const AMS_NUM_CELLS: usize = AMS_NUM_MODULES * AMS_CELLS_PER_MODULE;
/// NTC temperature sensors per module.
pub const AMS_NTC_PER_MODULE: usize = 40;
/// Total NTCs = modules × NTCs/module = 200.
pub const AMS_NUM_NTCS: usize = AMS_NUM_MODULES * AMS_NTC_PER_MODULE;

// ---- Stream ID ranges --------------------------------------------

/// First CAN ID in the cell-voltage stream (24 frames total).
pub const AMS_CELLV_BASE_ID: u16 = 0x680;
/// Last CAN ID in the cell-voltage stream.
pub const AMS_CELLV_LAST_ID: u16 = 0x697;
/// Number of frames in the cell-voltage stream.
pub const AMS_CELLV_NUM_FRAMES: usize = 24;
/// Sentinel value emitted in slots past the last real cell. The last
/// frame carries cells 92, 93, 94, plus this sentinel because 95 cells
/// don't divide evenly by 4 slots/frame.
pub const AMS_CELLV_SENTINEL: u16 = 0xFFFF;

/// First CAN ID in the NTC-temperature stream (25 frames total).
pub const AMS_NTC_BASE_ID: u16 = 0x6A0;
/// Last CAN ID in the NTC-temperature stream.
pub const AMS_NTC_LAST_ID: u16 = 0x6B8;
/// Number of frames in the NTC-temperature stream.
pub const AMS_NTC_NUM_FRAMES: usize = 25;

/// The diag block — FSM status, poll timing, and per-IC PEC counts.
///
/// This block is **non-contiguous**. The #252 issue body reserved
/// `0x6C2..=0x6C6` for balance / boot / crash / firmware-ID frames
/// the firmware hasn't shipped, so the AMS team's per-IC PEC frames
/// (#258) landed at `0x6C7` / `0x6C8` instead, leaving a gap:
///
/// | ID       | frame                              | source |
/// |----------|------------------------------------|--------|
/// | `0x6C0`  | FSM extended status                | #248   |
/// | `0x6C1`  | poll timing                        | #248   |
/// | `0x6C7`  | per-IC PEC count, ICs 0..7         | #258   |
/// | `0x6C8`  | per-IC PEC count, ICs 8..9 + rsvd  | #258   |
///
/// Source of truth: `docs/CAN_MAP.md` in IFS08-CE-AMS. (Note: that
/// doc's bus-cost prose still reads "51 frames" — stale since #258
/// added the two PEC frames; flagged back to the AMS team. The
/// table itself is correct, hence the 4 here.)
pub const AMS_DIAG_NUM_FRAMES: usize = 4;

/// CAN ID of the FSM extended-status frame.
pub const AMS_FSM_STATUS_ID: u16 = 0x6C0;
/// CAN ID of the poll-timing frame.
pub const AMS_POLL_TIMING_ID: u16 = 0x6C1;
/// CAN ID of the per-IC PEC frame covering ICs 0..=7.
pub const AMS_PER_IC_PEC_LO_ID: u16 = 0x6C7;
/// CAN ID of the per-IC PEC frame covering ICs 8..=9 (+ reserved).
pub const AMS_PER_IC_PEC_HI_ID: u16 = 0x6C8;

/// Number of monitor ICs in the pack: 2 per module × 5 modules.
/// Chain index → module: IC `2m` = upper, IC `2m+1` = lower of
/// module `m`.
pub const AMS_NUM_ICS: usize = 2 * AMS_NUM_MODULES;

/// Total frames the AMS emits per 1 Hz scan when armed: 24 + 25 + 4.
/// The Studio + CLI compare this against an observed scan-rate
/// counter and warn if the wire shape has drifted — the canary for
/// "the AMS firmware added or dropped frames since this constant was
/// last verified". When new diag-block frames ship, bump
/// `AMS_DIAG_NUM_FRAMES` and the math falls out automatically.
pub const AMS_EXPECTED_FRAMES_PER_SCAN: usize =
    AMS_CELLV_NUM_FRAMES + AMS_NTC_NUM_FRAMES + AMS_DIAG_NUM_FRAMES;

// ---- Decoded records --------------------------------------------

/// One frame's worth of decoded cell voltages.
///
/// `first_cell` is the index of slot[0] in the pack-wide cell array
/// (0..=`AMS_NUM_CELLS`-1). For the last frame, slot[3] carries
/// `AMS_CELLV_SENTINEL` instead of a real reading; consumers should
/// range-check against `AMS_NUM_CELLS` rather than rely on the
/// sentinel value (a real cell could in principle hit `0xFFFF` mV
/// if the gauge ever supported a 65 V cell, which it doesn't).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellVoltageFrame {
    /// 0-based frame index in the cell-voltage stream (0..24).
    pub frame_idx: u8,
    /// First cell index this frame covers (= `frame_idx * 4`).
    pub first_cell: u16,
    /// Four voltages in mV (big-endian u16 on the wire).
    pub voltages_mv: [u16; 4],
}

/// One frame's worth of decoded NTC temperatures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NtcTempFrame {
    /// 0-based frame index in the NTC stream (0..25).
    pub frame_idx: u8,
    /// First NTC index this frame covers (= `frame_idx * 8`).
    pub first_ntc: u16,
    /// Eight temperatures in °C (signed i8 on the wire).
    pub temps_c: [i8; 8],
}

/// AMS FSM state, mirrored from `ams::fsm::State` in the firmware.
///
/// Wire encoding: byte 0 of the `0x6C0` frame, value 0..=5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FsmState {
    Start = 0,
    Precharge = 1,
    Transition = 2,
    Run = 3,
    Charge = 4,
    Error = 5,
    /// Any state byte outside 0..=5. Slice 2 didn't see it during
    /// bring-up but firmware could in principle emit out-of-range
    /// values; surfacing them keeps the host honest.
    Unknown(u8),
}

impl FsmState {
    fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Start,
            1 => Self::Precharge,
            2 => Self::Transition,
            3 => Self::Run,
            4 => Self::Charge,
            5 => Self::Error,
            other => Self::Unknown(other),
        }
    }
}

/// Mode lock — which operating mode the cockpit's been told to use.
///
/// Wire encoding: byte 1 of `0x6C0`, value 0..=2. (Also appears in
/// `0x4A2`'s telemetry byte, bits 3:2.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ModeLock {
    Undecided = 0,
    Car = 1,
    Charger = 2,
    Unknown(u8),
}

impl ModeLock {
    fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Undecided,
            1 => Self::Car,
            2 => Self::Charger,
            other => Self::Unknown(other),
        }
    }
}

/// Which safety predicate latched the AMS into ERROR.
///
/// Wire encoding: byte 6 of `0x6C0` (#276). Latched once at the
/// transition into ERROR and held until the latch clears. `None`
/// (0) when not in an error state — which is also what a pre-#276
/// firmware sends, since byte 6 was reserved-zero before. So this
/// decodes correctly against both old and new firmware.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FaultReason {
    None = 0,
    ForceError = 1,
    BmsModuleOffline = 2,
    BmsStale = 3,
    CellUnderVoltage = 4,
    CellOverVoltage = 5,
    CellUnderTemp = 6,
    CellOverTemp = 7,
    CurrentSensorFault = 8,
    CurrentStale = 9,
    CurrentOverLimit = 10,
    VcuStale = 11,
    /// FSM-driven Error path — precharge timeout / TSMS / DASH_CHG drop.
    FsmError = 12,
    /// Any reason byte the firmware emits that this host build
    /// doesn't recognise — a forward-compat catch-all.
    Unknown(u8),
}

impl FaultReason {
    fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::None,
            1 => Self::ForceError,
            2 => Self::BmsModuleOffline,
            3 => Self::BmsStale,
            4 => Self::CellUnderVoltage,
            5 => Self::CellOverVoltage,
            6 => Self::CellUnderTemp,
            7 => Self::CellOverTemp,
            8 => Self::CurrentSensorFault,
            9 => Self::CurrentStale,
            10 => Self::CurrentOverLimit,
            11 => Self::VcuStale,
            12 => Self::FsmError,
            other => Self::Unknown(other),
        }
    }
}

/// Decoded `0x6C0` — FSM extended status. Layout per
/// `docs/CAN_MAP.md` in IFS08-CE-AMS:
///
/// | byte | field |
/// |---|---|
/// | 0 | FSM state (`ams::fsm::State`) |
/// | 1 | mode_locked (0=Undecided / 1=Car / 2=Charger) |
/// | 2 | bits: bit 1 = TSMS, bit 0 = DASH_CHG |
/// | 3 | AMS_OK GPIO (0/1) |
/// | 4..5 | PEC error total (big-endian u16) |
/// | 6 | fault_reason — latched-ERROR predicate branch (#276) |
/// | 7 | fault_detail — module index (BmsStale) / online mask |
/// |   | (BmsModuleOffline) / 0 otherwise |
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FsmStatusFrame {
    pub state: FsmState,
    pub mode_locked: ModeLock,
    pub tsms: bool,
    pub dash_chg: bool,
    pub ams_ok: bool,
    pub pec_error_total: u16,
    /// Why the AMS latched ERROR (byte 6). `None` when healthy or
    /// when talking to a pre-#276 firmware (reserved-zero byte).
    pub fault_reason: FaultReason,
    /// Context for `fault_reason` (byte 7): module index for
    /// `BmsStale`, `module_online_mask` for `BmsModuleOffline`,
    /// 0 otherwise.
    pub fault_detail: u8,
}

/// Decoded `0x6C7` / `0x6C8` — per-IC PEC error counts.
///
/// Each monitor IC reports a saturating `u8` count of PEC (CRC)
/// errors on its slave-bus link. `0x6C7` carries ICs 0..=7 (8 bytes,
/// one per IC); `0x6C8` carries ICs 8..=9 in bytes 0..1 with bytes
/// 2..7 reserved-zero. Chain index → module: IC `2m` = upper,
/// IC `2m+1` = lower of module `m`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PerIcPecFrame {
    /// Index of the first IC this frame covers (0 for `0x6C7`,
    /// 8 for `0x6C8`).
    pub first_ic: u8,
    /// Saturating per-IC PEC counts. For `0x6C8` only the first
    /// `valid` entries are real ICs; the rest mirror the reserved
    /// zero bytes.
    pub counts: [u8; 8],
    /// How many entries in `counts` map to real ICs (8 for `0x6C7`,
    /// 2 for `0x6C8`).
    pub valid: u8,
}

/// Decoded `0x6C1` — poll timing. Layout per AMS doc:
///
/// | bytes | field |
/// |---|---|
/// | 0..1 | last V-poll latency (big-endian u16, milliseconds) |
/// | 2..3 | worst-case V-poll latency (big-endian u16, milliseconds) |
/// | 4..7 | last T-sweep failure mask (little-endian u32) |
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PollTimingFrame {
    pub last_v_poll_ms: u16,
    pub worst_v_poll_ms: u16,
    pub t_sweep_fail_mask: u32,
}

/// A decoded pit-diag frame, dispatched by CAN ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PitDiagFrame {
    /// AMS replied to an arm/disarm command.
    Ack {
        /// `true` after a successful arm (`payload[0] == 0x01`),
        /// `false` after a disarm (`payload[0] == 0x00`).
        enabled: bool,
    },
    /// One of the 24 cell-voltage frames.
    CellVoltage(CellVoltageFrame),
    /// One of the 25 NTC-temperature frames.
    NtcTemp(NtcTempFrame),
    /// `0x6C0` — FSM extended status.
    FsmStatus(FsmStatusFrame),
    /// `0x6C1` — V-poll + T-sweep timing telemetry.
    PollTiming(PollTimingFrame),
    /// `0x6C7` / `0x6C8` — per-IC PEC error counts.
    PerIcPec(PerIcPecFrame),
}

// ---- Encode / decode --------------------------------------------

/// Build the CAN frame that arms (or disarms) the AMS pit-diag stream.
///
/// Returned frame can be sent directly via the transport layer —
/// it's an 11-bit standard ID with a 4-byte payload.
#[must_use]
pub fn build_arm_frame(enable: bool) -> CanFrame {
    let payload = if enable {
        AMS_ARM_ENABLE_PAYLOAD
    } else {
        AMS_ARM_DISABLE_PAYLOAD
    };
    // 4-byte payload always fits the 8-byte classic-CAN ceiling.
    CanFrame::new(AMS_ARM_ID, &payload).expect("4-byte payload always fits")
}

/// Decode a raw CAN frame into a pit-diag record.
///
/// Returns `None` if the frame ID isn't part of any pit-diag stream,
/// or if a recognised ID arrived with a payload too short to decode.
/// Recognised-but-undecoded IDs (the `0x6C0..=0x6C6` block) come back
/// as [`PitDiagFrame::Diag`] so the caller can still see them in
/// slice 1 even before typed decoders exist.
#[must_use]
pub fn decode_frame(frame: &CanFrame) -> Option<PitDiagFrame> {
    let id = frame.id;
    let payload = frame.payload();

    // ACK frame — 1 byte, 0x01 / 0x00.
    if id == AMS_ACK_ID {
        let enabled = payload.first().copied().unwrap_or(0) == 0x01;
        return Some(PitDiagFrame::Ack { enabled });
    }

    // Cell-voltage stream — 24 frames, 4 cells/frame, BE u16 mV.
    if (AMS_CELLV_BASE_ID..=AMS_CELLV_LAST_ID).contains(&id) {
        if payload.len() < 8 {
            return None;
        }
        let frame_idx = (id - AMS_CELLV_BASE_ID) as u8;
        let voltages_mv = [
            u16::from_be_bytes([payload[0], payload[1]]),
            u16::from_be_bytes([payload[2], payload[3]]),
            u16::from_be_bytes([payload[4], payload[5]]),
            u16::from_be_bytes([payload[6], payload[7]]),
        ];
        return Some(PitDiagFrame::CellVoltage(CellVoltageFrame {
            frame_idx,
            first_cell: u16::from(frame_idx) * 4,
            voltages_mv,
        }));
    }

    // NTC-temperature stream — 25 frames, 8 NTCs/frame, i8 °C.
    if (AMS_NTC_BASE_ID..=AMS_NTC_LAST_ID).contains(&id) {
        if payload.len() < 8 {
            return None;
        }
        let frame_idx = (id - AMS_NTC_BASE_ID) as u8;
        let mut temps_c = [0i8; 8];
        for (slot, src) in temps_c.iter_mut().zip(payload.iter().take(8)) {
            *slot = *src as i8;
        }
        return Some(PitDiagFrame::NtcTemp(NtcTempFrame {
            frame_idx,
            first_ntc: u16::from(frame_idx) * 8,
            temps_c,
        }));
    }

    // FSM extended status — 0x6C0.
    if id == AMS_FSM_STATUS_ID {
        if payload.len() < 6 {
            // Need at least bytes 0..5 (PEC u16 ends at byte 5).
            return None;
        }
        let cockpit_bits = payload[2];
        return Some(PitDiagFrame::FsmStatus(FsmStatusFrame {
            state: FsmState::from_byte(payload[0]),
            mode_locked: ModeLock::from_byte(payload[1]),
            tsms: (cockpit_bits & 0b0000_0010) != 0,
            dash_chg: (cockpit_bits & 0b0000_0001) != 0,
            ams_ok: payload[3] != 0,
            pec_error_total: u16::from_be_bytes([payload[4], payload[5]]),
            // Bytes 6/7 are the #276 fault-reason/detail. A pre-#276
            // firmware leaves them reserved-zero ⇒ reason decodes to
            // `None`, detail to 0 — so this is correct against both
            // old and new firmware. `.get()` keeps the 6-byte-minimum
            // contract: a short frame falls back to None/0 instead of
            // panicking.
            fault_reason: payload
                .get(6)
                .map(|b| FaultReason::from_byte(*b))
                .unwrap_or(FaultReason::None),
            fault_detail: payload.get(7).copied().unwrap_or(0),
        }));
    }

    // Poll timing — 0x6C1.
    if id == AMS_POLL_TIMING_ID {
        if payload.len() < 8 {
            return None;
        }
        return Some(PitDiagFrame::PollTiming(PollTimingFrame {
            last_v_poll_ms: u16::from_be_bytes([payload[0], payload[1]]),
            worst_v_poll_ms: u16::from_be_bytes([payload[2], payload[3]]),
            // T-sweep mask is LE u32 on the wire (the only LE field
            // in the pit-diag stream — the rest is BE).
            t_sweep_fail_mask: u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]),
        }));
    }

    // Per-IC PEC counts — 0x6C7 (ICs 0..7) and 0x6C8 (ICs 8..9).
    if id == AMS_PER_IC_PEC_LO_ID || id == AMS_PER_IC_PEC_HI_ID {
        if payload.len() < 8 {
            return None;
        }
        let mut counts = [0u8; 8];
        counts.copy_from_slice(&payload[..8]);
        let (first_ic, valid) = if id == AMS_PER_IC_PEC_LO_ID {
            (0u8, 8u8)
        } else {
            // 0x6C8: ICs 8..9 live in bytes 0..1; bytes 2..7 are
            // reserved-zero. `valid` tells the consumer to ignore the
            // reserved tail rather than render six phantom ICs.
            (8u8, (AMS_NUM_ICS as u8).saturating_sub(8))
        };
        return Some(PitDiagFrame::PerIcPec(PerIcPecFrame {
            first_ic,
            counts,
            valid,
        }));
    }

    None
}

/// Convenience: is `cell_idx` a real cell (vs. the sentinel slot in
/// the last frame)? Wraps the literal range check so callers stay
/// readable.
#[must_use]
pub const fn is_real_cell_index(cell_idx: u16) -> bool {
    (cell_idx as usize) < AMS_NUM_CELLS
}

// ---- Compile-time invariants ------------------------------------
//
// Belt-and-braces — if anyone tweaks one constant out of sync with
// the others, these `assert!` calls fail to compile. They live at
// module scope (not inside `#[cfg(test)]`) because they're not
// run-time tests — clippy's `assertions_on_constants` rightly
// flags assertions on fully-const expressions, since they'd
// silently no-op in release builds. `const _: () = assert!(...)`
// instead aborts compilation.

const _: () = assert!(AMS_NUM_MODULES * AMS_CELLS_PER_MODULE == AMS_NUM_CELLS);
const _: () = assert!(AMS_NUM_MODULES * AMS_NTC_PER_MODULE == AMS_NUM_NTCS);
const _: () = assert!(AMS_CELLV_NUM_FRAMES == (AMS_CELLV_LAST_ID - AMS_CELLV_BASE_ID + 1) as usize);
const _: () = assert!(AMS_NTC_NUM_FRAMES == (AMS_NTC_LAST_ID - AMS_NTC_BASE_ID + 1) as usize);
// 24 frames × 4 cells/frame = 96 slots > 95 cells (room for sentinel).
const _: () = assert!(AMS_CELLV_NUM_FRAMES * 4 > AMS_NUM_CELLS);
// 25 frames × 8 NTCs/frame = 200 = AMS_NUM_NTCS exactly.
const _: () = assert!(AMS_NTC_NUM_FRAMES * 8 == AMS_NUM_NTCS);
// Total scan = 53 frames: 24 cell-V + 25 NTC + 4 diag (FSM, poll,
// 2× per-IC PEC). The Studio + CLI display a warning if the observed
// scan rate drifts from this — the canary for "the AMS team added or
// dropped frames since this constant was last verified". When new
// diag-block frames ship, bump AMS_DIAG_NUM_FRAMES and the math falls
// out automatically.
const _: () = assert!(AMS_EXPECTED_FRAMES_PER_SCAN == 53);
// The diag block is non-contiguous (gap at 0x6C2..=0x6C6), so we
// can't assert a single base..=last range. Instead: pin the count
// and confirm the per-IC PEC pair is contiguous + sits above the
// FSM/poll pair.
const _: () = assert!(AMS_DIAG_NUM_FRAMES == 4);
const _: () = assert!(AMS_POLL_TIMING_ID == AMS_FSM_STATUS_ID + 1);
const _: () = assert!(AMS_PER_IC_PEC_HI_ID == AMS_PER_IC_PEC_LO_ID + 1);
const _: () = assert!(AMS_PER_IC_PEC_LO_ID > AMS_POLL_TIMING_ID);
// Two ICs per module; 0x6C8 carries the ICs past the 8 in 0x6C7.
const _: () = assert!(AMS_NUM_ICS == 10);
const _: () = assert!(AMS_NUM_ICS > 8);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_frame_encodes_correctly() {
        let on = build_arm_frame(true);
        assert_eq!(on.id, AMS_ARM_ID);
        assert_eq!(on.payload(), &[0xDE, 0xAD, 0xBE, 0xEF]);

        let off = build_arm_frame(false);
        assert_eq!(off.id, AMS_ARM_ID);
        assert_eq!(off.payload(), &[0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn decodes_ack_enabled() {
        let frame = CanFrame::new(AMS_ACK_ID, &[0x01]).unwrap();
        assert_eq!(
            decode_frame(&frame),
            Some(PitDiagFrame::Ack { enabled: true })
        );
    }

    #[test]
    fn decodes_ack_disabled() {
        let frame = CanFrame::new(AMS_ACK_ID, &[0x00]).unwrap();
        assert_eq!(
            decode_frame(&frame),
            Some(PitDiagFrame::Ack { enabled: false })
        );
    }

    #[test]
    fn decodes_ack_empty_payload_as_disabled() {
        // Defensive: an empty payload shouldn't panic. Treat absent
        // payload as disabled so flaky bus drops fail closed.
        let frame = CanFrame::new(AMS_ACK_ID, &[]).unwrap();
        assert_eq!(
            decode_frame(&frame),
            Some(PitDiagFrame::Ack { enabled: false })
        );
    }

    #[test]
    fn decodes_cell_voltage_mid_stream() {
        // Frame 0x685 (idx 5) carries cells 20..=23.
        let frame =
            CanFrame::new(0x685, &[0x0D, 0x48, 0x0D, 0x50, 0x0D, 0x55, 0x0D, 0x60]).unwrap();
        let decoded = decode_frame(&frame).unwrap();
        match decoded {
            PitDiagFrame::CellVoltage(c) => {
                assert_eq!(c.frame_idx, 5);
                assert_eq!(c.first_cell, 20);
                // 0x0D48 = 3400 mV, etc.
                assert_eq!(c.voltages_mv, [3400, 3408, 3413, 3424]);
            }
            other => panic!("expected CellVoltage, got {other:?}"),
        }
    }

    #[test]
    fn last_cell_voltage_frame_carries_sentinel() {
        // 0x697 is the 24th (last) frame; carries cells 92, 93, 94,
        // sentinel (0xFFFF) for the missing 96th cell.
        let frame = CanFrame::new(
            AMS_CELLV_LAST_ID,
            &[0x0D, 0x48, 0x0D, 0x50, 0x0D, 0x55, 0xFF, 0xFF],
        )
        .unwrap();
        let decoded = decode_frame(&frame).unwrap();
        match decoded {
            PitDiagFrame::CellVoltage(c) => {
                assert_eq!(c.first_cell, 92);
                assert_eq!(c.voltages_mv[3], AMS_CELLV_SENTINEL);
                assert!(is_real_cell_index(c.first_cell + 2));
                assert!(!is_real_cell_index(c.first_cell + 3));
            }
            other => panic!("expected CellVoltage, got {other:?}"),
        }
    }

    #[test]
    fn decodes_ntc_temperatures_with_signed_mix() {
        // Frame 0x6A2 (idx 2) — NTCs 16..=23. Mix +ve, -ve, INT8_MIN.
        let frame = CanFrame::new(
            0x6A2,
            &[
                25,   // +25 °C
                26,   // +26 °C
                24,   // +24 °C
                0xFE, // -2 °C
                22,   // +22 °C
                21,   // +21 °C
                0x80, // -128 °C (INT8_MIN, sentinel / unwired)
                19,   // +19 °C
            ],
        )
        .unwrap();
        let decoded = decode_frame(&frame).unwrap();
        match decoded {
            PitDiagFrame::NtcTemp(t) => {
                assert_eq!(t.frame_idx, 2);
                assert_eq!(t.first_ntc, 16);
                assert_eq!(t.temps_c, [25, 26, 24, -2, 22, 21, -128, 19]);
            }
            other => panic!("expected NtcTemp, got {other:?}"),
        }
    }

    #[test]
    fn decodes_fsm_status_run_state() {
        // 0x6C0 — typical in-Run snapshot: state=Run(3), mode=Car(1),
        // TSMS on + DASH_CHG on (cockpit byte 0b11 = 0x03), AMS_OK on,
        // PEC errors = 0x0042.
        let frame = CanFrame::new(
            AMS_FSM_STATUS_ID,
            &[0x03, 0x01, 0x03, 0x01, 0x00, 0x42, 0x00, 0x00],
        )
        .unwrap();
        let decoded = decode_frame(&frame).unwrap();
        match decoded {
            PitDiagFrame::FsmStatus(f) => {
                assert_eq!(f.state, FsmState::Run);
                assert_eq!(f.mode_locked, ModeLock::Car);
                assert!(f.tsms);
                assert!(f.dash_chg);
                assert!(f.ams_ok);
                assert_eq!(f.pec_error_total, 0x0042);
                // Healthy run: bytes 6/7 reserved-zero ⇒ no fault.
                assert_eq!(f.fault_reason, FaultReason::None);
                assert_eq!(f.fault_detail, 0);
            }
            other => panic!("expected FsmStatus, got {other:?}"),
        }
    }

    #[test]
    fn decodes_fsm_status_charge_state_no_tsms() {
        // Charger plugged in: state=Charge(4), mode=Charger(2),
        // TSMS off (chassis disarmed), DASH_CHG on. PEC = 0.
        let frame = CanFrame::new(
            AMS_FSM_STATUS_ID,
            &[0x04, 0x02, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00],
        )
        .unwrap();
        let decoded = decode_frame(&frame).unwrap();
        match decoded {
            PitDiagFrame::FsmStatus(f) => {
                assert_eq!(f.state, FsmState::Charge);
                assert_eq!(f.mode_locked, ModeLock::Charger);
                assert!(!f.tsms);
                assert!(f.dash_chg);
                assert!(f.ams_ok);
                assert_eq!(f.pec_error_total, 0);
            }
            other => panic!("expected FsmStatus, got {other:?}"),
        }
    }

    #[test]
    fn decodes_fsm_status_error_state_unknown_mode_byte() {
        // Defensive: a byte outside the documented 0..=2 mode range
        // surfaces as ModeLock::Unknown — slice 2 isn't going to
        // crash on a future firmware that adds a 4th mode.
        let frame = CanFrame::new(
            AMS_FSM_STATUS_ID,
            &[0x05, 0xAA, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00],
        )
        .unwrap();
        let decoded = decode_frame(&frame).unwrap();
        match decoded {
            PitDiagFrame::FsmStatus(f) => {
                assert_eq!(f.state, FsmState::Error);
                assert_eq!(f.mode_locked, ModeLock::Unknown(0xAA));
                assert_eq!(f.pec_error_total, 0xFFFF);
            }
            other => panic!("expected FsmStatus, got {other:?}"),
        }
    }

    #[test]
    fn decodes_poll_timing() {
        // 0x6C1 — last V-poll 12ms, worst V-poll 41ms,
        // T-sweep fail mask = 0xCAFE_BABE (LE on the wire ⇒ bytes
        // BE BA FE CA).
        let frame = CanFrame::new(
            AMS_POLL_TIMING_ID,
            &[0x00, 0x0C, 0x00, 0x29, 0xBE, 0xBA, 0xFE, 0xCA],
        )
        .unwrap();
        let decoded = decode_frame(&frame).unwrap();
        match decoded {
            PitDiagFrame::PollTiming(p) => {
                assert_eq!(p.last_v_poll_ms, 12);
                assert_eq!(p.worst_v_poll_ms, 41);
                assert_eq!(p.t_sweep_fail_mask, 0xCAFE_BABE);
            }
            other => panic!("expected PollTiming, got {other:?}"),
        }
    }

    #[test]
    fn fsm_status_short_payload_rejected() {
        // 5-byte payload is too short to reach the PEC u16 at bytes
        // 4..5 — decoder should reject rather than index out of
        // bounds. 6 bytes is the minimum that decodes (the reserved
        // tail at 6..7 is don't-care).
        let frame = CanFrame::new(AMS_FSM_STATUS_ID, &[0x00, 0x00, 0x00, 0x00, 0x00]).unwrap();
        assert_eq!(decode_frame(&frame), None);
    }

    #[test]
    fn decodes_fsm_status_fault_reason_and_detail() {
        // 0x6C0 with the #276 fault bytes populated: latched ERROR
        // on BmsStale (reason=3), offending module index 4 in byte 7.
        let frame = CanFrame::new(
            AMS_FSM_STATUS_ID,
            &[0x05, 0x01, 0x00, 0x00, 0x00, 0x00, 0x03, 0x04],
        )
        .unwrap();
        match decode_frame(&frame).unwrap() {
            PitDiagFrame::FsmStatus(f) => {
                assert_eq!(f.state, FsmState::Error);
                assert_eq!(f.fault_reason, FaultReason::BmsStale);
                assert_eq!(f.fault_detail, 4);
            }
            other => panic!("expected FsmStatus, got {other:?}"),
        }
    }

    #[test]
    fn fsm_status_unknown_fault_reason_byte() {
        // A reason byte past the documented 0..=12 range surfaces as
        // Unknown rather than silently mapping to None — forward-compat
        // with a firmware that adds a 13th predicate.
        let frame = CanFrame::new(
            AMS_FSM_STATUS_ID,
            &[0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x63, 0x00],
        )
        .unwrap();
        match decode_frame(&frame).unwrap() {
            PitDiagFrame::FsmStatus(f) => {
                assert_eq!(f.fault_reason, FaultReason::Unknown(0x63));
            }
            other => panic!("expected FsmStatus, got {other:?}"),
        }
    }

    #[test]
    fn six_byte_fsm_frame_decodes_with_no_fault() {
        // A 6-byte FSM frame (no bytes 6/7) still decodes — fault
        // fields fall back to None/0 rather than rejecting the frame.
        let frame =
            CanFrame::new(AMS_FSM_STATUS_ID, &[0x03, 0x01, 0x03, 0x01, 0x00, 0x00]).unwrap();
        match decode_frame(&frame).unwrap() {
            PitDiagFrame::FsmStatus(f) => {
                assert_eq!(f.state, FsmState::Run);
                assert_eq!(f.fault_reason, FaultReason::None);
                assert_eq!(f.fault_detail, 0);
            }
            other => panic!("expected FsmStatus, got {other:?}"),
        }
    }

    #[test]
    fn decodes_per_ic_pec_low_frame() {
        // 0x6C7 — ICs 0..7, one saturating count each.
        let frame = CanFrame::new(AMS_PER_IC_PEC_LO_ID, &[0, 1, 0, 0, 5, 0, 255, 2]).unwrap();
        match decode_frame(&frame).unwrap() {
            PitDiagFrame::PerIcPec(p) => {
                assert_eq!(p.first_ic, 0);
                assert_eq!(p.valid, 8);
                assert_eq!(p.counts, [0, 1, 0, 0, 5, 0, 255, 2]);
            }
            other => panic!("expected PerIcPec, got {other:?}"),
        }
    }

    #[test]
    fn decodes_per_ic_pec_high_frame() {
        // 0x6C8 — ICs 8..9 in bytes 0..1, bytes 2..7 reserved-zero.
        let frame = CanFrame::new(AMS_PER_IC_PEC_HI_ID, &[7, 3, 0, 0, 0, 0, 0, 0]).unwrap();
        match decode_frame(&frame).unwrap() {
            PitDiagFrame::PerIcPec(p) => {
                assert_eq!(p.first_ic, 8);
                // 10 ICs total − 8 in the low frame = 2 real here.
                assert_eq!(p.valid, 2);
                assert_eq!(p.counts[0], 7);
                assert_eq!(p.counts[1], 3);
            }
            other => panic!("expected PerIcPec, got {other:?}"),
        }
    }

    #[test]
    fn per_ic_pec_short_payload_rejected() {
        let frame = CanFrame::new(AMS_PER_IC_PEC_LO_ID, &[0, 1, 2]).unwrap();
        assert_eq!(decode_frame(&frame), None);
    }

    #[test]
    fn unrelated_id_returns_none() {
        let frame = CanFrame::new(0x123, &[0; 8]).unwrap();
        assert_eq!(decode_frame(&frame), None);
    }

    #[test]
    fn short_payload_on_recognised_id_returns_none() {
        // 0x680 expects 8 bytes; a 4-byte payload should be rejected
        // so we don't index out of bounds.
        let frame = CanFrame::new(0x680, &[0x0D, 0x48, 0x0D, 0x50]).unwrap();
        assert_eq!(decode_frame(&frame), None);
    }

    // Pack-geometry invariants are now compile-time `const _: () =
    // assert!(...)` at module scope — see the block above this
    // `tests` module. Keeping them out of `#[test]` keeps clippy
    // happy (no `assertions_on_constants` warnings) and turns the
    // invariant into a build break rather than a silent test pass.
}
