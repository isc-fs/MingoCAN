//! `config` subcommand — NVM parameter store + option-byte reads and
//! the WRP apply path.
//!
//! Sub-actions:
//!
//! | Sub             | Session? | Wire |
//! |-----------------|:--------:|------|
//! | `ob read`       | no | `CMD_OB_READ` → 16-byte status record |
//! | `ob apply-wrp`  | **yes** | session-gated `CMD_OB_APPLY_WRP` — auto-fills the brick-safety token |
//! | `nvm read`      | **yes** | `CMD_NVM_READ [key_le16]` |
//! | `nvm write`     | **yes** | `CMD_NVM_WRITE [key_le16, value…]` |
//! | `nvm erase`     | **yes** | `CMD_NVM_WRITE [key_le16]` with zero-byte value (tombstone) |
//!
//! The WRP apply action auto-fills the `BL_OB_APPLY_TOKEN` so operators
//! never type `0x00505257` by hand, prompts before issuing the op
//! (unless `--yes`), and documents the post-reset workflow. On real
//! hardware the device reboots immediately after ACK; the CLI prints
//! a hint pointing at `config ob read` for re-verification.

use std::io::Write;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde::Serialize;
use tracing::debug;

use super::GlobalFlags;
use crate::protocol::commands::{
    cmd_nvm_format, cmd_nvm_read, cmd_nvm_write, cmd_ob_apply_wrp, cmd_ob_read,
};
use crate::protocol::records::ObStatus;
use crate::protocol::Response;
use crate::session::{Session, SessionConfig};
use crate::transport::open_backend;

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Option-byte operations
    Ob {
        #[command(subcommand)]
        action: ObAction,
    },

    /// NVM key-value store operations
    Nvm {
        #[command(subcommand)]
        action: NvmAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum ObAction {
    /// Read option-byte snapshot (16-byte bl_ob_status_t record)
    Read,

    /// Apply WRP to one or more sectors; triggers device reset
    ApplyWrp {
        /// Sector bitmap (bit N set = protect sector N). Default 0x01
        /// protects sector 0 (the bootloader).
        #[arg(long = "sector-mask", default_value = "0x01", value_parser = parse_hex_u32)]
        sector_mask: u32,

        /// Skip the interactive confirmation prompt
        #[arg(long = "yes", default_value_t = false)]
        yes: bool,

        /// Milliseconds to wait for the device to come back after reset
        #[arg(long = "reset-wait-ms", default_value_t = 2_000)]
        reset_wait_ms: u32,
    },
}

#[derive(Debug, Subcommand)]
pub enum NvmAction {
    /// Read a parameter by key
    Read {
        /// 16-bit key, hex (`0x1000`) or decimal
        #[arg(value_parser = parse_hex_u16)]
        key: u16,
    },

    /// Write a parameter
    Write {
        /// 16-bit key, hex (`0x1000`) or decimal
        #[arg(value_parser = parse_hex_u16)]
        key: u16,

        /// Value, either a quoted UTF-8 string or `0x`-prefixed hex blob.
        /// Max 20 bytes (BL_NVM_MAX_VALUE_LEN).
        value: String,
    },

    /// Tombstone a parameter (value-length = 0)
    Erase {
        /// 16-bit key, hex (`0x1000`) or decimal
        #[arg(value_parser = parse_hex_u16)]
        key: u16,
    },

    /// Erase the entire NVM sector — every key + the metadata
    /// FLASHWORD. Destructive; bootloader 0.2+ only. Requires
    /// `--yes` or an interactive confirmation.
    Format {
        /// Skip the interactive confirmation prompt
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
}

pub async fn run(args: ConfigArgs, global: &GlobalFlags) -> Result<()> {
    match args.action {
        ConfigAction::Ob { action } => match action {
            ObAction::Read => run_ob_read(global).await,
            ObAction::ApplyWrp {
                sector_mask,
                yes,
                reset_wait_ms,
            } => run_ob_apply_wrp(sector_mask, yes, reset_wait_ms, global).await,
        },
        ConfigAction::Nvm { action } => match action {
            NvmAction::Read { key } => run_nvm_read(key, global).await,
            NvmAction::Write { key, value } => run_nvm_write(key, value, global).await,
            NvmAction::Erase { key } => run_nvm_erase(key, global).await,
            NvmAction::Format { yes } => run_nvm_format(yes, global).await,
        },
    }
}

// ---- Session / helper wiring ----

fn open_session(global: &GlobalFlags) -> Result<Session> {
    let backend = open_backend(global.interface, global.channel.as_deref(), global.bitrate)
        .context("opening CAN backend for config")?;
    let target_node = global.node_id.unwrap_or(0x3);
    let config = SessionConfig {
        target_node,
        keepalive_interval: Duration::from_millis(5_000),
        command_timeout: Duration::from_millis(u64::from(global.timeout_ms)),
        ..SessionConfig::default()
    };
    Ok(Session::attach(backend, config))
}

fn confirm_prompt(question: &str) -> bool {
    eprint!("{question} [y/N]: ");
    std::io::stderr().flush().ok();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

// ---- ob read ----

#[derive(Debug, Serialize)]
struct ObStatusJson {
    wrp_sector_mask: String,
    wrp_sectors: Vec<u8>,
    user_config: String,
    rdp_level: String,
    bor_level: String,
}

impl ObStatusJson {
    fn from_status(s: &ObStatus) -> Self {
        let mut protected = Vec::new();
        for sector in 0..8 {
            if s.is_sector_protected(sector) {
                protected.push(sector);
            }
        }
        Self {
            wrp_sector_mask: format!("0x{:08X}", s.wrp_sector_mask),
            wrp_sectors: protected,
            user_config: format!("0x{:08X}", s.user_config),
            rdp_level: format!("0x{:02X}", s.rdp_level),
            bor_level: format!("0x{:02X}", s.bor_level),
        }
    }
}

async fn run_ob_read(global: &GlobalFlags) -> Result<()> {
    debug!("config ob read: starting");
    let session = open_session(global)?;
    let resp = session
        .send_command(&cmd_ob_read())
        .await
        .context("sending OB_READ");
    let disconnect = session.disconnect().await;
    let resp = resp?;
    disconnect.ok();

    match resp {
        Response::Ack { payload, .. } => {
            if payload.len() < ObStatus::SIZE {
                bail!(
                    "OB_READ ACK too short: got {} bytes, need {}",
                    payload.len(),
                    ObStatus::SIZE
                );
            }
            let status = ObStatus::parse(&payload).context("parsing OB status record")?;
            render_ob_read(&status, global.json)
        }
        Response::Nack {
            rejected_opcode,
            code,
        } => bail!("device NACK'd OB_READ (opcode 0x{rejected_opcode:02X}): {code}"),
        other => bail!("unexpected reply to OB_READ: {}", other.kind_str()),
    }
}

fn render_ob_read(status: &ObStatus, json: bool) -> Result<()> {
    let snapshot = ObStatusJson::from_status(status);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if json {
        serde_json::to_writer_pretty(&mut out, &snapshot)
            .context("serialising OB status as JSON")?;
        writeln!(out).ok();
        return Ok(());
    }

    writeln!(out, "Option bytes")?;
    writeln!(out, "────────────")?;
    writeln!(out, "  WRP mask     : {}", snapshot.wrp_sector_mask)?;
    if snapshot.wrp_sectors.is_empty() {
        writeln!(out, "  WRP sectors  : (none — no sectors write-protected)")?;
    } else {
        let sectors_str = snapshot
            .wrp_sectors
            .iter()
            .map(|s| format!("#{s}"))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(out, "  WRP sectors  : {sectors_str}")?;
    }
    writeln!(out, "  User config  : {}", snapshot.user_config)?;
    writeln!(out, "  RDP level    : {}", snapshot.rdp_level)?;
    writeln!(out, "  BOR level    : {}", snapshot.bor_level)?;
    Ok(())
}

// ---- ob apply-wrp ----

async fn run_ob_apply_wrp(
    sector_mask: u32,
    yes: bool,
    reset_wait_ms: u32,
    global: &GlobalFlags,
) -> Result<()> {
    if !yes {
        eprintln!(
            "WARNING: OB_APPLY_WRP latches write-protection in flash option bytes.\n\
             On recent H7 silicon WRP can only be cleared via a full chip erase\n\
             through an external debugger. The device will reset after ACK."
        );
        let prompt = format!(
            "About to apply WRP with sector mask 0x{sector_mask:02X} to node \
             0x{:X}. Continue?",
            global.node_id.unwrap_or(0x3)
        );
        if !confirm_prompt(&prompt) {
            bail!("cancelled");
        }
    }

    let session = open_session(global)?;
    session
        .connect()
        .await
        .context("CONNECT before OB_APPLY_WRP")?;

    // Default sector_mask of 0x01 matches the bootloader's handler
    // default. Passing None to cmd_ob_apply_wrp would omit the mask
    // entirely and let the device use its own default (also 0x01);
    // we always send it explicitly so the CLI is the authority on
    // which sectors get latched.
    let payload = cmd_ob_apply_wrp(Some(sector_mask));
    let resp = session.send_command(&payload).await;

    // Stop keepalive cleanly before the device resets under us. The
    // disconnect call may itself error (device is about to reboot or
    // already has); silence that.
    let _ = session.disconnect().await;
    let resp = resp.context("sending OB_APPLY_WRP")?;

    match resp {
        Response::Ack { .. } => {
            if global.json {
                println!(
                    r#"{{"status":"ok","sector_mask":"0x{:02X}","reset_wait_ms":{}}}"#,
                    sector_mask, reset_wait_ms
                );
            } else {
                println!(
                    "OB_APPLY_WRP accepted with sector mask 0x{sector_mask:02X}.\n\
                     The device is now resetting. Wait ~{reset_wait_ms} ms, then run\n  \
                     can-flasher config ob read\n\
                     to confirm the latch took."
                );
            }
            Ok(())
        }
        Response::Nack {
            rejected_opcode,
            code,
        } => bail!("device NACK'd OB_APPLY_WRP (opcode 0x{rejected_opcode:02X}): {code}"),
        other => bail!("unexpected reply to OB_APPLY_WRP: {}", other.kind_str()),
    }
}

// ---- nvm read / write / erase ----

#[derive(Debug, Serialize)]
struct NvmReadJson {
    key: String,
    len: u8,
    value_hex: String,
    value_utf8: Option<String>,
}

async fn run_nvm_read(key: u16, global: &GlobalFlags) -> Result<()> {
    debug!(key = format!("0x{key:04X}"), "config nvm read");
    let session = open_session(global)?;
    session.connect().await.context("CONNECT before NVM_READ")?;

    let resp = session
        .send_command(&cmd_nvm_read(key))
        .await
        .context("sending NVM_READ");
    let _ = session.disconnect().await;
    let resp = resp?;

    match resp {
        Response::Ack { payload, .. } => {
            // payload = [len, value…]  (opcode already stripped)
            if payload.is_empty() {
                bail!("NVM_READ ACK missing length byte");
            }
            let len = payload[0] as usize;
            if payload.len() < 1 + len {
                bail!(
                    "NVM_READ ACK truncated: len={len} but only {} byte(s) of value present",
                    payload.len() - 1
                );
            }
            let value = &payload[1..1 + len];
            render_nvm_read(key, value, global.json)
        }
        Response::Nack {
            rejected_opcode: _,
            code: crate::protocol::opcodes::NackCode::NvmNotFound,
        } => bail!("key 0x{key:04X} not found in NVM"),
        Response::Nack {
            rejected_opcode,
            code,
        } => bail!("device NACK'd NVM_READ (opcode 0x{rejected_opcode:02X}): {code}"),
        other => bail!("unexpected reply to NVM_READ: {}", other.kind_str()),
    }
}

fn render_nvm_read(key: u16, value: &[u8], json: bool) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let hex = value
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join("");
    let utf8 = std::str::from_utf8(value).ok().map(|s| s.to_string());
    if json {
        let row = NvmReadJson {
            key: format!("0x{key:04X}"),
            len: value.len() as u8,
            value_hex: hex,
            value_utf8: utf8,
        };
        serde_json::to_writer_pretty(&mut out, &row)
            .context("serialising NVM read result as JSON")?;
        writeln!(out).ok();
        return Ok(());
    }
    writeln!(out, "Key 0x{key:04X}: {} byte(s)", value.len())?;
    writeln!(out, "  hex  : 0x{hex}")?;
    if let Some(s) = utf8 {
        writeln!(out, "  utf8 : {s:?}")?;
    } else {
        writeln!(out, "  utf8 : (not valid UTF-8)")?;
    }
    Ok(())
}

async fn run_nvm_write(key: u16, value: String, global: &GlobalFlags) -> Result<()> {
    let value_bytes =
        parse_nvm_value(&value).with_context(|| format!("parsing value argument '{value}'"))?;
    if value_bytes.is_empty() {
        bail!(
            "empty value would tombstone the key — use `config nvm erase 0x{key:04X}` \
             for that (explicit intent avoids accidents)"
        );
    }
    if value_bytes.len() > 20 {
        bail!(
            "value too long: {} bytes exceeds BL_NVM_MAX_VALUE_LEN (20)",
            value_bytes.len()
        );
    }

    let session = open_session(global)?;
    session
        .connect()
        .await
        .context("CONNECT before NVM_WRITE")?;

    let resp = session
        .send_command(&cmd_nvm_write(key, &value_bytes))
        .await
        .context("sending NVM_WRITE");
    let _ = session.disconnect().await;
    let resp = resp?;

    match resp {
        Response::Ack { .. } => {
            if global.json {
                println!(
                    r#"{{"status":"ok","key":"0x{key:04X}","bytes_written":{}}}"#,
                    value_bytes.len()
                );
            } else {
                println!(
                    "Wrote {} byte(s) to NVM key 0x{key:04X}.",
                    value_bytes.len()
                );
            }
            Ok(())
        }
        Response::Nack {
            rejected_opcode,
            code,
        } => bail!("device NACK'd NVM_WRITE (opcode 0x{rejected_opcode:02X}): {code}"),
        other => bail!("unexpected reply to NVM_WRITE: {}", other.kind_str()),
    }
}

async fn run_nvm_erase(key: u16, global: &GlobalFlags) -> Result<()> {
    let session = open_session(global)?;
    session
        .connect()
        .await
        .context("CONNECT before NVM_WRITE (erase)")?;

    // Tombstone: value_len = 0
    let resp = session
        .send_command(&cmd_nvm_write(key, &[]))
        .await
        .context("sending NVM_WRITE (tombstone)");
    let _ = session.disconnect().await;
    let resp = resp?;

    match resp {
        Response::Ack { .. } => {
            if global.json {
                println!(r#"{{"status":"ok","key":"0x{key:04X}","action":"erased"}}"#);
            } else {
                println!("NVM key 0x{key:04X} tombstoned.");
            }
            Ok(())
        }
        Response::Nack {
            rejected_opcode,
            code,
        } => bail!("device NACK'd NVM_WRITE (opcode 0x{rejected_opcode:02X}): {code}"),
        other => bail!("unexpected reply to NVM_WRITE: {}", other.kind_str()),
    }
}

// ---- nvm format ----

async fn run_nvm_format(yes: bool, global: &GlobalFlags) -> Result<()> {
    if !yes {
        eprintln!(
            "WARNING: NVM_FORMAT erases the entire NVM sector — every key + the\n\
             metadata FLASHWORD. There is no undo. The bootloader's internal\n\
             pointers reset after the format completes."
        );
        let prompt = format!(
            "About to format the NVM sector on node 0x{:X}. Continue?",
            global.node_id.unwrap_or(0x3)
        );
        if !confirm_prompt(&prompt) {
            bail!("cancelled");
        }
    }

    let session = open_session(global)?;
    session
        .connect()
        .await
        .context("CONNECT before NVM_FORMAT")?;

    let resp = session
        .send_command(&cmd_nvm_format())
        .await
        .context("sending NVM_FORMAT");
    let _ = session.disconnect().await;
    let resp = resp?;

    match resp {
        Response::Ack { .. } => {
            if global.json {
                println!(r#"{{"status":"ok","action":"formatted"}}"#);
            } else {
                println!("NVM sector formatted.");
            }
            Ok(())
        }
        Response::Nack {
            rejected_opcode,
            code,
        } => bail!("device NACK'd NVM_FORMAT (opcode 0x{rejected_opcode:02X}): {code}"),
        other => bail!("unexpected reply to NVM_FORMAT: {}", other.kind_str()),
    }
}

/// Parse the value argument to `config nvm write`. Accepts either a
/// `0x`-prefixed hex blob (even digit count required) or a plain
/// UTF-8 string.
fn parse_nvm_value(raw: &str) -> Result<Vec<u8>> {
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        if hex.is_empty() {
            bail!("hex value must have at least one digit pair");
        }
        if hex.len() % 2 != 0 {
            bail!(
                "hex value must have an even number of digits (got {}): '{hex}'",
                hex.len()
            );
        }
        let mut bytes = Vec::with_capacity(hex.len() / 2);
        for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
            let s = std::str::from_utf8(chunk)
                .with_context(|| format!("non-ASCII in hex pair at offset {}", i * 2))?;
            let b = u8::from_str_radix(s, 16)
                .with_context(|| format!("bad hex pair '{s}' at offset {}", i * 2))?;
            bytes.push(b);
        }
        Ok(bytes)
    } else {
        Ok(raw.as_bytes().to_vec())
    }
}

fn parse_hex_u16(raw: &str) -> Result<u16, String> {
    let trimmed = raw.trim();
    let (body, radix) = if let Some(rest) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        (rest, 16)
    } else {
        (trimmed, 10)
    };
    u16::from_str_radix(body, radix).map_err(|e| format!("invalid u16 '{raw}': {e}"))
}

fn parse_hex_u32(raw: &str) -> Result<u32, String> {
    let trimmed = raw.trim();
    let (body, radix) = if let Some(rest) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        (rest, 16)
    } else {
        (trimmed, 10)
    };
    u32::from_str_radix(body, radix).map_err(|e| format!("invalid u32 '{raw}': {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::records::{ObStatus, OB_APPLY_TOKEN};

    #[test]
    fn parse_nvm_value_accepts_plain_string() {
        assert_eq!(parse_nvm_value("hello").unwrap(), b"hello".to_vec());
        assert_eq!(parse_nvm_value("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn parse_nvm_value_accepts_hex_blob() {
        assert_eq!(
            parse_nvm_value("0xDEADBEEF").unwrap(),
            vec![0xDE, 0xAD, 0xBE, 0xEF]
        );
        assert_eq!(parse_nvm_value("0x00").unwrap(), vec![0x00]);
        assert_eq!(parse_nvm_value("0xAA").unwrap(), vec![0xAA]);
    }

    #[test]
    fn parse_nvm_value_accepts_lowercase_0x() {
        assert_eq!(parse_nvm_value("0XFF").unwrap(), vec![0xFF]);
    }

    #[test]
    fn parse_nvm_value_rejects_odd_digit_hex() {
        let err = parse_nvm_value("0xABC").unwrap_err();
        assert!(err.to_string().contains("even number of digits"));
    }

    #[test]
    fn parse_nvm_value_rejects_empty_hex() {
        let err = parse_nvm_value("0x").unwrap_err();
        assert!(err.to_string().contains("at least one digit pair"));
    }

    #[test]
    fn parse_nvm_value_rejects_bad_hex_char() {
        let err = parse_nvm_value("0xGG").unwrap_err();
        assert!(err.to_string().contains("bad hex pair"));
    }

    #[test]
    fn ob_status_json_lists_protected_sectors() {
        let status = ObStatus {
            wrp_sector_mask: 0b0000_1001, // sectors 0 + 3
            user_config: 0xCAFE_BABE,
            rdp_level: 0xAA,
            bor_level: 0x01,
            reserved: [0, 0],
            reserved_ext: 0,
        };
        let json = ObStatusJson::from_status(&status);
        assert_eq!(json.wrp_sectors, vec![0, 3]);
        assert_eq!(json.wrp_sector_mask, "0x00000009");
        assert_eq!(json.user_config, "0xCAFEBABE");
    }

    #[test]
    fn ob_status_json_has_empty_sectors_when_nothing_protected() {
        let status = ObStatus {
            wrp_sector_mask: 0,
            user_config: 0,
            rdp_level: 0xAA,
            bor_level: 0,
            reserved: [0, 0],
            reserved_ext: 0,
        };
        let json = ObStatusJson::from_status(&status);
        assert!(json.wrp_sectors.is_empty());
    }

    #[test]
    fn ob_apply_token_const_matches_protocol() {
        // Keeps the CLI and the protocol module in sync. If this
        // changes, host-side builders and device-side handlers both
        // need the new value.
        assert_eq!(OB_APPLY_TOKEN, 0x00505257);
    }
}
