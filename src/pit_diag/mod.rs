//! AMS pit-diag observer protocol.
//!
//! When armed, the AMS emits 56 frames at 1 Hz carrying the full
//! diagnostic picture: every cell voltage, every NTC temperature,
//! plus FSM state, balance mask, boot reason, crash post-mortem,
//! and firmware ID. This module handles the arm/disarm handshake
//! and decodes the wire frames into typed records the rest of the
//! crate (and the Studio app) can consume.
//!
//! ## Wire protocol (per AMS PR #248)
//!
//! - **Enable**:  emit `0x7F0` with payload `DE AD BE EF`.
//! - **Disable**: emit `0x7F0` with payload `00 00 00 00`.
//! - **ACK**:     AMS replies on `0x7F1` with 1 byte —
//!   `0x01` = enabled, `0x00` = disabled.
//! - **Stream IDs once armed**:
//!   - `0x680..=0x697` (24 frames) — cell voltages, 4 cells/frame,
//!     big-endian `u16` millivolts. The last frame's 4th slot is a
//!     `0xFFFF` sentinel because 95 cells don't divide evenly by 4.
//!   - `0x6A0..=0x6B8` (25 frames) — NTC temperatures, 8 NTCs/frame,
//!     signed `i8` °C. Exact (25 × 8 = 200).
//!   - `0x6C0..=0x6C6` (7 frames) — FSM / balance / boot / crash /
//!     firmware ID. Decoders for these land in slice 2.
//!
//! ## Scope
//!
//! Slice 1 (this module): handshake + cell-voltage + NTC-temp
//! decoders. Frames `0x6C0..=0x6C6` are recognised as in-range but
//! not decoded — `decode_frame` returns `None` for them so callers
//! can pass them through without warnings while slice 2 fills in
//! the typed records.
//!
//! VCU + UDV will get equivalent streams in their own ID ranges
//! (TBD per team). The plugin/profile abstraction lands in slice 5;
//! until then the symbol names here are AMS-prefixed (`AMS_*`) so
//! the eventual refactor is obvious.

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

/// First CAN ID in the FSM / balance / boot / crash / fw-ID block
/// (7 frames). Decoders for these arrive in slice 2; slice 1 just
/// recognises the range so callers can pass them through.
pub const AMS_DIAG_BASE_ID: u16 = 0x6C0;
/// Last CAN ID in the diag block.
pub const AMS_DIAG_LAST_ID: u16 = 0x6C6;

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

/// A decoded pit-diag frame, dispatched by CAN ID.
///
/// The `Diag` variant carries the un-decoded payload for IDs in
/// the FSM/balance/boot/crash/fw-ID block (`0x6C0..=0x6C6`) — slice 2
/// fills in typed variants for each.
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
    /// One of the 7 FSM/balance/boot/crash/fw-ID frames. Slice 2
    /// replaces this with a typed variant per ID.
    Diag {
        /// CAN ID (`0x6C0..=0x6C6`).
        id: u16,
        /// Raw payload, copied out of the frame.
        payload: [u8; 8],
        /// Live payload length (0..=8).
        len: u8,
    },
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

    // FSM/balance/boot/crash/fw-ID block — recognised, not typed yet.
    // Surface the raw payload so slice 1 callers can at least see the
    // frames arriving on the wire.
    if (AMS_DIAG_BASE_ID..=AMS_DIAG_LAST_ID).contains(&id) {
        return Some(PitDiagFrame::Diag {
            id,
            payload: frame.data,
            len: frame.len,
        });
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
        let frame = CanFrame::new(
            0x685,
            &[0x0D, 0x48, 0x0D, 0x50, 0x0D, 0x55, 0x0D, 0x60],
        )
        .unwrap();
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
                25,         // +25 °C
                26,         // +26 °C
                24,         // +24 °C
                0xFE,       // -2 °C
                22,         // +22 °C
                21,         // +21 °C
                0x80,       // -128 °C (INT8_MIN, sentinel / unwired)
                19,         // +19 °C
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
    fn diag_block_returns_raw_payload() {
        // 0x6C2 — balance DCC mask frame (slice 2 will decode this).
        let payload = [0xAA, 0xBB, 0xCC, 0xDD, 0x11, 0x22, 0x33, 0x44];
        let frame = CanFrame::new(0x6C2, &payload).unwrap();
        let decoded = decode_frame(&frame).unwrap();
        match decoded {
            PitDiagFrame::Diag { id, payload: p, len } => {
                assert_eq!(id, 0x6C2);
                assert_eq!(&p[..], &payload[..]);
                assert_eq!(len, 8);
            }
            other => panic!("expected Diag, got {other:?}"),
        }
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

    #[test]
    fn pack_geometry_constants_are_consistent() {
        // Belt-and-braces — if anyone tweaks one constant they should
        // see the assert fire immediately.
        assert_eq!(
            AMS_NUM_MODULES * AMS_CELLS_PER_MODULE,
            AMS_NUM_CELLS
        );
        assert_eq!(AMS_NUM_MODULES * AMS_NTC_PER_MODULE, AMS_NUM_NTCS);
        assert_eq!(
            AMS_CELLV_NUM_FRAMES,
            (AMS_CELLV_LAST_ID - AMS_CELLV_BASE_ID + 1) as usize
        );
        assert_eq!(
            AMS_NTC_NUM_FRAMES,
            (AMS_NTC_LAST_ID - AMS_NTC_BASE_ID + 1) as usize
        );
        // 24 frames × 4 cells/frame = 96 slots ≥ 95 cells + 1 sentinel.
        assert!(AMS_CELLV_NUM_FRAMES * 4 >= AMS_NUM_CELLS + 1);
        // 25 frames × 8 NTCs/frame = 200 = AMS_NUM_NTCS exactly.
        assert_eq!(AMS_NTC_NUM_FRAMES * 8, AMS_NUM_NTCS);
    }
}
