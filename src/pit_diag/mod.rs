//! AMS pit-diag observer protocol.
//!
//! When armed, the AMS emits 58 frames at 1 Hz carrying the full
//! diagnostic picture: every cell voltage, every NTC temperature,
//! plus a 9-frame diag block (FSM state, poll timing, cell
//! balancing, boot diag, crash post-mortem, firmware ID, per-IC PEC
//! counts). This module handles the arm/disarm handshake and decodes
//! the wire frames into typed records the rest of the crate (and the
//! Studio app) can consume.
//!
//! ## Wire protocol
//!
//! Source of truth: `docs/dbc/ams.dbc` in the AMS repo (IFS08-CE-AMS).
//! (`CAN_MAP.md` is the human-readable companion — but its table once
//! omitted `0x6C2..=0x6C6`, which is what misled catch-up PR #263
//! into a 53-frame undercount. Trust the DBC.)
//!
//! - **Enable**:  emit `0x7F0` with payload `DE AD BE EF`.
//! - **Disable**: emit `0x7F0` with payload `00 00 00 00`.
//! - **ACK**:     AMS replies on `0x7F1` with 1 byte —
//!   `0x01` = enabled, `0x00` = disabled.
//! - **Stream IDs once armed (58 frames / scan total)**:
//!   - `0x680..=0x697` (24 frames) — cell voltages, 4 cells/frame,
//!     big-endian `u16` millivolts. The last frame's 4th slot is a
//!     `0xFFFF` sentinel because 95 cells don't divide evenly by 4.
//!   - `0x6A0..=0x6B8` (25 frames) — NTC temperatures, 8 NTCs/frame,
//!     signed `i8` °C. Exact (25 × 8 = 200).
//!   - `0x6C0` — FSM extended status (state, mode_locked, TSMS,
//!     DASH_CHG, AMS_OK, PEC error total, #276 fault-reason/detail).
//!   - `0x6C1` — poll timing (V-poll ms last/worst, T-sweep mask).
//!   - `0x6C2` / `0x6C3` — balance DCC mask (95 bits) + cycle counts.
//!   - `0x6C4` — boot diag (jump reason, init progress, FDCAN start).
//!   - `0x6C5` — crash post-mortem from the previous boot.
//!   - `0x6C6` — firmware ID (semver + git hash + BL node-id).
//!   - `0x6C7` / `0x6C8` — per-IC PEC error counts (#258), one
//!     saturating `u8` per monitor IC (10 ICs = 2 × 5 modules).
//!
//! Endianness: cell-V are big-endian and the poll-timing V-poll
//! latencies are big-endian; everything else in the diag block
//! (T-sweep mask, balance, boot, crash) is little-endian, per the
//! DBC `@1+` markers.
//!
//! ## Scope
//!
//! Decoders are hand-coded against the AMS DBC; the slice 6 plan in
//! #252 swaps them for DBC consumption (the file now ships `VAL_`
//! tables for the enums). [`AMS_EXPECTED_FRAMES_PER_SCAN`] lets the
//! Studio + CLI flag a *frame-count* drift before silent
//! miscalibration — but note it can't catch an in-frame byte
//! repurposing (e.g. #276 moving `0x6C0[6..7]` from reserved to
//! fault-reason); those are caught by tracking the AMS DBC per-frame,
//! which is why the host carries an explicit per-frame layout here.
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

/// The diag block — `0x6C0..=0x6C8`, 9 contiguous frames.
///
/// History: this block looked non-contiguous from `CAN_MAP.md` for
/// a while because that doc's table skipped straight from `0x6C1`
/// to `0x6C7`, hiding the `0x6C2..=0x6C6` block. But the firmware
/// emitted those frames all along and `ams.dbc` modelled them — the
/// host (mistakenly built off the prose table in catch-up PR #263)
/// under-counted at 53. AMS #293 filled in the doc table; the real
/// stream is 9 diag frames, 58 total.
///
/// | ID       | frame                              | source |
/// |----------|------------------------------------|--------|
/// | `0x6C0`  | FSM extended status                | #248   |
/// | `0x6C1`  | poll timing                        | #248   |
/// | `0x6C2`  | balance DCC mask, cells 0..63      | —      |
/// | `0x6C3`  | balance DCC mask hi + cycle counts | —      |
/// | `0x6C4`  | boot diag (jump reason / progress) | —      |
/// | `0x6C5`  | crash post-mortem                  | —      |
/// | `0x6C6`  | firmware ID (semver + git + node)  | —      |
/// | `0x6C7`  | per-IC PEC count, ICs 0..7         | #258   |
/// | `0x6C8`  | per-IC PEC count, ICs 8..9 + rsvd  | #258   |
///
/// Source of truth: `docs/dbc/ams.dbc` in IFS08-CE-AMS (the human
/// `CAN_MAP.md` table is secondary — it's what misled #263).
pub const AMS_DIAG_BASE_ID: u16 = 0x6C0;
/// Last CAN ID in the diag block.
pub const AMS_DIAG_LAST_ID: u16 = 0x6C8;
/// Number of frames in the diag block (9 — contiguous 0x6C0..=0x6C8).
pub const AMS_DIAG_NUM_FRAMES: usize = 9;

/// CAN ID of the FSM extended-status frame.
pub const AMS_FSM_STATUS_ID: u16 = 0x6C0;
/// CAN ID of the poll-timing frame.
pub const AMS_POLL_TIMING_ID: u16 = 0x6C1;
/// CAN ID of the balance DCC mask frame A (cells 0..=63).
pub const AMS_BALANCE_MASK_A_ID: u16 = 0x6C2;
/// CAN ID of the balance DCC mask frame B (cells 64..=94 + cycle counts).
pub const AMS_BALANCE_MASK_B_ID: u16 = 0x6C3;
/// CAN ID of the boot-diag frame.
pub const AMS_BOOT_DIAG_ID: u16 = 0x6C4;
/// CAN ID of the crash post-mortem frame.
pub const AMS_POST_MORTEM_ID: u16 = 0x6C5;
/// CAN ID of the firmware-ID frame.
pub const AMS_FW_ID_ID: u16 = 0x6C6;
/// CAN ID of the per-IC PEC frame covering ICs 0..=7.
pub const AMS_PER_IC_PEC_LO_ID: u16 = 0x6C7;
/// CAN ID of the per-IC PEC frame covering ICs 8..=9 (+ reserved).
pub const AMS_PER_IC_PEC_HI_ID: u16 = 0x6C8;

/// Number of monitor ICs in the pack: 2 per module × 5 modules.
/// Chain index → module: IC `2m` = upper, IC `2m+1` = lower of
/// module `m`.
pub const AMS_NUM_ICS: usize = 2 * AMS_NUM_MODULES;

/// Total frames the AMS emits per 1 Hz scan when armed: 24 + 25 + 9
/// = 58. The Studio + CLI compare this against an observed scan-rate
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

/// Decoded balance (cell-discharge) state, reassembled from the two
/// `0x6C2` / `0x6C3` frames.
///
/// The DCC (discharge-cell) mask is 95 bits — one per cell. `0x6C2`
/// carries cells 0..=63 (`dcc_bits_lo64`, LE u64); `0x6C3` carries
/// cells 64..=94 in the low 31 bits of `dcc_bits_hi32` (bit 31
/// reserved) plus two cycle counters. Because the two halves arrive
/// in separate frames, the library decodes each into its own
/// [`PitDiagFrame`] variant and the consumer reassembles the
/// 95-bit picture; [`BalanceState::is_discharging`] does the bit
/// math so callers don't repeat it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct BalanceState {
    /// Cells 0..=63 discharge mask (`0x6C2`).
    pub dcc_lo: u64,
    /// Cells 64..=94 discharge mask in the low 31 bits (`0x6C3`).
    pub dcc_hi: u32,
    /// Total balance cycles since boot, mod 65536 (`0x6C3`).
    pub cycles_total: u16,
    /// Cycles where at least one DCC bit was set (`0x6C3`).
    pub cycles_active: u16,
}

impl BalanceState {
    /// Is `cell_idx` (0..[`AMS_NUM_CELLS`]) currently discharging?
    /// Returns `false` for out-of-range indices.
    #[must_use]
    pub fn is_discharging(&self, cell_idx: usize) -> bool {
        if cell_idx < 64 {
            (self.dcc_lo >> cell_idx) & 1 == 1
        } else if cell_idx < AMS_NUM_CELLS {
            (self.dcc_hi >> (cell_idx - 64)) & 1 == 1
        } else {
            false
        }
    }
}

/// `0x6C2` — balance DCC mask frame A: cells 0..=63 as an LE u64.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BalanceMaskAFrame {
    pub dcc_lo: u64,
}

/// `0x6C3` — balance DCC mask frame B: cells 64..=94 (low 31 bits)
/// plus the two cycle counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BalanceMaskBFrame {
    pub dcc_hi: u32,
    pub cycles_total: u16,
    pub cycles_active: u16,
}

/// Why the firmware (re)booted. Wire: `0x6C4[0..4]` LE u32. The two
/// non-zero values are ASCII tags written by the bootloader / app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JumpReason {
    /// Cold boot / power-on reset (value 0).
    PowerOn,
    /// `'JUMP'` (0x4A554D50) — jumped from the BL via a CAN trigger.
    CanTrigger,
    /// `'MANU'` (0x4D414E55) — manual jump request.
    Manual,
    /// Any other value — surfaced raw for forward-compat.
    Unknown(u32),
}

impl JumpReason {
    fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::PowerOn,
            0x4A55_4D50 => Self::CanTrigger,
            0x4D41_4E55 => Self::Manual,
            other => Self::Unknown(other),
        }
    }
}

/// Decoded `0x6C4` — boot diagnostics. Layout (all LE):
///
/// | bytes | field |
/// |---|---|
/// | 0..4 | jump_reason (u32, ASCII-tag enum) |
/// | 4    | app_init_progress (0..7 milestone; 7 = clean self-exit) |
/// | 5..8 | fdcan1_start_result (24-bit; 0 = HAL_OK) |
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootDiagFrame {
    pub jump_reason: JumpReason,
    /// Last init milestone reached (0..=7). 7 means the app booted
    /// cleanly through every milestone.
    pub app_init_progress: u8,
    /// Low 24 bits of the `HAL_FDCAN_Start` return — 0 is `HAL_OK`.
    pub fdcan1_start_result: u32,
}

/// Decoded `0x6C5` — crash post-mortem from the *previous* boot.
/// Layout (all LE):
///
/// | bytes | field |
/// |---|---|
/// | 0    | stack_overflow_seen (bool) |
/// | 1    | watermark_low_byte (saturates 0xFF) |
/// | 2..6 | task_addr_lo (u32 — low 32 bits of failing xTaskHandle) |
/// | 6..8 | malloc_failed_count (u16, saturates 0xFFFF) |
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PostMortemFrame {
    pub stack_overflow_seen: bool,
    pub watermark_low_byte: u8,
    pub task_addr_lo: u32,
    pub malloc_failed_count: u16,
}

impl PostMortemFrame {
    /// Did the previous boot record any fault worth surfacing?
    /// Drives whether the Studio shows the post-mortem banner.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        !self.stack_overflow_seen && self.task_addr_lo == 0 && self.malloc_failed_count == 0
    }
}

/// Decoded `0x6C6` — firmware identity. Bytes 0..3 semver, 3..7 the
/// first four bytes of the git hash, byte 7 the BL node-id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FwIdFrame {
    pub version_major: u8,
    pub version_minor: u8,
    pub version_patch: u8,
    /// First four bytes of the firmware git hash.
    pub git_hash: [u8; 4],
    /// Bootloader node-id (`firmware_info.reserved[0]`).
    pub bl_node_id: u8,
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
    /// `0x6C2` — balance DCC mask, cells 0..=63.
    BalanceMaskA(BalanceMaskAFrame),
    /// `0x6C3` — balance DCC mask hi + cycle counters.
    BalanceMaskB(BalanceMaskBFrame),
    /// `0x6C4` — boot diagnostics.
    BootDiag(BootDiagFrame),
    /// `0x6C5` — crash post-mortem from the previous boot.
    PostMortem(PostMortemFrame),
    /// `0x6C6` — firmware identity (semver + git hash + node-id).
    FwId(FwIdFrame),
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
            // The V-poll latencies are the only big-endian fields in
            // the diag block; the T-sweep mask and everything in the
            // 0x6C2..=0x6C6 frames below are little-endian.
            last_v_poll_ms: u16::from_be_bytes([payload[0], payload[1]]),
            worst_v_poll_ms: u16::from_be_bytes([payload[2], payload[3]]),
            t_sweep_fail_mask: u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]),
        }));
    }

    // Balance DCC mask A — 0x6C2: cells 0..=63 as an LE u64.
    if id == AMS_BALANCE_MASK_A_ID {
        if payload.len() < 8 {
            return None;
        }
        return Some(PitDiagFrame::BalanceMaskA(BalanceMaskAFrame {
            dcc_lo: u64::from_le_bytes([
                payload[0], payload[1], payload[2], payload[3], payload[4], payload[5], payload[6],
                payload[7],
            ]),
        }));
    }

    // Balance DCC mask B — 0x6C3: cells 64..=94 (low 31 bits) + cycles.
    if id == AMS_BALANCE_MASK_B_ID {
        if payload.len() < 8 {
            return None;
        }
        return Some(PitDiagFrame::BalanceMaskB(BalanceMaskBFrame {
            dcc_hi: u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]),
            cycles_total: u16::from_le_bytes([payload[4], payload[5]]),
            cycles_active: u16::from_le_bytes([payload[6], payload[7]]),
        }));
    }

    // Boot diag — 0x6C4.
    if id == AMS_BOOT_DIAG_ID {
        if payload.len() < 8 {
            return None;
        }
        return Some(PitDiagFrame::BootDiag(BootDiagFrame {
            jump_reason: JumpReason::from_u32(u32::from_le_bytes([
                payload[0], payload[1], payload[2], payload[3],
            ])),
            app_init_progress: payload[4],
            // 24-bit field, bytes 5..=7 LE; pad the high byte with 0.
            fdcan1_start_result: u32::from_le_bytes([payload[5], payload[6], payload[7], 0]),
        }));
    }

    // Crash post-mortem — 0x6C5.
    if id == AMS_POST_MORTEM_ID {
        if payload.len() < 8 {
            return None;
        }
        return Some(PitDiagFrame::PostMortem(PostMortemFrame {
            stack_overflow_seen: payload[0] != 0,
            watermark_low_byte: payload[1],
            task_addr_lo: u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]),
            malloc_failed_count: u16::from_le_bytes([payload[6], payload[7]]),
        }));
    }

    // Firmware ID — 0x6C6.
    if id == AMS_FW_ID_ID {
        if payload.len() < 8 {
            return None;
        }
        return Some(PitDiagFrame::FwId(FwIdFrame {
            version_major: payload[0],
            version_minor: payload[1],
            version_patch: payload[2],
            git_hash: [payload[3], payload[4], payload[5], payload[6]],
            bl_node_id: payload[7],
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
// Total scan = 58 frames: 24 cell-V + 25 NTC + 9 diag (0x6C0..=0x6C8).
// The Studio + CLI display a warning if the observed scan rate drifts
// from this — the canary for "the AMS team added or dropped frames
// since this constant was last verified". When new diag-block frames
// ship, bump AMS_DIAG_NUM_FRAMES and the math falls out automatically.
const _: () = assert!(AMS_EXPECTED_FRAMES_PER_SCAN == 58);
// The diag block is contiguous (0x6C0..=0x6C8) now that 0x6C2..=0x6C6
// are filled in, so a single range invariant holds again.
const _: () = assert!(AMS_DIAG_NUM_FRAMES == (AMS_DIAG_LAST_ID - AMS_DIAG_BASE_ID + 1) as usize);
const _: () = assert!(AMS_FSM_STATUS_ID == AMS_DIAG_BASE_ID);
const _: () = assert!(AMS_PER_IC_PEC_HI_ID == AMS_DIAG_LAST_ID);
// The balance mask spans 95 bits: 64 in frame A + 31 in frame B's
// low bits = one per cell.
const _: () = assert!(64 + 31 == AMS_NUM_CELLS);
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
    fn decodes_balance_mask_a_and_b_reassemble() {
        // Frame A: cells 0, 5, 63 discharging.
        let lo: u64 = (1 << 0) | (1 << 5) | (1 << 63);
        let frame_a = CanFrame::new(AMS_BALANCE_MASK_A_ID, &lo.to_le_bytes()).unwrap();
        // Frame B: cell 64 (hi bit 0) + cell 94 (hi bit 30) discharging;
        // 1234 total cycles, 56 active.
        let hi: u32 = (1 << 0) | (1 << 30);
        let mut b = [0u8; 8];
        b[0..4].copy_from_slice(&hi.to_le_bytes());
        b[4..6].copy_from_slice(&1234u16.to_le_bytes());
        b[6..8].copy_from_slice(&56u16.to_le_bytes());
        let frame_b = CanFrame::new(AMS_BALANCE_MASK_B_ID, &b).unwrap();

        let a = match decode_frame(&frame_a).unwrap() {
            PitDiagFrame::BalanceMaskA(a) => a,
            other => panic!("expected BalanceMaskA, got {other:?}"),
        };
        let bb = match decode_frame(&frame_b).unwrap() {
            PitDiagFrame::BalanceMaskB(b) => b,
            other => panic!("expected BalanceMaskB, got {other:?}"),
        };
        assert_eq!(bb.cycles_total, 1234);
        assert_eq!(bb.cycles_active, 56);

        // Reassemble + check the bit math, including the 64-boundary.
        let state = BalanceState {
            dcc_lo: a.dcc_lo,
            dcc_hi: bb.dcc_hi,
            cycles_total: bb.cycles_total,
            cycles_active: bb.cycles_active,
        };
        assert!(state.is_discharging(0));
        assert!(state.is_discharging(5));
        assert!(!state.is_discharging(6));
        assert!(state.is_discharging(63));
        assert!(state.is_discharging(64)); // first bit of frame B
        assert!(state.is_discharging(94)); // last real cell
        assert!(!state.is_discharging(93));
        assert!(!state.is_discharging(95)); // out of range
    }

    #[test]
    fn decodes_boot_diag_jump_reasons() {
        // 'JUMP' = 0x4A554D50, progress 7 (clean), fdcan OK (0).
        let mut p = [0u8; 8];
        p[0..4].copy_from_slice(&0x4A55_4D50u32.to_le_bytes());
        p[4] = 7;
        // bytes 5..7 = 0 ⇒ fdcan1_start_result 0
        let frame = CanFrame::new(AMS_BOOT_DIAG_ID, &p).unwrap();
        match decode_frame(&frame).unwrap() {
            PitDiagFrame::BootDiag(b) => {
                assert_eq!(b.jump_reason, JumpReason::CanTrigger);
                assert_eq!(b.app_init_progress, 7);
                assert_eq!(b.fdcan1_start_result, 0);
            }
            other => panic!("expected BootDiag, got {other:?}"),
        }

        // Cold boot (0) + a non-zero 24-bit HAL status.
        let mut p2 = [0u8; 8];
        p2[4] = 3;
        p2[5] = 0x01; // fdcan result low byte
        let frame2 = CanFrame::new(AMS_BOOT_DIAG_ID, &p2).unwrap();
        match decode_frame(&frame2).unwrap() {
            PitDiagFrame::BootDiag(b) => {
                assert_eq!(b.jump_reason, JumpReason::PowerOn);
                assert_eq!(b.app_init_progress, 3);
                assert_eq!(b.fdcan1_start_result, 1);
            }
            other => panic!("expected BootDiag, got {other:?}"),
        }
    }

    #[test]
    fn decodes_post_mortem_clean_and_crashed() {
        // Clean: all zero ⇒ is_clean().
        let clean = CanFrame::new(AMS_POST_MORTEM_ID, &[0; 8]).unwrap();
        match decode_frame(&clean).unwrap() {
            PitDiagFrame::PostMortem(p) => assert!(p.is_clean()),
            other => panic!("expected PostMortem, got {other:?}"),
        }

        // Crashed: stack overflow on a task at 0x2000_1234, watermark
        // 8 words, 2 malloc failures.
        let mut p = [0u8; 8];
        p[0] = 1; // stack_overflow_seen
        p[1] = 8; // watermark
        p[2..6].copy_from_slice(&0x2000_1234u32.to_le_bytes());
        p[6..8].copy_from_slice(&2u16.to_le_bytes());
        let frame = CanFrame::new(AMS_POST_MORTEM_ID, &p).unwrap();
        match decode_frame(&frame).unwrap() {
            PitDiagFrame::PostMortem(p) => {
                assert!(!p.is_clean());
                assert!(p.stack_overflow_seen);
                assert_eq!(p.watermark_low_byte, 8);
                assert_eq!(p.task_addr_lo, 0x2000_1234);
                assert_eq!(p.malloc_failed_count, 2);
            }
            other => panic!("expected PostMortem, got {other:?}"),
        }
    }

    #[test]
    fn decodes_fw_id() {
        // AMS v1.6.0, git AB CD EF 01, BL node-id 2.
        let frame = CanFrame::new(AMS_FW_ID_ID, &[1, 6, 0, 0xAB, 0xCD, 0xEF, 0x01, 0x02]).unwrap();
        match decode_frame(&frame).unwrap() {
            PitDiagFrame::FwId(f) => {
                assert_eq!(
                    (f.version_major, f.version_minor, f.version_patch),
                    (1, 6, 0)
                );
                assert_eq!(f.git_hash, [0xAB, 0xCD, 0xEF, 0x01]);
                assert_eq!(f.bl_node_id, 2);
            }
            other => panic!("expected FwId, got {other:?}"),
        }
    }

    #[test]
    fn new_diag_frames_reject_short_payloads() {
        for id in [
            AMS_BALANCE_MASK_A_ID,
            AMS_BALANCE_MASK_B_ID,
            AMS_BOOT_DIAG_ID,
            AMS_POST_MORTEM_ID,
            AMS_FW_ID_ID,
        ] {
            let frame = CanFrame::new(id, &[0, 1, 2, 3]).unwrap();
            assert_eq!(decode_frame(&frame), None, "id {id:#X} should reject short");
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

    // Pack-geometry invariants are now compile-time `const _: () =
    // assert!(...)` at module scope — see the block above this
    // `tests` module. Keeping them out of `#[test]` keeps clippy
    // happy (no `assertions_on_constants` warnings) and turns the
    // invariant into a build break rather than a silent test pass.
}
