//! ECU (VCU) pit-diag observer protocol.
//!
//! Companion to the AMS observer in the parent module. The ECU exposes
//! a *separate*, much smaller stream than the AMS: when armed it emits
//! five frames at 100 ms carrying the vehicle-control picture — FSM /
//! inverter state, the two APPS pedal channels, the brake, the
//! inverter DC-bus / RPM / error, and a firmware-ID frame.
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
//!     inverter error code.
//!   - `0x703` — fwinfo: firmware semver + first 4 bytes of the git hash.
//!   - `0x705` — brake: physical brake pressure (×0.1 bar) + brake %.
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

/// Number of stream frames emitted per 100 ms scan when armed.
pub const ECU_EXPECTED_FRAMES_PER_SCAN: usize = 5;

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
/// `App_State`; the firmware only models the two values it gates on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EcuInvState {
    /// 3 — inverter standby.
    Standby,
    /// 4 — inverter ready.
    Ready,
    /// Any value outside the known table.
    Unknown(u8),
}

impl EcuInvState {
    /// Decode the raw inverter-state byte.
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        match b {
            3 => Self::Standby,
            4 => Self::Ready,
            other => Self::Unknown(other),
        }
    }
}

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
    /// Inverter error / state code (DEM_Code).
    pub inv_error: u8,
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
    /// `0x703` — firmware identity.
    FwInfo(EcuFwInfoFrame),
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
        // dc_bus=0x0258 (600V), rpm=-1000 (BE i32), err=0x07.
        let rpm = (-1000i32).to_be_bytes();
        let p = [0x02, 0x58, rpm[0], rpm[1], rpm[2], rpm[3], 0x07];
        let frame = CanFrame::new(ECU_INVERTER_ID, &p).unwrap();
        match decode_frame(&frame).unwrap() {
            EcuPitDiagFrame::Inverter(inv) => {
                assert_eq!(inv.dc_bus_voltage, 600);
                assert_eq!(inv.inv_rpm, -1000);
                assert_eq!(inv.inv_error, 0x07);
            }
            other => panic!("expected Inverter, got {other:?}"),
        }
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
