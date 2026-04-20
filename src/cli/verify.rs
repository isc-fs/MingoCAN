//! `verify` subcommand — compare installed image against a binary.
//!
//! Flow:
//!
//! 1. Parse the firmware file via [`firmware::loader::load`] — format
//!    detected from magic / extension (ELF / HEX / BIN).
//! 2. Validate the image lands in the bootloader's app region
//!    (`0x08020000`..=`0x080DFFFF`); surface any overlap with the
//!    bootloader's own sector as [`ExitCodeHint::ProtectionViolation`].
//! 3. Compute `(crc32, size, packed_version)`; the CRC is
//!    `CRC-32/ISO-HDLC`, byte-for-byte matching `bl_flash_crc32` on
//!    the device side.
//! 4. Open a session, `CONNECT`, issue
//!    `CMD_FLASH_VERIFY(crc, size, version)`.
//! 5. Map the reply:
//!    - `ACK` → exit 0 ("image matches")
//!    - `NACK(CRC_MISMATCH)` → exit 2 ([`ExitCodeHint::VerifyMismatch`])
//!    - `NACK(…)` / transport error → the default exit 99
//!
//! The device NACKs with `PROTECTED_ADDR` or `OUT_OF_BOUNDS` only if
//! the host somehow managed to ship a verify request outside the
//! writable range — we already block those cases client-side so they
//! should never reach the bus.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use tracing::debug;

use super::{exit_err, ExitCodeHint, GlobalFlags};
use crate::firmware::{self, loader};
use crate::protocol::commands::cmd_flash_verify;
use crate::protocol::opcodes::NackCode;
use crate::protocol::Response;
use crate::session::{Session, SessionConfig};
use crate::transport::open_backend;

#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// Path to .elf, .bin, or .hex firmware file
    #[arg(value_name = "FIRMWARE")]
    pub firmware: PathBuf,

    /// Load address for raw `.bin` inputs. Required for binary files;
    /// ignored for ELF / HEX which carry their own addresses.
    #[arg(long = "address", value_parser = parse_hex_u32)]
    pub address: Option<u32>,
}

pub async fn run(args: VerifyArgs, global: &GlobalFlags) -> Result<()> {
    debug!(firmware = %args.firmware.display(), "verify: loading image");

    // ---- Load + validate image ----

    // `loader::load` now runs per-segment validation before
    // composing — a linker file with a segment in sector 0 fails
    // here rather than after we've allocated a padded buffer.
    // `loader::classify` splits the error family into the
    // per-REQUIREMENTS exit codes (protection violation for
    // address-space problems, input-file error for malformed files
    // and missing --address).
    let image = loader::load(&args.firmware, args.address).map_err(|e| {
        let hint = loader::classify(&e);
        exit_err(
            hint,
            format!("could not load firmware '{}': {e}", args.firmware.display()),
        )
    })?;

    // Belt-and-braces: `loader::load` has already validated each
    // input segment against the app region, so this check should
    // never fire on a real input. Kept as a guard against a future
    // loader refactor that forgets to call `validate_segments`.
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
        // Image is in-region but starts above BL_APP_BASE. The
        // device CRCs from BL_APP_BASE, so our CRC wouldn't match
        // the installed bytes — refuse with a clear message
        // pointing at the linker script.
        return Err(exit_err(
            ExitCodeHint::InputFileError,
            format!(
                "firmware base 0x{:08X} must equal BL_APP_BASE (0x{:08X}); \
                 adjust your linker script so the image starts at 0x{:08X}",
                image.base_addr,
                firmware::BL_APP_BASE,
                firmware::BL_APP_BASE,
            ),
        ));
    }

    let crc = image.crc32();
    let size = image.size();
    let version = image.packed_version();

    debug!(
        crc = format!("0x{crc:08X}"),
        size,
        version = format!("0x{version:08X}"),
        "verify: sending FLASH_VERIFY"
    );

    // ---- Open session + CONNECT + FLASH_VERIFY ----

    let session = open_session(global)?;
    session
        .connect()
        .await
        .context("CONNECT before FLASH_VERIFY")?;

    let resp = session
        .send_command(&cmd_flash_verify(crc, size, version))
        .await
        .context("sending FLASH_VERIFY");

    // Always try to disconnect cleanly, even when the command
    // itself failed. FLASH_VERIFY on success writes the metadata
    // FLASHWORD and returns; no device-side reset, so disconnect
    // should work.
    let _ = session.disconnect().await;
    let resp = resp?;

    match resp {
        Response::Ack { .. } => {
            if global.json {
                println!(
                    r#"{{"status":"ok","firmware":"{}","crc":"0x{crc:08X}","size":{size},"version":"0x{version:08X}"}}"#,
                    args.firmware.display()
                );
            } else {
                println!(
                    "Verified: installed image matches {} (crc=0x{crc:08X}, size={size} B, version=0x{version:08X}).",
                    args.firmware.display()
                );
            }
            Ok(())
        }
        Response::Nack {
            code: NackCode::CrcMismatch,
            ..
        } => Err(exit_err(
            ExitCodeHint::VerifyMismatch,
            format!(
                "installed image differs from {} (device-computed CRC does not match 0x{crc:08X})",
                args.firmware.display()
            ),
        )),
        Response::Nack {
            rejected_opcode,
            code,
        } => Err(anyhow::anyhow!(
            "device NACK'd FLASH_VERIFY (opcode 0x{rejected_opcode:02X}): {code}"
        )),
        other => Err(anyhow::anyhow!(
            "unexpected reply to FLASH_VERIFY: {}",
            other.kind_str()
        )),
    }
}

fn open_session(global: &GlobalFlags) -> Result<Session> {
    let backend = open_backend(global.interface, global.channel.as_deref(), global.bitrate)
        .context("opening CAN backend for verify")?;
    let target_node = global.node_id.unwrap_or(0x3);
    let config = SessionConfig {
        target_node,
        keepalive_interval: std::time::Duration::from_millis(5_000),
        command_timeout: std::time::Duration::from_millis(u64::from(global.timeout_ms)),
        ..SessionConfig::default()
    };
    Ok(Session::attach(backend, config))
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

    #[test]
    fn parse_hex_u32_handles_both_forms() {
        assert_eq!(parse_hex_u32("0x08020000").unwrap(), 0x0802_0000);
        assert_eq!(parse_hex_u32("0X08020000").unwrap(), 0x0802_0000);
        assert_eq!(parse_hex_u32("16").unwrap(), 16);
    }
}
