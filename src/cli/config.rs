//! `config` subcommand — NVM parameter store + option-byte reads and
//! the WRP apply path.
//!
//! The WRP apply action is deliberately chatty in design: it fills in
//! the `BL_OB_APPLY_TOKEN` brick-safety belt automatically so the
//! operator never has to type "0x00505257", prompts before issuing the
//! op, waits for the device to reset, reconnects, and re-reads the
//! mask to confirm the latch took. All of that lands in feat/…-config;
//! today this file just shapes the CLI.

use anyhow::{bail, Result};
use clap::{Args, Subcommand};

use super::GlobalFlags;

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
}

pub async fn run(_args: ConfigArgs, _global: &GlobalFlags) -> Result<()> {
    bail!(
        "`config` is not implemented yet — pending feat branches for \
         protocol core + transport backend. See REQUIREMENTS.md § \
         config subcommand."
    )
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
