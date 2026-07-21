//! Opcodes, NACK codes, notify opcodes, and reset modes.
//!
//! Two policies split the enums here:
//!
//! - **Strict, `#[repr(u8)]`** — types we **emit** onto the wire.
//!   Parsing an unknown value from a peer is a bug / version mismatch
//!   we want to surface as an error, not silently swallow. See
//!   [`CommandOpcode`], [`NotifyOpcode`], [`ResetMode`].
//! - **Lenient, with `Unknown(u8)` fallback** — types the peer **sends
//!   to us** where forward-compat matters more than strictness. A
//!   future bootloader release that introduces a new NACK code
//!   should still land in the host's logs readably, not crash the
//!   flasher. See [`NackCode`].

use super::ParseError;

// ---- Command opcodes (host → device) ----

/// Every CMD opcode the bootloader accepts, per `bl_proto.h`. The
/// flasher tracks the contract at `BL_PROTO_VERSION_MAJOR.MINOR =
/// 0.2` (see [`commands::PROTOCOL_VERSION_MAJOR`] /
/// [`commands::PROTOCOL_VERSION_MINOR`]). Newer opcodes added in
/// 0.2 (e.g. `CMD_NVM_FORMAT = 0x82`) are annotated inline.
///
/// [`commands::PROTOCOL_VERSION_MAJOR`]: super::commands::PROTOCOL_VERSION_MAJOR
/// [`commands::PROTOCOL_VERSION_MINOR`]: super::commands::PROTOCOL_VERSION_MINOR
///
/// Strict parsing: the flasher never emits anything outside this
/// list. If a new opcode appears on the bus we don't recognise, it's
/// a version-skew signal and we surface it as an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CommandOpcode {
    Connect = 0x01,
    Disconnect = 0x02,
    Discover = 0x03,
    GetFwInfo = 0x04,
    GetHealth = 0x05,
    FlashErase = 0x10,
    FlashWrite = 0x11,
    FlashReadCrc = 0x12,
    FlashVerify = 0x13,
    /// microSD log-file transfer service (`0x2x` "storage" group,
    /// IFS08-CE-AMS#406 / can-flasher #506). Read-only in v1 —
    /// `0x26 LOGFS_DELETE` is deliberately NOT implemented yet.
    LogfsList = 0x21,
    LogfsOpen = 0x22,
    LogfsRead = 0x23,
    LogfsCrc = 0x24,
    LogfsClose = 0x25,
    /// Seal the ACTIVE log and return its index, so the run that just
    /// finished is immediately listable (IFS08-CE-AMS#454).
    LogfsFinalize = 0x27,
    LogStreamStart = 0x30,
    LogStreamStop = 0x31,
    LiveDataStart = 0x32,
    LiveDataStop = 0x33,
    DtcRead = 0x40,
    DtcClear = 0x41,
    ObRead = 0x50,
    ObApplyWrp = 0x51,
    Reset = 0x60,
    Jump = 0x61,
    NvmRead = 0x80,
    NvmWrite = 0x81,
    /// Bootloader 0.2 added a wholesale NVM wipe. Erases the NVM
    /// sector unconditionally; requires a 4-byte confirmation token
    /// ([`commands::NVM_FORMAT_TOKEN`]) as the payload to guard the
    /// destructive call. Wrong / missing token NACKs with
    /// [`NackCode::NvmWrongToken`].
    NvmFormat = 0x82,
}

impl CommandOpcode {
    /// Raw byte as transmitted / received.
    pub fn as_byte(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for CommandOpcode {
    type Error = ParseError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Ok(match value {
            0x01 => Self::Connect,
            0x02 => Self::Disconnect,
            0x03 => Self::Discover,
            0x04 => Self::GetFwInfo,
            0x05 => Self::GetHealth,
            0x10 => Self::FlashErase,
            0x11 => Self::FlashWrite,
            0x12 => Self::FlashReadCrc,
            0x13 => Self::FlashVerify,
            0x21 => Self::LogfsList,
            0x22 => Self::LogfsOpen,
            0x23 => Self::LogfsRead,
            0x24 => Self::LogfsCrc,
            0x25 => Self::LogfsClose,
            0x27 => Self::LogfsFinalize,
            0x30 => Self::LogStreamStart,
            0x31 => Self::LogStreamStop,
            0x32 => Self::LiveDataStart,
            0x33 => Self::LiveDataStop,
            0x40 => Self::DtcRead,
            0x41 => Self::DtcClear,
            0x50 => Self::ObRead,
            0x51 => Self::ObApplyWrp,
            0x60 => Self::Reset,
            0x61 => Self::Jump,
            0x80 => Self::NvmRead,
            0x81 => Self::NvmWrite,
            0x82 => Self::NvmFormat,
            _ => return Err(ParseError::Invalid("unknown command opcode")),
        })
    }
}

// ---- Notify opcodes (device → host, unsolicited) ----

/// The four unsolicited-notification opcodes the device sends.
/// `TYPE = NOTIFY` in the frame ID; the first payload byte is one of
/// these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NotifyOpcode {
    Heartbeat = 0xF0,
    Dtc = 0xF1,
    Log = 0xF2,
    LiveData = 0xF3,
}

impl NotifyOpcode {
    pub fn as_byte(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for NotifyOpcode {
    type Error = ParseError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Ok(match value {
            0xF0 => Self::Heartbeat,
            0xF1 => Self::Dtc,
            0xF2 => Self::Log,
            0xF3 => Self::LiveData,
            _ => return Err(ParseError::Invalid("unknown notify opcode")),
        })
    }
}

// ---- NACK codes (device → host, typed error) ----

/// Every NACK code the bootloader emits (0.2 contract), plus an
/// [`Unknown`] fallback so forward-compat with a newer bootloader
/// doesn't crash the flasher — the host still gets a readable
/// "NACK 0x?? (unknown)" line in its logs.
///
/// Byte values match `bl_proto.h`. `0x05` (replay counter low) is
/// allocated but unreachable on the current contract — it belongs to the
/// deferred Phase-5 surface.
///
/// **Namespace note:** `0x04` was also pencilled in for Phase-5
/// (signature invalid), but IFS08-CE-AMS#406 has since assigned it to
/// LOGFS `FILE_NOT_FOUND`. That is safe today because LOGFS rides
/// `MessageType::AppCtrl` (`0x06`) rather than the bootloader's `Cmd`
/// namespace — but if Phase-5 ever ships, `0x04` must be disambiguated
/// by message type, not by value alone.
///
/// [`Unknown`]: NackCode::Unknown
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NackCode {
    ProtectedAddr,
    OutOfBounds,
    CrcMismatch,
    BadSession,
    FlashHw,
    Busy,
    TransportTimeout,
    TransportError,
    ProtocolVersion,
    NoValidApp,
    NvmNotFound,
    NvmFull,
    ObWrongToken,
    /// Returned by `CMD_NVM_FORMAT` when the 4-byte confirmation
    /// token at the start of the command payload doesn't match
    /// `BL_NVM_FORMAT_TOKEN`. Same shape as `ObWrongToken` (the
    /// option-byte equivalent) — guards a destructive operation
    /// from accidental dispatch.
    NvmWrongToken,
    /// LOGFS group (#506, table finalised in IFS08-CE-AMS#406). Note
    /// `BAD_HANDLE` is **0x11**, not `0x08` — `0x08` stays `Busy`, which
    /// LOGFS reuses. `OUT_OF_BOUNDS` is likewise the existing `0x02`.
    FileNotFound,
    BadHandle,
    NoSdCard,
    FsError,
    ReadError,
    Unsupported,
    Unknown(u8),
}

impl NackCode {
    /// Byte value as seen on the wire.
    pub fn as_byte(self) -> u8 {
        match self {
            Self::ProtectedAddr => 0x01,
            Self::OutOfBounds => 0x02,
            Self::CrcMismatch => 0x03,
            Self::BadSession => 0x06,
            Self::FlashHw => 0x07,
            Self::Busy => 0x08,
            Self::TransportTimeout => 0x09,
            Self::TransportError => 0x0A,
            Self::ProtocolVersion => 0x0B,
            Self::NoValidApp => 0x0C,
            Self::NvmNotFound => 0x0D,
            Self::NvmFull => 0x0E,
            Self::ObWrongToken => 0x0F,
            Self::NvmWrongToken => 0x10,
            Self::FileNotFound => 0x04,
            Self::BadHandle => 0x11,
            Self::NoSdCard => 0x12,
            Self::FsError => 0x13,
            Self::ReadError => 0x14,
            Self::Unsupported => 0xFE,
            Self::Unknown(byte) => byte,
        }
    }

    /// Parse a NACK byte, accepting unknown values as `Unknown(byte)`.
    /// Total: never fails.
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            0x01 => Self::ProtectedAddr,
            0x02 => Self::OutOfBounds,
            0x03 => Self::CrcMismatch,
            0x06 => Self::BadSession,
            0x07 => Self::FlashHw,
            0x08 => Self::Busy,
            0x09 => Self::TransportTimeout,
            0x0A => Self::TransportError,
            0x0B => Self::ProtocolVersion,
            0x0C => Self::NoValidApp,
            0x0D => Self::NvmNotFound,
            0x0E => Self::NvmFull,
            0x0F => Self::ObWrongToken,
            0x10 => Self::NvmWrongToken,
            0x04 => Self::FileNotFound,
            0x11 => Self::BadHandle,
            0x12 => Self::NoSdCard,
            0x13 => Self::FsError,
            0x14 => Self::ReadError,
            0xFE => Self::Unsupported,
            other => Self::Unknown(other),
        }
    }

    /// Human-readable short name used for log / error messages. The
    /// `Display` impl below forwards to this.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ProtectedAddr => "PROTECTED_ADDR",
            Self::OutOfBounds => "OUT_OF_BOUNDS",
            Self::CrcMismatch => "CRC_MISMATCH",
            Self::BadSession => "BAD_SESSION",
            Self::FlashHw => "FLASH_HW",
            Self::Busy => "BUSY",
            Self::TransportTimeout => "TRANSPORT_TIMEOUT",
            Self::TransportError => "TRANSPORT_ERROR",
            Self::ProtocolVersion => "PROTOCOL_VERSION",
            Self::NoValidApp => "NO_VALID_APP",
            Self::NvmNotFound => "NVM_NOT_FOUND",
            Self::NvmFull => "NVM_FULL",
            Self::ObWrongToken => "OB_WRONG_TOKEN",
            Self::NvmWrongToken => "NVM_WRONG_TOKEN",
            Self::FileNotFound => "FILE_NOT_FOUND",
            Self::BadHandle => "BAD_HANDLE",
            Self::NoSdCard => "NO_SD_CARD",
            Self::FsError => "FS_ERROR",
            Self::ReadError => "READ_ERROR",
            Self::Unsupported => "UNSUPPORTED",
            Self::Unknown(_) => "UNKNOWN",
        }
    }
}

impl std::fmt::Display for NackCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(byte) => write!(f, "UNKNOWN(0x{byte:02X})"),
            other => write!(f, "{} (0x{:02X})", other.as_str(), other.as_byte()),
        }
    }
}

// ---- Reset modes (argument to CMD_RESET) ----

/// Reset mode passed as the single argument byte to `CMD_RESET`.
///
/// Variants and values match `handle_reset` in the bootloader:
/// modes 0 and 1 both call `NVIC_SystemReset`, mode 2 sets the
/// RTC BKP0R boot-request magic first, mode 3 jumps directly to
/// the installed application without a reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ResetMode {
    Hard = 0,
    Soft = 1,
    Bootloader = 2,
    App = 3,
}

impl ResetMode {
    pub fn as_byte(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for ResetMode {
    type Error = ParseError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Self::Hard,
            1 => Self::Soft,
            2 => Self::Bootloader,
            3 => Self::App,
            _ => return Err(ParseError::Invalid("reset mode out of range (0..=3)")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_opcode_roundtrip() {
        for raw in 0u8..=0xFF {
            if let Ok(op) = CommandOpcode::try_from(raw) {
                assert_eq!(op.as_byte(), raw, "opcode {op:?} roundtrip");
            }
        }
        // Spot-check the ones we definitely know.
        assert_eq!(CommandOpcode::Connect.as_byte(), 0x01);
        assert_eq!(CommandOpcode::ObApplyWrp.as_byte(), 0x51);
        assert_eq!(CommandOpcode::NvmWrite.as_byte(), 0x81);
    }

    #[test]
    fn command_opcode_rejects_unknown() {
        // 0x00 is not a valid command; 0x20 (CMD_MEM_READ from the
        // earlier draft) is gone in v1.0.0.
        assert!(CommandOpcode::try_from(0x00).is_err());
        assert!(CommandOpcode::try_from(0x20).is_err());
        assert!(CommandOpcode::try_from(0xFF).is_err());
    }

    #[test]
    fn notify_opcode_roundtrip() {
        for op in [
            NotifyOpcode::Heartbeat,
            NotifyOpcode::Dtc,
            NotifyOpcode::Log,
            NotifyOpcode::LiveData,
        ] {
            assert_eq!(NotifyOpcode::try_from(op.as_byte()).unwrap(), op);
        }
        assert!(NotifyOpcode::try_from(0x00).is_err());
        assert!(NotifyOpcode::try_from(0xEF).is_err());
        assert!(NotifyOpcode::try_from(0xF4).is_err());
    }

    #[test]
    fn nack_code_roundtrip_known() {
        let known = [
            (NackCode::ProtectedAddr, 0x01),
            (NackCode::OutOfBounds, 0x02),
            (NackCode::CrcMismatch, 0x03),
            (NackCode::BadSession, 0x06),
            (NackCode::FlashHw, 0x07),
            (NackCode::Busy, 0x08),
            (NackCode::TransportTimeout, 0x09),
            (NackCode::TransportError, 0x0A),
            (NackCode::ProtocolVersion, 0x0B),
            (NackCode::NoValidApp, 0x0C),
            (NackCode::NvmNotFound, 0x0D),
            (NackCode::NvmFull, 0x0E),
            (NackCode::ObWrongToken, 0x0F),
            (NackCode::Unsupported, 0xFE),
        ];
        for (code, byte) in known {
            assert_eq!(code.as_byte(), byte);
            assert_eq!(NackCode::from_byte(byte), code);
        }
    }

    #[test]
    fn nack_code_unknown_falls_through() {
        // 0x05 is still reserved for Phase-5 scope and never emitted, so
        // it parses as Unknown. (0x04 used to sit here too, but
        // IFS08-CE-AMS#406 assigned it to LOGFS FILE_NOT_FOUND — see the
        // namespace note on NackCode.)
        assert_eq!(NackCode::from_byte(0x05), NackCode::Unknown(0x05));
        assert_eq!(NackCode::from_byte(0x15), NackCode::Unknown(0x15));
        assert_eq!(NackCode::from_byte(0xA5), NackCode::Unknown(0xA5));
    }

    #[test]
    fn logfs_nack_codes_match_the_final_firmware_table() {
        // IFS08-CE-AMS#406. BAD_HANDLE is 0x11 — NOT 0x08, which stays
        // Busy and is reused by LOGFS.
        assert_eq!(NackCode::from_byte(0x04), NackCode::FileNotFound);
        assert_eq!(NackCode::from_byte(0x11), NackCode::BadHandle);
        assert_eq!(NackCode::from_byte(0x12), NackCode::NoSdCard);
        assert_eq!(NackCode::from_byte(0x13), NackCode::FsError);
        assert_eq!(NackCode::from_byte(0x14), NackCode::ReadError);
        assert_eq!(NackCode::from_byte(0x08), NackCode::Busy);
        assert_eq!(NackCode::from_byte(0x02), NackCode::OutOfBounds);
        // round-trip
        for c in [
            NackCode::FileNotFound,
            NackCode::BadHandle,
            NackCode::NoSdCard,
            NackCode::FsError,
            NackCode::ReadError,
        ] {
            assert_eq!(NackCode::from_byte(c.as_byte()), c);
        }
    }

    #[test]
    fn nack_code_display_is_useful() {
        assert_eq!(
            format!("{}", NackCode::ProtectedAddr),
            "PROTECTED_ADDR (0x01)"
        );
        assert_eq!(format!("{}", NackCode::Unknown(0xAB)), "UNKNOWN(0xAB)");
    }

    #[test]
    fn reset_mode_roundtrip() {
        for m in [
            ResetMode::Hard,
            ResetMode::Soft,
            ResetMode::Bootloader,
            ResetMode::App,
        ] {
            assert_eq!(ResetMode::try_from(m.as_byte()).unwrap(), m);
        }
        assert!(ResetMode::try_from(4).is_err());
        assert!(ResetMode::try_from(255).is_err());
    }
}
