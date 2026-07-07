//! uDV (driverless) pit-diag observer protocol.
//!
//! Companion to the AMS + ECU observers. The uDV re-emits its autonomous-
//! system state as CAN frames on the shared ACU bus (FDCAN2) so a bench with
//! just a CAN tool can read it — no ROS / DVPC needed. Source of truth:
//! `Core/Inc/pit_diag.h` + `Core/Src/pit_diag.cpp` in `IFS08-DV-uDV` (#106).
//!
//! ## Wire protocol
//!
//! - **Arm**:  emit `0x7DE` with payload `DE AD BE EF` (big-endian magic
//!   `0xDEADBEEF`). Sticky — enables the stream for the power cycle. (The
//!   firmware has no disarm frame; it clears on reboot. `PITDIAG_STREAM_ALWAYS`
//!   compiles it always-on.)
//! - **Stream IDs once armed (~10 Hz, fwinfo ~1 Hz)** — all 8 bytes,
//!   **little-endian**, IDs deconflicted from the ECU (`0x7xx`) + AMS (`0x6xx`):
//!   - `0x7A0` status: AS state, 10-bit signal mask, mission, EBS-init, ASSI.
//!   - `0x7A1` res: RES status/bits, radio quality, frame age, steering.
//!   - `0x7A2` pipe: /dv/status + age, accel/steer cmds + age, setup bits.
//!   - `0x7A3` health: heap (words), task-armed mask, reset flags, uptime.
//!   - `0x7A4` fwinfo: git short hash, stub mask, heap size.
//!
//! Bit masks (`signals`, `res_bits`, `setup_bits`, `task_mask`, …) are kept
//! raw here; the consumer decodes individual bits for display, mirroring how
//! the pit tool renders the AMS/ECU flag fields.

use crate::protocol::CanFrame;

// ---- Wire-level constants ----------------------------------------

/// CAN ID the uDV listens on for the arm command.
pub const UDV_ARM_ID: u16 = 0x7DE;
/// Arm payload — big-endian magic `0xDEADBEEF` (same sentinel as AMS/ECU).
pub const UDV_ARM_ENABLE_PAYLOAD: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];

/// `0x7A0` — AS state, signal mask, mission, EBS-init, ASSI mode.
pub const UDV_STATUS_ID: u16 = 0x7A0;
/// `0x7A1` — RES status/bits, radio, frame age, steering.
pub const UDV_RES_ID: u16 = 0x7A1;
/// `0x7A2` — /dv pipe: status + control commands + ages + setup bits.
pub const UDV_PIPE_ID: u16 = 0x7A2;
/// `0x7A3` — firmware health (heap, task mask, reset flags, uptime).
pub const UDV_HEALTH_ID: u16 = 0x7A3;
/// `0x7A4` — firmware identity (git hash + stub mask + heap size).
pub const UDV_FWINFO_ID: u16 = 0x7A4;

/// Cyclic (~10 Hz) frames per scan: status / res / pipe / health. `fwinfo`
/// is ~1 Hz and not counted here (parity with the ECU health frame).
pub const UDV_EXPECTED_FRAMES_PER_SCAN: usize = 4;

/// `res_age_ms` / `dv_age_ms` sentinel — `0xFFFF` means "never seen".
pub const UDV_AGE_NEVER: u16 = 0xFFFF;

// ---- Enums (mirror the firmware VAL_ tables) ---------------------

/// Autonomous-system state (`0x7A0` byte 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UdvAsState {
    Off,
    Ready,
    Driving,
    Emergency,
    Finished,
    Other(u8),
}
impl UdvAsState {
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Off,
            1 => Self::Ready,
            2 => Self::Driving,
            3 => Self::Emergency,
            4 => Self::Finished,
            other => Self::Other(other),
        }
    }
}

/// EBS-init sub-state (`0x7A0` byte 4, 0..=8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UdvEbsInit {
    Start,
    WaitLow,
    CheckPressure,
    WaitTs,
    CheckAct1,
    WaitInter,
    CheckAct2,
    Failed,
    Done,
    Other(u8),
}
impl UdvEbsInit {
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Start,
            1 => Self::WaitLow,
            2 => Self::CheckPressure,
            3 => Self::WaitTs,
            4 => Self::CheckAct1,
            5 => Self::WaitInter,
            6 => Self::CheckAct2,
            7 => Self::Failed,
            8 => Self::Done,
            other => Self::Other(other),
        }
    }
}

/// ASSI (autonomous status indicator) mode (`0x7A0` byte 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UdvAssi {
    Off,
    Ready,
    Driving,
    Emergency,
    Finished,
    Other(u8),
}
impl UdvAssi {
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Off,
            1 => Self::Ready,
            2 => Self::Driving,
            3 => Self::Emergency,
            4 => Self::Finished,
            other => Self::Other(other),
        }
    }
}

/// RES (remote-emergency-stop) status (`0x7A1` byte 1, signed −2..=2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UdvResStatus {
    None,
    Timeout,
    Ok,
    Estop,
    Go,
    Other(i8),
}
impl UdvResStatus {
    #[must_use]
    pub fn from_byte(b: i8) -> Self {
        match b {
            -2 => Self::None,
            -1 => Self::Timeout,
            0 => Self::Ok,
            1 => Self::Estop,
            2 => Self::Go,
            other => Self::Other(other),
        }
    }
}

/// Steering-motor state (`0x7A1` byte 6, signed −1/0/1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UdvSteerMotor {
    Emergency,
    Off,
    On,
    Other(i8),
}
impl UdvSteerMotor {
    #[must_use]
    pub fn from_byte(b: i8) -> Self {
        match b {
            -1 => Self::Emergency,
            0 => Self::Off,
            1 => Self::On,
            other => Self::Other(other),
        }
    }
}

// ---- Frame records -----------------------------------------------

/// `0x7A0` — status. `signals` is the raw 10-bit mask (b0 ASMS, b1 TS,
/// b2 SDC_open, b3 EBS_act, b4 ABS_ok, b5 brakes, b6 mission_sel, b7 R2D,
/// b8 standstill, b9 finished); `stub_mask` bits: b0 EBS, b1 DVPC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdvStatusFrame {
    pub as_state: UdvAsState,
    pub signals: u16,
    /// Mission id, `-1` = none.
    pub mission_id: i8,
    pub ebs_init: UdvEbsInit,
    pub stub_mask: u8,
    pub assi: UdvAssi,
    pub diag_armed: bool,
}

/// `0x7A1` — RES + steering. `bits`: b0 estop, b1 go, b2 pre_alarm,
/// b3 brake_over_limit, b4 listen_go, b5 sdc_res_open, b6 ts_active_can.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdvResFrame {
    /// Raw `0x191` data[0] passthrough.
    pub raw_0x191: u8,
    pub res_status: UdvResStatus,
    pub bits: u8,
    pub radio_quality: u8,
    /// RES frame age, ms (`0xFFFF` = never seen).
    pub res_age_ms: u16,
    pub steer_motor: UdvSteerMotor,
    pub lws_status: u8,
}

/// `0x7A2` — /dv pipe. `setup_bits`: b0 in_progress, b1 ready, b2 going,
/// b3 emergency, b4 finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdvPipeFrame {
    pub dv_status: u8,
    pub dv_age_ms: u16,
    /// Accel command, signed percent.
    pub accel_cmd_pct: i8,
    /// Steering command, signed (normalised × 100).
    pub steer_cmd: i8,
    pub ctrl_age_ms: u16,
    pub setup_bits: u8,
}

/// `0x7A3` — health. Heap is reported in **words** (÷4); multiply by 4 for
/// bytes. `task_mask`: b0 IMU, b1 CAN, b2 APP. `flags`: b0 IWDG-reset boot,
/// b1 emergency latched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdvHealthFrame {
    pub free_heap_words: u16,
    pub min_free_heap_words: u16,
    pub task_mask: u8,
    pub flags: u8,
    /// Stalled task index, `-1` = none.
    pub stalled_task: i8,
    /// Seconds since boot (wraps at 255).
    pub uptime_s: u8,
}

/// `0x7A4` — firmware identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdvFwInfoFrame {
    /// Git short hash (little-endian u32 on the wire).
    pub git_hash: u32,
    pub stub_mask: u8,
    pub heap_size_kb: u8,
    pub uptime_s: u8,
}

/// A decoded uDV pit-diag frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UdvPitDiagFrame {
    /// `0x7A0` — AS status.
    Status(UdvStatusFrame),
    /// `0x7A1` — RES + steering.
    Res(UdvResFrame),
    /// `0x7A2` — /dv pipe.
    Pipe(UdvPipeFrame),
    /// `0x7A3` — firmware health.
    Health(UdvHealthFrame),
    /// `0x7A4` — firmware identity.
    FwInfo(UdvFwInfoFrame),
}

// ---- Encode / decode ---------------------------------------------

/// Build the arm frame (`0x7DE` + `DE AD BE EF`). There is no disarm frame —
/// the firmware clears the sticky flag on reboot.
#[must_use]
pub fn build_arm_frame() -> CanFrame {
    CanFrame::new(UDV_ARM_ID, &UDV_ARM_ENABLE_PAYLOAD).expect("4-byte payload always fits")
}

/// Decode a raw CAN frame into a uDV pit-diag record, or `None` if the ID
/// isn't part of the stream or the payload is too short.
#[must_use]
pub fn decode_frame(frame: &CanFrame) -> Option<UdvPitDiagFrame> {
    let p = frame.payload();
    match frame.id {
        UDV_STATUS_ID => {
            if p.len() < 8 {
                return None;
            }
            Some(UdvPitDiagFrame::Status(UdvStatusFrame {
                as_state: UdvAsState::from_byte(p[0]),
                signals: u16::from_le_bytes([p[1], p[2]]),
                mission_id: p[3] as i8,
                ebs_init: UdvEbsInit::from_byte(p[4]),
                stub_mask: p[5],
                assi: UdvAssi::from_byte(p[6]),
                diag_armed: p[7] != 0,
            }))
        }
        UDV_RES_ID => {
            if p.len() < 8 {
                return None;
            }
            Some(UdvPitDiagFrame::Res(UdvResFrame {
                raw_0x191: p[0],
                res_status: UdvResStatus::from_byte(p[1] as i8),
                bits: p[2],
                radio_quality: p[3],
                res_age_ms: u16::from_le_bytes([p[4], p[5]]),
                steer_motor: UdvSteerMotor::from_byte(p[6] as i8),
                lws_status: p[7],
            }))
        }
        UDV_PIPE_ID => {
            if p.len() < 8 {
                return None;
            }
            Some(UdvPitDiagFrame::Pipe(UdvPipeFrame {
                dv_status: p[0],
                dv_age_ms: u16::from_le_bytes([p[1], p[2]]),
                accel_cmd_pct: p[3] as i8,
                steer_cmd: p[4] as i8,
                ctrl_age_ms: u16::from_le_bytes([p[5], p[6]]),
                setup_bits: p[7],
            }))
        }
        UDV_HEALTH_ID => {
            if p.len() < 8 {
                return None;
            }
            Some(UdvPitDiagFrame::Health(UdvHealthFrame {
                free_heap_words: u16::from_le_bytes([p[0], p[1]]),
                min_free_heap_words: u16::from_le_bytes([p[2], p[3]]),
                task_mask: p[4],
                flags: p[5],
                stalled_task: p[6] as i8,
                uptime_s: p[7],
            }))
        }
        UDV_FWINFO_ID => {
            if p.len() < 8 {
                return None;
            }
            Some(UdvPitDiagFrame::FwInfo(UdvFwInfoFrame {
                git_hash: u32::from_le_bytes([p[0], p[1], p[2], p[3]]),
                stub_mask: p[4],
                heap_size_kb: p[5],
                uptime_s: p[6],
            }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_decodes() {
        // as=2 (Driving), signals=0x0203 (b0 ASMS, b1 TS, b9 finished),
        // mission=3, ebs=8 (Done), stub=0x01 (EBS), assi=2, armed=1.
        let p = [2, 0x03, 0x02, 3, 8, 0x01, 2, 1];
        let f = CanFrame::new(UDV_STATUS_ID, &p).unwrap();
        match decode_frame(&f).unwrap() {
            UdvPitDiagFrame::Status(s) => {
                assert_eq!(s.as_state, UdvAsState::Driving);
                assert_eq!(s.signals, 0x0203);
                assert_eq!(s.mission_id, 3);
                assert_eq!(s.ebs_init, UdvEbsInit::Done);
                assert_eq!(s.stub_mask, 0x01);
                assert_eq!(s.assi, UdvAssi::Driving);
                assert!(s.diag_armed);
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn res_decodes_signed_fields() {
        // status=-1 (Timeout), steer=-1 (Emergency), age=0xFFFF (never).
        let p = [0x22, 0xFF, 0x40, 88, 0xFF, 0xFF, 0xFF, 0x07];
        let f = CanFrame::new(UDV_RES_ID, &p).unwrap();
        match decode_frame(&f).unwrap() {
            UdvPitDiagFrame::Res(r) => {
                assert_eq!(r.raw_0x191, 0x22);
                assert_eq!(r.res_status, UdvResStatus::Timeout);
                assert_eq!(r.bits, 0x40);
                assert_eq!(r.res_age_ms, UDV_AGE_NEVER);
                assert_eq!(r.steer_motor, UdvSteerMotor::Emergency);
            }
            other => panic!("expected Res, got {other:?}"),
        }
    }

    #[test]
    fn pipe_and_health_decode() {
        let p = [1, 0x2C, 0x01, 100i8 as u8, (-50i8) as u8, 0x10, 0x00, 0x0F];
        match decode_frame(&CanFrame::new(UDV_PIPE_ID, &p).unwrap()).unwrap() {
            UdvPitDiagFrame::Pipe(pi) => {
                assert_eq!(pi.dv_age_ms, 300);
                assert_eq!(pi.accel_cmd_pct, 100);
                assert_eq!(pi.steer_cmd, -50);
                assert_eq!(pi.ctrl_age_ms, 16);
                assert_eq!(pi.setup_bits, 0x0F);
            }
            other => panic!("expected Pipe, got {other:?}"),
        }
        let h = [0x00, 0x04, 0x00, 0x02, 0x05, 0x02, 0xFF, 42];
        match decode_frame(&CanFrame::new(UDV_HEALTH_ID, &h).unwrap()).unwrap() {
            UdvPitDiagFrame::Health(he) => {
                assert_eq!(he.free_heap_words, 1024);
                assert_eq!(he.min_free_heap_words, 512);
                assert_eq!(he.task_mask, 0x05);
                assert_eq!(he.flags, 0x02);
                assert_eq!(he.stalled_task, -1);
                assert_eq!(he.uptime_s, 42);
            }
            other => panic!("expected Health, got {other:?}"),
        }
    }

    #[test]
    fn arm_frame_is_deadbeef() {
        let f = build_arm_frame();
        assert_eq!(f.id, UDV_ARM_ID);
        assert_eq!(f.payload(), &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn unrelated_id_returns_none() {
        assert!(decode_frame(&CanFrame::new(0x700, &[0; 8]).unwrap()).is_none());
    }
}
