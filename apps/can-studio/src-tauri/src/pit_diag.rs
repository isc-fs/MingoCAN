// Pit-diag observer — Tier 2.
//
// Wraps the `can_flasher::pit_diag` library module(s) with a Tauri
// command surface and a streaming task, generalized over a `Profile`:
// AMS (0x7F0 arm / 0x6C0..=0x6C8 stream) and ECU (0x7E0 arm /
// 0x700..=0x705 stream). uDV has no firmware protocol yet and is
// rejected with a typed "not implemented" message. Two commands:
//
//   pit_diag_enable(request)   sends the profile's arm frame, waits
//                              for the ACK on the profile's ACK ID,
//                              then spawns a reader task that decodes
//                              that profile's stream and emits
//                              `pit-diag:frame` events.
//
//   pit_diag_disable()         sends the disarm frame, waits briefly
//                              for the disarm ACK, then tears down
//                              the reader task. Idempotent — calling
//                              when nothing's running is a no-op.
//
// State management mirrors live_data.rs / bus_monitor.rs: a
// `tauri::manage`d slot holds the running task's stop-signal +
// JoinHandle so the stop command can signal a clean shutdown and
// wait for the task to actually exit.
//
// Bus-sharing note: this runs ALONGSIDE the bus monitor on Linux
// (SocketCAN multiplexes readers in-kernel). On macOS/Windows with
// PCAN/Vector backends, only one process can own the adapter, so
// the operator has to stop the bus monitor before arming pit-diag.
// Slice 1 leaves that as an operator concern; a single shared
// multiplexed backend lands later if it bites in practice.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tracing::warn;

use can_flasher::pit_diag::ecu::{
    self, EcuBrakeFrame, EcuFsmState, EcuFwInfoFrame, EcuInvState, EcuInverterFrame,
    EcuPedalsFrame, EcuPitDiagFrame, EcuStatusFrame, ECU_ACK_ID,
};
use can_flasher::pit_diag::{
    build_arm_frame, decode_frame, BalanceMaskAFrame, BalanceMaskBFrame, BootDiagFrame,
    CellVoltageFrame, FaultReason, FsmState, FsmStatusFrame, FwIdFrame, JumpReason, ModeLock,
    NtcTempFrame, PerIcPecFrame, PitDiagFrame, PollTimingFrame, AMS_ACK_ID,
};
use can_flasher::protocol::CanFrame;
use can_flasher::transport::open_backend;

use crate::flash::parse_interface;

const FRAME_EVENT: &str = "pit-diag:frame";
const STATUS_EVENT: &str = "pit-diag:status";

/// How long to wait for the AMS to ACK an arm/disarm command before
/// declaring it offline. The firmware ACKs within ~10ms in the happy
/// case; 2s leaves plenty of headroom for a noisy bus + slow USB
/// adapter without blocking the UI longer than feels responsive.
const ACK_TIMEOUT: Duration = Duration::from_millis(2_000);

/// Per-recv timeout inside the streaming task. Same idea as the bus
/// monitor — short enough that a stop-signal lands within one cycle,
/// long enough not to burn CPU on idle wakeups.
const STREAM_POLL_TIMEOUT: Duration = Duration::from_millis(50);

// ---- Shared state -----------------------------------------------

#[derive(Default)]
pub struct PitDiagState {
    inner: Mutex<Option<Running>>,
}

struct Running {
    stop_signal: Arc<Notify>,
    task: JoinHandle<()>,
}

// ---- Request from frontend --------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PitDiagRequest {
    pub interface: String,
    pub channel: Option<String>,
    pub bitrate: u32,
    /// Which ECU profile to arm. Slice 1 only supports `"ams"`; the
    /// field lives in the request from day one so the slice-5 plugin
    /// layer doesn't need a wire-protocol bump.
    #[serde(default = "default_profile")]
    pub profile: String,
}

fn default_profile() -> String {
    "ams".to_string()
}

/// The pit-diag profiles wired end-to-end. Each maps to a distinct
/// firmware stream (different arm/ACK IDs + decoders), so the arm
/// handshake and the streaming task are generalized over this enum.
/// `uDV` has no firmware stream yet and is rejected before we get here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Profile {
    Ams,
    Ecu,
}

impl Profile {
    /// Parse the request's profile string; `Err` carries the
    /// UI-facing "not implemented" message for unsupported profiles.
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "ams" => Ok(Self::Ams),
            "ecu" => Ok(Self::Ecu),
            other => Err(format!(
                "pit-diag is not implemented for profile '{other}' yet — only 'ams' and 'ecu' have an arm handshake and frame stream"
            )),
        }
    }

    /// CAN ID this profile ACKs arm/disarm commands on.
    fn ack_id(self) -> u16 {
        match self {
            Self::Ams => AMS_ACK_ID,
            Self::Ecu => ECU_ACK_ID,
        }
    }

    /// Build the arm (or disarm) frame for this profile's firmware.
    fn arm_frame(self, enable: bool) -> CanFrame {
        match self {
            Self::Ams => build_arm_frame(enable),
            Self::Ecu => ecu::build_arm_frame(enable),
        }
    }

    /// Decode a raw frame into a UI event for this profile, or `None`
    /// if the frame isn't part of this profile's stream.
    fn decode_event(self, frame: &CanFrame) -> Option<PitDiagEvent> {
        match self {
            Self::Ams => decode_frame(frame).map(PitDiagEvent::from_library),
            Self::Ecu => ecu::decode_frame(frame).map(PitDiagEvent::from_ecu),
        }
    }
}

/// Display name for the ECU FSM state — camelCase so the frontend can
/// switch on it; mirrors the firmware `VAL_` table for `0x700`.
fn ecu_fsm_state_name(s: EcuFsmState) -> String {
    match s {
        EcuFsmState::WaitInvVdcConfig => "waitInvVdcConfig".into(),
        EcuFsmState::Precharge => "precharge".into(),
        EcuFsmState::WaitStartBrake => "waitStartBrake".into(),
        EcuFsmState::R2dDelay => "r2dDelay".into(),
        EcuFsmState::WaitInvStandby => "waitInvStandby".into(),
        EcuFsmState::Active => "active".into(),
        EcuFsmState::AmsError => "amsError".into(),
        EcuFsmState::Unknown(b) => format!("unknown(0x{b:02X})"),
    }
}

/// Display name for the ECU inverter state. camelCase; mirrors the
/// firmware `VAL_` table.
fn ecu_inv_state_name(s: EcuInvState) -> String {
    match s {
        EcuInvState::Standby => "standby".into(),
        EcuInvState::Ready => "ready".into(),
        EcuInvState::Unknown(b) => format!("unknown(0x{b:02X})"),
    }
}

// ---- Streamed events --------------------------------------------

/// Status events for the arm / disarm / error lifecycle. Lets the
/// frontend show a clear "armed / waiting / failed" indicator
/// independent of the per-frame stream.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PitDiagStatus {
    /// AMS ACKed the arm frame; streaming task running.
    Armed { profile: String },
    /// Reader task exited cleanly (operator hit Disable, or app
    /// closed).
    Stopped,
    /// Something went sideways — backend error, ACK timeout, IPC
    /// emit failure. The reader task tears down before this fires.
    Error { message: String },
}

/// String form of the FSM state, ready for direct UI rendering. Stays
/// in lockstep with `can_flasher::pit_diag::FsmState` but uses
/// camelCase so the frontend can switch on the value without a
/// translation layer.
fn fsm_state_name(s: FsmState) -> String {
    match s {
        FsmState::Start => "start".into(),
        FsmState::Precharge => "precharge".into(),
        FsmState::Transition => "transition".into(),
        FsmState::Run => "run".into(),
        FsmState::Charge => "charge".into(),
        FsmState::Error => "error".into(),
        FsmState::Unknown(b) => format!("unknown(0x{b:02X})"),
    }
}

fn mode_lock_name(m: ModeLock) -> String {
    match m {
        ModeLock::Undecided => "undecided".into(),
        ModeLock::Car => "car".into(),
        ModeLock::Charger => "charger".into(),
        ModeLock::Unknown(b) => format!("unknown(0x{b:02X})"),
    }
}

/// String form of the latched-ERROR fault reason. camelCase so the
/// frontend can switch on the value; mirrors the firmware's
/// `FaultReason` enum names (#276).
fn fault_reason_name(r: FaultReason) -> String {
    match r {
        FaultReason::None => "none".into(),
        FaultReason::ForceError => "forceError".into(),
        FaultReason::BmsModuleOffline => "bmsModuleOffline".into(),
        FaultReason::BmsStale => "bmsStale".into(),
        FaultReason::CellUnderVoltage => "cellUnderVoltage".into(),
        FaultReason::CellOverVoltage => "cellOverVoltage".into(),
        FaultReason::CellUnderTemp => "cellUnderTemp".into(),
        FaultReason::CellOverTemp => "cellOverTemp".into(),
        FaultReason::CurrentSensorFault => "currentSensorFault".into(),
        FaultReason::CurrentStale => "currentStale".into(),
        FaultReason::CurrentOverLimit => "currentOverLimit".into(),
        FaultReason::VcuStale => "vcuStale".into(),
        FaultReason::FsmError => "fsmError".into(),
        FaultReason::Unknown(b) => format!("unknown(0x{b:02X})"),
    }
}

/// String form of the boot jump-reason (`0x6C4`). camelCase, mirrors
/// the library `JumpReason` enum.
fn jump_reason_name(j: JumpReason) -> String {
    match j {
        JumpReason::PowerOn => "powerOn".into(),
        JumpReason::CanTrigger => "canTrigger".into(),
        JumpReason::Manual => "manual".into(),
        JumpReason::Unknown(v) => format!("unknown(0x{v:08X})"),
    }
}

/// Per-frame event. Discriminated union — the `kind` tag tells the
/// frontend which variant payload to expect. Mirrors the library's
/// `PitDiagFrame` enum but with camelCase field names + a stable
/// `kind` discriminator for the JS side.
#[derive(Debug, Clone, Serialize)]
// `rename_all` renames the variant names (the `kind` tag values);
// `rename_all_fields` is REQUIRED to also camelCase the *fields* of
// each struct variant — container `rename_all` alone does not cascade
// into variant fields, so without this the wire carries snake_case
// (`frame_idx`, `fsm_state`, `apps1_raw`, …) while the whole frontend
// reads camelCase, leaving every field `undefined`. That broke the
// live decode for both AMS and ECU panels.
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PitDiagEvent {
    /// `payload[0]` from the 0x7F1 ACK frame. Mostly used during
    /// arm/disarm transitions; spurious ACKs on a quiet bus just
    /// echo the current state.
    Ack {
        /// `true` after a successful arm, `false` after a disarm.
        enabled: bool,
    },
    /// One of the 24 cell-voltage frames decoded.
    CellVoltage {
        frame_idx: u8,
        first_cell: u16,
        voltages_mv: [u16; 4],
    },
    /// One of the 25 NTC-temperature frames decoded.
    NtcTemp {
        frame_idx: u8,
        first_ntc: u16,
        temps_c: [i8; 8],
    },
    /// `0x6C0` — FSM extended status. Stringified state + mode +
    /// fault-reason for the frontend's convenience; cockpit flags
    /// as bools; PEC count + fault detail as raw integers.
    FsmStatus {
        state: String,
        mode_locked: String,
        tsms: bool,
        dash_chg: bool,
        ams_ok: bool,
        pec_error_total: u16,
        fault_reason: String,
        fault_detail: u8,
    },
    /// `0x6C1` — V-poll + T-sweep timing telemetry.
    PollTiming {
        last_v_poll_ms: u16,
        worst_v_poll_ms: u16,
        t_sweep_fail_mask: u32,
    },
    /// `0x6C2` — balance DCC mask, cells 0..=63 (LE u64, sent as a
    /// decimal string since JSON numbers can't safely hold a full
    /// u64 in JS).
    BalanceMaskA { dcc_lo: String },
    /// `0x6C3` — balance DCC mask hi (cells 64..=94) + cycle counters.
    BalanceMaskB {
        dcc_hi: u32,
        cycles_total: u16,
        cycles_active: u16,
    },
    /// `0x6C4` — boot diagnostics.
    BootDiag {
        jump_reason: String,
        app_init_progress: u8,
        fdcan1_start_result: u32,
    },
    /// `0x6C5` — crash post-mortem from the previous boot.
    PostMortem {
        stack_overflow_seen: bool,
        watermark_low_byte: u8,
        task_addr_lo: u32,
        malloc_failed_count: u16,
        /// `true` when nothing crashed — lets the UI suppress the
        /// banner without re-deriving the predicate.
        clean: bool,
    },
    /// `0x6C6` — firmware identity.
    FwId {
        version_major: u8,
        version_minor: u8,
        version_patch: u8,
        git_hash: [u8; 4],
        bl_node_id: u8,
    },
    /// `0x6C7` / `0x6C8` — per-IC PEC error counts.
    PerIcPec {
        first_ic: u8,
        valid: u8,
        counts: [u8; 8],
    },

    // ---- ECU profile (0x700..=0x705) ----
    /// ECU `0x700` — FSM / inverter state, cockpit flags, torque, min cell-V.
    EcuStatus {
        fsm_state: String,
        inv_state: String,
        ev_2_3: bool,
        t11_8_9: bool,
        rtds_active: bool,
        ok_precharge: bool,
        start_button: bool,
        torque_pct: u8,
        v_cell_min_mv: u16,
        torque_cmd: i16,
    },
    /// ECU `0x701` — APPS pedal channels + brake raw ADC.
    EcuPedals {
        apps1_raw: u16,
        apps2_raw: u16,
        brake_raw: u16,
        apps1_pct: u8,
        apps2_pct: u8,
    },
    /// ECU `0x705` — physical brake pressure (deci-bar) + brake %.
    EcuBrake {
        brake_pressure_dbar: u16,
        brake_pct: u8,
    },
    /// ECU `0x702` — inverter DC-bus voltage, RPM (signed), error code.
    EcuInverter {
        dc_bus_voltage: u16,
        inv_rpm: i32,
        inv_error: u8,
    },
    /// ECU `0x703` — firmware semver + git-hash prefix.
    EcuFwInfo {
        version_major: u8,
        version_minor: u8,
        version_patch: u8,
        git_hash: [u8; 4],
    },
}

impl PitDiagEvent {
    fn from_library(frame: PitDiagFrame) -> Self {
        match frame {
            PitDiagFrame::Ack { enabled } => Self::Ack { enabled },
            PitDiagFrame::CellVoltage(CellVoltageFrame {
                frame_idx,
                first_cell,
                voltages_mv,
            }) => Self::CellVoltage {
                frame_idx,
                first_cell,
                voltages_mv,
            },
            PitDiagFrame::NtcTemp(NtcTempFrame {
                frame_idx,
                first_ntc,
                temps_c,
            }) => Self::NtcTemp {
                frame_idx,
                first_ntc,
                temps_c,
            },
            PitDiagFrame::FsmStatus(FsmStatusFrame {
                state,
                mode_locked,
                tsms,
                dash_chg,
                ams_ok,
                pec_error_total,
                fault_reason,
                fault_detail,
            }) => Self::FsmStatus {
                state: fsm_state_name(state),
                mode_locked: mode_lock_name(mode_locked),
                tsms,
                dash_chg,
                ams_ok,
                pec_error_total,
                fault_reason: fault_reason_name(fault_reason),
                fault_detail,
            },
            PitDiagFrame::PollTiming(PollTimingFrame {
                last_v_poll_ms,
                worst_v_poll_ms,
                t_sweep_fail_mask,
            }) => Self::PollTiming {
                last_v_poll_ms,
                worst_v_poll_ms,
                t_sweep_fail_mask,
            },
            PitDiagFrame::BalanceMaskA(BalanceMaskAFrame { dcc_lo }) => Self::BalanceMaskA {
                // u64 as a decimal string — JS numbers lose precision
                // above 2^53, and the discharge mask uses the full 64.
                dcc_lo: dcc_lo.to_string(),
            },
            PitDiagFrame::BalanceMaskB(BalanceMaskBFrame {
                dcc_hi,
                cycles_total,
                cycles_active,
            }) => Self::BalanceMaskB {
                dcc_hi,
                cycles_total,
                cycles_active,
            },
            PitDiagFrame::BootDiag(BootDiagFrame {
                jump_reason,
                app_init_progress,
                fdcan1_start_result,
            }) => Self::BootDiag {
                jump_reason: jump_reason_name(jump_reason),
                app_init_progress,
                fdcan1_start_result,
            },
            PitDiagFrame::PostMortem(pm) => Self::PostMortem {
                stack_overflow_seen: pm.stack_overflow_seen,
                watermark_low_byte: pm.watermark_low_byte,
                task_addr_lo: pm.task_addr_lo,
                malloc_failed_count: pm.malloc_failed_count,
                clean: pm.is_clean(),
            },
            PitDiagFrame::FwId(FwIdFrame {
                version_major,
                version_minor,
                version_patch,
                git_hash,
                bl_node_id,
            }) => Self::FwId {
                version_major,
                version_minor,
                version_patch,
                git_hash,
                bl_node_id,
            },
            PitDiagFrame::PerIcPec(PerIcPecFrame {
                first_ic,
                counts,
                valid,
            }) => Self::PerIcPec {
                first_ic,
                valid,
                counts,
            },
        }
    }

    /// Map a decoded ECU library frame into a UI event. Mirrors
    /// `from_library` for the `0x700..=0x705` stream; enum states are
    /// stringified so the frontend switches on names, not numbers.
    fn from_ecu(frame: EcuPitDiagFrame) -> Self {
        match frame {
            EcuPitDiagFrame::Ack { enabled } => Self::Ack { enabled },
            EcuPitDiagFrame::Status(EcuStatusFrame {
                fsm_state,
                inv_state,
                ev_2_3,
                t11_8_9,
                rtds_active,
                ok_precharge,
                start_button,
                torque_pct,
                v_cell_min_mv,
                torque_cmd,
            }) => Self::EcuStatus {
                fsm_state: ecu_fsm_state_name(fsm_state),
                inv_state: ecu_inv_state_name(inv_state),
                ev_2_3,
                t11_8_9,
                rtds_active,
                ok_precharge,
                start_button,
                torque_pct,
                v_cell_min_mv,
                torque_cmd,
            },
            EcuPitDiagFrame::Pedals(EcuPedalsFrame {
                apps1_raw,
                apps2_raw,
                brake_raw,
                apps1_pct,
                apps2_pct,
            }) => Self::EcuPedals {
                apps1_raw,
                apps2_raw,
                brake_raw,
                apps1_pct,
                apps2_pct,
            },
            EcuPitDiagFrame::Brake(EcuBrakeFrame {
                brake_pressure_dbar,
                brake_pct,
            }) => Self::EcuBrake {
                brake_pressure_dbar,
                brake_pct,
            },
            EcuPitDiagFrame::Inverter(EcuInverterFrame {
                dc_bus_voltage,
                inv_rpm,
                inv_error,
            }) => Self::EcuInverter {
                dc_bus_voltage,
                inv_rpm,
                inv_error,
            },
            EcuPitDiagFrame::FwInfo(EcuFwInfoFrame {
                fw_major,
                fw_minor,
                fw_patch,
                git_hash,
            }) => Self::EcuFwInfo {
                version_major: fw_major,
                version_minor: fw_minor,
                version_patch: fw_patch,
                git_hash,
            },
        }
    }
}

// ---- Commands ---------------------------------------------------

#[tauri::command]
pub async fn pit_diag_enable(
    app: AppHandle,
    state: State<'_, PitDiagState>,
    request: PitDiagRequest,
) -> Result<(), String> {
    // Profile gate — AMS (0x7F0/0x6Cx) and ECU (0x7E0/0x70x) each have
    // their own arm handshake + frame stream. uDV has no firmware
    // protocol yet, so `parse` rejects it with a clean, typed message
    // the view renders as its "not available yet" placeholder rather
    // than attempting to arm.
    let profile = Profile::parse(&request.profile)?;
    let ack_id = profile.ack_id();

    // Idempotency — stop any prior session first. Mirrors bus_monitor.
    {
        let mut slot = state.inner.lock().await;
        if let Some(prev) = slot.take() {
            prev.stop_signal.notify_waiters();
            let _ = prev.task.await;
        }
    }

    let interface = parse_interface(&request.interface)?;

    // Open the backend synchronously so we can fail-fast on bad
    // adapter config rather than swallow it inside the task.
    let backend = open_backend(interface, request.channel.as_deref(), request.bitrate)
        .map_err(|e| format!("opening backend: {e}"))?;

    // Send the arm frame and wait for the ACK before declaring
    // success. If the board isn't on the bus or doesn't have the
    // pit-diag firmware, this surfaces as a clean error in the UI
    // rather than a silent "armed but nothing arriving".
    backend
        .send(profile.arm_frame(true))
        .await
        .map_err(|e| format!("sending arm frame: {e}"))?;

    let started = Instant::now();
    let mut acked_enabled = false;
    while started.elapsed() < ACK_TIMEOUT {
        match backend.recv(STREAM_POLL_TIMEOUT).await {
            Ok(frame) if frame.id == ack_id => {
                if let Some(PitDiagEvent::Ack { enabled }) = profile.decode_event(&frame) {
                    acked_enabled = enabled;
                    // Surface the ACK to the frontend immediately
                    // so an enabled=true ACK is visible before any
                    // stream frames land.
                    let _ = app.emit(FRAME_EVENT, &PitDiagEvent::Ack { enabled });
                    if enabled {
                        break;
                    }
                }
            }
            Ok(_) => {
                // Frame from some other ID — ignore during arm wait.
            }
            Err(err) => {
                let msg = err.to_string().to_lowercase();
                if msg.contains("timed out")
                    || msg.contains("timeout")
                    || msg.contains("would block")
                {
                    continue; // poll cycle expired without a frame
                }
                return Err(format!("waiting for ACK: {err}"));
            }
        }
    }

    if !acked_enabled {
        return Err(format!(
            "no ACK from the board within {}ms — is it on the bus, with pit-diag firmware?",
            ACK_TIMEOUT.as_millis()
        ));
    }

    // ---- Streaming task ----
    let stop_signal = Arc::new(Notify::new());
    let stop_for_task = stop_signal.clone();
    let app_for_task = app.clone();
    let profile_label = request.profile.clone();

    let task = tokio::spawn(async move {
        let _ = app_for_task.emit(
            STATUS_EVENT,
            &PitDiagStatus::Armed {
                profile: profile_label,
            },
        );
        let stop = stop_for_task.notified();
        tokio::pin!(stop);

        loop {
            tokio::select! {
                _ = &mut stop => {
                    let _ = app_for_task.emit(STATUS_EVENT, &PitDiagStatus::Stopped);
                    return;
                }
                result = backend.recv(STREAM_POLL_TIMEOUT) => {
                    match result {
                        Ok(frame) => {
                            if let Some(event) = profile.decode_event(&frame) {
                                if let Err(err) = app_for_task.emit(FRAME_EVENT, &event) {
                                    warn!(?err, "pit_diag: emit failed; stopping");
                                    let _ = app_for_task.emit(
                                        STATUS_EVENT,
                                        &PitDiagStatus::Error {
                                            message: format!("emit failed: {err}"),
                                        },
                                    );
                                    return;
                                }
                            }
                            // Frames outside the pit-diag ID space
                            // (decode_frame returned None) are ignored —
                            // the bus monitor view is the place to see
                            // non-pit-diag traffic.
                        }
                        Err(err) => {
                            // Timeouts are benign — quiet bus during the
                            // poll window. Don't spam the frontend.
                            let msg = err.to_string().to_lowercase();
                            if msg.contains("timed out") || msg.contains("timeout") || msg.contains("would block") {
                                continue;
                            }
                            let _ = app_for_task.emit(
                                STATUS_EVENT,
                                &PitDiagStatus::Error { message: err.to_string() },
                            );
                            return;
                        }
                    }
                }
            }
        }
    });

    let mut slot = state.inner.lock().await;
    *slot = Some(Running { stop_signal, task });
    Ok(())
}

#[tauri::command]
pub async fn pit_diag_disable(
    app: AppHandle,
    state: State<'_, PitDiagState>,
    request: PitDiagRequest,
) -> Result<(), String> {
    // Send the disarm frame regardless of whether we have a running
    // task — operators expect Disable to always emit the disarm so
    // the board stops streaming, even after a tool restart left the
    // flag set. Best-effort: a failed send (or an unknown profile)
    // still falls through to the task teardown.
    let interface = parse_interface(&request.interface)?;
    if let Ok(profile) = Profile::parse(&request.profile) {
        if let Ok(backend) = open_backend(interface, request.channel.as_deref(), request.bitrate) {
            let _ = backend.send(profile.arm_frame(false)).await;
        }
    }

    // ---- Tear down any running reader task ----
    let mut slot = state.inner.lock().await;
    if let Some(prev) = slot.take() {
        prev.stop_signal.notify_waiters();
        let _ = prev.task.await;
    }
    // Re-fire a Stopped status in case the task hadn't yet — gives
    // the frontend a deterministic state to render to.
    let _ = app.emit(STATUS_EVENT, &PitDiagStatus::Stopped);
    Ok(())
}
