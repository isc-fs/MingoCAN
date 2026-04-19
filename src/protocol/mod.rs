//! Wire-format core — types and parsers for the CAN bootloader
//! protocol.
//!
//! This module is pure: no I/O, no transport dependencies, nothing
//! platform-specific. Everything here is byte shuffling and state
//! machines; all of it is unit-tested. Higher layers (`transport::*`
//! for adapter I/O, `cli::*` for orchestration) build on top.
//!
//! The source of truth for the wire format is
//! `Core/Inc/bl_proto.h` + `Core/Inc/bl_isotp.h` in the bootloader
//! repository. Any field layout or constant here must match those
//! headers exactly — if you need to change one, change the bootloader
//! first and open a REQUIREMENTS.md / bl_proto.h / protocol update
//! triplet.
//!
//! ## Layout
//!
//! - [`ids`] — 11-bit frame ID encode/decode, message-type enum,
//!   host / broadcast node constants.
//! - [`opcodes`] — command opcodes, NACK codes, notify opcodes,
//!   reset modes. Strict `#[repr(u8)]` where we never emit unknown
//!   values; lenient `Unknown(u8)` fallback where we need to parse
//!   what the device sent without crashing on a future opcode.
//! - [`isotp`] — ISO-TP SF/FF/CF/FC encoder (stateless iterator) and
//!   RX reassembler (state machine). 1024-byte max message length
//!   matches the bootloader's `BL_ISOTP_MAX_MSG`.
//! - [`records`] — fixed-layout record structs ([`records::FirmwareInfo`],
//!   [`records::HealthRecord`], [`records::LiveDataSnapshot`],
//!   [`records::DtcEntry`], [`records::ObStatus`]) with parse / encode
//!   helpers and flag-bit constants.
//! - [`commands`] — typed command-payload builders. Each helper emits
//!   an `[opcode, args…]` buffer ready to hand to the ISO-TP segmenter.
//! - [`responses`] — typed response parser that takes a completed
//!   ISO-TP reassembly + the CAN message type and returns a
//!   [`Response`] enum.

pub mod commands;
pub mod ids;
pub mod isotp;
pub mod opcodes;
pub mod records;
pub mod responses;

pub use ids::{FrameId, MessageType, BROADCAST_NODE_ID, HOST_NODE_ID};
pub use isotp::{IsoTpError, IsoTpSegmenter, Reassembler};
pub use opcodes::{CommandOpcode, NackCode, NotifyOpcode, ResetMode};
pub use records::{DtcEntry, FirmwareInfo, HealthRecord, LiveDataSnapshot, ObStatus, ResetCause};
pub use responses::Response;

/// The classic-CAN frame this crate shuttles up and down the stack.
///
/// `data` always has a capacity of 8 bytes — classic CAN's payload
/// ceiling. `len` says how many of those bytes are live; the rest is
/// don't-care padding. ISO-TP always fills all 8 bytes on TX (padding
/// with `0x00`); adapters may hand us shorter frames on RX, which is
/// why `len` exists rather than a `[u8; 8]` alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanFrame {
    /// 11-bit standard identifier. Only bits 0..=10 are used.
    pub id: u16,
    /// Payload bytes. Bytes past `len` are don't-care.
    pub data: [u8; 8],
    /// Live payload length, 0..=8.
    pub len: u8,
}

impl CanFrame {
    /// Max classic-CAN payload length, in bytes.
    pub const MAX_LEN: usize = 8;

    /// Build a frame from an 11-bit ID and a payload slice. Fails if
    /// the payload is longer than 8 bytes.
    pub fn new(id: u16, payload: &[u8]) -> Result<Self, ParseError> {
        if payload.len() > Self::MAX_LEN {
            return Err(ParseError::PayloadTooLong {
                got: payload.len(),
                max: Self::MAX_LEN,
            });
        }
        let mut data = [0u8; 8];
        data[..payload.len()].copy_from_slice(payload);
        Ok(Self {
            id,
            data,
            len: payload.len() as u8,
        })
    }

    /// Slice view of the live bytes, ignoring padding.
    pub fn payload(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }
}

/// Every parse / encode helper in the protocol module funnels into
/// this one error type. Subsystems downcast / inspect specific
/// variants; callers that just want to log a message can use
/// `Display`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Frame payload exceeded the 8-byte classic-CAN ceiling.
    #[error("frame payload too long: got {got} bytes, max {max}")]
    PayloadTooLong { got: usize, max: usize },

    /// Message type nibble in the 11-bit ID is not one of the six
    /// defined values.
    #[error("unknown message type 0x{0:X} in frame ID")]
    UnknownMessageType(u8),

    /// Source or destination node ID overflows its 4-bit field. Only
    /// surfaces from `FrameId::build` — parsed IDs are already
    /// masked.
    #[error("node id 0x{0:X} does not fit in 4 bits")]
    NodeIdOutOfRange(u8),

    /// 11-bit frame ID has bits set above bit 10.
    #[error("frame ID 0x{0:X} has bits set above bit 10 (not a valid 11-bit standard ID)")]
    IdOutOfRange(u16),

    /// A fixed-layout record (health / live-data / firmware-info /
    /// DTC / OB status) was shorter than expected.
    #[error("record too short: got {got} bytes, need {need}")]
    RecordTooShort { got: usize, need: usize },

    /// A fixed-layout record with a magic field did not match.
    #[error("record magic mismatch: got 0x{got:08X}, want 0x{want:08X}")]
    MagicMismatch { got: u32, want: u32 },

    /// A record-version byte / field was not in the supported range.
    /// Host tools consume older records fine (forward-compat), but
    /// a major-version bump in the future means new parse paths.
    #[error("unsupported record version 0x{got:08X}, supported major {supported_major}")]
    UnsupportedRecordVersion { got: u32, supported_major: u16 },

    /// Generic "parse got weird bytes" bucket for cases that don't
    /// deserve their own variant yet. Try to attach a message.
    #[error("{0}")]
    Invalid(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_frame_round_trip() {
        let f = CanFrame::new(0x123, &[0xAA, 0xBB, 0xCC]).unwrap();
        assert_eq!(f.id, 0x123);
        assert_eq!(f.len, 3);
        assert_eq!(f.payload(), &[0xAA, 0xBB, 0xCC]);
        // Padding is zero.
        assert_eq!(&f.data[3..], &[0u8; 5]);
    }

    #[test]
    fn can_frame_rejects_oversize() {
        let err = CanFrame::new(0x000, &[0; 9]).unwrap_err();
        assert!(matches!(err, ParseError::PayloadTooLong { got: 9, max: 8 }));
    }

    #[test]
    fn can_frame_empty_is_ok() {
        let f = CanFrame::new(0x000, &[]).unwrap();
        assert_eq!(f.len, 0);
        assert_eq!(f.payload(), &[] as &[u8]);
    }
}
