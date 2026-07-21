//! Data-logs (LOGFS) commands — list and pull the microSD car-data logs
//! off a node over CAN (#506, firmware spec IFS08-CE-AMS#406).
//!
//! Rides the existing CONNECT session + ISO-TP via the shared
//! `can_flasher` protocol layer; nothing here re-implements the wire
//! format. Read-only — there is no delete command in v1.
//!
//! `logs_pull` streams progress to the frontend over
//! [`EVENT_NAME`] so the UI can show a bar + ETA on what is, at classic
//! CAN speeds, a minutes-long transfer.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use can_flasher::firmware::crc32;
use can_flasher::protocol::commands::{
    cmd_logfs_close, cmd_logfs_crc, cmd_logfs_finalize, cmd_logfs_list, cmd_logfs_open,
    cmd_logfs_read,
};
use can_flasher::protocol::logfs::{self, MAX_READ_LEN};
use can_flasher::protocol::Response;
use can_flasher::session::{Session, SessionConfig, SessionError};
use can_flasher::transport::open_backend;

use crate::flash::parse_interface;

/// Progress events for an in-flight `logs_pull`.
pub const EVENT_NAME: &str = "logs://progress";

/// Marker the frontend matches to render a cancel as a neutral outcome
/// rather than a failure.
pub const CANCELLED_MSG: &str = "cancelled by operator";

/// Set by [`logs_cancel`], polled between reads by [`logs_pull`]. A pull
/// can run 3-7 minutes at classic-CAN speeds, so aborting has to be
/// possible without tearing down the app.
static CANCEL_PULL: AtomicBool = AtomicBool::new(false);

/// Held for the duration of any LOGFS operation. There is one CAN
/// adapter, and a pull holds it for minutes — so a second command
/// (another pull, or a List click on a stale window) must be told no
/// rather than race for the device and fail with an opaque
/// adapter-in-use error from the driver.
static LOGS_BUSY: AtomicBool = AtomicBool::new(false);

/// Floor for the LOGFS command timeout, regardless of the operator's
/// adapter setting. The app default is sized for bootloader commands that
/// answer out of RAM; a LOGFS round trip additionally waits on a FatFs
/// read from a microSD card behind a shared lock, so it can legitimately
/// take over a second — and a spurious timeout mid-pull throws away
/// minutes of transfer. A higher setting is still honoured.
const LOGFS_TIMEOUT_FLOOR_MS: u32 = 2_000;

/// How many times to re-send an idempotent LOGFS command before failing.
const LOGFS_RETRY_ATTEMPTS: u32 = 3;

/// Linear backoff base — attempt N waits `N * this`.
const LOGFS_RETRY_BACKOFF_MS: u64 = 60;

/// RAII holder for [`LOGS_BUSY`], so the flag clears on every exit path
/// including the `?` ones.
struct BusyGuard;

impl BusyGuard {
    fn acquire() -> Result<Self, String> {
        if LOGS_BUSY.swap(true, Ordering::SeqCst) {
            return Err("another log transfer is already running on this \
                        adapter — wait for it to finish or cancel it"
                .to_string());
        }
        Ok(Self)
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        LOGS_BUSY.store(false, Ordering::SeqCst);
    }
}

/// A failed round trip, plus whether re-sending it could plausibly help.
struct AckError {
    message: String,
    retryable: bool,
}

impl AckError {
    fn fatal(message: String) -> Self {
        Self {
            message,
            retryable: false,
        }
    }
}

// Lets every existing `?` in a `Result<_, String>` function keep working.
impl From<AckError> for String {
    fn from(e: AckError) -> Self {
        e.message
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsRequest {
    pub interface: String,
    pub channel: Option<String>,
    pub bitrate: u32,
    pub node_id: Option<u8>,
    pub timeout_ms: u32,
}

/// One log file as listed by the node.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogFileSnapshot {
    pub index: u16,
    pub name: String,
    pub size: u32,
    /// **Monotonic / boot-relative** — the AMS has no set RTC. The UI
    /// must render this as an ordering / uptime value, never a date.
    pub mtime_monotonic: u32,
}

/// Emitted repeatedly during a pull.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullProgress {
    pub index: u16,
    pub name: String,
    pub received: u32,
    pub total: u32,
}

/// Returned when a pull completes.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullResult {
    pub path: String,
    pub bytes: u32,
    /// `true` when the node's CRC matched the bytes we received.
    pub crc_verified: bool,
}

fn open_session(request: &LogsRequest) -> Result<Session, String> {
    // Never guess the target board (FMEA #271 G2, same rule as flash).
    // The old default of 0x3 is uDV — a real board, and not the log
    // source — which also made the bootloader probe answer and mislead
    // the operator into reflashing the wrong ECU.
    let target_node = request.node_id.ok_or_else(|| {
        "no node id selected: pick the target board (the microSD log \
         service is AMS-only today) before listing or pulling logs"
            .to_string()
    })?;
    let interface = parse_interface(&request.interface)?;
    let backend = open_backend(interface, request.channel.as_deref(), request.bitrate)
        .map_err(|e| format!("open backend: {e}"))?;
    Ok(Session::attach(
        backend,
        SessionConfig {
            // Caller-supplied — nothing hardcodes the AMS address, so the
            // pending 0x01 -> 0x02 move (IFS08-CE-AMS#403) is a settings change.
            target_node,
            keepalive_interval: Duration::from_millis(5_000),
            command_timeout: Duration::from_millis(u64::from(
                request.timeout_ms.max(LOGFS_TIMEOUT_FLOOR_MS),
            )),
            ..SessionConfig::default()
        },
    ))
}

/// Send one LOGFS command, unwrapping the ACK body (opcode already
/// stripped by the response parser).
async fn ack_body(session: &Session, payload: Vec<u8>, what: &str) -> Result<Vec<u8>, AckError> {
    // Remember what we asked for: the ACK echoes the opcode back, and
    // checking the echo catches a reply belonging to a *different*
    // command (a stale one that landed late, or a dispatcher that ran the
    // wrong handler). Unchecked, those bytes get parsed as this command's
    // body and turn into silent nonsense.
    let expected_opcode = payload.first().copied();

    // LOGFS rides APP_CTRL (0x06), not CMD (0x00) — see IFS08-CE-AMS#406.
    // The bootloader silently drops APP_CTRL, so a timeout is ambiguous:
    // probe with a command the BL answers to tell "in bootloader" apart
    // from "dead node / wrong id".
    let reply = match session.send_app_command(&payload).await {
        Err(SessionError::CommandTimeout { .. }) if session.probe_bootloader().await => {
            return Err(AckError::fatal(format!(
                "no reply to {what}: the node is alive but running the bootloader, \
                 where the log service isn't available — boot the application firmware"
            )))
        }
        // Only transport-level failures are worth another go. A NACK is a
        // considered answer and a bootloader diagnosis is a persistent
        // state; re-sending either hides the real message.
        Err(e) => {
            let retryable = matches!(
                e,
                SessionError::CommandTimeout { .. } | SessionError::Transport(_)
            );
            return Err(AckError {
                message: format!("send {what}: {e}"),
                retryable,
            });
        }
        Ok(reply) => reply,
    };
    match reply {
        Response::Ack { opcode, payload } => match expected_opcode {
            Some(want) if opcode != want => Err(AckError::fatal(format!(
                "reply to {what} echoes opcode 0x{opcode:02X}, expected 0x{want:02X} \
                 — replies are out of step with requests"
            ))),
            _ => Ok(payload),
        },
        Response::Nack {
            rejected_opcode,
            code,
        } => Err(AckError::fatal(format!(
            "device NACK'd {what} (opcode 0x{rejected_opcode:02X}): {code}"
        ))),
        other => Err(AckError::fatal(format!(
            "unexpected reply to {what}: {}",
            other.kind_str()
        ))),
    }
}

/// [`ack_body`] with retries, for **idempotent** opcodes only.
///
/// LOGFS reads are ranged and stateless firmware-side, so re-requesting
/// the same window is safe; LIST is a pure read of a cursor page. A
/// multi-MB pull is thousands of round trips over several minutes, and
/// without this one blip on a shared bus discards the whole transfer.
/// Deliberately *not* used for OPEN (allocates a handle) or CLOSE (frees
/// one).
async fn ack_body_retrying(
    session: &Session,
    payload: Vec<u8>,
    what: &str,
) -> Result<Vec<u8>, AckError> {
    let mut attempt = 1u32;
    loop {
        match ack_body(session, payload.clone(), what).await {
            Ok(body) => return Ok(body),
            Err(e) if attempt < LOGFS_RETRY_ATTEMPTS && e.retryable => {
                tokio::time::sleep(Duration::from_millis(
                    LOGFS_RETRY_BACKOFF_MS * u64::from(attempt),
                ))
                .await;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Walk `LOGFS_LIST` to completion, following the cursor.
async fn list_all(session: &Session) -> Result<Vec<logfs::LogEntry>, String> {
    let mut all = Vec::new();
    let mut cursor = 0u16;
    loop {
        let body = ack_body_retrying(session, cmd_logfs_list(cursor), "LOGFS_LIST").await?;
        let page = logfs::parse_list(&body).map_err(|e| format!("parse LOGFS_LIST: {e}"))?;
        let is_last = page.is_last();
        let next = page.next_cursor;
        all.extend(page.entries);
        if is_last {
            break;
        }
        if next == cursor {
            return Err(format!("LOGFS_LIST cursor stuck at {cursor}"));
        }
        cursor = next;
    }
    Ok(all)
}

#[tauri::command]
pub async fn logs_list(request: LogsRequest) -> Result<Vec<LogFileSnapshot>, String> {
    let _busy = BusyGuard::acquire()?;
    let session = open_session(&request)?;
    session
        .app_connect()
        .await
        .map_err(|e| format!("app CONNECT before LOGFS_LIST: {e}"))?;
    let entries = list_all(&session).await;
    let _ = session.app_disconnect().await;

    Ok(entries?
        .into_iter()
        .map(|e| LogFileSnapshot {
            index: e.index,
            name: e.name,
            size: e.size,
            mtime_monotonic: e.mtime,
        })
        .collect())
}

#[tauri::command]
pub async fn logs_pull(
    app: AppHandle,
    request: LogsRequest,
    index: u16,
    dest_dir: String,
) -> Result<PullResult, String> {
    let _busy = BusyGuard::acquire()?;
    CANCEL_PULL.store(false, Ordering::Relaxed);
    let session = open_session(&request)?;
    session
        .app_connect()
        .await
        .map_err(|e| format!("app CONNECT before LOGFS pull: {e}"))?;

    let result = pull_inner(&app, &session, index, &dest_dir).await;
    let _ = session.app_disconnect().await;
    result
}

async fn pull_inner(
    app: &AppHandle,
    session: &Session,
    index: u16,
    dest_dir: &str,
) -> Result<PullResult, String> {
    // Resolve the name from the listing so the saved file keeps the
    // node's 8.3 filename.
    let entries = list_all(session).await?;
    let entry = entries
        .into_iter()
        .find(|e| e.index == index)
        .ok_or_else(|| format!("no log with index {index} on the card"))?;

    let body = ack_body(session, cmd_logfs_open(entry.index), "LOGFS_OPEN").await?;
    let open = logfs::parse_open(&body).map_err(|e| format!("parse LOGFS_OPEN: {e}"))?;

    let mut data: Vec<u8> = Vec::with_capacity(open.size as usize);
    let mut offset = 0u32;
    loop {
        let body = ack_body_retrying(
            session,
            cmd_logfs_read(open.handle, offset, MAX_READ_LEN),
            "LOGFS_READ",
        )
        .await?;
        let out = logfs::parse_read(MAX_READ_LEN, &body);
        data.extend_from_slice(&out.data);
        offset = offset.saturating_add(out.data.len() as u32);

        let _ = app.emit(
            EVENT_NAME,
            PullProgress {
                index: entry.index,
                name: entry.name.clone(),
                received: offset,
                total: open.size,
            },
        );

        if out.eof {
            break;
        }
        if out.data.is_empty() {
            return Err(format!("LOGFS_READ stalled at offset {offset} before EOF"));
        }
        // Poll between reads — each is one bounded ISO-TP round trip, so
        // a cancel lands within a few hundred ms. Close the handle so the
        // node doesn't leak it.
        if CANCEL_PULL.load(Ordering::Relaxed) {
            let _ = ack_body(session, cmd_logfs_close(open.handle), "LOGFS_CLOSE").await;
            return Err(CANCELLED_MSG.to_string());
        }
    }

    if open.size > 0 && data.len() as u32 != open.size {
        return Err(format!(
            "size mismatch for {}: OPEN said {} B, got {} B",
            entry.name,
            open.size,
            data.len()
        ));
    }

    // The firmware keeps a running CRC while logging and seals it with
    // the file, so OPEN carries a real crc32 — no extra round trip. Only
    // fall back to LOGFS_CRC if the node declined to provide one.
    let want = if open.crc_deferred() {
        let body = ack_body(session, cmd_logfs_crc(open.handle), "LOGFS_CRC").await?;
        logfs::parse_crc(&body).map_err(|e| format!("parse LOGFS_CRC: {e}"))?
    } else {
        open.crc32
    };
    let got = crc32(&data);
    if want != got {
        let _ = ack_body(session, cmd_logfs_close(open.handle), "LOGFS_CLOSE").await;
        return Err(format!(
            "CRC mismatch for {}: node 0x{want:08X}, received 0x{got:08X}",
            entry.name
        ));
    }

    let _ = ack_body(session, cmd_logfs_close(open.handle), "LOGFS_CLOSE").await?;

    let dir = PathBuf::from(dest_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = unique_path(&dir, &entry.name);
    std::fs::write(&path, &data).map_err(|e| format!("write {}: {e}", path.display()))?;

    Ok(PullResult {
        path: path.display().to_string(),
        bytes: data.len() as u32,
        crc_verified: true,
    })
}

/// Never clobber an existing download — `LOG0001.CSV` → `LOG0001.CSV.1`.
fn unique_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let base = dir.join(name);
    if !base.exists() {
        return base;
    }
    for n in 1..1000 {
        let candidate = dir.join(format!("{name}.{n}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    base
}

/// Seal the log currently being written so the run that just happened can
/// be pulled without power-cycling the car.
///
/// Returns the index the sealed log now occupies. The node NACKs
/// `FILE_NOT_FOUND` when there is nothing to seal — no active file, or no
/// rows written to it yet.
#[tauri::command]
pub async fn logs_finalize(request: LogsRequest) -> Result<u16, String> {
    let _busy = BusyGuard::acquire()?;
    let session = open_session(&request)?;
    session
        .app_connect()
        .await
        .map_err(|e| format!("app CONNECT before LOGFS_FINALIZE: {e}"))?;
    let body = ack_body(&session, cmd_logfs_finalize(), "LOGFS_FINALIZE").await;
    let _ = session.app_disconnect().await;

    let index = logfs::parse_finalize(&body?).map_err(|e| format!("parse LOGFS_FINALIZE: {e}"))?;
    Ok(index)
}

/// Ask an in-flight [`logs_pull`] to stop. Cooperative: the transfer
/// aborts at the next read boundary and the file is not written.
#[tauri::command]
pub fn logs_cancel() {
    CANCEL_PULL.store(true, Ordering::Relaxed);
}
