//! CLI argument types — one top-level `Cli` struct, the global flag
//! bundle every subcommand sees, and the subcommand enum that
//! dispatches into `flash` / `verify` / `discover` / …
//!
//! Keeping clap's `#[derive(Parser)]` surfaces here lets individual
//! `cli::<cmd>` modules stay focused on behaviour rather than arg
//! parsing. The types themselves are public so those modules can
//! accept them by value.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

pub mod adapters;
pub mod config;
pub mod diagnose;
pub mod discover;
pub mod flash;
pub mod replay;
pub mod verify;

// ---- Top-level ----

#[derive(Debug, Parser)]
#[command(
    name    = "can-flasher",
    version,
    about   = "Host-side CAN flasher for the isc-fs STM32 CAN bootloader",
    long_about = None,
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalFlags,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Flash firmware to a device
    Flash(flash::FlashArgs),

    /// Verify flash contents against a binary without writing
    Verify(verify::VerifyArgs),

    /// Scan the bus and list all bootloader-mode devices
    Discover(discover::DiscoverArgs),

    /// Read/clear DTCs, stream logs, stream live data, session health
    Diagnose(diagnose::DiagnoseArgs),

    /// Read/write device configuration (NVM) and option bytes (WRP)
    Config(config::ConfigArgs),

    /// Record or replay a CAN session (testing)
    Replay(replay::ReplayArgs),

    /// List detected CAN adapters on this machine
    Adapters,
}

// ---- Shared global flags ----

#[derive(Debug, Args)]
pub struct GlobalFlags {
    /// CAN backend
    #[arg(short = 'i', long = "interface", value_enum, default_value_t = InterfaceType::Slcan, global = true)]
    pub interface: InterfaceType,

    /// Adapter channel (see REQUIREMENTS.md § --channel format table)
    #[arg(short = 'c', long = "channel", global = true)]
    pub channel: Option<String>,

    /// Nominal CAN bitrate
    #[arg(
        short = 'b',
        long = "bitrate",
        default_value_t = 500_000,
        global = true
    )]
    pub bitrate: u32,

    /// Target node ID (hex `0x0A` or decimal `10`). Defaults to broadcast (0xF).
    #[arg(long = "node-id", global = true, value_parser = parse_node_id)]
    pub node_id: Option<u8>,

    /// Per-frame timeout in milliseconds
    #[arg(long = "timeout", default_value_t = 500, global = true)]
    pub timeout_ms: u32,

    /// Machine-readable JSON output on stdout
    #[arg(long = "json", default_value_t = false, global = true)]
    pub json: bool,

    /// Append session to audit log (SQLite)
    #[arg(long = "log", global = true)]
    pub log_path: Option<PathBuf>,

    /// Trace-level logging
    #[arg(long = "verbose", default_value_t = false, global = true)]
    pub verbose: bool,

    /// Override operator name in audit log
    #[arg(long = "operator", global = true)]
    pub operator: Option<String>,
}

/// Backend identifier. The Linux-only `Socketcan` variant compiles on
/// every platform so `--help` output stays uniform; the selector in
/// `open_backend` routes it through `SocketCanBackend` on Linux and
/// errors with a clear message on Windows / macOS (handled later in
/// `feat/4-socketcan-backend`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InterfaceType {
    /// SLCAN serial — CANable and compatible adapters, all platforms
    Slcan,
    /// Native SocketCAN kernel socket — Linux only
    Socketcan,
    /// PEAK PCAN — SocketCAN on Linux, PCAN-Basic SDK on Win/macOS
    Pcan,
    /// In-process virtual bus for testing
    Virtual,
}

/// Parse a node ID from either `0x0A`-style hex or plain decimal.
/// Lives here (rather than a shared `util::`) so feat/2 stays one
/// module deep; it'll migrate once a real utility module appears.
fn parse_node_id(raw: &str) -> Result<u8, String> {
    let trimmed = raw.trim();
    let (body, radix) = if let Some(rest) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        (rest, 16)
    } else {
        (trimmed, 10)
    };
    u8::from_str_radix(body, radix)
        .map_err(|e| format!("invalid node id '{raw}': {e}"))
        .and_then(|n| {
            if n > 0xF {
                Err(format!(
                    "node id must fit in 4 bits (0x0..0xF); got 0x{n:X}"
                ))
            } else {
                Ok(n)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::parse_node_id;

    #[test]
    fn node_id_accepts_hex() {
        assert_eq!(parse_node_id("0x3").unwrap(), 0x3);
        assert_eq!(parse_node_id("0xF").unwrap(), 0xF);
    }

    #[test]
    fn node_id_accepts_decimal() {
        assert_eq!(parse_node_id("3").unwrap(), 3);
        assert_eq!(parse_node_id("15").unwrap(), 0xF);
    }

    #[test]
    fn node_id_rejects_overflow() {
        assert!(parse_node_id("16").is_err());
        assert!(parse_node_id("0x10").is_err());
    }

    #[test]
    fn node_id_rejects_junk() {
        assert!(parse_node_id("").is_err());
        assert!(parse_node_id("0xZZ").is_err());
    }
}
