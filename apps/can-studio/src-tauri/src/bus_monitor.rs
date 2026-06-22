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
//
// v0.2.1 adds capture-to-file: a parallel persistent stream that
// writes a candump-format line per frame to a user-chosen file,
// independent of the visible frame buffer (which is bounded for
// UI sanity). Capture is toggleable mid-stream — operators can
// hit Save after spotting something interesting and only persist
// from that moment forward.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::fs::File;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::task::JoinHandle;
use tracing::warn;

use can_flasher::pit_diag::{build_arm_frame, ecu};
use can_flasher::protocol::CanFrame;
use can_flasher::transport::{open_backend, TransportError};

use crate::dbc::{decode, snapshot_lookup, DbcState};
use crate::flash::parse_interface;

const FRAME_EVENT: &str = "bus_monitor:frame";
const STATUS_EVENT: &str = "bus_monitor:status";
const CAPTURE_EVENT: &str = "bus_monitor:capture";
const SIGNALS_EVENT: &str = "bus_monitor:signals";

// ---- Shared state ----

/// The capture writer is wrapped in an `Arc<Mutex<…>>` so the
/// reader task and the capture commands can both touch it
/// without taking ownership. Swapping in/out a writer toggles
/// capture mid-stream.
type SharedCapture = Arc<Mutex<Option<CaptureWriter>>>;

struct CaptureWriter {
    path: PathBuf,
    writer: BufWriter<File>,
    /// Frames written since capture started. Surfaced to the
    /// frontend so the operator sees a count rising in real time.
    frames: u64,
}

#[derive(Default)]
pub struct BusMonitorState {
    inner: Mutex<Option<Running>>,
    capture: SharedCapture,
}

struct Running {
    stop_signal: Arc<Notify>,
    task: JoinHandle<()>,
    /// Logical channel name we emit into the candump line (column
    /// 2). Falls back to "can0" when the operator didn't pick one.
    capture_channel: String,
    /// TX side of the monitor's transmit channel. Commands push a
    /// frame here and the monitor task sends it through the adapter
    /// it already owns — so we can transmit (e.g. the pit-diag arm
    /// frame) without opening a second handle on the same bus, which
    /// PCAN/SLCAN don't allow.
    tx_frames: mpsc::UnboundedSender<CanFrame>,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStartRequest {
    /// Absolute path. The frontend uses the native save dialog so
    /// the operator picks a writable location; we don't second-
    /// guess it. We *do* refuse if no monitor session is running,
    /// because there'd be nothing to capture.
    pub path: String,
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
    /// The adapter went away mid-session (USB unplugged / driver
    /// unloaded). Distinct from `Error` so the UI can show a calm
    /// "adapter disconnected" and re-offer Start, rather than a red
    /// failure.
    Disconnected,
    Error {
        message: String,
    },
}

/// Capture-state events — emitted whenever capture starts, stops,
/// or makes meaningful progress. The reader task fires a Progress
/// event at most every 100 frames or 500ms so the UI updates
/// without burning a Tauri IPC per frame.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BusMonitorCaptureEvent {
    Started { path: String },
    Stopped { path: String, frames: u64 },
    Progress { path: String, frames: u64 },
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
    // Drop any pending capture too — a fresh monitor session is a
    // fresh recording context.
    close_capture(&app, &state.capture).await;

    let interface = parse_interface(&request.interface)?;
    let channel = request.channel.clone();
    let bitrate = request.bitrate;
    let poll_timeout = Duration::from_millis(u64::from(request.poll_timeout_ms.max(20)));
    let capture_channel = channel
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("can0")
        .to_string();

    // Open the backend synchronously so we can fail-fast on bad
    // adapter config rather than swallowing it inside the task.
    let backend = open_backend(interface, channel.as_deref(), bitrate)
        .map_err(|e| format!("opening backend: {e}"))?;

    let stop_signal = Arc::new(Notify::new());
    let stop_for_task = stop_signal.clone();
    let app_for_task = app.clone();
    let capture_for_task = state.capture.clone();
    let capture_channel_for_task = capture_channel.clone();
    // TX path — commands (e.g. arm pit-diag) push frames here; the
    // task sends them through `backend`, the same handle it receives
    // on. One owner of the adapter, both directions.
    let (tx_frames, mut tx_rx) = mpsc::unbounded_channel::<CanFrame>();

    let task = tokio::spawn(async move {
        let _ = app_for_task.emit(STATUS_EVENT, &BusMonitorStatus::Started);
        let started = Instant::now();
        let started_wall = SystemTime::now();
        let mut last_progress_emit = Instant::now();
        let mut progress_since_last: u64 = 0;
        let stop = stop_for_task.notified();
        tokio::pin!(stop);

        loop {
            tokio::select! {
                _ = &mut stop => {
                    let _ = app_for_task.emit(STATUS_EVENT, &BusMonitorStatus::Stopped);
                    return;
                }
                Some(frame) = tx_rx.recv() => {
                    if let Err(err) = backend.send(frame).await {
                        warn!(?err, "bus_monitor: TX failed");
                    }
                }
                result = backend.recv(poll_timeout) => {
                    match result {
                        Ok(frame) => {
                            let ts_ms = started.elapsed().as_millis() as u64;
                            let payload = BusMonitorFrame {
                                ts_ms,
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

                            // Decode against the loaded DBC (if any)
                            // and emit decoded signals as a separate
                            // event. Empty when no DBC is loaded or
                            // the frame's ID isn't in the schema.
                            let dbc_state = app_for_task.state::<DbcState>();
                            if let Some((dbc, by_id)) =
                                snapshot_lookup(dbc_state.inner()).await
                            {
                                let decoded = decode(&dbc, &by_id, &frame);
                                if !decoded.is_empty() {
                                    let _ = app_for_task.emit(SIGNALS_EVENT, &decoded);
                                }
                            }

                            // Mirror to capture file if active.
                            if let Some(progress) = write_capture_line(
                                &capture_for_task,
                                &capture_channel_for_task,
                                started_wall,
                                ts_ms,
                                &frame,
                            )
                            .await
                            {
                                progress_since_last += 1;
                                // Rate-limit progress emits: at
                                // most every 100 frames OR every
                                // 500ms so the UI feels live
                                // without burning IPC.
                                if progress_since_last >= 100
                                    || last_progress_emit.elapsed()
                                        >= Duration::from_millis(500)
                                {
                                    // Flush the capture buffer on the
                                    // same cadence as the progress
                                    // emit. The OS still has the file
                                    // even if we later abort (e.g. a
                                    // driver crash on unplug), so an
                                    // unexpected kill loses at most this
                                    // ~100-frame / 500ms window instead
                                    // of the whole 64 KB BufWriter tail.
                                    flush_capture(&capture_for_task).await;
                                    let _ = app_for_task.emit(
                                        CAPTURE_EVENT,
                                        &BusMonitorCaptureEvent::Progress {
                                            path: progress.path,
                                            frames: progress.frames,
                                        },
                                    );
                                    last_progress_emit = Instant::now();
                                    progress_since_last = 0;
                                }
                            }
                        }
                        Err(TransportError::Disconnected) => {
                            // The adapter was removed mid-session (its
                            // reader signalled hardware-gone). Emit a
                            // calm Disconnected status and stop — the
                            // UI returns to idle so the operator can
                            // re-plug and Start again.
                            warn!("bus_monitor: adapter disconnected; stopping");
                            // Finalize any capture first so the
                            // recording up to the unplug is preserved —
                            // the BufWriter holds up to 64 KB unflushed,
                            // and the task is about to return without
                            // the normal stop-command path running.
                            close_capture(&app_for_task, &capture_for_task).await;
                            let _ = app_for_task
                                .emit(STATUS_EVENT, &BusMonitorStatus::Disconnected);
                            return;
                        }
                        Err(err) => {
                            // An empty poll window — no frame arrived
                            // within `poll_timeout` — is the COMMON
                            // case on any real bus (AMS telemetry is
                            // ~500 ms apart, the poll is ~50 ms), not an
                            // error. `TransportError::Timeout` displays
                            // as "timed out … after Nms" — note the
                            // SPACE, so a bare `contains("timeout")`
                            // misses it and every idle poll was surfaced
                            // as a fatal error that stopped the monitor.
                            // Match both spellings.
                            let msg = err.to_string();
                            let lower = msg.to_lowercase();
                            let benign = lower.contains("timed out")
                                || lower.contains("timeout")
                                || lower.contains("would block");
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
    *slot = Some(Running {
        stop_signal,
        task,
        capture_channel,
        tx_frames,
    });
    Ok(())
}

/// Arm (or disarm) pit-diag from inside the bus monitor — sends BOTH
/// the AMS (`0x7F0`) and ECU (`0x7E0`) arm/disarm frames through the
/// monitor's own adapter handle, so whichever board is on the bus
/// responds without the operator having to know which node it is (or
/// to stop the monitor and open a second session, which PCAN/SLCAN
/// forbid). The arm payload is identical for both (`DE AD BE EF`); only
/// the ID differs, and a board harmlessly ignores the frame addressed
/// to the other. The monitor must be running; the decoded diagnostic
/// frames then flow in like any other traffic and the Signals view
/// fills in.
#[tauri::command]
pub async fn bus_monitor_arm_pit_diag(
    state: State<'_, BusMonitorState>,
    enable: bool,
) -> Result<(), String> {
    let slot = state.inner.lock().await;
    let running = slot
        .as_ref()
        .ok_or("start the bus monitor before arming pit-diag")?;
    let stopped = || "bus monitor stopped — restart it and try again".to_string();
    running
        .tx_frames
        .send(build_arm_frame(enable))
        .map_err(|_| stopped())?;
    running
        .tx_frames
        .send(ecu::build_arm_frame(enable))
        .map_err(|_| stopped())?;
    Ok(())
}

#[tauri::command]
pub async fn bus_monitor_stop(
    app: AppHandle,
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
    close_capture(&app, &state.capture).await;
    Ok(())
}

#[tauri::command]
pub async fn bus_monitor_capture_start(
    app: AppHandle,
    state: State<'_, BusMonitorState>,
    request: CaptureStartRequest,
) -> Result<(), String> {
    // Refuse if no monitor is running — there'd be no frames to
    // record. The frontend should already be gating the button,
    // but defence-in-depth.
    {
        let slot = state.inner.lock().await;
        if slot.is_none() {
            return Err("monitor is not running — start it first".into());
        }
    }

    let path = PathBuf::from(&request.path);
    let file = File::create(&path)
        .await
        .map_err(|e| format!("create capture file: {e}"))?;
    let writer = BufWriter::with_capacity(64 * 1024, file);

    // Close any existing capture first — operators can switch
    // files without explicitly stopping.
    close_capture(&app, &state.capture).await;

    {
        let mut slot = state.capture.lock().await;
        *slot = Some(CaptureWriter {
            path: path.clone(),
            writer,
            frames: 0,
        });
    }

    let _ = app.emit(
        CAPTURE_EVENT,
        &BusMonitorCaptureEvent::Started {
            path: path.display().to_string(),
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn bus_monitor_capture_stop(
    app: AppHandle,
    state: State<'_, BusMonitorState>,
) -> Result<(), String> {
    close_capture(&app, &state.capture).await;
    Ok(())
}

// ---- Capture helpers ----

/// Result returned from a single capture-line write. The reader
/// task uses this to emit Progress events at a sensible cadence
/// (vs. every frame).
struct CaptureProgress {
    path: String,
    frames: u64,
}

/// Write one candump-format line to the active capture file (if
/// any). Returns the post-write frame count so the caller can
/// decide whether to emit a Progress event. Errors are logged
/// and surfaced via a Capture::Error event, but they *don't*
/// kill the monitor session — losing the recording shouldn't
/// take the live view down with it.
async fn write_capture_line(
    capture: &SharedCapture,
    channel: &str,
    started_wall: SystemTime,
    ts_ms: u64,
    frame: &can_flasher::protocol::CanFrame,
) -> Option<CaptureProgress> {
    let mut slot = capture.lock().await;
    let writer = slot.as_mut()?;

    // candump default format:
    //   (1234567890.123456) can0 1A3#DEADBEEF
    // Wall-clock seconds.us derived from the session-start
    // SystemTime + the relative ts. The session-start anchor
    // means timestamps are monotonic within a capture even if
    // the system clock jumps mid-recording (which would break a
    // pure SystemTime::now() approach).
    let wall = started_wall + Duration::from_millis(ts_ms);
    let unix = wall.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = unix.as_secs();
    let micros = unix.subsec_micros();

    let mut data_hex = String::with_capacity(16);
    for b in &frame.data[..usize::from(frame.len.min(8))] {
        data_hex.push_str(&format!("{:02X}", b));
    }

    let line = format!(
        "({}.{:06}) {} {:03X}#{}\n",
        secs, micros, channel, frame.id, data_hex
    );

    if let Err(err) = writer.writer.write_all(line.as_bytes()).await {
        warn!(?err, "bus_monitor: capture write failed");
        return None;
    }
    writer.frames += 1;
    Some(CaptureProgress {
        path: writer.path.display().to_string(),
        frames: writer.frames,
    })
}

/// Flush the active capture's buffer to the OS without closing it.
/// Called periodically while recording so an unexpected process death
/// (e.g. a driver crash on unplug) loses at most the frames since the
/// last flush, not the whole BufWriter tail. No-op if nothing is open.
async fn flush_capture(capture: &SharedCapture) {
    let mut slot = capture.lock().await;
    if let Some(writer) = slot.as_mut() {
        if let Err(err) = writer.writer.flush().await {
            warn!(?err, "bus_monitor: periodic capture flush failed");
        }
    }
}

/// Flush and close the active capture, emitting a Stopped event
/// with the final frame count. No-op if nothing is open.
async fn close_capture(app: &AppHandle, capture: &SharedCapture) {
    let mut slot = capture.lock().await;
    let Some(mut writer) = slot.take() else {
        return;
    };
    if let Err(err) = writer.writer.flush().await {
        warn!(?err, "bus_monitor: capture flush failed on close");
    }
    if let Err(err) = writer.writer.shutdown().await {
        warn!(?err, "bus_monitor: capture shutdown failed on close");
    }
    let _ = app.emit(
        CAPTURE_EVENT,
        &BusMonitorCaptureEvent::Stopped {
            path: writer.path.display().to_string(),
            frames: writer.frames,
        },
    );
}
