// Generic CAN bus monitor — Tier 1.
//
// Opens an adapter via `can_flasher::transport::open_backend` and
// polls `recv(timeout)` in a loop, timestamping every frame on
// receive and streaming it to the frontend as `bus_monitor:frame`
// events. Independent of the bootloader protocol — the monitor
// doesn't speak CONNECT, doesn't attach a Session, doesn't filter
// by ID. It just shows what the wire shows.
//
// State management mirrors live_data.rs: a `tauri::manage`d slot
// holds the running task's stop-signal + JoinHandle so the stop
// command can signal a clean shutdown and wait for the task to
// actually exit (vs. just aborting it, which would leak the
// backend handle).

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tracing::warn;

use can_flasher::transport::open_backend;

use crate::flash::parse_interface;

const FRAME_EVENT: &str = "bus_monitor:frame";
const STATUS_EVENT: &str = "bus_monitor:status";

// ---- Shared state ----

#[derive(Default)]
pub struct BusMonitorState {
    inner: Mutex<Option<Running>>,
}

struct Running {
    stop_signal: Arc<Notify>,
    task: JoinHandle<()>,
}

// ---- Request from frontend ----

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BusMonitorRequest {
    pub interface: String,
    pub channel: Option<String>,
    pub bitrate: u32,
    /// Per-recv timeout. Shorter = lower stop-signal latency at the
    /// cost of more idle wakeups. 50ms is a reasonable balance — a
    /// stop request lands within one poll cycle.
    pub poll_timeout_ms: u32,
}

// ---- Streamed events ----

/// Per-frame event. Timestamped on receive in this process (the
/// transport layer doesn't surface hardware timestamps for most
/// backends, and rolling our own keeps the wire format uniform
/// across adapters). `tsMs` is milliseconds since the monitor
/// session started, not Unix time — operators care about deltas,
/// not wall clock.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusMonitorFrame {
    pub ts_ms: u64,
    /// 11-bit CAN ID (we don't yet support 29-bit extended IDs;
    /// the transport's `CanFrame.id` is `u16`).
    pub id: u16,
    pub dlc: u8,
    /// Little-endian byte sequence as it appeared on the wire,
    /// truncated to `dlc` bytes by the frontend. We always emit
    /// the full 8-byte buffer for stable shape.
    pub data: [u8; 8],
}

/// Status events for the start / stop / error lifecycle. Lets the
/// frontend show a clear "monitor connected / failed" indicator
/// independent of frame-arrival pace.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BusMonitorStatus {
    Started,
    Stopped,
    Error { message: String },
}

// ---- Commands ----

#[tauri::command]
pub async fn bus_monitor_start(
    app: AppHandle,
    state: State<'_, BusMonitorState>,
    request: BusMonitorRequest,
) -> Result<(), String> {
    // ---- Idempotency: stop any prior session first ----
    {
        let mut slot = state.inner.lock().await;
        if let Some(prev) = slot.take() {
            prev.stop_signal.notify_waiters();
            let _ = prev.task.await;
        }
    }

    let interface = parse_interface(&request.interface)?;
    let channel = request.channel.clone();
    let bitrate = request.bitrate;
    let poll_timeout = Duration::from_millis(u64::from(request.poll_timeout_ms.max(20)));

    // Open the backend synchronously so we can fail-fast on bad
    // adapter config rather than swallowing it inside the task.
    let backend = open_backend(interface, channel.as_deref(), bitrate)
        .map_err(|e| format!("opening backend: {e}"))?;

    let stop_signal = Arc::new(Notify::new());
    let stop_for_task = stop_signal.clone();
    let app_for_task = app.clone();

    let task = tokio::spawn(async move {
        let _ = app_for_task.emit(STATUS_EVENT, &BusMonitorStatus::Started);
        let started = Instant::now();
        let stop = stop_for_task.notified();
        tokio::pin!(stop);

        loop {
            tokio::select! {
                _ = &mut stop => {
                    let _ = app_for_task.emit(STATUS_EVENT, &BusMonitorStatus::Stopped);
                    return;
                }
                result = backend.recv(poll_timeout) => {
                    match result {
                        Ok(frame) => {
                            let payload = BusMonitorFrame {
                                ts_ms: started.elapsed().as_millis() as u64,
                                id: frame.id,
                                dlc: frame.len,
                                data: frame.data,
                            };
                            if let Err(err) = app_for_task.emit(FRAME_EVENT, &payload) {
                                warn!(?err, "bus_monitor: emit failed; stopping");
                                let _ = app_for_task.emit(
                                    STATUS_EVENT,
                                    &BusMonitorStatus::Error {
                                        message: format!("emit failed: {err}"),
                                    },
                                );
                                return;
                            }
                        }
                        Err(err) => {
                            // Timeouts are the common case — no
                            // frames on the bus during the poll
                            // window. They're not errors. Most
                            // transports surface these as a
                            // distinct error kind, but the trait
                            // is a generic Result — we treat any
                            // string containing "timeout" / "wait"
                            // as benign so we don't spam the
                            // frontend with phantom errors.
                            let msg = err.to_string();
                            let benign = msg.to_lowercase().contains("timeout")
                                || msg.to_lowercase().contains("would block");
                            if !benign {
                                let _ = app_for_task.emit(
                                    STATUS_EVENT,
                                    &BusMonitorStatus::Error { message: msg },
                                );
                                return;
                            }
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
pub async fn bus_monitor_stop(
    state: State<'_, BusMonitorState>,
) -> Result<(), String> {
    let mut slot = state.inner.lock().await;
    let Some(prev) = slot.take() else {
        // No-op when nothing is running — matches live_data_stop's
        // idempotent contract.
        return Ok(());
    };
    prev.stop_signal.notify_waiters();
    // We don't propagate the join result — a panicked task
    // shouldn't take the whole app down, and the next `start`
    // already handles the prior task being gone.
    let _ = prev.task.await;
    Ok(())
}
