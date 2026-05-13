// Tauri commands wrapping `can-flasher`'s diagnose subsurface:
// session health, DTC read, DTC clear. Same orchestration as
// `can_flasher::cli::diagnose` — open a session, fire the command,
// parse the reply, hand back a `#[derive(Serialize)]` snapshot.
//
// `health` and `read_dtcs` are sessionless (no `CMD_CONNECT`) — they
// re-use the keepalive command channel. `clear_dtcs` is a write op
// so we wrap it with connect / disconnect, mirroring the CLI.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use can_flasher::cli::InterfaceType;
use can_flasher::protocol::commands::{cmd_dtc_clear, cmd_dtc_read, cmd_get_health};
use can_flasher::protocol::records::{DtcEntry, HealthRecord};
use can_flasher::protocol::Response;
use can_flasher::session::{Session, SessionConfig};
use can_flasher::transport::open_backend;

// ---- Shared request ----

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnoseRequest {
    pub interface: String,
    pub channel: Option<String>,
    pub bitrate: u32,
    pub node_id: Option<u8>,
    pub timeout_ms: u32,
}

// ---- Health ----

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthSnapshot {
    pub uptime_seconds: u32,
    pub reset_cause: String,
    pub reset_cause_raw: u32,
    pub flash_write_count: u32,
    pub dtc_count: u32,
    pub last_dtc_code: u32,
    pub session_active: bool,
    pub valid_app_present: bool,
    pub wrp_protected: bool,
    pub raw_flags: u32,
}

impl From<&HealthRecord> for HealthSnapshot {
    fn from(r: &HealthRecord) -> Self {
        Self {
            uptime_seconds: r.uptime_seconds,
            reset_cause: r
                .reset_cause()
                .map(|c| c.as_str().to_string())
                .unwrap_or_else(|| format!("UNKNOWN (0x{:02X})", r.reset_cause)),
            reset_cause_raw: r.reset_cause,
            flash_write_count: r.flash_write_count,
            dtc_count: r.dtc_count,
            last_dtc_code: r.last_dtc_code,
            session_active: r.session_active(),
            valid_app_present: r.valid_app_present(),
            wrp_protected: r.wrp_protected(),
            raw_flags: r.flags,
        }
    }
}

#[tauri::command]
pub async fn health(request: DiagnoseRequest) -> Result<HealthSnapshot, String> {
    let session = open_session(&request)?;
    let resp = session
        .send_command(&cmd_get_health())
        .await
        .map_err(|e| format!("send GET_HEALTH: {e}"))?;
    let _ = session.disconnect().await;
    match resp {
        Response::Ack { payload, .. } => {
            if payload.len() < HealthRecord::SIZE {
                return Err(format!(
                    "GET_HEALTH ACK too short: got {} bytes, need at least {}",
                    payload.len(),
                    HealthRecord::SIZE,
                ));
            }
            let record = HealthRecord::parse(&payload)
                .map_err(|e| format!("parse HealthRecord: {e}"))?;
            Ok((&record).into())
        }
        Response::Nack {
            rejected_opcode,
            code,
        } => Err(format!(
            "device NACK'd GET_HEALTH (opcode 0x{rejected_opcode:02X}): {code}"
        )),
        other => Err(format!("unexpected reply: {}", other.kind_str())),
    }
}

// ---- DTC read ----

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DtcSnapshot {
    pub code: u16,
    pub severity: String,
    pub severity_raw: u8,
    pub occurrence_count: u8,
    pub first_seen_uptime_seconds: u32,
    pub last_seen_uptime_seconds: u32,
    pub context_data: u32,
}

impl From<&DtcEntry> for DtcSnapshot {
    fn from(e: &DtcEntry) -> Self {
        Self {
            code: e.code,
            severity: e.severity().as_str().to_string(),
            severity_raw: e.severity,
            occurrence_count: e.occurrence_count,
            first_seen_uptime_seconds: e.first_seen_uptime_seconds,
            last_seen_uptime_seconds: e.last_seen_uptime_seconds,
            context_data: e.context_data,
        }
    }
}

#[tauri::command]
pub async fn read_dtcs(request: DiagnoseRequest) -> Result<Vec<DtcSnapshot>, String> {
    let session = open_session(&request)?;
    let resp = session
        .send_command(&cmd_dtc_read())
        .await
        .map_err(|e| format!("send DTC_READ: {e}"))?;
    let _ = session.disconnect().await;
    match resp {
        Response::Ack { payload, .. } => {
            let mut entries: Vec<DtcSnapshot> = Vec::new();
            let mut off = 0;
            while off + DtcEntry::SIZE <= payload.len() {
                let entry = DtcEntry::parse(&payload[off..off + DtcEntry::SIZE])
                    .map_err(|e| format!("parse DtcEntry: {e}"))?;
                entries.push((&entry).into());
                off += DtcEntry::SIZE;
            }
            Ok(entries)
        }
        Response::Nack {
            rejected_opcode,
            code,
        } => Err(format!(
            "device NACK'd DTC_READ (opcode 0x{rejected_opcode:02X}): {code}"
        )),
        other => Err(format!("unexpected reply: {}", other.kind_str())),
    }
}

// ---- DTC clear ----

#[tauri::command]
pub async fn clear_dtcs(request: DiagnoseRequest) -> Result<(), String> {
    let session = open_session(&request)?;
    session
        .connect()
        .await
        .map_err(|e| format!("CONNECT before DTC_CLEAR: {e}"))?;

    let resp = session.send_command(&cmd_dtc_clear()).await;

    // Always attempt disconnect — even on error — so we don't strand
    // a session on the device.
    let _ = session.disconnect().await;

    match resp.map_err(|e| format!("send DTC_CLEAR: {e}"))? {
        Response::Ack { .. } => Ok(()),
        Response::Nack {
            rejected_opcode,
            code,
        } => Err(format!(
            "device NACK'd DTC_CLEAR (opcode 0x{rejected_opcode:02X}): {code}"
        )),
        other => Err(format!("unexpected reply: {}", other.kind_str())),
    }
}

// ---- Helpers ----

fn open_session(request: &DiagnoseRequest) -> Result<Session, String> {
    let interface = parse_interface(&request.interface)?;
    let backend = open_backend(interface, request.channel.as_deref(), request.bitrate)
        .map_err(|e| format!("open backend: {e}"))?;
    let target_node = request.node_id.unwrap_or(0x3);
    Ok(Session::attach(
        backend,
        SessionConfig {
            target_node,
            keepalive_interval: Duration::from_millis(5_000),
            command_timeout: Duration::from_millis(u64::from(request.timeout_ms)),
            ..SessionConfig::default()
        },
    ))
}

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
