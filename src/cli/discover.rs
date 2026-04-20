//! `discover` subcommand — broadcast `CMD_DISCOVER` and report every
//! bootloader on the bus.
//!
//! Flow:
//!
//! 1. Open the adapter via [`transport::open_backend`].
//! 2. Wrap in [`Session::attach`]. Discover is session-less — no
//!    `connect()` — so only the RX task + command plumbing comes up.
//! 3. Issue a [`Session::broadcast`] of `cmd_discover()` with the
//!    user-supplied timeout window. Every responder replies once
//!    with `[CMD_DISCOVER, node_id, major, minor]` as a
//!    `TYPE=DISCOVER` frame.
//! 4. For each responder, fire follow-up `CMD_GET_FW_INFO` and
//!    `CMD_GET_HEALTH` via [`Session::send_command_to`]. Both are
//!    session-less on the bootloader side, so no CONNECT handshake
//!    is needed. Failures degrade gracefully: a node with no app
//!    installed shows "(no app)" for its FW columns; any other NACK
//!    or timeout shows "(error: …)".
//! 5. Render the collected rows either as a human-readable fixed-
//!    width table or as a JSON object (`--json`) that downstream
//!    tooling can consume.

use std::io::Write;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;
use serde::Serialize;
use tracing::debug;

use super::GlobalFlags;
use crate::protocol::commands::{cmd_discover, cmd_get_fw_info, cmd_get_health};
use crate::protocol::ids::MessageType;
use crate::protocol::opcodes::NackCode;
use crate::protocol::records::{FirmwareInfo, HealthRecord, ResetCause};
use crate::protocol::{Response, BROADCAST_NODE_ID};
use crate::session::{Session, SessionConfig};
use crate::transport::open_backend;

#[derive(Debug, Args)]
pub struct DiscoverArgs {
    /// How long to wait for replies after the broadcast, in milliseconds
    #[arg(long = "timeout-ms", default_value_t = 500)]
    pub timeout_ms: u32,
}

/// One discovered responder's row in the output table / JSON array.
#[derive(Debug, Serialize)]
pub struct DiscoverRow {
    pub node_id: u8,
    pub proto_major: u8,
    pub proto_minor: u8,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub fw_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
    /// Present only when the FW record fetch failed — e.g.
    /// `"no app installed"`, `"NACK NVM_NOT_FOUND"`, `"timed out"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fw_error: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrp_protected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_cause: Option<String>,
    /// Present only when the health fetch failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_error: Option<String>,
}

impl DiscoverRow {
    fn new(node_id: u8, proto_major: u8, proto_minor: u8) -> Self {
        Self {
            node_id,
            proto_major,
            proto_minor,
            fw_version: None,
            git_hash: None,
            product: None,
            fw_error: None,
            wrp_protected: None,
            reset_cause: None,
            health_error: None,
        }
    }
}

pub async fn run(args: DiscoverArgs, global: &GlobalFlags) -> Result<()> {
    debug!(?args, "discover: starting");

    let backend = open_backend(global.interface, global.channel.as_deref(), global.bitrate)
        .context("opening CAN backend for discover")?;

    // Session config: discover is broadcast + session-less follow-ups;
    // target_node is only used for the broadcast itself (which goes
    // to dst=0xF anyway) and the keepalive (which we won't hit since
    // we never connect). Pick 0x0 as a no-op default.
    let config = SessionConfig {
        target_node: 0x0,
        keepalive_interval: Duration::from_millis(5_000),
        command_timeout: Duration::from_millis(u64::from(global.timeout_ms)),
        ..SessionConfig::default()
    };
    let session = Session::attach(backend, config);

    let broadcast_window = Duration::from_millis(u64::from(args.timeout_ms));
    let replies = session
        .broadcast(
            &cmd_discover(),
            MessageType::DiscoverRequest,
            broadcast_window,
        )
        .await
        .context("broadcasting CMD_DISCOVER")?;

    let mut rows = Vec::new();
    for reply in replies {
        if let Response::Discover {
            node_id,
            proto_major,
            proto_minor,
        } = reply
        {
            // Filter out the broadcast address — we never expect a
            // device to respond claiming node_id 0xF, but guard
            // defensively so a misbehaving stub can't derail the
            // enrichment loop.
            if node_id == BROADCAST_NODE_ID {
                continue;
            }
            let mut row = DiscoverRow::new(node_id, proto_major, proto_minor);
            enrich_with_fw_info(&session, &mut row).await;
            enrich_with_health(&session, &mut row).await;
            rows.push(row);
        }
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if global.json {
        serde_json::to_writer_pretty(&mut out, &rows)
            .context("serialising discover report as JSON")?;
        writeln!(out).ok();
    } else {
        write_human(&mut out, &rows).context("writing discover report")?;
    }

    // Explicit disconnect is a no-op (we never connected) but keeps
    // the tokio tasks shut down deterministically.
    let _ = session.disconnect().await;
    Ok(())
}

async fn enrich_with_fw_info(session: &Session, row: &mut DiscoverRow) {
    let outcome = session
        .send_command_to(row.node_id, &cmd_get_fw_info())
        .await;
    match outcome {
        Ok(Response::Ack { payload, .. }) => {
            // `Response::Ack.payload` has the opcode byte already
            // stripped (see `responses.rs::parse`), so we expect
            // exactly the record here — NOT `[opcode, record]`.
            if payload.len() < FirmwareInfo::SIZE {
                row.fw_error = Some(format!(
                    "GET_FW_INFO ACK too short: got {} bytes",
                    payload.len()
                ));
                return;
            }
            match FirmwareInfo::parse(&payload) {
                Ok(fw) => {
                    let (maj, min, patch) = fw.version();
                    row.fw_version = Some(format!("{maj}.{min}.{patch}"));
                    row.git_hash = Some(format_git_hash(&fw.git_hash));
                    let name = fw.product_name();
                    if !name.is_empty() {
                        row.product = Some(name.to_string());
                    }
                }
                Err(err) => {
                    row.fw_error = Some(format!("bad firmware-info record: {err}"));
                }
            }
        }
        Ok(Response::Nack {
            code: NackCode::NoValidApp,
            ..
        }) => {
            row.fw_error = Some("no app installed".to_string());
        }
        Ok(Response::Nack { code, .. }) => {
            row.fw_error = Some(format!("NACK {code}"));
        }
        Ok(other) => {
            row.fw_error = Some(format!("unexpected reply: {}", other.kind_str()));
        }
        Err(err) => {
            row.fw_error = Some(format!("{err}"));
        }
    }
}

async fn enrich_with_health(session: &Session, row: &mut DiscoverRow) {
    let outcome = session
        .send_command_to(row.node_id, &cmd_get_health())
        .await;
    match outcome {
        Ok(Response::Ack { payload, .. }) => {
            // Same note as `enrich_with_fw_info`: payload is the
            // 32-byte record with the opcode already stripped.
            if payload.len() < HealthRecord::SIZE {
                row.health_error = Some(format!(
                    "GET_HEALTH ACK too short: got {} bytes",
                    payload.len()
                ));
                return;
            }
            match HealthRecord::parse(&payload) {
                Ok(health) => {
                    row.wrp_protected = Some(health.wrp_protected());
                    row.reset_cause = health
                        .reset_cause()
                        .map(|rc: ResetCause| rc.as_str().to_string());
                }
                Err(err) => {
                    row.health_error = Some(format!("bad health record: {err}"));
                }
            }
        }
        Ok(Response::Nack { code, .. }) => {
            row.health_error = Some(format!("NACK {code}"));
        }
        Ok(other) => {
            row.health_error = Some(format!("unexpected reply: {}", other.kind_str()));
        }
        Err(err) => {
            row.health_error = Some(format!("{err}"));
        }
    }
}

fn format_git_hash(bytes: &[u8; 8]) -> String {
    // Show the first 4 bytes as 8 hex chars (matches `git log --oneline`
    // short-hash width). Full 16-char hash is in the JSON output —
    // regenerate it there if anyone needs it.
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3]
    )
}

// ---- Human-readable formatting ----

fn write_human<W: Write>(out: &mut W, rows: &[DiscoverRow]) -> std::io::Result<()> {
    if rows.is_empty() {
        writeln!(
            out,
            "No bootloaders replied to CMD_DISCOVER within the window.\n\
             Hints:\n  \
               * Check that the adapter is up (`can-flasher adapters`).\n  \
               * Confirm the bitrate matches the bus (`--bitrate 500000` is the default).\n  \
               * Try a longer timeout (`--timeout-ms 2000`).\n  \
               * Verify the device has power and is in bootloader mode."
        )?;
        return Ok(());
    }

    // Fixed column set. Widths are recomputed per-run so the output
    // stays tight when short product names appear.
    const H_NODE: &str = "Node";
    const H_PROTO: &str = "Proto";
    const H_FW_VER: &str = "FW Version";
    const H_HASH: &str = "Git Hash";
    const H_PRODUCT: &str = "Product";
    const H_WRP: &str = "WRP";
    const H_RESET: &str = "Reset Cause";

    let node_str: Vec<String> = rows
        .iter()
        .map(|r| format!("0x{:02X}", r.node_id))
        .collect();
    let proto_str: Vec<String> = rows
        .iter()
        .map(|r| format!("{}.{}", r.proto_major, r.proto_minor))
        .collect();
    let fw_str: Vec<String> = rows
        .iter()
        .map(|r| {
            r.fw_version
                .clone()
                .unwrap_or_else(|| r.fw_error.clone().unwrap_or_else(|| "—".to_string()))
        })
        .collect();
    let hash_str: Vec<String> = rows
        .iter()
        .map(|r| r.git_hash.clone().unwrap_or_else(|| "—".to_string()))
        .collect();
    let product_str: Vec<String> = rows
        .iter()
        .map(|r| r.product.clone().unwrap_or_else(|| "—".to_string()))
        .collect();
    let wrp_str: Vec<String> = rows
        .iter()
        .map(|r| match r.wrp_protected {
            Some(true) => "✓".to_string(),
            Some(false) => "✗".to_string(),
            None => "—".to_string(),
        })
        .collect();
    let reset_str: Vec<String> = rows
        .iter()
        .map(|r| {
            r.reset_cause
                .clone()
                .unwrap_or_else(|| r.health_error.clone().unwrap_or_else(|| "—".to_string()))
        })
        .collect();

    let w_node = max_width(H_NODE, &node_str);
    let w_proto = max_width(H_PROTO, &proto_str);
    let w_fw = max_width(H_FW_VER, &fw_str);
    let w_hash = max_width(H_HASH, &hash_str);
    let w_prod = max_width(H_PRODUCT, &product_str);
    let w_wrp = max_width(H_WRP, &wrp_str);
    let w_reset = max_width(H_RESET, &reset_str);

    writeln!(
        out,
        "{:<w_node$}  {:<w_proto$}  {:<w_fw$}  {:<w_hash$}  {:<w_prod$}  {:<w_wrp$}  {:<w_reset$}",
        H_NODE, H_PROTO, H_FW_VER, H_HASH, H_PRODUCT, H_WRP, H_RESET,
    )?;
    let sep = |n: usize| "─".repeat(n);
    writeln!(
        out,
        "{}  {}  {}  {}  {}  {}  {}",
        sep(w_node),
        sep(w_proto),
        sep(w_fw),
        sep(w_hash),
        sep(w_prod),
        sep(w_wrp),
        sep(w_reset),
    )?;
    for i in 0..rows.len() {
        writeln!(
            out,
            "{:<w_node$}  {:<w_proto$}  {:<w_fw$}  {:<w_hash$}  {:<w_prod$}  {:<w_wrp$}  {:<w_reset$}",
            node_str[i],
            proto_str[i],
            fw_str[i],
            hash_str[i],
            product_str[i],
            wrp_str[i],
            reset_str[i],
        )?;
    }
    Ok(())
}

fn max_width(header: &str, cells: &[String]) -> usize {
    cells
        .iter()
        .map(|s| display_width(s))
        .chain(std::iter::once(display_width(header)))
        .max()
        .unwrap_or(0)
}

/// Approximate display width — one column per scalar. Good enough
/// for the ASCII + occasional ✓ / ✗ / — glyphs this table uses;
/// all of them render as one column in standard terminal fonts. We
/// deliberately skip `unicode-width` for what's a purely cosmetic
/// concern.
fn display_width(s: &str) -> usize {
    s.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rows() -> Vec<DiscoverRow> {
        vec![
            DiscoverRow {
                node_id: 0x03,
                proto_major: 0,
                proto_minor: 1,
                fw_version: Some("1.4.2".into()),
                git_hash: Some("a1b2c3d4".into()),
                product: Some("IFS08-CE-ECU".into()),
                fw_error: None,
                wrp_protected: Some(true),
                reset_cause: Some("POWER_ON".into()),
                health_error: None,
            },
            DiscoverRow {
                node_id: 0x05,
                proto_major: 0,
                proto_minor: 1,
                fw_version: None,
                git_hash: None,
                product: None,
                fw_error: Some("no app installed".into()),
                wrp_protected: Some(false),
                reset_cause: Some("IWDG".into()),
                health_error: None,
            },
        ]
    }

    fn human_output(rows: &[DiscoverRow]) -> String {
        let mut buf = Vec::new();
        write_human(&mut buf, rows).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn empty_report_prints_helpful_hint() {
        let s = human_output(&[]);
        assert!(s.contains("No bootloaders replied"));
        assert!(s.contains("--timeout-ms"));
    }

    #[test]
    fn human_output_has_all_columns() {
        let s = human_output(&sample_rows());
        for header in [
            "Node",
            "Proto",
            "FW Version",
            "Git Hash",
            "Product",
            "WRP",
            "Reset Cause",
        ] {
            assert!(s.contains(header), "missing column header {header}: {s}");
        }
    }

    #[test]
    fn human_output_formats_healthy_row_fields() {
        let s = human_output(&sample_rows());
        assert!(s.contains("0x03"));
        assert!(s.contains("0.1"));
        assert!(s.contains("1.4.2"));
        assert!(s.contains("a1b2c3d4"));
        assert!(s.contains("IFS08-CE-ECU"));
        assert!(s.contains("✓"));
        assert!(s.contains("POWER_ON"));
    }

    #[test]
    fn human_output_substitutes_em_dash_for_missing_fields() {
        let rows = vec![DiscoverRow::new(0x02, 0, 1)];
        let s = human_output(&rows);
        // Proto populated, everything else is "—".
        assert!(s.contains("0x02"));
        assert!(s.contains("—"));
    }

    #[test]
    fn human_output_surfaces_fw_error_in_fw_column() {
        let rows = vec![DiscoverRow {
            node_id: 0x05,
            proto_major: 0,
            proto_minor: 1,
            fw_error: Some("no app installed".into()),
            ..DiscoverRow::new(0x05, 0, 1)
        }];
        let s = human_output(&rows);
        assert!(s.contains("no app installed"));
    }

    #[test]
    fn json_shape_omits_missing_optionals() {
        let rows = vec![DiscoverRow::new(0x02, 0, 1)];
        let json = serde_json::to_string(&rows).unwrap();
        assert!(json.contains("\"node_id\":2"));
        assert!(!json.contains("\"fw_version\":"));
        assert!(!json.contains("\"wrp_protected\":"));
    }

    #[test]
    fn json_shape_carries_populated_fields() {
        let json = serde_json::to_string(&sample_rows()).unwrap();
        assert!(json.contains("\"fw_version\":\"1.4.2\""));
        assert!(json.contains("\"git_hash\":\"a1b2c3d4\""));
        assert!(json.contains("\"product\":\"IFS08-CE-ECU\""));
        assert!(json.contains("\"wrp_protected\":true"));
        assert!(json.contains("\"reset_cause\":\"POWER_ON\""));
        assert!(json.contains("\"fw_error\":\"no app installed\""));
    }

    #[test]
    fn format_git_hash_produces_8_hex_chars() {
        let bytes = [0xAB, 0xCD, 0xEF, 0x01, 0xFF, 0xEE, 0xDD, 0xCC];
        assert_eq!(format_git_hash(&bytes), "abcdef01");
    }

    #[test]
    fn max_width_respects_header_when_wider_than_data() {
        let cells = vec!["a".to_string(), "b".to_string()];
        assert_eq!(max_width("FW Version", &cells), "FW Version".len());
    }
}
