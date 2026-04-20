//! `flash` subcommand — program firmware to a device.
//!
//! Wiring layer on top of the engines that landed earlier in Phase 5:
//!
//! 1. **`firmware::loader::load`** parses ELF / HEX / raw .bin into a
//!    normalised `Image` and fails early with the right exit code
//!    (3 for address-space violations, 8 for malformed input).
//! 2. **Session + CONNECT** — opens the backend, attaches a session,
//!    performs `CMD_CONNECT`. Timeout maps to exit 4 (DeviceNotFound).
//! 3. **WRP policy** — `--require-wrp` gates on `CMD_OB_READ`
//!    (fails with exit 7 if sector 0 not latched). `--apply-wrp`
//!    issues `CMD_OB_APPLY_WRP` first; the bootloader resets after
//!    OB writes, so the session layer's reconnect-on-BAD_SESSION
//!    path kicks in before the flash pipeline starts.
//! 4. **`FlashManager::run`** drives the diff/erase/write/verify/
//!    commit pipeline. A background tokio task renders events
//!    through an indicatif `MultiProgress` (human) or emits
//!    JSON-per-line (`--json`).
//! 5. **Optional `CMD_JUMP`** — default true. On success the
//!    bootloader ACKs and jumps to the app at `BL_APP_BASE`.
//! 6. **Exit-code routing** — every error type funnels through
//!    `exit_err(hint, ...)` so the CLI exit code matches the
//!    REQUIREMENTS.md table.
//!
//! Audit logging (SQLite row, REQUIREMENTS § 8.3) is deferred to
//! post-v1 (`feat/18-audit-log`) — the tool works without it and
//! rusqlite would bloat the single-static-binary promise.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use clap::Args;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::Serialize;
use tokio::sync::mpsc;
use tracing::debug;

use super::{exit_err, ExitCodeHint, GlobalFlags};
use crate::firmware::{self, loader};
use crate::flash::{FlashConfig, FlashEvent, FlashManager, FlashReport, SectorRole};
use crate::protocol::commands::{cmd_jump, cmd_ob_apply_wrp, cmd_ob_read};
use crate::protocol::opcodes::CommandOpcode;
use crate::protocol::records::ObStatus;
use crate::protocol::Response;
use crate::session::{Session, SessionConfig};
use crate::transport::open_backend;

// ---- Args ----

#[derive(Debug, Args)]
pub struct FlashArgs {
    /// Path to .elf, .bin, or .hex firmware file
    #[arg(value_name = "FIRMWARE")]
    pub firmware: PathBuf,

    /// Override load address (required for raw .bin only)
    #[arg(long = "address", value_name = "ADDR", value_parser = parse_hex_u32)]
    pub address: Option<u32>,

    /// Abort if bootloader sector not write-protected
    #[arg(long = "require-wrp", default_value_t = false)]
    pub require_wrp: bool,

    /// Apply WRP if missing, then continue
    #[arg(long = "apply-wrp", default_value_t = false)]
    pub apply_wrp: bool,

    /// Only flash sectors that differ from device contents
    #[arg(long = "diff", default_value_t = true, overrides_with = "no_diff")]
    pub diff: bool,

    /// Force-write every sector regardless of device CRC
    #[arg(long = "no-diff", default_value_t = false)]
    pub no_diff: bool,

    /// Validate and simulate without sending erase/write commands
    #[arg(long = "dry-run", default_value_t = false)]
    pub dry_run: bool,

    /// Readback CRC verification after flash
    #[arg(
        long = "verify-after",
        default_value_t = true,
        overrides_with = "no_verify_after"
    )]
    pub verify_after: bool,

    /// Skip post-flash verification
    #[arg(long = "no-verify-after", default_value_t = false)]
    pub no_verify_after: bool,

    /// Jump to application after successful flash
    #[arg(long = "jump", default_value_t = true, overrides_with = "no_jump")]
    pub jump: bool,

    /// Stay in bootloader mode after flash
    #[arg(long = "no-jump", default_value_t = false)]
    pub no_jump: bool,

    /// Session keepalive interval in milliseconds
    #[arg(long = "keepalive-ms", default_value_t = 5_000)]
    pub keepalive_ms: u32,
}

// ---- Entry point ----

pub async fn run(args: FlashArgs, global: &GlobalFlags) -> Result<()> {
    debug!(firmware = %args.firmware.display(), "flash: loading image");

    // ---- 1. Load + validate firmware ----

    let image = loader::load(&args.firmware, args.address).map_err(|e| {
        exit_err(
            loader::classify(&e),
            format!("could not load firmware '{}': {e}", args.firmware.display()),
        )
    })?;

    // Belt-and-braces — the loader already validated per-segment,
    // so this guard only fires if a future loader refactor forgets
    // to call `validate_segments`.
    if let Err(e) = image.validate_fits_app_region() {
        return Err(exit_err(
            ExitCodeHint::ProtectionViolation,
            format!(
                "firmware '{}' does not fit the app region: {e}",
                args.firmware.display()
            ),
        ));
    }
    if image.base_addr != firmware::BL_APP_BASE {
        return Err(exit_err(
            ExitCodeHint::InputFileError,
            format!(
                "firmware base 0x{:08X} must equal BL_APP_BASE (0x{:08X}) — \
                 adjust your linker script",
                image.base_addr,
                firmware::BL_APP_BASE,
            ),
        ));
    }

    let wall_start = SystemTime::now();

    // ---- 2. Open session + CONNECT ----

    let session = open_session(global, args.keepalive_ms)?;
    let (proto_major, proto_minor) = session
        .connect()
        .await
        .map_err(|e| exit_err(ExitCodeHint::DeviceNotFound, format!("CONNECT failed: {e}")))?;

    // ---- 3. WRP policy (pre-flash) ----

    let ob_status =
        apply_wrp_policy(&session, args.require_wrp, args.apply_wrp, &args.firmware).await?;

    // ---- 4. Run the flash pipeline ----

    let config = build_flash_config(&args);
    let (tx, rx) = mpsc::unbounded_channel::<FlashEvent>();
    let json_mode = global.json;
    let progress_task = tokio::spawn(render_progress(rx, json_mode));

    let manager = FlashManager::new(&session, &image, config.clone());
    let report_result = manager.run(Some(tx)).await;
    let _ = progress_task.await;

    let report = match report_result {
        Ok(r) => r,
        Err(e) => {
            // Best-effort disconnect, then surface the error with
            // the engine-assigned exit code.
            let _ = session.disconnect().await;
            return Err(exit_err(
                e.exit_code_hint(),
                format!("flash failed on '{}': {e}", args.firmware.display()),
            ));
        }
    };

    // ---- 5. Optional JUMP ----

    // `--no-jump` wins over `--jump` — same convention as diff /
    // verify-after. clap's `overrides_with` only settles conflicts
    // when both flags are present; a lone `--no-jump` leaves the
    // default-true `jump` untouched, so we reconcile here.
    let jump = !args.no_jump && args.jump;
    if jump && !args.dry_run {
        fire_jump(&session).await?;
    }

    // Disconnect before we print — the device's reset/jump
    // sometimes eats the ACK, so we disconnect best-effort and
    // ignore errors here.
    let _ = session.disconnect().await;

    // ---- 6. Report ----

    print_report(
        &args,
        global,
        &image,
        &report,
        &ob_status,
        proto_major,
        proto_minor,
        wall_start,
        jump,
    )?;
    Ok(())
}

// ---- WRP policy ----

/// Returns the [`ObStatus`] we saw (or applied). Used in the final
/// report's `bootloader.wrp_protected` field.
async fn apply_wrp_policy(
    session: &Session,
    require_wrp: bool,
    apply_wrp: bool,
    firmware: &std::path::Path,
) -> Result<ObStatus> {
    // Always read OB up-front so the report reflects reality, even
    // when neither flag is set.
    let ob = read_ob_status(session).await?;
    let sector0_protected = ob.is_sector_protected(0);

    if apply_wrp && !sector0_protected {
        debug!("flash: --apply-wrp and sector 0 unprotected — issuing OB_APPLY_WRP");
        // Default bitmap (0x01) protects sector 0, which is exactly
        // what we want before a flash pipeline runs.
        let resp = session
            .send_command(&cmd_ob_apply_wrp(None))
            .await
            .map_err(|e| {
                exit_err(
                    ExitCodeHint::WrpNotApplied,
                    format!("OB_APPLY_WRP failed: {e}"),
                )
            })?;
        match resp {
            Response::Ack { opcode, .. } if opcode == CommandOpcode::ObApplyWrp.as_byte() => {}
            Response::Nack {
                rejected_opcode,
                code,
            } => {
                return Err(exit_err(
                    ExitCodeHint::WrpNotApplied,
                    format!("OB_APPLY_WRP NACK'd (opcode 0x{rejected_opcode:02X}): {code}"),
                ));
            }
            other => {
                return Err(exit_err(
                    ExitCodeHint::WrpNotApplied,
                    format!("unexpected reply to OB_APPLY_WRP: {}", other.kind_str()),
                ));
            }
        }
        // Bootloader resets after OB writes; the session layer
        // re-establishes the connection on the next command via
        // the BAD_SESSION retry path. Issue a second OB_READ so the
        // report reflects the now-applied mask.
        let after = read_ob_status(session).await?;
        return Ok(after);
    }

    if require_wrp && !sector0_protected {
        return Err(exit_err(
            ExitCodeHint::WrpNotApplied,
            format!(
                "--require-wrp: sector 0 is not WRP-protected on this device \
                 (wrp_sector_mask=0x{:08X}); aborting flash of '{}'. \
                 Pass --apply-wrp to latch it automatically.",
                ob.wrp_sector_mask,
                firmware.display(),
            ),
        ));
    }

    Ok(ob)
}

async fn read_ob_status(session: &Session) -> Result<ObStatus> {
    // No `ExitCodeHint` here — OB_READ failures during the flash
    // pipeline fall through to the generic exit 99. They're a
    // protocol problem, not a user-facing policy failure; the
    // message is what matters.
    let resp = session
        .send_command(&cmd_ob_read())
        .await
        .context("sending OB_READ")?;
    match resp {
        Response::Ack { opcode, payload } => {
            if opcode != CommandOpcode::ObRead.as_byte() {
                return Err(anyhow::anyhow!(
                    "OB_READ returned wrong opcode 0x{opcode:02X}"
                ));
            }
            let status = ObStatus::parse(&payload).context("could not parse OB_READ payload")?;
            Ok(status)
        }
        other => Err(anyhow::anyhow!(
            "unexpected reply to OB_READ: {}",
            other.kind_str()
        )),
    }
}

// ---- JUMP ----

async fn fire_jump(session: &Session) -> Result<()> {
    let resp = session
        .send_command(&cmd_jump(firmware::BL_APP_BASE))
        .await
        .map_err(|e| exit_err(ExitCodeHint::FlashError, format!("JUMP failed: {e}")))?;
    match resp {
        Response::Ack { opcode, .. } if opcode == CommandOpcode::Jump.as_byte() => Ok(()),
        Response::Nack {
            rejected_opcode,
            code,
        } => Err(exit_err(
            ExitCodeHint::FlashError,
            format!("JUMP NACK'd (opcode 0x{rejected_opcode:02X}): {code}"),
        )),
        other => Err(exit_err(
            ExitCodeHint::FlashError,
            format!("unexpected reply to JUMP: {}", other.kind_str()),
        )),
    }
}

// ---- Progress rendering ----

async fn render_progress(mut rx: mpsc::UnboundedReceiver<FlashEvent>, json: bool) {
    if json {
        while let Some(event) = rx.recv().await {
            if let Ok(line) = serde_json::to_string(&JsonEvent::from(&event)) {
                println!("{line}");
            }
        }
        return;
    }

    let multi = MultiProgress::new();
    let overall = multi.add(ProgressBar::new_spinner());
    overall.set_style(
        ProgressStyle::with_template("{spinner:.green} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    overall.set_message("Flashing…");
    overall.enable_steady_tick(Duration::from_millis(120));

    let mut per_sector: Option<(u8, ProgressBar)> = None;

    while let Some(event) = rx.recv().await {
        match event {
            FlashEvent::PlanningSector { sector, role } => {
                let txt = match role {
                    SectorRole::Skip => format!("Sector {sector}: already matches — skipping"),
                    SectorRole::Write => format!("Sector {sector}: queued for rewrite"),
                };
                multi.println(txt).ok();
            }
            FlashEvent::Erased { sector } => {
                multi.println(format!("Sector {sector}: erased")).ok();
            }
            FlashEvent::ChunkWritten {
                sector,
                bytes,
                total,
            } => {
                let bar = match &per_sector {
                    Some((s, bar)) if *s == sector => bar.clone(),
                    _ => {
                        // First chunk of a new sector — finish any
                        // previous bar and start a fresh one.
                        if let Some((_, old)) = per_sector.take() {
                            old.finish_and_clear();
                        }
                        let bar = multi.add(ProgressBar::new(u64::from(total)));
                        bar.set_style(
                            ProgressStyle::with_template(
                                "  sector {prefix} [{bar:30.cyan/blue}] {bytes}/{total_bytes}",
                            )
                            .unwrap_or_else(|_| ProgressStyle::default_bar())
                            .progress_chars("█▉▊▋▌▍▎▏ "),
                        );
                        bar.set_prefix(format!("{sector}"));
                        per_sector = Some((sector, bar.clone()));
                        bar
                    }
                };
                bar.set_position(u64::from(bytes));
                if bytes >= total {
                    bar.finish_and_clear();
                    per_sector = None;
                }
            }
            FlashEvent::SectorVerified { sector, crc } => {
                multi
                    .println(format!("Sector {sector}: verified (crc=0x{crc:08X})"))
                    .ok();
            }
            FlashEvent::Committing => {
                overall.set_message("Committing metadata…");
            }
            FlashEvent::Done { report } => {
                overall.finish_with_message(format!(
                    "Done — erased {} written {} skipped {} in {} ms",
                    report.sectors_erased.len(),
                    report.sectors_written.len(),
                    report.sectors_skipped.len(),
                    report.duration.as_millis(),
                ));
            }
        }
    }
}

// ---- Report output ----

#[derive(Serialize)]
struct JsonReport<'a> {
    status: &'static str,
    firmware: JsonFirmware<'a>,
    bootloader: JsonBootloader,
    sectors_erased: &'a [u8],
    sectors_written: &'a [u8],
    sectors_skipped: &'a [u8],
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

#[derive(Serialize)]
struct JsonFirmware<'a> {
    path: &'a str,
    crc32: String,
    size_bytes: u32,
    version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    product_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_hash: Option<String>,
}

#[derive(Serialize)]
struct JsonBootloader {
    proto_major: u8,
    proto_minor: u8,
    wrp_protected: bool,
    wrp_sector_mask: String,
}

#[allow(clippy::too_many_arguments)]
fn print_report(
    args: &FlashArgs,
    global: &GlobalFlags,
    image: &firmware::Image,
    report: &FlashReport,
    ob: &ObStatus,
    proto_major: u8,
    proto_minor: u8,
    wall_start: SystemTime,
    jump: bool,
) -> Result<()> {
    let wall_ms = wall_start
        .elapsed()
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    if global.json {
        let (major, minor, patch) = image
            .fw_info
            .as_ref()
            .map(|fw| fw.version())
            .unwrap_or((0, 0, 0));
        let version = format!("{major}.{minor}.{patch}");
        let product = image
            .fw_info
            .as_ref()
            .map(|fw| fw.product_name().to_string());
        let git_hash = image.fw_info.as_ref().map(|fw| hex_encode(&fw.git_hash));
        let json = JsonReport {
            status: if args.dry_run { "dry-run" } else { "ok" },
            firmware: JsonFirmware {
                path: args.firmware.to_str().unwrap_or(""),
                crc32: format!("0x{:08X}", report.crc32),
                size_bytes: report.size,
                version,
                product_name: product,
                git_hash,
            },
            bootloader: JsonBootloader {
                proto_major,
                proto_minor,
                wrp_protected: ob.is_sector_protected(0),
                wrp_sector_mask: format!("0x{:08X}", ob.wrp_sector_mask),
            },
            sectors_erased: &report.sectors_erased,
            sectors_written: &report.sectors_written,
            sectors_skipped: &report.sectors_skipped,
            duration_ms: wall_ms,
            error: None,
        };
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        let verb = if args.dry_run { "Dry-ran" } else { "Flashed" };
        println!(
            "{verb} {} (crc=0x{:08X}, size={} B, version=0x{:08X}).",
            args.firmware.display(),
            report.crc32,
            report.size,
            report.version,
        );
        println!(
            "  sectors: erased={:?}, written={:?}, skipped={:?}",
            report.sectors_erased, report.sectors_written, report.sectors_skipped,
        );
        println!(
            "  duration: {} ms (engine), {wall_ms} ms (wall)",
            report.duration.as_millis()
        );
        if jump && !args.dry_run {
            println!("  jumped to app at 0x{:08X}.", firmware::BL_APP_BASE);
        }
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---- Config + session helpers ----

fn build_flash_config(args: &FlashArgs) -> FlashConfig {
    // Negative flags (--no-diff / --no-verify-after / --no-jump)
    // win over their positive counterparts — clap's `overrides_with`
    // only handles one direction, so we settle ties here.
    let diff = if args.no_diff { false } else { args.diff };
    let verify_after = if args.no_verify_after {
        false
    } else {
        args.verify_after
    };
    FlashConfig {
        diff,
        dry_run: args.dry_run,
        verify_after,
        write_chunk_size: crate::flash::DEFAULT_WRITE_CHUNK,
        final_commit: !args.dry_run,
    }
}

fn open_session(global: &GlobalFlags, keepalive_ms: u32) -> Result<Session> {
    let backend = open_backend(global.interface, global.channel.as_deref(), global.bitrate)
        .map_err(|e| {
            exit_err(
                ExitCodeHint::AdapterMissing,
                format!("opening CAN backend: {e}"),
            )
        })?;
    let target_node = global.node_id.unwrap_or(0x3);
    let config = SessionConfig {
        target_node,
        keepalive_interval: Duration::from_millis(u64::from(keepalive_ms)),
        command_timeout: Duration::from_millis(u64::from(global.timeout_ms)),
        ..SessionConfig::default()
    };
    Ok(Session::attach(backend, config))
}

// ---- JSON event shape ----

/// Mirror of [`FlashEvent`] as a line-JSON object for `--json` mode.
/// Kept separate so the engine's event enum stays a pure Rust type
/// (no serde leakage).
#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum JsonEvent {
    Planning { sector: u8, role: &'static str },
    Erased { sector: u8 },
    Written { sector: u8, bytes: u32, total: u32 },
    Verified { sector: u8, crc: String },
    Committing,
    Done { duration_ms: u64 },
}

impl JsonEvent {
    fn from(ev: &FlashEvent) -> Self {
        match ev {
            FlashEvent::PlanningSector { sector, role } => Self::Planning {
                sector: *sector,
                role: match role {
                    SectorRole::Skip => "skip",
                    SectorRole::Write => "write",
                },
            },
            FlashEvent::Erased { sector } => Self::Erased { sector: *sector },
            FlashEvent::ChunkWritten {
                sector,
                bytes,
                total,
            } => Self::Written {
                sector: *sector,
                bytes: *bytes,
                total: *total,
            },
            FlashEvent::SectorVerified { sector, crc } => Self::Verified {
                sector: *sector,
                crc: format!("0x{crc:08X}"),
            },
            FlashEvent::Committing => Self::Committing,
            FlashEvent::Done { report } => Self::Done {
                duration_ms: report.duration.as_millis() as u64,
            },
        }
    }
}

// ---- hex u32 parser ----

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

    #[test]
    fn build_flash_config_honours_negative_flags() {
        let args = FlashArgs {
            firmware: PathBuf::from("x.bin"),
            address: None,
            require_wrp: false,
            apply_wrp: false,
            diff: true,
            no_diff: true,
            dry_run: false,
            verify_after: true,
            no_verify_after: true,
            jump: true,
            no_jump: false,
            keepalive_ms: 5000,
        };
        let cfg = build_flash_config(&args);
        assert!(!cfg.diff, "--no-diff should win");
        assert!(!cfg.verify_after, "--no-verify-after should win");
        assert!(cfg.final_commit, "final_commit only off under dry_run");
    }

    #[test]
    fn build_flash_config_dry_run_skips_final_commit() {
        let args = FlashArgs {
            firmware: PathBuf::from("x.bin"),
            address: None,
            require_wrp: false,
            apply_wrp: false,
            diff: true,
            no_diff: false,
            dry_run: true,
            verify_after: true,
            no_verify_after: false,
            jump: true,
            no_jump: false,
            keepalive_ms: 5000,
        };
        let cfg = build_flash_config(&args);
        assert!(cfg.dry_run);
        assert!(!cfg.final_commit, "dry-run disables final FLASH_VERIFY");
    }

    #[test]
    fn parse_hex_u32_handles_both_forms() {
        assert_eq!(parse_hex_u32("0x08020000").unwrap(), 0x0802_0000);
        assert_eq!(parse_hex_u32("16").unwrap(), 16);
    }

    #[test]
    fn hex_encode_lowercase() {
        assert_eq!(hex_encode(&[0xDE, 0xAD]), "dead");
    }
}
