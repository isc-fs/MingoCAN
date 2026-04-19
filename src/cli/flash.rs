//! `flash` subcommand — program firmware to a device.
//!
//! Behaviour lands in a later feat branch (sequence: protocol core →
//! virtual backend → SLCAN backend → flash pipeline). This file
//! defines the arg struct today so `clap` generates the right
//! `--help` output against the skeleton.

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Args;

use super::GlobalFlags;

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

pub async fn run(_args: FlashArgs, _global: &GlobalFlags) -> Result<()> {
    bail!(
        "`flash` is not implemented yet — pending feat branches for \
         protocol core + transport backend + flash pipeline. See \
         REQUIREMENTS.md § flash subcommand for the target shape."
    )
}

/// Parse a 32-bit integer from hex (`0x…`) or decimal. Used by flash
/// address overrides and a few other integer flags that accept both
/// forms. Lives here in feat/2; moves to a shared util module once a
/// second caller appears.
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
