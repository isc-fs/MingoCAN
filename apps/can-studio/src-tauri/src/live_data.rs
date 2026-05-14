// Tauri commands for the streaming live-data view.
//
// Two commands plus a shared `LiveDataState` resource:
//
//   live_data_start(request)  spawns a background task that opens a
//                              session, fires CMD_LIVE_DATA_START,
//                              subscribes to NOTIFY_LIVE_DATA, parses
//                              each snapshot into a serializable
//                              shape, and emits it to the frontend
//                              as `live_data:event` events. Returns
//                              once the initial ACK lands; the task
//                              keeps running until live_data_stop is
//                              invoked.
//   live_data_stop()           signals the running task via a
//                              tokio::Notify, then awaits its
//                              JoinHandle. Idempotent — calling it
//                              twice or while nothing is running is
//                              a no-op.
//
// Same orchestration as `can_flasher::cli::diagnose::run_live_data`,
// but driven by frontend buttons instead of Ctrl-C.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{broadcast::error::RecvError, Mutex, Notify};
use tokio::task::JoinHandle;
use tracing::warn;

use can_flasher::cli::InterfaceType;
use can_flasher::protocol::commands::{cmd_live_data_start, cmd_live_data_stop};
use can_flasher::protocol::opcodes::NotifyOpcode;
use can_flasher::protocol::records::LiveDataSnapshot;
use can_flasher::protocol::Response;
use can_flasher::session::{Session, SessionConfig};
use can_flasher::transport::open_backend;

const EVENT_NAME: &str = "live_data:event";

// ---- Shared state ----

#[derive(Default)]
pub struct LiveDataState {
    inner: Mutex<Option<Running>>,
}

struct Running {
    stop_signal: Arc<Notify>,
    task: JoinHandle<()>,
}

// ---- Request from frontend ----

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveDataRequest {
    pub interface: String,
    pub channel: Option<String>,
    pub bitrate: u32,
    pub node_id: Option<u8>,
    pub timeout_ms: u32,
    /// Snapshot rate in Hz; CLI accepts 1..=50.
    pub rate_hz: u8,
}

// ---- Streamed event ----

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LiveDataStreamEvent {
    Status {
        status: &'static str,
        message: Option<String>,
    },
    Snapshot(SnapshotPayload),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotPayload {
    pub uptime_ms: u32,
    pub frames_rx: u16,
    pub frames_tx: u16,
    pub nacks_sent: u16,
    pub dtc_count: u16,
    pub last_dtc_code: u16,
    pub flags: u8,
    pub last_opcode: u8,
    pub last_flash_addr: u32,
    pub isotp_rx_progress: u32,
    pub session_age_ms: u32,
    pub session_active: bool,
    pub valid_app_present: bool,
    pub log_streaming: bool,
    pub livedata_streaming: bool,
    pub wrp_protected: bool,
}

impl From<&LiveDataSnapshot> for SnapshotPayload {
    fn from(s: &LiveDataSnapshot) -> Self {
        Self {
            uptime_ms: s.uptime_ms,
            frames_rx: s.frames_rx,
            frames_tx: s.frames_tx,
            nacks_sent: s.nacks_sent,
            dtc_count: s.dtc_count,
            last_dtc_code: s.last_dtc_code,
            flags: s.flags,
            last_opcode: s.last_opcode,
            last_flash_addr: s.last_flash_addr,
            isotp_rx_progress: s.isotp_rx_progress,
            session_age_ms: s.session_age_ms,
            session_active: s.session_active(),
            valid_app_present: s.valid_app_present(),
            log_streaming: s.log_streaming(),
            livedata_streaming: s.livedata_streaming(),
            wrp_protected: s.wrp_protected(),
        }
    }
}

// ---- Commands ----

#[tauri::command]
pub async fn live_data_start(
    app: AppHandle,
    state: State<'_, LiveDataState>,
    request: LiveDataRequest,
) -> Result<(), String> {
    // Idempotent — if a previous stream is still alive, refuse rather
    // than silently leaking sessions. The frontend should call
    // live_data_stop first.
    {
        let guard = state.inner.lock().await;
        if guard.is_some() {
            return Err("live-data stream already running — stop it first".into());
        }
    }

    if !(1..=50).contains(&request.rate_hz) {
        return Err(format!(
            "rateHz must be in 1..=50 (got {})",
            request.rate_hz
        ));
    }

    let interface = parse_interface(&request.interface)?;
    let backend = open_backend(interface, request.channel.as_deref(), request.bitrate)
        .map_err(|e| format!("open backend: {e}"))?;
    let target_node = request.node_id.unwrap_or(0x3);
    let session = Session::attach(
        backend,
        SessionConfig {
            target_node,
            keepalive_interval: Duration::from_millis(5_000),
            command_timeout: Duration::from_millis(u64::from(request.timeout_ms)),
            ..SessionConfig::default()
        },
    );

    session
        .connect()
        .await
        .map_err(|e| format!("CONNECT before LIVE_DATA_START: {e}"))?;

    let mut subscriber = session.subscribe_notifications();

    match session
        .send_command(&cmd_live_data_start(request.rate_hz))
        .await
    {
        Ok(Response::Ack { .. }) => {}
        Ok(Response::Nack {
            rejected_opcode,
            code,
        }) => {
            let _ = session.disconnect().await;
            return Err(format!(
                "device NACK'd LIVE_DATA_START (opcode 0x{rejected_opcode:02X}): {code}"
            ));
        }
        Ok(other) => {
            let _ = session.disconnect().await;
            return Err(format!(
                "unexpected reply to LIVE_DATA_START: {}",
                other.kind_str()
            ));
        }
        Err(e) => {
            let _ = session.disconnect().await;
            return Err(format!("send LIVE_DATA_START: {e}"));
        }
    }

    let _ = app.emit(
        EVENT_NAME,
        &LiveDataStreamEvent::Status {
            status: "running",
            message: Some(format!("streaming @ {} Hz", request.rate_hz)),
        },
    );

    // ---- Spawn the streaming task ----

    let stop_signal = Arc::new(Notify::new());
    let stop_signal_for_task = Arc::clone(&stop_signal);

    let task = tokio::spawn(async move {
        let live_data_op = NotifyOpcode::LiveData.as_byte();
        loop {
            tokio::select! {
                _ = stop_signal_for_task.notified() => {
                    break;
                }
                res = subscriber.recv() => match res {
                    Ok(Response::Notify { opcode, payload }) if opcode == live_data_op => {
                        match LiveDataSnapshot::parse(&payload) {
                            Ok(snap) => {
                                let _ = app.emit(
                                    EVENT_NAME,
                                    &LiveDataStreamEvent::Snapshot((&snap).into()),
                                );
                            }
                            Err(err) => {
                                warn!(?err, "live_data: snapshot parse failed");
                            }
                        }
                    }
                    Ok(_) => {} // other notifications — ignore
                    Err(RecvError::Lagged(n)) => {
                        warn!(dropped = n, "live_data: subscriber lagged");
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        }

        // Tell the device to stop emitting, then disconnect. Both
        // best-effort — by the time we get here the operator has
        // already moved on; surfacing a stray timeout would just
        // pollute the log.
        let _ = session.send_command(&cmd_live_data_stop()).await;
        let _ = session.disconnect().await;
        let _ = app.emit(
            EVENT_NAME,
            &LiveDataStreamEvent::Status {
                status: "stopped",
                message: None,
            },
        );
    });

    // Stash so live_data_stop can find us.
    {
        let mut guard = state.inner.lock().await;
        *guard = Some(Running { stop_signal, task });
    }

    Ok(())
}

#[tauri::command]
pub async fn live_data_stop(state: State<'_, LiveDataState>) -> Result<(), String> {
    let taken = {
        let mut guard = state.inner.lock().await;
        guard.take()
    };
    if let Some(running) = taken {
        running.stop_signal.notify_one();
        // Await the task so the second call to start can't race with
        // the previous task's tear-down.
        let _ = running.task.await;
    }
    Ok(())
}

// ---- Helpers ----

fn parse_interface(s: &str) -> Result<InterfaceType, String> {
    match s {
        "slcan" => Ok(InterfaceType::Slcan),
        "socketcan" => Ok(InterfaceType::Socketcan),
        "pcan" => Ok(InterfaceType::Pcan),
        "vector" => Ok(InterfaceType::Vector),
        "virtual" => Ok(InterfaceType::Virtual),
        other => Err(format!("unknown interface: {other}")),
    }
}
