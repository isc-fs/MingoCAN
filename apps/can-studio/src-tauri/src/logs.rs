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
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use can_flasher::firmware::crc32;
use can_flasher::protocol::commands::{
    cmd_logfs_close, cmd_logfs_crc, cmd_logfs_list, cmd_logfs_open, cmd_logfs_read,
};
use can_flasher::protocol::logfs::{self, MAX_READ_LEN};
use can_flasher::protocol::Response;
use can_flasher::session::{Session, SessionConfig};
use can_flasher::transport::open_backend;

use crate::flash::parse_interface;

/// Progress events for an in-flight `logs_pull`.
pub const EVENT_NAME: &str = "logs://progress";

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
    let interface = parse_interface(&request.interface)?;
    let backend = open_backend(interface, request.channel.as_deref(), request.bitrate)
        .map_err(|e| format!("open backend: {e}"))?;
    Ok(Session::attach(
        backend,
        SessionConfig {
            // Node id is caller-supplied — nothing hardcodes the AMS
            // address, so the pending 0x01 -> 0x02 move is a settings change.
            target_node: request.node_id.unwrap_or(0x3),
            keepalive_interval: Duration::from_millis(5_000),
            command_timeout: Duration::from_millis(u64::from(request.timeout_ms)),
            ..SessionConfig::default()
        },
    ))
}

/// Send one LOGFS command, unwrapping the ACK body (opcode already
/// stripped by the response parser).
async fn ack_body(session: &Session, payload: Vec<u8>, what: &str) -> Result<Vec<u8>, String> {
    match session
        .send_command(&payload)
        .await
        .map_err(|e| format!("send {what}: {e}"))?
    {
        Response::Ack { payload, .. } => Ok(payload),
        Response::Nack {
            rejected_opcode,
            code,
        } => Err(format!(
            "device NACK'd {what} (opcode 0x{rejected_opcode:02X}): {code}"
        )),
        other => Err(format!("unexpected reply to {what}: {}", other.kind_str())),
    }
}

/// Walk `LOGFS_LIST` to completion, following the cursor.
async fn list_all(session: &Session) -> Result<Vec<logfs::LogEntry>, String> {
    let mut all = Vec::new();
    let mut cursor = 0u16;
    loop {
        let body = ack_body(session, cmd_logfs_list(cursor), "LOGFS_LIST").await?;
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
    let session = open_session(&request)?;
    session
        .connect()
        .await
        .map_err(|e| format!("CONNECT before LOGFS_LIST: {e}"))?;
    let entries = list_all(&session).await;
    let _ = session.disconnect().await;

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
    let session = open_session(&request)?;
    session
        .connect()
        .await
        .map_err(|e| format!("CONNECT before LOGFS pull: {e}"))?;

    let result = pull_inner(&app, &session, index, &dest_dir).await;
    let _ = session.disconnect().await;
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
        let body = ack_body(
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
    }

    if open.size > 0 && data.len() as u32 != open.size {
        return Err(format!(
            "size mismatch for {}: OPEN said {} B, got {} B",
            entry.name,
            open.size,
            data.len()
        ));
    }

    // Verify against the node's CRC. crc32 == 0 at OPEN means the node
    // deferred it, which is the agreed default — either way we ask.
    let body = ack_body(session, cmd_logfs_crc(open.handle), "LOGFS_CRC").await?;
    let want = logfs::parse_crc(&body).map_err(|e| format!("parse LOGFS_CRC: {e}"))?;
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
