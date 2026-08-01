//! ECU (VCU) pit-diag observer protocol.
//!
//! Companion to the AMS observer in the parent module. The ECU exposes
//! a *separate*, much smaller stream than the AMS: when armed it emits
//! [`ECU_EXPECTED_FRAMES_PER_SCAN`] frames at 100 ms carrying the
//! vehicle-control picture — FSM / inverter state, the two APPS pedal
//! channels, the brake, the inverter DC-bus / RPM / error and its two
//! lower fault layers, the DV handshake, and a firmware-ID frame — plus
//! an ungated `0x704` health frame at 1 Hz.
//!
//! ## Wire protocol
//!
//! Source of truth: `Core/Inc/can/messages/*.def` in the ECU repo
//! (`IFS08-CE-ECU`). The `.def` files are the DBCinator DSL; the host
//! mirrors their per-field endianness exactly.
//!
//! - **Enable**:  emit `0x7E0` with payload `DE AD BE EF`
//!   (big-endian magic `0xDEADBEEF`).
//! - **Disable**: emit `0x7E0` with payload `00 00 00 00`.
//! - **ACK**:     ECU replies on `0x7E1` with 1 byte — `0x01` =
//!   enabled, `0x00` = disabled (acyclic).
//! - **Stream IDs once armed (100 ms each)**:
//!   - `0x700` — status: FSM state, inverter state, 5 control-flag
//!     bits, torque %, min cell voltage (mV), torque command.
//!   - `0x701` — pedals: APPS1/APPS2 raw ADC + computed %, brake raw ADC.
//!   - `0x702` — inverter: DC-bus voltage (V), motor RPM (signed),
//!     inverter error code, and the mode word the ECU is commanding.
//!   - `0x703` — fwinfo: firmware semver + first 4 bytes of the git hash.
//!   - `0x705` — brake: physical brake pressure (×0.1 bar) + brake %.
//!   - `0x706` — inverter temperatures (board / power stage / motors).
//!   - `0x707` — the DV (driverless) handshake view.
//!   - `0x708` — the inverter's L1/L2 fault layers + commanded burst.
//!   - `0x704` — firmware health. **Ungated** and 1 Hz, not part of the
//!     100 ms cyclic set above.
//!
//! Endianness: the multi-byte numeric fields (cell-V, torque cmd,
//! APPS/brake raw, DC-bus, RPM, brake pressure, git hash) are
//! big-endian per the `FIELD_BE*` markers; the single-byte fields and
//! the bit flags are position-only. No ID overlaps the AMS stream
//! (`0x680..=0x6C8`, `0x7F0/0x7F1`), so the two decoders are independent.
//!
//! Note the arm *payload* (`DE AD BE EF`) is the same sentinel the AMS
//! uses; only the arm/ACK IDs differ (`0x7E0/0x7E1` here vs the AMS
//! `0x7F0/0x7F1`).

use crate::protocol::CanFrame;

// ---- Wire-level constants ----------------------------------------

/// CAN ID the ECU listens on for arm/disarm commands.
pub const ECU_ARM_ID: u16 = 0x7E0;
/// CAN ID the ECU uses to ACK arm/disarm commands.
pub const ECU_ACK_ID: u16 = 0x7E1;
/// Arm payload — big-endian magic `0xDEADBEEF`.
pub const ECU_ARM_ENABLE_PAYLOAD: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];
/// Disarm payload — all zeros.
pub const ECU_ARM_DISABLE_PAYLOAD: [u8; 4] = [0x00, 0x00, 0x00, 0x00];

// ---- Stream IDs --------------------------------------------------

/// `0x700` — FSM / inverter state + control flags + torque + min cell-V.
pub const ECU_STATUS_ID: u16 = 0x700;
/// `0x701` — APPS pedal channels + brake raw ADC.
pub const ECU_PEDALS_ID: u16 = 0x701;
/// `0x702` — inverter DC-bus voltage, RPM (signed), error code.
pub const ECU_INVERTER_ID: u16 = 0x702;
/// `0x703` — firmware semver + git-hash prefix.
pub const ECU_FWINFO_ID: u16 = 0x703;
/// `0x705` — physical brake pressure + brake %.
pub const ECU_BRAKE_ID: u16 = 0x705;
/// `0x706` — inverter temperatures (board / power-stage / motor1 / motor2).
pub const ECU_INVERTER_TEMPS_ID: u16 = 0x706;
/// `0x704` — firmware health (heap, per-task liveness, reset cause, faults).
/// Emitted at 1 Hz (slower than the 100 ms cyclic frames) from DiagTask.
pub const ECU_HEALTH_ID: u16 = 0x704;
/// `0x708` — the inverter's two LOWER fault layers, forwarded from `0x461`
/// (IFS08-CE-ECU #168). 100 ms while armed.
pub const ECU_INV_FAULTS_ID: u16 = 0x708;
/// `0x707` — the ECU's view of the DV (driverless) integration (#109):
/// R2D/torque-stream freshness + the TX-side autonomy handshake verdicts +
/// the conditioned autonomous torque. 100 ms while armed.
pub const ECU_DV_ID: u16 = 0x707;

/// Inverter temperature sentinel — raw `0xFF` (= 205 °C after the −50
/// offset) means the NX/EMC inverter reports that sensor as disconnected.
pub const ECU_INV_TEMP_DISCONNECTED_C: i16 = 205;

/// Number of CYCLIC (100 ms) stream frames emitted per scan when armed:
/// status / pedals / inverter / fwinfo / brake / inverter-temps / dv (#109) /
/// inv-faults (#168). The `0x704` health frame is acyclic-ish (1 Hz) and not
/// counted here.
pub const ECU_EXPECTED_FRAMES_PER_SCAN: usize = 8;

// ---- Enums -------------------------------------------------------

/// Vehicle-control FSM state (`0x700` byte 0). Mirrors the firmware's
/// `ecu::CtrlState`; names come from the DBC `VAL_` table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EcuFsmState {
    /// 0 — waiting for the inverter VDC config handshake.
    WaitInvVdcConfig,
    /// 1 — precharging the DC bus.
    Precharge,
    /// 2 — waiting for the start + brake R2D gesture.
    WaitStartBrake,
    /// 3 — ready-to-drive sound delay.
    R2dDelay,
    /// 4 — waiting for the inverter to report Standby.
    WaitInvStandby,
    /// 5 — driving / torque enabled.
    Active,
    /// 6 — latched on an AMS error.
    AmsError,
    /// Any value outside the known table (forward-compat).
    Unknown(u8),
}

impl EcuFsmState {
    /// Decode the raw state byte.
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::WaitInvVdcConfig,
            1 => Self::Precharge,
            2 => Self::WaitStartBrake,
            3 => Self::R2dDelay,
            4 => Self::WaitInvStandby,
            5 => Self::Active,
            6 => Self::AmsError,
            other => Self::Unknown(other),
        }
    }
}

/// Inverter application state (`0x700` byte 1). Mirrors the inverter
/// `App_State` — the full seven-value table the firmware names in the DSL
/// (`pit_diag_status.def`, IFS08-CE-ECU #150) and therefore in `ecu.dbc`.
///
/// Decoding only the two values the FSM *gates* on (Standby / Ready) left
/// every state that matters when the drive will not come up — off, both
/// faults, shutdown — rendering as `unknown(0x..)`, which is precisely the
/// case the pit tool exists for (#528).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EcuInvState {
    /// 0 — inverter off.
    Off,
    /// 3 — inverter standby.
    Standby,
    /// 4 — inverter ready.
    Ready,
    /// 6 — torque enabled (drive).
    TorqueEnable,
    /// 10 — soft fault; the ECU clears it by commanding [`INV_MODE_FAULT`].
    SoftFault,
    /// 11 — hard fault; cleared by commanding [`INV_MODE_HARD_FAULT_RESET`].
    HardFault,
    /// 13 — shutdown.
    Shutdown,
    /// Any value outside the known table.
    Unknown(u8),
}

impl EcuInvState {
    /// Decode the raw inverter-state byte.
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Off,
            3 => Self::Standby,
            4 => Self::Ready,
            6 => Self::TorqueEnable,
            10 => Self::SoftFault,
            11 => Self::HardFault,
            13 => Self::Shutdown,
            other => Self::Unknown(other),
        }
    }

    /// `true` for the two fault states — the inverter is refusing to leave
    /// fault until the ECU commands the matching reset word.
    #[must_use]
    pub fn is_fault(self) -> bool {
        matches!(self, Self::SoftFault | Self::HardFault)
    }
}

/// MCU reset cause (`0x704` byte 5). Mirrors the firmware `ecu::ResetCause`
/// `VAL_` table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EcuResetCause {
    /// 0 — cause not determined.
    Unknown,
    /// 1 — power-on reset.
    PowerOn,
    /// 2 — NRST pin reset.
    Pin,
    /// 3 — software reset.
    Software,
    /// 4 — independent watchdog (the failure mode `0x704` exists to catch).
    Iwdg,
    /// 5 — window watchdog.
    Wwdg,
    /// 6 — low-power / brown-out reset.
    LowPower,
    /// Any value outside the known table.
    Other(u8),
}

impl EcuResetCause {
    /// Decode the raw reset-cause byte.
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Unknown,
            1 => Self::PowerOn,
            2 => Self::Pin,
            3 => Self::Software,
            4 => Self::Iwdg,
            5 => Self::Wwdg,
            6 => Self::LowPower,
            other => Self::Other(other),
        }
    }
}

// ---- Commanded inverter mode (`0x702` inv_mode_cmd) ---------------

/// `inv_mode_cmd` sentinel for "the frame carried no commanded mode" — a
/// short (DLC 7) frame from firmware older than IFS08-CE-ECU #150. Not a
/// valid `App_State_Req`, so it can't collide with a real command.
pub const INV_MODE_NONE: u8 = 0;
/// `App_State_Req` 0x01 — command the inverter off.
pub const INV_MODE_OFF: u8 = 0x01;
/// `App_State_Req` 0x04 — command Ready (the `WaitInvStandby` word).
pub const INV_MODE_READY: u8 = 0x04;
/// `App_State_Req` 0x06 — command torque enable (drive).
pub const INV_MODE_TORQUE_ENABLE: u8 = 0x06;
/// `App_State_Req` 0x0D — hard-fault reset, for [`EcuInvState::HardFault`].
pub const INV_MODE_HARD_FAULT_RESET: u8 = 0x0D;
/// `App_State_Req` 0x13 — soft-fault reset, for [`EcuInvState::SoftFault`].
pub const INV_MODE_FAULT: u8 = 0x13;

/// Whether the ECU is running a stored pedal calibration or fell back to
/// its compile-time defaults (`0x704` bits 44-45, IFS08-CE-ECU #169).
///
/// Rides the **ungated** health frame on purpose: an operator can see
/// without arming anything whether the calibration they just committed is
/// actually live. A silently-ignored calibration is otherwise
/// indistinguishable from an applied one — and since these values gate the
/// EV.2.3 torque cut and the driverless R2D, "I committed it" is not the
/// same question as "is it in force". The per-rule reason for a rejection
/// lives on the `0x7E3` calibration-session frame, not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EcuCalStatus {
    /// 0 — no stored calibration; running compile-time defaults.
    Defaults,
    /// 1 — a stored calibration was read and applied.
    Loaded,
    /// 2 — a stored calibration was found but failed validation; the ECU
    /// fell back to defaults rather than run it.
    InvalidFellBack,
    /// 3 — a stored calibration was found with an unrecognised record
    /// version; fell back to defaults.
    BadVersionFellBack,
}

impl EcuCalStatus {
    /// Decode the 2-bit field. Every one of the four values is defined, so
    /// this is total — no `Unknown` variant is reachable.
    #[must_use]
    pub fn from_bits(b: u8) -> Self {
        match b & 0x03 {
            0 => Self::Defaults,
            1 => Self::Loaded,
            2 => Self::InvalidFellBack,
            _ => Self::BadVersionFellBack,
        }
    }

    /// `true` when the ECU is NOT running a stored calibration — either
    /// none exists, or the one that does was rejected. Both mean the
    /// pedals are on compile-time defaults.
    #[must_use]
    pub fn is_fallback(self) -> bool {
        !matches!(self, Self::Loaded)
    }
}

/// Name for the mode word the ECU is **commanding** on `0x360` (`0x702`
/// `inv_mode_cmd`, 7 bits at bit 57 — IFS08-CE-ECU #150). Mirrors the
/// `ecu.dbc` `VAL_` table, whose values are decimal: `HardFaultReset` is
/// 13 = `0x0D`, `Fault` is 19 = `0x13`.
///
/// Everything else in `0x702` is what the inverter *reports*; this is the
/// only field that says what the ECU is asking for. The pair is the whole
/// diagnostic — it separates "the ECU is commanding the wrong word" from
/// "the ECU is commanding correctly and the inverter is refusing", the
/// ambiguity that stalled the TS-off recovery debug (IFS08-CE-ECU #148).
///
/// [`INV_MODE_NONE`] returns `"none"`; anything else outside the table
/// returns `"unknown"` and the caller shows the raw value.
#[must_use]
pub fn inv_mode_cmd_name(mode: u8) -> &'static str {
    match mode {
        INV_MODE_NONE => "none",
        INV_MODE_OFF => "Off",
        INV_MODE_READY => "Ready",
        INV_MODE_TORQUE_ENABLE => "TorqueEnable",
        INV_MODE_HARD_FAULT_RESET => "HardFaultReset",
        INV_MODE_FAULT => "Fault",
        _ => "unknown",
    }
}

/// Name for an inverter DEM fault code (`0x702` `inv_error`) — the EPowerLabs
/// W90 (EMC150) fault table (User Manual §9.2.3, mirrors the `ecu.dbc`
/// `VAL_` table, IFS08-CE-ECU #124). Codes outside `0..=15` return `"unknown"`;
/// the caller renders those as *undocumented* (see [`DEM_UNDOCUMENTED_NOTE`])
/// rather than as a lookup failure — the W90 §9.2.3 table genuinely stops at
/// 15 and the inverter does emit codes above it (code 22 on the car, #528).
#[must_use]
pub fn dem_fault_name(code: u8) -> &'static str {
    match code {
        0 => "NoFault",
        1 => "LostMsg",
        2 => "Undervoltage",
        3 => "PwrStgOvertemp",
        4 => "PwrStgTempDegradation",
        5 => "EMCtrFault",
        6 => "TaskOverrun",
        7 => "CAN1_BusOff",
        8 => "EmachineOvertemp",
        9 => "PhaseCurrentSensorRange",
        10 => "PwrStgTempSensorRange",
        11 => "DCBusVoltageSensorRange",
        12 => "DPBoardOvertemp",
        13 => "DRVBoardOvertemp",
        14 => "AuxSupplyRange",
        15 => "EmachineOverspeed",
        _ => "unknown",
    }
}

/// Suffix for a DEM code [`dem_fault_name`] can't name. Says *the table
/// stops here*, not *the lookup broke* — without it a bare `code 0x16` reads
/// as a tool failure and costs a debug detour (#528).
pub const DEM_UNDOCUMENTED_NOTE: &str = "undocumented — not in W90 §9.2.3";

// ---- Frame records -----------------------------------------------

/// `0x700` — FSM / inverter state, cockpit control flags, torque, and
/// minimum cell voltage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcuStatusFrame {
    /// Vehicle-control FSM state.
    pub fsm_state: EcuFsmState,
    /// Inverter application state.
    pub inv_state: EcuInvState,
    /// Byte 2 bit 0 — EV 2.3 plausibility OK.
    pub ev_2_3: bool,
    /// Byte 2 bit 1 — T11.8/9 plausibility OK.
    pub t11_8_9: bool,
    /// Byte 2 bit 2 — ready-to-drive sound active.
    pub rtds_active: bool,
    /// Byte 2 bit 3 — precharge complete.
    pub ok_precharge: bool,
    /// Byte 2 bit 4 — start button pressed.
    pub start_button: bool,
    /// Byte 2 bit 5 — DV (driverless) drive latched this cycle (#109).
    pub dv_mode: bool,
    /// Byte 2 bit 6 — **sticky**: at least one CAN frame has been dropped by a
    /// full TX queue since boot (IFS08-CE-ECU #127). Never clears short of a
    /// reset. A safety cyclic may have been lost — the `0x100` heartbeat the
    /// AMS watchdogs at 200 ms to hold the AIRs closed rides that same queue.
    pub tx_dropped: bool,
    /// Commanded torque, percent.
    pub torque_pct: u8,
    /// Minimum cell voltage seen by the AMS, millivolts (big-endian).
    pub v_cell_min_mv: u16,
    /// Raw torque command sent to the inverter (signed, big-endian).
    pub torque_cmd: i16,
}

/// `0x701` — the two APPS pedal channels plus the raw brake ADC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcuPedalsFrame {
    /// APPS channel 1 raw ADC (big-endian).
    pub apps1_raw: u16,
    /// APPS channel 2 raw ADC (big-endian).
    pub apps2_raw: u16,
    /// Brake-sensor raw ADC (big-endian).
    pub brake_raw: u16,
    /// APPS channel 1 computed percent.
    pub apps1_pct: u8,
    /// APPS channel 2 computed percent.
    pub apps2_pct: u8,
}

/// `0x705` — physical brake values from the S_BRAKE pressure sensor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcuBrakeFrame {
    /// Brake pressure in deci-bar — multiply by `0.1` for bar
    /// (the DBC field has scale `0.1`, big-endian).
    pub brake_pressure_dbar: u16,
    /// Brake percent.
    pub brake_pct: u8,
}

/// `0x702` — inverter DC-bus voltage, motor RPM, and error code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcuInverterFrame {
    /// DC-bus voltage, volts (big-endian).
    pub dc_bus_voltage: u16,
    /// Motor speed, RPM — **signed** (big-endian).
    pub inv_rpm: i32,
    /// Inverter DEM fault code (`DEM_Code` low byte) — name via
    /// [`dem_fault_name`].
    pub inv_error: u8,
    /// `dem_present` (byte 7 bit 0, #121): the fault is active **now** (`true`)
    /// vs latched history (`false`). The NX boots latched — code set but this
    /// bit clear. `false` on a short (DLC 7) frame from older firmware.
    pub dem_present: bool,
    /// `inv_mode_cmd` (byte 7 bits 1-7, #150): the `App_State_Req` the ECU is
    /// **commanding** on `0x360` right now — name via [`inv_mode_cmd_name`].
    /// Pair it with the reported [`EcuStatusFrame::inv_state`]; that's the
    /// commanding-X / reporting-Y read the panel exists for.
    /// [`INV_MODE_NONE`] on a short (DLC 7) frame from older firmware.
    pub inv_mode_cmd: u8,
}

/// `0x703` — firmware semantic version + git-hash prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcuFwInfoFrame {
    /// Firmware major version.
    pub fw_major: u8,
    /// Firmware minor version.
    pub fw_minor: u8,
    /// Firmware patch version.
    pub fw_patch: u8,
    /// First 4 bytes of the git hash (big-endian on the wire, so the
    /// array reads as the hex prefix).
    pub git_hash: [u8; 4],
}

/// `0x706` — the NX/EMC inverter's four temperatures, forwarded from
/// `0x464`. Each wire byte is `raw`; decoded `°C = raw − 50`. A value of
/// [`ECU_INV_TEMP_DISCONNECTED_C`] (205 °C, raw `0xFF`) means that sensor
/// is disconnected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcuInverterTempsFrame {
    /// Inverter control-board temperature, °C.
    pub board_degc: i16,
    /// Power-stage (IGBT) temperature, °C.
    pub pwrstg_degc: i16,
    /// Motor temperature sensor 1, °C.
    pub motor1_degc: i16,
    /// Motor temperature sensor 2, °C.
    pub motor2_degc: i16,
}

/// `0x704` — firmware-health telemetry (parity with the AMS health diag).
/// Emitted from DiagTask so it survives a ControlTask stall.
///
/// Byte layout tracks `pit_diag_health.def`, which moved twice after this
/// decoder was first written (#528): TelemetryTask took byte 4 bit 3 — pushing
/// `task_diag` to bit 4 — and `reset_cause` was narrowed to byte 5 bits 0-2 to
/// free bit 3 for `stub_brake`. Reading byte 5 whole made a `stub_brake` board
/// report `reset_cause` 8..=14, i.e. `unknown` on a board that had just
/// power-on reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcuHealthFrame {
    /// Current free heap, bytes (big-endian).
    pub free_heap: u16,
    /// Minimum free heap ever observed, bytes (big-endian).
    pub min_free_heap: u16,
    /// ControlTask stepped since the previous health frame.
    pub task_control: bool,
    /// CAN-RX task stepped.
    pub task_can_rx: bool,
    /// CAN-TX task stepped.
    pub task_can_tx: bool,
    /// TelemetryTask stepped (byte 4 bit 3 — dashboard + radio snapshot).
    pub task_telemetry: bool,
    /// DiagTask stepped.
    pub task_diag: bool,
    /// Bench stub — AMS absent, precharge gate faked (byte 4 bit 5).
    pub stub_no_ams: bool,
    /// Bench stub — inverter absent (byte 4 bit 6).
    pub stub_no_inverter: bool,
    /// Bench stub — start button forced (byte 4 bit 7).
    pub stub_start: bool,
    /// Bench stub — brake reading injected (byte 5 bit 3). Gates nothing on
    /// its own, but a live `brake_raw` on a car nobody is braking is worth
    /// seeing on the health card.
    pub stub_brake: bool,
    /// Pedal-calibration provenance (byte 5 bits 4-5, #169) — stored and
    /// applied, or fell back to compile-time defaults.
    pub cal_status: EcuCalStatus,
    /// Cause of the most recent MCU reset (byte 5 bits 0-2).
    pub reset_cause: EcuResetCause,
    /// Seconds since boot (wraps at 255).
    pub uptime_s: u8,
    /// Sticky last-fault sentinel latched across the reset (`0x00` = none;
    /// `0xF1..=0xF7` = HardFault…AssertFailed per the firmware table).
    pub last_fault: u8,
}

/// `0x707` — the ECU's view of the DV (driverless) integration (#109). The
/// `dv_mode` latch itself rides `0x700` (`EcuStatusFrame::dv_mode`); this frame
/// carries the handshake around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcuDvFrame {
    /// uDV `0x510` R2D request is set AND fresh.
    pub dv_r2d_req: bool,
    /// uDV `0x507` torque stream is fresh.
    pub dv_cmd_fresh: bool,
    /// ECU TX `0x504` — tractive-system-active view.
    pub ts_active: bool,
    /// ECU TX `0x505` — EBS hard-braking verdict.
    pub brake_over_limit: bool,
    /// ECU TX `0x511` — R2D confirmed (== DV drive latched).
    pub r2d_confirm: bool,
    /// Conditioned autonomous torque actually applied, percent (0..100).
    pub dv_torque_pct: u8,
    /// Mechanical rpm streamed to the uDV on `0x506` (signed, little-endian).
    pub motor_rpm_mech: i16,
}

/// `0x708` — the inverter's two lower fault layers, forwarded from `0x461`
/// (IFS08-CE-ECU #168), plus the commanded side of the fault handshake and a
/// freshness measure for the state the ECU is steering on.
///
/// The W90 has three fault layers and they cascade upward (manual §9.2):
/// L1 `PwrStg_BitState` (hardware protection), L2 `EMCtrl_FOC_BitState`
/// (machine control), L3 `DEM_Code` — and only L3 rides `0x702`. That made a
/// latched DEM refusing to clear indistinguishable from a live L1/L2
/// condition holding it up. **If an L1 bit is asserted, no CAN command clears
/// the DEM** — the root cause is still present. That is the difference
/// between a firmware bug and a wiring or interlock fault, and
/// [`hvil_open`](Self::pwrstg_hvil_open) is the load-bearing one.
///
/// Both layers are bitmasks, not enums — several bits can be set at once.
/// Note `pwrstg_alive`, `pwrstg_enable` and `emctrl_init_ok` are **health**
/// bits: set is normal, **clear** is the anomaly. [`l1_anomalies`] and
/// [`l2_anomalies`] fold that inversion in so callers can't get it backwards.
///
/// [`l1_anomalies`]: Self::l1_anomalies
/// [`l2_anomalies`]: Self::l2_anomalies
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcuInvFaultsFrame {
    // ---- L1: power stage (manual §9.2.1), bits 0-8 ----
    /// Health bit — power stage alive. **Clear is the anomaly.**
    pub pwrstg_alive: bool,
    /// Health bit — power stage enabled. **Clear is the anomaly.**
    pub pwrstg_enable: bool,
    /// Under-voltage lockout.
    pub pwrstg_uvlo: bool,
    /// Desaturation (IGBT short-circuit protection).
    pub pwrstg_desat: bool,
    /// Dead-time violation.
    pub pwrstg_dt_violation: bool,
    /// **HVIL open** — the interlock loop is broken. No CAN command clears a
    /// DEM while this is asserted; go and find the unmated connector.
    pub pwrstg_hvil_open: bool,
    /// Over-current protection.
    pub pwrstg_ocp: bool,
    /// Over-voltage, threshold 1.
    pub pwrstg_ovp_th1: bool,
    /// Over-voltage, threshold 2.
    pub pwrstg_ovp_th2: bool,

    // ---- L2: electric machine control (manual §9.2.2), bits 16-23 ----
    /// Health bit — machine control initialised. **Clear is the anomaly.**
    pub emctrl_init_ok: bool,
    /// Position-feedback fault (resolver / encoder).
    pub emctrl_posfb: bool,
    /// Active short circuit engaged.
    pub emctrl_asc: bool,
    /// Phase-current imbalance.
    pub emctrl_curr_imbalance: bool,
    /// Power-stage fault — manual §9.2.2 defines this as literally "see the
    /// L1 faults", so it is the cascade marker: when set, read L1.
    pub emctrl_pwrstg_fault: bool,
    /// Current derating active.
    pub emctrl_curr_derating: bool,
    /// Control loop de-locked.
    pub emctrl_loop_delocked: bool,
    /// Phase-current acquisition fault.
    pub emctrl_phcurr_acq: bool,

    // ---- Commanded side of the handshake (byte 3) ----
    /// How many follow-up words the ECU sent after the primary recovery word
    /// (2 bits, 0..=3). The pit tool sits on FDCAN2 while the inverter
    /// setpoints go out on FDCAN1, so without this you cannot tell "the ECU
    /// never emitted the burst" from "it did and the inverter ignored it" —
    /// `0x702` `inv_mode_cmd` only carries the primary word. Read together
    /// with [`EcuStatusFrame::tx_dropped`]: burst emitted and nothing dropped
    /// means it reached the FDCAN1 TX FIFO.
    pub cmd_follow_n: u8,
    /// The ECU asserted `Flt_Clear` on `0x360`.
    pub cmd_flt_clear: bool,

    // ---- Freshness of the state the ladder is driven by (bytes 4-5) ----
    /// Milliseconds since the last `0x461`, saturating at 255. The whole
    /// climb/fault ladder is driven by `inv_state`, which comes from `0x461`;
    /// anything near 255 means far too stale to steer a state machine with.
    pub inv_state_age_ms: u8,
    /// Increments once per `0x461` received, wrapping. Since `0x708` is
    /// emitted every 100 ms, **the delta between consecutive frames is the
    /// arrival count per 100 ms** — 10 means a 10 ms period, 1 means 100 ms,
    /// mostly-zero means 500 ms. That measurement needs no FDCAN1 access,
    /// which the pit adapter does not have.
    pub inv_state_seq: u8,
}

impl EcuInvFaultsFrame {
    /// Names of the active L1 anomalies, health-bit inversion already
    /// applied. Empty means the power stage is clean.
    #[must_use]
    pub fn l1_anomalies(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        // Health bits: absence is the fault.
        if !self.pwrstg_alive {
            v.push("PwrStg not alive");
        }
        if !self.pwrstg_enable {
            v.push("PwrStg not enabled");
        }
        for (set, name) in [
            (self.pwrstg_uvlo, "UVLO"),
            (self.pwrstg_desat, "Desat"),
            (self.pwrstg_dt_violation, "DeadTimeViolation"),
            (self.pwrstg_hvil_open, "HVIL_Open"),
            (self.pwrstg_ocp, "OCP"),
            (self.pwrstg_ovp_th1, "OVP_Th1"),
            (self.pwrstg_ovp_th2, "OVP_Th2"),
        ] {
            if set {
                v.push(name);
            }
        }
        v
    }

    /// Names of the active L2 anomalies, health-bit inversion already applied.
    #[must_use]
    pub fn l2_anomalies(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if !self.emctrl_init_ok {
            v.push("EMCtrl not initialised");
        }
        for (set, name) in [
            (self.emctrl_posfb, "PosFeedback"),
            (self.emctrl_asc, "ASC"),
            (self.emctrl_curr_imbalance, "CurrentImbalance"),
            (self.emctrl_pwrstg_fault, "PwrStgFault (see L1)"),
            (self.emctrl_curr_derating, "CurrentDerating"),
            (self.emctrl_loop_delocked, "LoopDelocked"),
            (self.emctrl_phcurr_acq, "PhaseCurrentAcq"),
        ] {
            if set {
                v.push(name);
            }
        }
        v
    }

    /// `true` while an L1 condition is holding a DEM up. **No CAN command
    /// will clear the fault in this state** — the root cause is still live,
    /// so a recovery burst is wasted effort and the operator should be sent
    /// looking at hardware instead.
    #[must_use]
    pub fn l1_blocks_dem_clear(&self) -> bool {
        !self.l1_anomalies().is_empty()
    }
}

/// A decoded ECU pit-diag frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EcuPitDiagFrame {
    /// ECU replied to an arm/disarm command (`0x7E1`).
    Ack {
        /// `true` after a successful arm, `false` after a disarm.
        enabled: bool,
    },
    /// `0x700` — FSM / inverter status.
    Status(EcuStatusFrame),
    /// `0x701` — APPS pedals + brake raw.
    Pedals(EcuPedalsFrame),
    /// `0x705` — physical brake.
    Brake(EcuBrakeFrame),
    /// `0x702` — inverter telemetry.
    Inverter(EcuInverterFrame),
    /// `0x706` — inverter temperatures.
    InverterTemps(EcuInverterTempsFrame),
    /// `0x703` — firmware identity.
    FwInfo(EcuFwInfoFrame),
    /// `0x704` — firmware health.
    Health(EcuHealthFrame),
    /// `0x707` — DV (driverless) integration view.
    Dv(EcuDvFrame),
    /// `0x708` — inverter L1/L2 fault layers + commanded burst (#168).
    InvFaults(EcuInvFaultsFrame),
}

// ---- Encode / decode ---------------------------------------------

/// Build the CAN frame that arms (or disarms) the ECU pit-diag stream.
///
/// Standard 11-bit ID, 4-byte payload — ready to send directly.
#[must_use]
pub fn build_arm_frame(enable: bool) -> CanFrame {
    let payload = if enable {
        ECU_ARM_ENABLE_PAYLOAD
    } else {
        ECU_ARM_DISABLE_PAYLOAD
    };
    CanFrame::new(ECU_ARM_ID, &payload).expect("4-byte payload always fits")
}

/// Decode a raw CAN frame into an ECU pit-diag record.
///
/// Returns `None` if the frame ID isn't part of the ECU pit-diag
/// stream, or if a recognised ID arrived with a payload too short to
/// decode.
#[must_use]
pub fn decode_frame(frame: &CanFrame) -> Option<EcuPitDiagFrame> {
    let id = frame.id;
    let p = frame.payload();

    match id {
        ECU_ACK_ID => {
            let enabled = p.first().copied().unwrap_or(0) == 0x01;
            Some(EcuPitDiagFrame::Ack { enabled })
        }
        ECU_STATUS_ID => {
            if p.len() < 8 {
                return None;
            }
            let flags = p[2];
            Some(EcuPitDiagFrame::Status(EcuStatusFrame {
                fsm_state: EcuFsmState::from_byte(p[0]),
                inv_state: EcuInvState::from_byte(p[1]),
                ev_2_3: (flags & 0x01) != 0,
                t11_8_9: (flags & 0x02) != 0,
                rtds_active: (flags & 0x04) != 0,
                ok_precharge: (flags & 0x08) != 0,
                start_button: (flags & 0x10) != 0,
                dv_mode: (flags & 0x20) != 0,
                tx_dropped: (flags & 0x40) != 0,
                torque_pct: p[3],
                v_cell_min_mv: u16::from_be_bytes([p[4], p[5]]),
                torque_cmd: i16::from_be_bytes([p[6], p[7]]),
            }))
        }
        ECU_PEDALS_ID => {
            if p.len() < 8 {
                return None;
            }
            Some(EcuPitDiagFrame::Pedals(EcuPedalsFrame {
                apps1_raw: u16::from_be_bytes([p[0], p[1]]),
                apps2_raw: u16::from_be_bytes([p[2], p[3]]),
                brake_raw: u16::from_be_bytes([p[4], p[5]]),
                apps1_pct: p[6],
                apps2_pct: p[7],
            }))
        }
        ECU_BRAKE_ID => {
            if p.len() < 3 {
                return None;
            }
            Some(EcuPitDiagFrame::Brake(EcuBrakeFrame {
                brake_pressure_dbar: u16::from_be_bytes([p[0], p[1]]),
                brake_pct: p[2],
            }))
        }
        ECU_INVERTER_ID => {
            if p.len() < 7 {
                return None;
            }
            Some(EcuPitDiagFrame::Inverter(EcuInverterFrame {
                dc_bus_voltage: u16::from_be_bytes([p[0], p[1]]),
                inv_rpm: i32::from_be_bytes([p[2], p[3], p[4], p[5]]),
                inv_error: p[6],
                // dem_present rides byte 7 bit 0 and inv_mode_cmd its bits
                // 1-7 (DLC 8); older DLC-7 frames carry no such byte —
                // default to latched (false) / no command (INV_MODE_NONE).
                dem_present: p.get(7).is_some_and(|b| b & 0x01 != 0),
                inv_mode_cmd: p.get(7).map_or(INV_MODE_NONE, |b| b >> 1),
            }))
        }
        ECU_FWINFO_ID => {
            if p.len() < 7 {
                return None;
            }
            Some(EcuPitDiagFrame::FwInfo(EcuFwInfoFrame {
                fw_major: p[0],
                fw_minor: p[1],
                fw_patch: p[2],
                git_hash: [p[3], p[4], p[5], p[6]],
            }))
        }
        ECU_INVERTER_TEMPS_ID => {
            if p.len() < 4 {
                return None;
            }
            // Each byte: °C = raw − 50.
            let degc = |raw: u8| i16::from(raw) - 50;
            Some(EcuPitDiagFrame::InverterTemps(EcuInverterTempsFrame {
                board_degc: degc(p[0]),
                pwrstg_degc: degc(p[1]),
                motor1_degc: degc(p[2]),
                motor2_degc: degc(p[3]),
            }))
        }
        ECU_HEALTH_ID => {
            if p.len() < 8 {
                return None;
            }
            let live = p[4];
            let cause = p[5];
            Some(EcuPitDiagFrame::Health(EcuHealthFrame {
                free_heap: u16::from_be_bytes([p[0], p[1]]),
                min_free_heap: u16::from_be_bytes([p[2], p[3]]),
                task_control: (live & 0x01) != 0,
                task_can_rx: (live & 0x02) != 0,
                task_can_tx: (live & 0x04) != 0,
                task_telemetry: (live & 0x08) != 0,
                task_diag: (live & 0x10) != 0,
                stub_no_ams: (live & 0x20) != 0,
                stub_no_inverter: (live & 0x40) != 0,
                stub_start: (live & 0x80) != 0,
                stub_brake: (cause & 0x08) != 0,
                // Byte 5 is packed: reset_cause b0-b2, stub_brake b3,
                // cal_status b4-b5. b6-b7 remain free.
                cal_status: EcuCalStatus::from_bits(cause >> 4),
                reset_cause: EcuResetCause::from_byte(cause & 0x07),
                uptime_s: p[6],
                last_fault: p[7],
            }))
        }
        ECU_DV_ID => {
            if p.len() < 4 {
                return None;
            }
            let flags = p[0];
            Some(EcuPitDiagFrame::Dv(EcuDvFrame {
                dv_r2d_req: (flags & 0x01) != 0,
                dv_cmd_fresh: (flags & 0x02) != 0,
                ts_active: (flags & 0x04) != 0,
                brake_over_limit: (flags & 0x08) != 0,
                r2d_confirm: (flags & 0x10) != 0,
                dv_torque_pct: p[1],
                motor_rpm_mech: i16::from_le_bytes([p[2], p[3]]),
            }))
        }
        ECU_INV_FAULTS_ID => {
            if p.len() < 6 {
                return None;
            }
            // L1 is bits 0-8: all of byte 0 plus bit 0 of byte 1. L2 is
            // byte 2. The commanded handshake is byte 3 bits 0-2.
            let l1_lo = p[0];
            let l1_hi = p[1];
            let l2 = p[2];
            let cmd = p[3];
            Some(EcuPitDiagFrame::InvFaults(EcuInvFaultsFrame {
                pwrstg_alive: (l1_lo & 0x01) != 0,
                pwrstg_enable: (l1_lo & 0x02) != 0,
                pwrstg_uvlo: (l1_lo & 0x04) != 0,
                pwrstg_desat: (l1_lo & 0x08) != 0,
                pwrstg_dt_violation: (l1_lo & 0x10) != 0,
                pwrstg_hvil_open: (l1_lo & 0x20) != 0,
                pwrstg_ocp: (l1_lo & 0x40) != 0,
                pwrstg_ovp_th1: (l1_lo & 0x80) != 0,
                // bit 8 — the ninth L1 bit, alone in byte 1.
                pwrstg_ovp_th2: (l1_hi & 0x01) != 0,
                emctrl_init_ok: (l2 & 0x01) != 0,
                emctrl_posfb: (l2 & 0x02) != 0,
                emctrl_asc: (l2 & 0x04) != 0,
                emctrl_curr_imbalance: (l2 & 0x08) != 0,
                emctrl_pwrstg_fault: (l2 & 0x10) != 0,
                emctrl_curr_derating: (l2 & 0x20) != 0,
                emctrl_loop_delocked: (l2 & 0x40) != 0,
                emctrl_phcurr_acq: (l2 & 0x80) != 0,
                cmd_follow_n: cmd & 0x03,
                cmd_flt_clear: (cmd & 0x04) != 0,
                inv_state_age_ms: p[4],
                inv_state_seq: p[5],
            }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_frame_round_trip() {
        let on = build_arm_frame(true);
        assert_eq!(on.id, ECU_ARM_ID);
        assert_eq!(on.payload(), &[0xDE, 0xAD, 0xBE, 0xEF]);
        let off = build_arm_frame(false);
        assert_eq!(off.payload(), &[0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn ack_decodes() {
        let on = CanFrame::new(ECU_ACK_ID, &[0x01]).unwrap();
        assert_eq!(
            decode_frame(&on),
            Some(EcuPitDiagFrame::Ack { enabled: true })
        );
        let off = CanFrame::new(ECU_ACK_ID, &[0x00]).unwrap();
        assert_eq!(
            decode_frame(&off),
            Some(EcuPitDiagFrame::Ack { enabled: false })
        );
    }

    #[test]
    fn status_decodes() {
        // fsm=5 (Active), inv=4 (Ready), flags=0b10101 (ev_2_3 +
        // rtds_active + start_button), torque=42%, v_cell_min=3500mV,
        // torque_cmd=-300.
        let p = [
            0x05,
            0x04,
            0b0001_0101,
            42,
            0x0D,
            0xAC, // 3500
            0xFE,
            0xD4, // -300 as i16 BE
        ];
        let frame = CanFrame::new(ECU_STATUS_ID, &p).unwrap();
        match decode_frame(&frame).unwrap() {
            EcuPitDiagFrame::Status(s) => {
                assert_eq!(s.fsm_state, EcuFsmState::Active);
                assert_eq!(s.inv_state, EcuInvState::Ready);
                assert!(s.ev_2_3 && s.rtds_active && s.start_button);
                assert!(!s.t11_8_9 && !s.ok_precharge);
                assert_eq!(s.torque_pct, 42);
                assert_eq!(s.v_cell_min_mv, 3500);
                assert_eq!(s.torque_cmd, -300);
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn pedals_decode() {
        // apps1=0x0102, apps2=0x0304, brake=0x0506, apps1%=10, apps2%=11.
        let p = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 10, 11];
        let frame = CanFrame::new(ECU_PEDALS_ID, &p).unwrap();
        match decode_frame(&frame).unwrap() {
            EcuPitDiagFrame::Pedals(ped) => {
                assert_eq!(ped.apps1_raw, 0x0102);
                assert_eq!(ped.apps2_raw, 0x0304);
                assert_eq!(ped.brake_raw, 0x0506);
                assert_eq!(ped.apps1_pct, 10);
                assert_eq!(ped.apps2_pct, 11);
            }
            other => panic!("expected Pedals, got {other:?}"),
        }
    }

    #[test]
    fn brake_decodes() {
        // 123 deci-bar = 12.3 bar, 55%.
        let frame = CanFrame::new(ECU_BRAKE_ID, &[0x00, 123, 55]).unwrap();
        match decode_frame(&frame).unwrap() {
            EcuPitDiagFrame::Brake(b) => {
                assert_eq!(b.brake_pressure_dbar, 123);
                assert_eq!(b.brake_pct, 55);
            }
            other => panic!("expected Brake, got {other:?}"),
        }
    }

    #[test]
    fn inverter_decodes_signed_rpm() {
        // dc_bus=0x0258 (600V), rpm=-1000 (BE i32), err=0x07. 7-byte frame:
        // no dem_present byte -> latched (false).
        let rpm = (-1000i32).to_be_bytes();
        let p = [0x02, 0x58, rpm[0], rpm[1], rpm[2], rpm[3], 0x07];
        let frame = CanFrame::new(ECU_INVERTER_ID, &p).unwrap();
        match decode_frame(&frame).unwrap() {
            EcuPitDiagFrame::Inverter(inv) => {
                assert_eq!(inv.dc_bus_voltage, 600);
                assert_eq!(inv.inv_rpm, -1000);
                assert_eq!(inv.inv_error, 0x07);
                assert_eq!(dem_fault_name(inv.inv_error), "CAN1_BusOff");
                assert!(!inv.dem_present);
                assert_eq!(inv.inv_mode_cmd, INV_MODE_NONE);
                assert_eq!(inv_mode_cmd_name(inv.inv_mode_cmd), "none");
            }
            other => panic!("expected Inverter, got {other:?}"),
        }
    }

    #[test]
    fn inverter_decodes_dem_present_and_names() {
        // 8-byte frame: err=2 (Undervoltage), byte7 bit0 set -> active now.
        let p = [0, 0, 0, 0, 0, 0, 2, 0x01];
        match decode_frame(&CanFrame::new(ECU_INVERTER_ID, &p).unwrap()).unwrap() {
            EcuPitDiagFrame::Inverter(inv) => {
                assert_eq!(inv.inv_error, 2);
                assert_eq!(dem_fault_name(inv.inv_error), "Undervoltage");
                assert!(inv.dem_present);
            }
            other => panic!("expected Inverter, got {other:?}"),
        }
        assert_eq!(dem_fault_name(15), "EmachineOverspeed");
        assert_eq!(dem_fault_name(200), "unknown");
    }

    #[test]
    fn inverter_decodes_mode_cmd_from_the_reference_capture() {
        // The #528 capture, decoded by hand at the time:
        //   0x702  01 63 00 00 00 00 16 27
        // dc_bus=355V, dem=22 (above the W90 table), byte7=0x27 ->
        // dem_present=1 + inv_mode_cmd=0x13 (the ECU commanding the
        // soft-fault reset continuously).
        let p = [0x01, 0x63, 0x00, 0x00, 0x00, 0x00, 0x16, 0x27];
        match decode_frame(&CanFrame::new(ECU_INVERTER_ID, &p).unwrap()).unwrap() {
            EcuPitDiagFrame::Inverter(inv) => {
                assert_eq!(inv.dc_bus_voltage, 355);
                assert_eq!(inv.inv_error, 22);
                assert_eq!(dem_fault_name(inv.inv_error), "unknown");
                assert!(inv.dem_present);
                assert_eq!(inv.inv_mode_cmd, INV_MODE_FAULT);
                assert_eq!(inv_mode_cmd_name(inv.inv_mode_cmd), "Fault");
            }
            other => panic!("expected Inverter, got {other:?}"),
        }
    }

    #[test]
    fn inv_mode_cmd_names_cover_the_val_table() {
        assert_eq!(inv_mode_cmd_name(INV_MODE_OFF), "Off");
        assert_eq!(inv_mode_cmd_name(INV_MODE_READY), "Ready");
        assert_eq!(inv_mode_cmd_name(INV_MODE_TORQUE_ENABLE), "TorqueEnable");
        assert_eq!(
            inv_mode_cmd_name(INV_MODE_HARD_FAULT_RESET),
            "HardFaultReset"
        );
        assert_eq!(inv_mode_cmd_name(INV_MODE_FAULT), "Fault");
        assert_eq!(inv_mode_cmd_name(0x7F), "unknown");
    }

    #[test]
    fn inv_mode_cmd_uses_all_seven_bits() {
        // 7 bits at bit 57: the widest value the field can hold must survive
        // the shift without bleeding dem_present into it.
        let p = [0, 0, 0, 0, 0, 0, 0, 0xFE];
        match decode_frame(&CanFrame::new(ECU_INVERTER_ID, &p).unwrap()).unwrap() {
            EcuPitDiagFrame::Inverter(inv) => {
                assert_eq!(inv.inv_mode_cmd, 0x7F);
                assert!(!inv.dem_present);
            }
            other => panic!("expected Inverter, got {other:?}"),
        }
    }

    #[test]
    fn inv_state_covers_the_full_val_table() {
        // All seven the firmware names (#150) — every one but Standby/Ready
        // used to fall through to Unknown, including the fault states the
        // pit tool exists to show (#528).
        for (raw, want) in [
            (0u8, EcuInvState::Off),
            (3, EcuInvState::Standby),
            (4, EcuInvState::Ready),
            (6, EcuInvState::TorqueEnable),
            (10, EcuInvState::SoftFault),
            (11, EcuInvState::HardFault),
            (13, EcuInvState::Shutdown),
        ] {
            assert_eq!(EcuInvState::from_byte(raw), want, "raw {raw}");
        }
        assert_eq!(EcuInvState::from_byte(9), EcuInvState::Unknown(9));
        assert!(EcuInvState::SoftFault.is_fault() && EcuInvState::HardFault.is_fault());
        assert!(!EcuInvState::Ready.is_fault() && !EcuInvState::Off.is_fault());
    }

    #[test]
    fn fwinfo_decodes() {
        let p = [1, 6, 2, 0xAB, 0xCD, 0xEF, 0x01];
        let frame = CanFrame::new(ECU_FWINFO_ID, &p).unwrap();
        match decode_frame(&frame).unwrap() {
            EcuPitDiagFrame::FwInfo(fw) => {
                assert_eq!((fw.fw_major, fw.fw_minor, fw.fw_patch), (1, 6, 2));
                assert_eq!(fw.git_hash, [0xAB, 0xCD, 0xEF, 0x01]);
            }
            other => panic!("expected FwInfo, got {other:?}"),
        }
    }

    #[test]
    fn inverter_temps_decode_offset_and_sentinel() {
        // board=25 (raw 75), pwrstg=60 (raw 110), motor1=-10 (raw 40),
        // motor2=disconnected (raw 0xFF => 205).
        let frame = CanFrame::new(ECU_INVERTER_TEMPS_ID, &[75, 110, 40, 0xFF]).unwrap();
        match decode_frame(&frame).unwrap() {
            EcuPitDiagFrame::InverterTemps(t) => {
                assert_eq!(t.board_degc, 25);
                assert_eq!(t.pwrstg_degc, 60);
                assert_eq!(t.motor1_degc, -10);
                assert_eq!(t.motor2_degc, ECU_INV_TEMP_DISCONNECTED_C);
            }
            other => panic!("expected InverterTemps, got {other:?}"),
        }
    }

    #[test]
    fn health_decodes() {
        // free=0x1234, min=0x0800, live=0b1_0111 (control+can_rx+can_tx+diag,
        // telemetry stalled), reset=4 (IWDG), uptime=42, last_fault=0xF5
        // (stack overflow).
        let p = [0x12, 0x34, 0x08, 0x00, 0b1_0111, 4, 42, 0xF5];
        let frame = CanFrame::new(ECU_HEALTH_ID, &p).unwrap();
        match decode_frame(&frame).unwrap() {
            EcuPitDiagFrame::Health(h) => {
                assert_eq!(h.free_heap, 0x1234);
                assert_eq!(h.min_free_heap, 0x0800);
                assert!(h.task_control && h.task_can_rx && h.task_can_tx && h.task_diag);
                assert!(!h.task_telemetry);
                assert_eq!(h.reset_cause, EcuResetCause::Iwdg);
                assert_eq!(h.uptime_s, 42);
                assert_eq!(h.last_fault, 0xF5);
            }
            other => panic!("expected Health, got {other:?}"),
        }
    }

    #[test]
    fn health_task_diag_is_bit4_not_bit3() {
        // TelemetryTask took byte 4 bit 3 and pushed task_diag to bit 4
        // (#528). A board with only telemetry alive must not read as
        // "diag alive" — that was the pre-fix behaviour.
        let p = [0, 0, 0, 0, 0b0000_1000, 0, 0, 0];
        match decode_frame(&CanFrame::new(ECU_HEALTH_ID, &p).unwrap()).unwrap() {
            EcuPitDiagFrame::Health(h) => {
                assert!(h.task_telemetry);
                assert!(!h.task_diag);
            }
            other => panic!("expected Health, got {other:?}"),
        }
    }

    #[test]
    fn health_masks_stub_brake_out_of_reset_cause() {
        // byte5 = 0x09 -> stub_brake (bit 3) + PowerOn (bits 0-2 = 1). Reading
        // the byte whole yielded 9 -> Other(9) -> "unknown reset" on a board
        // that had plainly just powered on (#528).
        let p = [0, 0, 0, 0, 0, 0x09, 15, 0];
        match decode_frame(&CanFrame::new(ECU_HEALTH_ID, &p).unwrap()).unwrap() {
            EcuPitDiagFrame::Health(h) => {
                assert_eq!(h.reset_cause, EcuResetCause::PowerOn);
                assert!(h.stub_brake);
            }
            other => panic!("expected Health, got {other:?}"),
        }
    }

    #[test]
    fn health_decodes_cal_status_alongside_its_bitfield_neighbours() {
        // Byte 5 packs three things: reset_cause b0-b2, stub_brake b3,
        // cal_status b4-b5. 0x2D = 0b0010_1101 -> cause 5 (WWDG),
        // stub_brake set, cal_status 2 (InvalidFellBack). Each must come
        // out independent of the others.
        let p = [0, 0, 0, 0, 0, 0x2D, 0, 0];
        match decode_frame(&CanFrame::new(ECU_HEALTH_ID, &p).unwrap()).unwrap() {
            EcuPitDiagFrame::Health(h) => {
                assert_eq!(h.reset_cause, EcuResetCause::Wwdg);
                assert!(h.stub_brake);
                assert_eq!(h.cal_status, EcuCalStatus::InvalidFellBack);
                assert!(h.cal_status.is_fallback());
            }
            other => panic!("expected Health, got {other:?}"),
        }

        // A board running a stored calibration: cal_status 1 in b4-b5
        // (0x10), PowerOn, no stub.
        let p = [0, 0, 0, 0, 0, 0x11, 0, 0];
        match decode_frame(&CanFrame::new(ECU_HEALTH_ID, &p).unwrap()).unwrap() {
            EcuPitDiagFrame::Health(h) => {
                assert_eq!(h.cal_status, EcuCalStatus::Loaded);
                assert!(!h.cal_status.is_fallback());
                assert_eq!(h.reset_cause, EcuResetCause::PowerOn);
                assert!(!h.stub_brake);
            }
            other => panic!("expected Health, got {other:?}"),
        }
    }

    #[test]
    fn cal_status_is_total_over_two_bits() {
        // All four values are defined, so no input can fall through, and
        // only Loaded means a stored calibration is actually in force.
        assert_eq!(EcuCalStatus::from_bits(0), EcuCalStatus::Defaults);
        assert_eq!(EcuCalStatus::from_bits(1), EcuCalStatus::Loaded);
        assert_eq!(EcuCalStatus::from_bits(2), EcuCalStatus::InvalidFellBack);
        assert_eq!(EcuCalStatus::from_bits(3), EcuCalStatus::BadVersionFellBack);
        // High bits are masked off, not misread.
        assert_eq!(EcuCalStatus::from_bits(0xFE), EcuCalStatus::InvalidFellBack);
        assert!(!EcuCalStatus::Loaded.is_fallback());
        for s in [
            EcuCalStatus::Defaults,
            EcuCalStatus::InvalidFellBack,
            EcuCalStatus::BadVersionFellBack,
        ] {
            assert!(s.is_fallback(), "{s:?} leaves the pedals on defaults");
        }
    }

    #[test]
    fn health_decodes_stub_announces() {
        // byte4 b5-b7 = the bench-stub cluster; all clear on a flight build.
        let p = [0, 0, 0, 0, 0b1110_0000, 0, 0, 0];
        match decode_frame(&CanFrame::new(ECU_HEALTH_ID, &p).unwrap()).unwrap() {
            EcuPitDiagFrame::Health(h) => {
                assert!(h.stub_no_ams && h.stub_no_inverter && h.stub_start);
                assert!(!h.task_control);
            }
            other => panic!("expected Health, got {other:?}"),
        }
        let flight = [0, 0, 0, 0, 0b0001_1111, 1, 0, 0];
        match decode_frame(&CanFrame::new(ECU_HEALTH_ID, &flight).unwrap()).unwrap() {
            EcuPitDiagFrame::Health(h) => {
                assert!(!h.stub_no_ams && !h.stub_no_inverter && !h.stub_start);
                assert!(!h.stub_brake);
            }
            other => panic!("expected Health, got {other:?}"),
        }
    }

    #[test]
    fn status_carries_dv_mode() {
        // byte2 = 0x30 → bit4 start_button + bit5 dv_mode (#109); rtds clear.
        let p = [5, 4, 0x30, 60, 0xAC, 0x00, 0x00, 0x00];
        let frame = CanFrame::new(ECU_STATUS_ID, &p).unwrap();
        match decode_frame(&frame).unwrap() {
            EcuPitDiagFrame::Status(s) => {
                assert!(s.dv_mode);
                assert!(s.start_button);
                assert!(!s.rtds_active);
                assert!(!s.tx_dropped);
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn status_carries_tx_dropped() {
        // byte2 bit 6 (#127) — sticky since boot. It sits one bit above
        // dv_mode, so decoding the flags byte one bit short misses it
        // entirely, which is how it went unnoticed until #528's follow-up.
        let p = [5, 4, 0x40, 0, 0, 0, 0, 0];
        match decode_frame(&CanFrame::new(ECU_STATUS_ID, &p).unwrap()).unwrap() {
            EcuPitDiagFrame::Status(s) => {
                assert!(s.tx_dropped);
                assert!(!s.dv_mode, "bit 5 must not bleed into bit 6");
            }
            other => panic!("expected Status, got {other:?}"),
        }
        // And the neighbouring bit stays independent.
        let p = [5, 4, 0x20, 0, 0, 0, 0, 0];
        match decode_frame(&CanFrame::new(ECU_STATUS_ID, &p).unwrap()).unwrap() {
            EcuPitDiagFrame::Status(s) => assert!(s.dv_mode && !s.tx_dropped),
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn dv_frame_decodes() {
        // byte0 flags = bits 0,1,4 (r2d_req + cmd_fresh + r2d_confirm);
        // ts_active/brake_over_limit clear. torque 80%, rpm 1500 (LE).
        let p = [0b0001_0011u8, 80, 0xDC, 0x05, 0, 0, 0, 0];
        let frame = CanFrame::new(ECU_DV_ID, &p).unwrap();
        match decode_frame(&frame).unwrap() {
            EcuPitDiagFrame::Dv(d) => {
                assert!(d.dv_r2d_req && d.dv_cmd_fresh && d.r2d_confirm);
                assert!(!d.ts_active && !d.brake_over_limit);
                assert_eq!(d.dv_torque_pct, 80);
                assert_eq!(d.motor_rpm_mech, 1500);
            }
            other => panic!("expected Dv, got {other:?}"),
        }
    }

    #[test]
    fn inv_faults_decodes_both_layers_and_freshness() {
        // byte0 = HVIL_Open (bit 5) + the two health bits set (alive,
        // enable) => 0x23. byte1 bit 0 = OVP_Th2, the ninth L1 bit.
        // byte2 = init_ok + PwrStgFault (the cascade marker) => 0x11.
        // byte3 = follow_n 2 + Flt_Clear => 0b110 = 0x06.
        let p = [0x23, 0x01, 0x11, 0x06, 40, 7];
        match decode_frame(&CanFrame::new(ECU_INV_FAULTS_ID, &p).unwrap()).unwrap() {
            EcuPitDiagFrame::InvFaults(f) => {
                assert!(f.pwrstg_alive && f.pwrstg_enable);
                assert!(f.pwrstg_hvil_open);
                assert!(f.pwrstg_ovp_th2, "ninth L1 bit lives in byte 1");
                assert!(!f.pwrstg_ocp && !f.pwrstg_uvlo);
                assert!(f.emctrl_init_ok && f.emctrl_pwrstg_fault);
                assert_eq!(f.cmd_follow_n, 2);
                assert!(f.cmd_flt_clear);
                assert_eq!(f.inv_state_age_ms, 40);
                assert_eq!(f.inv_state_seq, 7);

                // Health bits set => not anomalies; the fault bits are.
                assert_eq!(f.l1_anomalies(), vec!["HVIL_Open", "OVP_Th2"]);
                assert_eq!(f.l2_anomalies(), vec!["PwrStgFault (see L1)"]);
                assert!(f.l1_blocks_dem_clear());
            }
            other => panic!("expected InvFaults, got {other:?}"),
        }
    }

    #[test]
    fn inv_faults_treats_cleared_health_bits_as_anomalies() {
        // All-zero payload: every fault bit clear, but alive / enable /
        // init_ok are HEALTH bits, so clear is the anomaly. Reading them
        // the same way as the fault bits would call this frame clean.
        let p = [0, 0, 0, 0, 0, 0];
        match decode_frame(&CanFrame::new(ECU_INV_FAULTS_ID, &p).unwrap()).unwrap() {
            EcuPitDiagFrame::InvFaults(f) => {
                assert_eq!(
                    f.l1_anomalies(),
                    vec!["PwrStg not alive", "PwrStg not enabled"]
                );
                assert_eq!(f.l2_anomalies(), vec!["EMCtrl not initialised"]);
                assert!(f.l1_blocks_dem_clear());
            }
            other => panic!("expected InvFaults, got {other:?}"),
        }

        // A genuinely clean power stage: health bits set, nothing else.
        let clean = [0x03, 0x00, 0x01, 0x00, 5, 200];
        match decode_frame(&CanFrame::new(ECU_INV_FAULTS_ID, &clean).unwrap()).unwrap() {
            EcuPitDiagFrame::InvFaults(f) => {
                assert!(f.l1_anomalies().is_empty());
                assert!(f.l2_anomalies().is_empty());
                assert!(
                    !f.l1_blocks_dem_clear(),
                    "clean L1 must not claim it blocks a DEM clear"
                );
            }
            other => panic!("expected InvFaults, got {other:?}"),
        }
    }

    #[test]
    fn inv_faults_rejects_a_short_frame() {
        // DLC 6 is the contract; the draft that briefly carried DLC 4 had
        // no age/seq bytes and never shipped, so a short frame is corrupt
        // rather than old-firmware.
        let short = [0x03, 0x00, 0x01, 0x00];
        assert!(decode_frame(&CanFrame::new(ECU_INV_FAULTS_ID, &short).unwrap()).is_none());
    }

    #[test]
    fn unknown_enum_values_pass_through() {
        let p = [0xFF, 0x09, 0, 0, 0, 0, 0, 0];
        let frame = CanFrame::new(ECU_STATUS_ID, &p).unwrap();
        match decode_frame(&frame).unwrap() {
            EcuPitDiagFrame::Status(s) => {
                assert_eq!(s.fsm_state, EcuFsmState::Unknown(0xFF));
                assert_eq!(s.inv_state, EcuInvState::Unknown(0x09));
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn short_frames_and_foreign_ids_reject() {
        // Short status.
        assert_eq!(
            decode_frame(&CanFrame::new(ECU_STATUS_ID, &[0, 1, 2]).unwrap()),
            None
        );
        // Short inverter (needs 7).
        assert_eq!(
            decode_frame(&CanFrame::new(ECU_INVERTER_ID, &[0; 6]).unwrap()),
            None
        );
        // Foreign ID (an AMS cell-V frame) is not ours.
        assert_eq!(decode_frame(&CanFrame::new(0x680, &[0; 8]).unwrap()), None);
    }
}
