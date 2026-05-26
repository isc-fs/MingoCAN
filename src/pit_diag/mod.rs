//! AMS pit-diag observer protocol.
//!
//! When armed, the AMS emits 51 frames at 1 Hz carrying the
//! diagnostic picture: every cell voltage, every NTC temperature,
//! plus FSM state + poll-timing telemetry. This module handles the
//! arm/disarm handshake and decodes the wire frames into typed
//! records the rest of the crate (and the Studio app) can consume.
//!
//! ## Wire protocol
//!
//! Source of truth: `docs/CAN_MAP.md` in the AMS repo (IFS08-CE-AMS).
//! The #252 issue body in this repo lists more diag-block frames
//! (balance / boot / crash / firmware-ID) — those are forward-
//! looking; the firmware ships 2 today (`0x6C0` + `0x6C1`).
//!
//! - **Enable**:  emit `0x7F0` with payload `DE AD BE EF`.
//! - **Disable**: emit `0x7F0` with payload `00 00 00 00`.
//! - **ACK**:     AMS replies on `0x7F1` with 1 byte —
//!   `0x01` = enabled, `0x00` = disabled.
//! - **Stream IDs once armed (51 frames / scan total)**:
//!   - `0x680..=0x697` (24 frames) — cell voltages, 4 cells/frame,
//!     big-endian `u16` millivolts. The last frame's 4th slot is a
//!     `0xFFFF` sentinel because 95 cells don't divide evenly by 4.
//!   - `0x6A0..=0x6B8` (25 frames) — NTC temperatures, 8 NTCs/frame,
//!     signed `i8` °C. Exact (25 × 8 = 200).
//!   - `0x6C0` — FSM extended status (state, mode_locked, TSMS,
//!     DASH_CHG, AMS_OK, PEC error total).
//!   - `0x6C1` — Poll timing (last V-poll ms, worst V-poll ms,
//!     T-sweep failure mask).
//!
//! ## Scope
//!
//! Slice 1 shipped the handshake + cell-voltage + NTC-temp decoders.
//! Slice 2 (this slice) adds typed [`FsmStatusFrame`] +
//! [`PollTimingFrame`] decoders for the diag block, plus
//! [`AMS_EXPECTED_FRAMES_PER_SCAN`] which the Studio uses to detect
//! wire-shape drift before any silent miscalibration takes hold.
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

/// First CAN ID in the FSM / poll-timing diag block.
///
/// The #252 issue body lists seven frames here (`0x6C0..=0x6C6` for
/// FSM / poll / balance / boot / crash / firmware-ID), but the AMS
/// firmware's `docs/CAN_MAP.md` (the source of truth — see IFS08-CE-AMS)
/// currently only documents `0x6C0` and `0x6C1`. The firmware itself
/// confirms it: the doc's bus-cost note reads "51 frames × ~12 bytes-
/// on-wire", which is 24 + 25 + **2** — not the issue's projected 56.
///
/// Slice 2 lines up the host-side decoder with the real wire shape.
/// The remaining frames (balance / boot / crash / firmware-ID) land
/// in future slices once the AMS team ships them. The constant +
/// invariant pair keeps the host honest: when the AMS doc grows to
/// list more frames, this constant moves with it and the
/// `AMS_EXPECTED_FRAMES_PER_SCAN` math falls out automatically.
pub const AMS_DIAG_BASE_ID: u16 = 0x6C0;
/// Last CAN ID currently emitted in the diag block.
pub const AMS_DIAG_LAST_ID: u16 = 0x6C1;
/// Number of frames in the diag block today (2 — FSM + poll timing).
pub const AMS_DIAG_NUM_FRAMES: usize = 2;

/// CAN ID of the FSM extended-status frame.
pub const AMS_FSM_STATUS_ID: u16 = 0x6C0;
/// CAN ID of the poll-timing frame.
pub const AMS_POLL_TIMING_ID: u16 = 0x6C1;

/// Total frames the AMS emits per 1 Hz scan when armed: 24 + 25 + 2.
/// The Studio compares this against an observed scan-rate counter
/// and banners a warning if the wire shape has drifted — if a future
/// firmware version adds or drops frames, this is the signal the
/// operator sees before the brittleness of the hand-coded layout
/// bites them.
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
/// | 6..7 | reserved |
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FsmStatusFrame {
    pub state: FsmState,
    pub mode_locked: ModeLock,
    pub tsms: bool,
    pub dash_chg: bool,
    pub ams_ok: bool,
    pub pec_error_total: u16,
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
            // Need at least bytes 0..5 (PEC u16 ends at byte 5);
            // 6..7 are reserved, so a 6-byte payload still decodes.
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
// Diag block size matches the (base, last) range.
const _: () = assert!(AMS_DIAG_NUM_FRAMES == (AMS_DIAG_LAST_ID - AMS_DIAG_BASE_ID + 1) as usize);
// Total scan = 51 frames per the AMS doc's bus-cost note ("51
// frames × ~12 bytes-on-wire"). The Studio displays a warning if
// the observed scan rate drifts from this — that's the canary for
// "the AMS team added or dropped frames since this constant was
// last verified". When new diag-block frames ship, bump
// AMS_DIAG_LAST_ID and AMS_DIAG_NUM_FRAMES together.
const _: () = assert!(AMS_EXPECTED_FRAMES_PER_SCAN == 51);
// Both named diag IDs must live inside the diag block range.
const _: () =
    assert!(AMS_FSM_STATUS_ID >= AMS_DIAG_BASE_ID && AMS_FSM_STATUS_ID <= AMS_DIAG_LAST_ID);
const _: () =
    assert!(AMS_POLL_TIMING_ID >= AMS_DIAG_BASE_ID && AMS_POLL_TIMING_ID <= AMS_DIAG_LAST_ID);

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
