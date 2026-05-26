// AMS pit-diag observer — Tier 2.
//
// Wraps the `can_flasher::pit_diag` library module with a Tauri
// command surface and a streaming task. Two commands:
//
//   pit_diag_enable(request)   sends the arm frame (0x7F0#DEADBEEF),
//                              waits for the ACK on 0x7F1, then
//                              spawns a reader task that decodes
//                              the 56-frame stream and emits
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

use can_flasher::pit_diag::{
    build_arm_frame, decode_frame, CellVoltageFrame, NtcTempFrame, PitDiagFrame, AMS_ACK_ID,
};
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

/// Per-frame event. Discriminated union — the `kind` tag tells the
/// frontend which variant payload to expect. Mirrors the library's
/// `PitDiagFrame` enum but with camelCase field names + a stable
/// `kind` discriminator for the JS side.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
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
    /// One of the 7 FSM/balance/boot/crash/fw-ID frames. Slice 2
    /// replaces this with typed variants; for now the raw payload
    /// makes it across so the operator can at least see the frame
    /// arriving in the bus monitor.
    Diag {
        id: u16,
        data: [u8; 8],
        dlc: u8,
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
            PitDiagFrame::Diag { id, payload, len } => Self::Diag {
                id,
                data: payload,
                dlc: len,
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
    // Profile gate — slice 1 only knows AMS. Future profiles plug
    // in here once VCU/UDV IDs land.
    if request.profile != "ams" {
        return Err(format!(
            "unknown pit-diag profile '{}': only 'ams' is supported in slice 1",
            request.profile
        ));
    }

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
    // success. If the AMS isn't on the bus or doesn't have the
    // pit-diag firmware, this surfaces as a clean error in the UI
    // rather than a silent "armed but nothing arriving".
    backend
        .send(build_arm_frame(true))
        .await
        .map_err(|e| format!("sending arm frame: {e}"))?;

    let started = Instant::now();
    let mut acked_enabled = false;
    while started.elapsed() < ACK_TIMEOUT {
        match backend.recv(STREAM_POLL_TIMEOUT).await {
            Ok(frame) if frame.id == AMS_ACK_ID => {
                if let Some(PitDiagFrame::Ack { enabled }) = decode_frame(&frame) {
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
                if msg.contains("timeout") || msg.contains("would block") {
                    continue; // poll cycle expired without a frame
                }
                return Err(format!("waiting for ACK: {err}"));
            }
        }
    }

    if !acked_enabled {
        return Err(format!(
            "no ACK from AMS within {}ms — is it on the bus, with pit-diag firmware?",
            ACK_TIMEOUT.as_millis()
        ));
    }

    // ---- Streaming task ----
    let stop_signal = Arc::new(Notify::new());
    let stop_for_task = stop_signal.clone();
    let app_for_task = app.clone();
    let profile = request.profile.clone();

    let task = tokio::spawn(async move {
        let _ = app_for_task.emit(
            STATUS_EVENT,
            &PitDiagStatus::Armed {
                profile: profile.clone(),
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
                            if let Some(record) = decode_frame(&frame) {
                                let event = PitDiagEvent::from_library(record);
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
                            if msg.contains("timeout") || msg.contains("would block") {
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
    // the AMS stops streaming, even after a tool restart left the
    // flag set. Best-effort: a failed send still falls through to
    // the task teardown.
    let interface = parse_interface(&request.interface)?;
    if let Ok(backend) =
        open_backend(interface, request.channel.as_deref(), request.bitrate)
    {
        let _ = backend.send(build_arm_frame(false)).await;
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
