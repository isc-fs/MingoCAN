//! Operator pedal-calibration protocol (`0x7E2`..=`0x7E5`).
//!
//! Lets a pit operator calibrate the APPS and brake endpoints over CAN with
//! no rebuild and no reflash (IFS08-CE-ECU #169, can-flasher #534). Until
//! this existed, the four APPS endpoints and three brake thresholds were
//! compile-time `constexpr` in `ecu_config.hpp`: only someone with a
//! toolchain could change them, so in practice they did not get changed —
//! `BrakePressedRaw` and `BrakeDvHardRaw` sat on IFS06 placeholders while
//! gating the EV.2.3 torque cut and the driverless R2D respectively.
//!
//! ## Relationship to pit-diag
//!
//! Separate protocol, same bus. This module **writes** to the ECU, whereas
//! [`crate::pit_diag`] only observes — which is why it lives apart rather
//! than as another profile there.
//!
//! It is not self-sufficient: the operator needs live pedal values to know
//! when to capture, and those come from `0x701 PitDiag_pedals`. So the
//! pit-diag stream must be **armed** (`0x7E0`) alongside a calibration
//! session. The ECU enforces nothing here; the client must arrange it.
//!
//! ## Wire protocol
//!
//! Source of truth: `Core/Inc/can/messages/pit_cal_*.def` plus
//! `Core/Inc/app/cal_session.hpp` and `pedal_cal.hpp` in the ECU repo.
//!
//! - **`0x7E2` command** (host → ECU): byte 0 [`CalCommand`], byte 1 the
//!   [`CalPoint`] for `Capture`, bytes 2-3 reserved, bytes 4-7 a
//!   big-endian guard word whose meaning depends on the command — see
//!   [`build_command`].
//! - **`0x7E3` status** (ECU → host): emitted immediately in response to
//!   every command, and at 100 ms while a session is open.
//! - **`0x7E4` / `0x7E5`** (ECU → host): the APPS and brake value sets,
//!   emitted as a pair after `ReadStored` or `ReadStaged`.
//!
//! ## The read-back ambiguity
//!
//! `0x7E4`/`0x7E5` carry four `u16` each and are therefore full — neither
//! frame can say whether it is the *stored* or the *staged* set. The only
//! discriminator is [`CalStatusFrame::last_cmd`] on the preceding `0x7E3`.
//! A client that shows stored and staged side by side (which the review
//! step must) has to issue the two reads in sequence and attribute each
//! pair by the status frame that preceded it. [`ReadKind`] exists to make
//! that attribution explicit at the type level rather than implicit in
//! call ordering.
//!
//! This was raised on IFS08-CE-ECU#169 — one spare bit in `0x7E3` would
//! remove the inference entirely — and `cal_load` subsequently took byte 5,
//! leaving bytes 6-7 free. Until that changes, correlate carefully.

use crate::protocol::CanFrame;

// ---- Wire-level constants ----------------------------------------

/// `0x7E2` — host → ECU calibration command.
pub const CAL_CMD_ID: u16 = 0x7E2;
/// `0x7E3` — ECU → host session status.
pub const CAL_STATUS_ID: u16 = 0x7E3;
/// `0x7E4` — ECU → host APPS value set.
pub const CAL_APPS_ID: u16 = 0x7E4;
/// `0x7E5` — ECU → host brake value set.
pub const CAL_BRAKE_ID: u16 = 0x7E5;

/// Guard word for the commands that open a session or destroy a
/// calibration (`Enter`, `ResetDefaults`). Stops a stray frame on a bus
/// shared with the AMS and the uDV from doing either.
///
/// `Commit` deliberately does **not** use it — see [`build_command`].
pub const CAL_GUARD_MAGIC: u32 = 0xCA11_B0DE;

/// The ECU abandons a session after this long with no command, discarding
/// everything staged. A client must not rely on it: send
/// [`CalCommand::Abort`] when the operator navigates away.
pub const CAL_SESSION_TIMEOUT_MS: u32 = 30_000;

/// Length of the canonical calibration record the CRC covers.
const CAL_RECORD_LEN: usize = 18;
/// Version byte at the head of that record.
const CAL_RECORD_VERSION: u8 = 1;

/// Highest raw value a 12-bit ADC reading can take.
pub const CAL_ADC_MAX: u16 = 4095;

// ---- Commands ----------------------------------------------------

/// A command the host can send on `0x7E2`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CalCommand {
    /// Ask for a status frame without changing anything.
    Poll = 0,
    /// Open a calibration session. Guarded.
    Enter = 1,
    /// Sample the [`CalPoint`] in `arg` into the staged set.
    Capture = 2,
    /// Emit the committed values on `0x7E4`/`0x7E5`.
    ReadStored = 3,
    /// Emit the in-session staged values on `0x7E4`/`0x7E5`.
    ReadStaged = 4,
    /// Validate and write the staged set to NVM. Carries a CRC, not the
    /// magic — see [`build_command`].
    Commit = 5,
    /// Discard the staged set and close the session.
    Abort = 6,
    /// Restore compile-time defaults and commit them. Guarded, and
    /// destructive — surface it well away from the normal flow.
    ResetDefaults = 7,
}

impl CalCommand {
    /// Raw wire value.
    #[must_use]
    pub fn as_byte(self) -> u8 {
        self as u8
    }

    /// `true` when this command must carry [`CAL_GUARD_MAGIC`].
    #[must_use]
    pub fn needs_guard_magic(self) -> bool {
        matches!(self, Self::Enter | Self::ResetDefaults)
    }
}

/// A point the operator captures, sent in `arg` alongside
/// [`CalCommand::Capture`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CalPoint {
    /// Accelerator fully released. Captures both APPS channels at once —
    /// they are two sensors on one pedal.
    AppsRest = 1,
    /// Accelerator held against the mechanical stop. Both channels.
    AppsFull = 2,
    /// Brake fully released.
    BrakeRest = 3,
    /// Brake pressed firmly — the top of the usable span.
    BrakePressed = 4,
    /// Accelerator held at roughly half travel.
    ///
    /// The one point that is not an endpoint, and the only place the
    /// T.11.8.9 failure mode is actually measured. Endpoints cannot catch
    /// it: both channels normalise to 0..100 % by construction, so they
    /// agree at rest and full travel however badly they diverge in
    /// between, and mid-travel divergence from sensor non-linearity is
    /// exactly what the disagreement cut exists to detect.
    AppsMid = 5,
}

impl CalPoint {
    /// Raw wire value.
    #[must_use]
    pub fn as_byte(self) -> u8 {
        self as u8
    }

    /// Bit this point sets in [`CalStatusFrame::captured_mask`].
    #[must_use]
    pub fn mask_bit(self) -> u8 {
        match self {
            Self::AppsRest => 1 << 0,
            Self::AppsFull => 1 << 1,
            Self::BrakeRest => 1 << 2,
            Self::BrakePressed => 1 << 3,
            Self::AppsMid => 1 << 4,
        }
    }

    /// Every point, in the order an operator works through them.
    #[must_use]
    pub fn all() -> [Self; 5] {
        [
            Self::AppsRest,
            Self::AppsFull,
            Self::AppsMid,
            Self::BrakeRest,
            Self::BrakePressed,
        ]
    }

    /// What the operator has to physically do. The wizard shows this;
    /// it is deliberately phrased for someone who is not a firmware
    /// engineer.
    #[must_use]
    pub fn instruction(self) -> &'static str {
        match self {
            Self::AppsRest => "Release the accelerator fully and let the reading settle.",
            Self::AppsFull => "Press the accelerator to the mechanical stop and hold it.",
            Self::AppsMid => "Hold the accelerator at roughly half travel and keep it steady.",
            Self::BrakeRest => "Release the brake fully and let the reading settle.",
            Self::BrakePressed => "Press the brake firmly, as in hard braking, and hold it.",
        }
    }
}

/// Mask value meaning every capture point has been taken.
pub const CAL_ALL_POINTS_MASK: u8 = 0x1F;

// ---- Status enums ------------------------------------------------

/// Session state (`0x7E3` byte 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalSessionState {
    /// No session open.
    Idle,
    /// Session open, capturing.
    Active,
    /// Staged set passed validation.
    Validated,
    /// Write in progress.
    Committing,
    /// Write completed and verified.
    Committed,
    /// Something failed; `result` says what.
    Error,
    /// Outside the known table.
    Unknown(u8),
}

impl CalSessionState {
    /// Decode the raw state byte.
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Idle,
            1 => Self::Active,
            2 => Self::Validated,
            3 => Self::Committing,
            4 => Self::Committed,
            5 => Self::Error,
            other => Self::Unknown(other),
        }
    }
}

/// Result of the last command (`0x7E3` byte 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalResult {
    /// Command accepted.
    Ok,
    /// Guard word missing or wrong.
    BadGuard,
    /// Command needs an open session.
    NotInSession,
    /// Entry conditions not met.
    VehicleNotSafe,
    /// The signal was still moving when captured.
    SampleUnstable,
    /// `Commit` before every point was captured.
    MissingPoints,
    /// See [`CalStatusFrame::validation_flags`].
    ValidationFailed,
    /// The storage write did not verify.
    NvmWriteFailed,
    /// Unrecognised command byte.
    UnknownCmd,
    /// Outside the known table.
    Unknown(u8),
}

impl CalResult {
    /// Decode the raw result byte.
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Ok,
            1 => Self::BadGuard,
            2 => Self::NotInSession,
            3 => Self::VehicleNotSafe,
            4 => Self::SampleUnstable,
            5 => Self::MissingPoints,
            6 => Self::ValidationFailed,
            7 => Self::NvmWriteFailed,
            8 => Self::UnknownCmd,
            other => Self::Unknown(other),
        }
    }

    /// A sentence an operator can act on. #534 requires that no result
    /// code ever reaches the UI as a bare number.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Ok => "Accepted.",
            Self::BadGuard => {
                "The ECU rejected the safety word on that command. This is a client bug, \
                 not something the operator can fix — do not retry blindly."
            }
            Self::NotInSession => {
                "No calibration session is open. It may have timed out after 30 s of \
                 inactivity, in which case anything captured has been discarded and the \
                 stored calibration is untouched. Start again."
            }
            Self::VehicleNotSafe => {
                "The ECU refused to open a session because the car is not in a safe state. \
                 It requires the tractive system inactive, the control FSM out of Active, \
                 zero motor rpm and zero commanded torque."
            }
            Self::SampleUnstable => {
                "The reading was still moving when captured. Hold the pedal steady and \
                 wait for the live value to settle before capturing again."
            }
            Self::MissingPoints => {
                "Not every capture point has been taken yet. All five are required before \
                 committing."
            }
            Self::ValidationFailed => {
                "The ECU rejected the calibration and wrote nothing — the car is \
                 unchanged. Either a safety check failed (the flags say which), or \
                 the consistency CRC did not match, meaning this tool and the ECU \
                 disagree about what was captured. An empty flag set points at the \
                 CRC: abort and capture again rather than retrying the commit."
            }
            Self::NvmWriteFailed => {
                "The ECU could not write the calibration to flash, or wrote it and failed \
                 to read it back. The previous calibration is still in force. If this \
                 repeats, the storage region may be full and need compaction."
            }
            Self::UnknownCmd => {
                "The ECU did not recognise the command. The tool and the firmware are \
                 probably out of step — check the ECU firmware version."
            }
            Self::Unknown(_) => {
                "The ECU returned a result code this tool does not recognise. It is newer \
                 than this build; check the ECU firmware version."
            }
        }
    }

    /// `true` for anything other than [`CalResult::Ok`].
    #[must_use]
    pub fn is_error(self) -> bool {
        !matches!(self, Self::Ok)
    }
}

/// Where the ECU's running calibration came from (`0x7E3` byte 5). Mirrors
/// the `cal_status` field on the ungated `0x704` health frame — see
/// [`crate::pit_diag::ecu::EcuCalStatus`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalLoad {
    /// No stored calibration; running compile-time defaults.
    Defaults,
    /// A stored calibration was read and applied.
    Loaded,
    /// One was stored but failed validation; defaults are in force.
    InvalidFellBack,
    /// One was stored with an unrecognised record version; defaults are in
    /// force.
    BadVersionFellBack,
    /// Outside the known table.
    Unknown(u8),
}

impl CalLoad {
    /// Decode the raw byte.
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Defaults,
            1 => Self::Loaded,
            2 => Self::InvalidFellBack,
            3 => Self::BadVersionFellBack,
            other => Self::Unknown(other),
        }
    }

    /// `true` unless a stored calibration is actually in force. Two of the
    /// four values mean "found one, rejected it, running defaults", so
    /// treating anything-but-`Defaults` as success would be wrong.
    #[must_use]
    pub fn is_fallback(self) -> bool {
        !matches!(self, Self::Loaded)
    }
}

// ---- Validation flags --------------------------------------------

/// APPS1 travel span below the minimum.
pub const CAL_FLAG_APPS1_SPAN_TOO_SMALL: u8 = 1 << 0;
/// APPS2 travel span below the minimum.
pub const CAL_FLAG_APPS2_SPAN_TOO_SMALL: u8 = 1 << 1;
/// The two APPS channels disagree — either their spans are wildly
/// different, or (when `AppsMid` was captured) they read different
/// percentages at half travel.
pub const CAL_FLAG_APPS_SPAN_MISMATCH: u8 = 1 << 2;
/// An APPS maximum is at or below its minimum.
pub const CAL_FLAG_APPS_NOT_MONOTONIC: u8 = 1 << 3;
/// Brake travel span below the minimum.
pub const CAL_FLAG_BRAKE_SPAN_TOO_SMALL: u8 = 1 << 4;
/// Brake thresholds out of order.
pub const CAL_FLAG_BRAKE_ORDER: u8 = 1 << 5;
/// A value sits outside the 12-bit ADC range.
pub const CAL_FLAG_OUT_OF_ADC_RANGE: u8 = 1 << 6;

/// Explain a `validation_flags` bitmask in plain language, one sentence
/// per failed rule. Empty when the mask is zero.
///
/// #534 requires `validation_flags` never to be surfaced as a hex number,
/// so this is the only sanctioned rendering.
#[must_use]
pub fn validation_messages(flags: u8) -> Vec<&'static str> {
    let mut v = Vec::new();
    if flags & CAL_FLAG_APPS1_SPAN_TOO_SMALL != 0 {
        v.push(
            "Accelerator channel 1 barely moved between released and full travel. \
             The pedal was probably not pressed all the way, or that sensor is not moving.",
        );
    }
    if flags & CAL_FLAG_APPS2_SPAN_TOO_SMALL != 0 {
        v.push(
            "Accelerator channel 2 barely moved between released and full travel. \
             The pedal was probably not pressed all the way, or that sensor is not moving.",
        );
    }
    if flags & CAL_FLAG_APPS_SPAN_MISMATCH != 0 {
        v.push(
            "The two accelerator channels do not agree with each other. Committing this \
             would either trip the plausibility cut constantly or defeat it — check both \
             sensors are connected and moving together over the whole travel.",
        );
    }
    if flags & CAL_FLAG_APPS_NOT_MONOTONIC != 0 {
        v.push(
            "An accelerator channel reads no higher at full travel than at rest. The \
             rest and full captures were probably taken the wrong way round, or a \
             sensor is disconnected.",
        );
    }
    if flags & CAL_FLAG_BRAKE_SPAN_TOO_SMALL != 0 {
        v.push(
            "The brake barely moved between released and pressed. Press the pedal firmly \
             during the pressed capture.",
        );
    }
    if flags & CAL_FLAG_BRAKE_ORDER != 0 {
        v.push(
            "The derived brake thresholds came out in the wrong order — the arming point \
             must sit below the driverless hard-braking point, which must sit below the \
             pressed point. Re-capture the brake with a firmer press.",
        );
    }
    if flags & CAL_FLAG_OUT_OF_ADC_RANGE != 0 {
        v.push("A captured value is outside the valid 0-4095 range for a 12-bit reading.");
    }
    v
}

// ---- Frame records -----------------------------------------------

/// `0x7E3` — calibration session status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalStatusFrame {
    /// Where the session is.
    pub session_state: CalSessionState,
    /// Echo of the last command byte the ECU processed. The **only**
    /// discriminator for whether a following `0x7E4`/`0x7E5` pair is the
    /// stored or the staged set — see the module docs.
    pub last_cmd: u8,
    /// Outcome of that command.
    pub result: CalResult,
    /// Bit per capture point taken; [`CAL_ALL_POINTS_MASK`] when complete.
    pub captured_mask: u8,
    /// Meaningful when `result` is [`CalResult::ValidationFailed`] —
    /// render via [`validation_messages`].
    pub validation_flags: u8,
    /// Provenance of the calibration currently in force.
    pub cal_load: CalLoad,
}

impl CalStatusFrame {
    /// Which read the next `0x7E4`/`0x7E5` pair answers, if this status
    /// frame was the reply to a read command.
    #[must_use]
    pub fn pending_read(&self) -> Option<ReadKind> {
        match self.last_cmd {
            x if x == CalCommand::ReadStored.as_byte() => Some(ReadKind::Stored),
            x if x == CalCommand::ReadStaged.as_byte() => Some(ReadKind::Staged),
            _ => None,
        }
    }

    /// `true` once every capture point has been taken.
    #[must_use]
    pub fn all_points_captured(&self) -> bool {
        self.captured_mask & CAL_ALL_POINTS_MASK == CAL_ALL_POINTS_MASK
    }

    /// Points still outstanding, in operator order.
    #[must_use]
    pub fn missing_points(&self) -> Vec<CalPoint> {
        CalPoint::all()
            .into_iter()
            .filter(|p| self.captured_mask & p.mask_bit() == 0)
            .collect()
    }
}

/// Whether a value read-back is the committed set or the staged one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadKind {
    /// The calibration currently written to NVM.
    Stored,
    /// The set captured in the open session, not yet committed.
    Staged,
}

/// `0x7E4` — the four APPS endpoints, big-endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CalAppsFrame {
    /// APPS1 raw reading at rest.
    pub apps1_min: u16,
    /// APPS1 raw reading at full travel.
    pub apps1_max: u16,
    /// APPS2 raw reading at rest.
    pub apps2_min: u16,
    /// APPS2 raw reading at full travel.
    pub apps2_max: u16,
}

/// `0x7E5` — the brake rest point and the three derived thresholds,
/// big-endian.
///
/// Only `brake_rest` and `brake_pressed` are captured; `brake_arm` and
/// `brake_dv_hard` are **derived by the ECU**. The client must display
/// what a read-back returns rather than recomputing them — a second
/// implementation of the derivation would drift from the firmware's, which
/// is the failure mode #528 was about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CalBrakeFrame {
    /// Raw reading with the brake released.
    pub brake_rest: u16,
    /// Threshold at which the brake counts as pressed for R2D arming.
    pub brake_arm: u16,
    /// Threshold the driverless R2D requires, broadcast to the uDV on
    /// `0x505` as this ECU's EBS verdict.
    pub brake_dv_hard: u16,
    /// Threshold at which EV.2.3 treats the brake as pressed.
    pub brake_pressed: u16,
}

/// A decoded calibration frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalFrame {
    /// `0x7E3` — session status.
    Status(CalStatusFrame),
    /// `0x7E4` — APPS values.
    Apps(CalAppsFrame),
    /// `0x7E5` — brake values.
    Brake(CalBrakeFrame),
}

// ---- CRC ---------------------------------------------------------

/// CRC-32 over the ECU's canonical 18-byte calibration record.
///
/// Mirrors `cal_crc32()` in `Core/Src/app/pedal_cal_nvm.cpp`: the record is
/// a version byte, a zero pad, then the eight `u16` values big-endian in a
/// fixed order; the CRC is CRC-32/ISO-HDLC (reflected polynomial
/// `0xEDB88320`, init and final-xor `0xFFFFFFFF`) — the same variant the
/// bootloader uses.
///
/// This is what [`CalCommand::Commit`] carries. The ECU recomputes it over
/// its own staged set and rejects a mismatch, so a client that has lost
/// track of the session cannot commit values it does not actually hold.
#[must_use]
pub fn cal_crc32(apps: &CalAppsFrame, brake: &CalBrakeFrame) -> u32 {
    let mut rec = [0u8; CAL_RECORD_LEN];
    rec[0] = CAL_RECORD_VERSION;
    rec[1] = 0;
    let values = [
        apps.apps1_min,
        apps.apps1_max,
        apps.apps2_min,
        apps.apps2_max,
        brake.brake_rest,
        brake.brake_arm,
        brake.brake_dv_hard,
        brake.brake_pressed,
    ];
    for (i, v) in values.iter().enumerate() {
        rec[2 + i * 2] = (v >> 8) as u8;
        rec[2 + i * 2 + 1] = (v & 0xFF) as u8;
    }

    let mut crc: u32 = 0xFFFF_FFFF;
    for byte in rec {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc ^ 0xFFFF_FFFF
}

// ---- Encode / decode ---------------------------------------------

/// Build a `0x7E2` command frame.
///
/// The guard word in bytes 4-7 is command-dependent, and getting it wrong
/// is not a silent failure — the ECU answers [`CalResult::BadGuard`]:
///
/// - [`CalCommand::Enter`] and [`CalCommand::ResetDefaults`] carry
///   [`CAL_GUARD_MAGIC`], so a stray frame cannot open a session or wipe a
///   calibration.
/// - [`CalCommand::Commit`] carries [`cal_crc32`] of the staged set
///   instead. Guard and consistency check are the same field. Note a
///   mismatch answers [`CalResult::ValidationFailed`] with **empty**
///   `validation_flags`, not `BadGuard` — the firmware reuses the
///   validation path for it.
/// - Everything else ignores it; this sends zero.
///
/// `staged` is required for `Commit` and ignored otherwise. Passing `None`
/// for a `Commit` is a programming error and yields a frame the ECU will
/// reject rather than a panic — but callers should not rely on that.
#[must_use]
pub fn build_command(
    cmd: CalCommand,
    point: Option<CalPoint>,
    staged: Option<(&CalAppsFrame, &CalBrakeFrame)>,
) -> CanFrame {
    let guard: u32 = if cmd.needs_guard_magic() {
        CAL_GUARD_MAGIC
    } else if cmd == CalCommand::Commit {
        staged.map_or(0, |(a, b)| cal_crc32(a, b))
    } else {
        0
    };

    let g = guard.to_be_bytes();
    let payload = [
        cmd.as_byte(),
        point.map_or(0, CalPoint::as_byte),
        0,
        0,
        g[0],
        g[1],
        g[2],
        g[3],
    ];
    CanFrame::new(CAL_CMD_ID, &payload).expect("8-byte payload always fits")
}

/// Decode a raw CAN frame into a calibration record.
///
/// Returns `None` if the ID is not part of this protocol, or the payload
/// is too short.
#[must_use]
pub fn decode_frame(frame: &CanFrame) -> Option<CalFrame> {
    let p = frame.payload();
    let be = |lo: usize| u16::from_be_bytes([p[lo], p[lo + 1]]);

    match frame.id {
        CAL_STATUS_ID => {
            if p.len() < 6 {
                return None;
            }
            Some(CalFrame::Status(CalStatusFrame {
                session_state: CalSessionState::from_byte(p[0]),
                last_cmd: p[1],
                result: CalResult::from_byte(p[2]),
                captured_mask: p[3],
                validation_flags: p[4],
                cal_load: CalLoad::from_byte(p[5]),
            }))
        }
        CAL_APPS_ID => {
            if p.len() < 8 {
                return None;
            }
            Some(CalFrame::Apps(CalAppsFrame {
                apps1_min: be(0),
                apps1_max: be(2),
                apps2_min: be(4),
                apps2_max: be(6),
            }))
        }
        CAL_BRAKE_ID => {
            if p.len() < 8 {
                return None;
            }
            Some(CalFrame::Brake(CalBrakeFrame {
                brake_rest: be(0),
                brake_arm: be(2),
                brake_dv_hard: be(4),
                brake_pressed: be(6),
            }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_and_reset_carry_the_magic() {
        for cmd in [CalCommand::Enter, CalCommand::ResetDefaults] {
            let f = build_command(cmd, None, None);
            assert_eq!(f.id, CAL_CMD_ID);
            let p = f.payload();
            assert_eq!(p[0], cmd.as_byte());
            assert_eq!(
                u32::from_be_bytes([p[4], p[5], p[6], p[7]]),
                CAL_GUARD_MAGIC,
                "{cmd:?} must be guarded"
            );
        }
    }

    #[test]
    fn unguarded_commands_send_zero() {
        for cmd in [
            CalCommand::Poll,
            CalCommand::Capture,
            CalCommand::ReadStored,
            CalCommand::ReadStaged,
            CalCommand::Abort,
        ] {
            let f = build_command(cmd, None, None);
            let p = f.payload();
            assert_eq!(u32::from_be_bytes([p[4], p[5], p[6], p[7]]), 0, "{cmd:?}");
        }
    }

    #[test]
    fn capture_carries_the_point_in_arg() {
        let f = build_command(CalCommand::Capture, Some(CalPoint::AppsMid), None);
        let p = f.payload();
        assert_eq!(p[0], 2);
        assert_eq!(p[1], 5, "APPS_MID is arg 5");
    }

    /// The CRC contract with the firmware. Values are today's shipped
    /// calibration, so this vector is also a regression guard on the
    /// record serialisation (version byte, zero pad, 8x u16 big-endian).
    #[test]
    fn commit_carries_a_crc_of_the_staged_set() {
        let apps = CalAppsFrame {
            apps1_min: 2490,
            apps1_max: 3350,
            apps2_min: 2345,
            apps2_max: 3025,
        };
        let brake = CalBrakeFrame {
            brake_rest: 580,
            brake_arm: 750,
            brake_dv_hard: 2500,
            brake_pressed: 3000,
        };
        let crc = cal_crc32(&apps, &brake);

        let f = build_command(CalCommand::Commit, None, Some((&apps, &brake)));
        let p = f.payload();
        assert_eq!(
            u32::from_be_bytes([p[4], p[5], p[6], p[7]]),
            crc,
            "COMMIT carries the CRC, not the magic"
        );
        assert_ne!(crc, CAL_GUARD_MAGIC);

        // Any change to any field must change the CRC — that is the whole
        // point of it as a consistency check.
        let mut other = apps;
        other.apps1_min += 1;
        assert_ne!(cal_crc32(&other, &brake), crc);
        let mut other_brake = brake;
        other_brake.brake_pressed -= 1;
        assert_ne!(cal_crc32(&apps, &other_brake), crc);
    }

    #[test]
    fn crc_matches_the_firmware_implementation() {
        // Reference values produced by compiling the ECU's own
        // `cal_crc32()` (Core/Src/app/pedal_cal_nvm.cpp @ origin/dev)
        // on the host and running it over these exact inputs. This is
        // the contract COMMIT is checked against — a mismatch means
        // every commit is rejected with BadGuard.
        let apps = CalAppsFrame {
            apps1_min: 2490,
            apps1_max: 3350,
            apps2_min: 2345,
            apps2_max: 3025,
        };
        let brake = CalBrakeFrame {
            brake_rest: 580,
            brake_arm: 750,
            brake_dv_hard: 2500,
            brake_pressed: 3000,
        };
        assert_eq!(cal_crc32(&apps, &brake), 1_833_724_637);
        assert_eq!(
            cal_crc32(&CalAppsFrame::default(), &CalBrakeFrame::default()),
            2_286_516_652
        );
    }

    /// Cross-check the CRC-32 variant itself against the standard
    /// "123456789" check value, so a wrong polynomial or missing final
    /// XOR is caught independently of the record layout.
    #[test]
    fn crc32_variant_is_iso_hdlc() {
        fn crc32(data: &[u8]) -> u32 {
            let mut crc: u32 = 0xFFFF_FFFF;
            for b in data {
                crc ^= u32::from(*b);
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        (crc >> 1) ^ 0xEDB8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            crc ^ 0xFFFF_FFFF
        }
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn status_decodes() {
        // Active, last_cmd=CAPTURE, OK, three points captured, no flags,
        // running a stored calibration.
        let p = [1, 2, 0, 0b0000_0111, 0, 1];
        match decode_frame(&CanFrame::new(CAL_STATUS_ID, &p).unwrap()).unwrap() {
            CalFrame::Status(s) => {
                assert_eq!(s.session_state, CalSessionState::Active);
                assert_eq!(s.last_cmd, CalCommand::Capture.as_byte());
                assert_eq!(s.result, CalResult::Ok);
                assert!(!s.result.is_error());
                assert_eq!(s.cal_load, CalLoad::Loaded);
                assert!(!s.cal_load.is_fallback());
                assert!(!s.all_points_captured());
                assert_eq!(
                    s.missing_points(),
                    vec![CalPoint::AppsMid, CalPoint::BrakePressed]
                );
                assert_eq!(s.pending_read(), None);
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn status_flags_a_complete_capture_set() {
        let p = [1, 2, 0, CAL_ALL_POINTS_MASK, 0, 0];
        match decode_frame(&CanFrame::new(CAL_STATUS_ID, &p).unwrap()).unwrap() {
            CalFrame::Status(s) => {
                assert!(s.all_points_captured());
                assert!(s.missing_points().is_empty());
                assert_eq!(s.cal_load, CalLoad::Defaults);
                assert!(s.cal_load.is_fallback());
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    /// The read-back ambiguity: the pair that follows can only be
    /// attributed via `last_cmd`.
    #[test]
    fn pending_read_distinguishes_stored_from_staged() {
        let mk = |last_cmd: u8| {
            let p = [1, last_cmd, 0, 0, 0, 1];
            match decode_frame(&CanFrame::new(CAL_STATUS_ID, &p).unwrap()).unwrap() {
                CalFrame::Status(s) => s,
                other => panic!("expected Status, got {other:?}"),
            }
        };
        assert_eq!(
            mk(CalCommand::ReadStored.as_byte()).pending_read(),
            Some(ReadKind::Stored)
        );
        assert_eq!(
            mk(CalCommand::ReadStaged.as_byte()).pending_read(),
            Some(ReadKind::Staged)
        );
        assert_eq!(mk(CalCommand::Poll.as_byte()).pending_read(), None);
    }

    #[test]
    fn apps_and_brake_decode_big_endian() {
        let p = [0x09, 0xBA, 0x0D, 0x16, 0x09, 0x29, 0x0B, 0xD1];
        match decode_frame(&CanFrame::new(CAL_APPS_ID, &p).unwrap()).unwrap() {
            CalFrame::Apps(a) => {
                assert_eq!(a.apps1_min, 2490);
                assert_eq!(a.apps1_max, 3350);
                assert_eq!(a.apps2_min, 2345);
                assert_eq!(a.apps2_max, 3025);
            }
            other => panic!("expected Apps, got {other:?}"),
        }
        let p = [0x02, 0x44, 0x02, 0xEE, 0x09, 0xC4, 0x0B, 0xB8];
        match decode_frame(&CanFrame::new(CAL_BRAKE_ID, &p).unwrap()).unwrap() {
            CalFrame::Brake(b) => {
                assert_eq!(b.brake_rest, 580);
                assert_eq!(b.brake_arm, 750);
                assert_eq!(b.brake_dv_hard, 2500);
                assert_eq!(b.brake_pressed, 3000);
            }
            other => panic!("expected Brake, got {other:?}"),
        }
    }

    #[test]
    fn short_frames_and_foreign_ids_reject() {
        assert!(decode_frame(&CanFrame::new(CAL_STATUS_ID, &[1, 2, 0, 0, 0]).unwrap()).is_none());
        assert!(decode_frame(&CanFrame::new(CAL_APPS_ID, &[0; 7]).unwrap()).is_none());
        assert!(decode_frame(&CanFrame::new(CAL_BRAKE_ID, &[0; 7]).unwrap()).is_none());
        // 0x7E0/0x7E1 are the pit-diag arm handshake, not ours.
        assert!(decode_frame(&CanFrame::new(0x7E0, &[0; 8]).unwrap()).is_none());
        assert!(decode_frame(&CanFrame::new(0x700, &[0; 8]).unwrap()).is_none());
    }

    #[test]
    fn every_result_code_has_a_message() {
        for raw in 0u8..=8 {
            let r = CalResult::from_byte(raw);
            assert!(!r.message().is_empty(), "{r:?}");
            assert_ne!(r.is_error(), raw == 0, "{r:?}");
        }
        // Forward-compat: an unknown code still explains itself rather
        // than reaching the operator as a number.
        assert!(CalResult::from_byte(200)
            .message()
            .contains("does not recognise"));
    }

    #[test]
    fn every_validation_flag_has_a_message() {
        assert!(validation_messages(0).is_empty());
        for bit in 0..7 {
            let msgs = validation_messages(1 << bit);
            assert_eq!(msgs.len(), 1, "bit {bit} must map to exactly one message");
        }
        // Several rules can fail at once.
        assert_eq!(
            validation_messages(
                CAL_FLAG_APPS1_SPAN_TOO_SMALL | CAL_FLAG_BRAKE_ORDER | CAL_FLAG_OUT_OF_ADC_RANGE
            )
            .len(),
            3
        );
    }

    #[test]
    fn capture_points_cover_the_mask() {
        let mut all = 0u8;
        for p in CalPoint::all() {
            assert!(!p.instruction().is_empty(), "{p:?}");
            all |= p.mask_bit();
        }
        assert_eq!(all, CAL_ALL_POINTS_MASK, "every point must be in the mask");
    }
}
