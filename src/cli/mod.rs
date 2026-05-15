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
pub mod send_raw;
#[cfg(feature = "swd")]
pub mod swd_flash;
pub mod verify;

// ---- Typed exit-code hints ----

/// Marker error type a subcommand attaches to its `anyhow::Error` so
/// `main` can pick a specific exit code from the REQUIREMENTS.md
/// table instead of falling back to 99. Wrap with
/// [`exit_err`] / [`bail_exit!`] at the subcommand layer; `main`'s
/// `map_error_to_exit_code` walks the error chain looking for this
/// type via `downcast_ref`.
///
/// Numeric values match `ExitCodeValue` in `main.rs`; keep the two
/// tables in sync when new codes land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ExitCodeHint {
    #[error("flash or write error")]
    FlashError,
    #[error("verification mismatch")]
    VerifyMismatch,
    #[error("protection violation (address in BL sector)")]
    ProtectionViolation,
    #[error("device not found / timeout")]
    DeviceNotFound,
    #[error("WRP not applied")]
    WrpNotApplied,
    #[error("input file error")]
    InputFileError,
    #[error("adapter not found or SDK missing")]
    AdapterMissing,
    /// User hit Ctrl-C. Conventional shell exit code 130 (128 + SIGINT).
    /// Subcommands that catch SIGINT should map to this; the cleanup
    /// path (best-effort CMD_DISCONNECT, close port) runs before we
    /// return so the device is left in a clean state for the next run.
    #[error("interrupted by user (SIGINT)")]
    Interrupted,
}

impl ExitCodeHint {
    /// Byte-valued exit code this hint maps to.
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::FlashError => 1,
            Self::VerifyMismatch => 2,
            Self::ProtectionViolation => 3,
            Self::DeviceNotFound => 4,
            Self::WrpNotApplied => 7,
            Self::InputFileError => 8,
            Self::AdapterMissing => 9,
            Self::Interrupted => 130,
        }
    }
}

/// Build an `anyhow::Error` that carries an [`ExitCodeHint`] plus a
/// human-readable message. The hint sits at the root of the chain;
/// the message wraps it via `context` so `Display` prints the
/// message first ("installed image differs…") and the hint second
/// ("verification mismatch"). `main::map_error_to_exit_code` walks
/// the chain via `downcast_ref::<ExitCodeHint>` and picks the code.
pub fn exit_err(hint: ExitCodeHint, message: impl std::fmt::Display) -> anyhow::Error {
    anyhow::Error::new(hint).context(message.to_string())
}

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

    /// Send one raw CAN frame — generic primitive for app-level
    /// commands (e.g. running-app reboot-to-BL, bench probes)
    SendRaw(send_raw::SendRawArgs),

    /// List detected CAN adapters on this machine
    Adapters,

    /// (`--features swd`) Flash the bootloader to a bare chip via
    /// SWD (ST-LINK). Solves the "first boot" problem for ECUs
    /// that haven't run the CAN bootloader yet.
    #[cfg(feature = "swd")]
    SwdFlash(swd_flash::SwdFlashArgs),
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
    /// Vector XL Driver Library — VN-series adapters (Windows; Linux planned)
    Vector,
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
    use super::{parse_node_id, ExitCodeHint};

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

    #[test]
    fn exit_codes_match_requirements_table() {
        // Pin the numeric values — these leak into shell scripts and
        // CI assertions, so accidental renumbering is a breaking
        // change. Keep in sync with REQUIREMENTS.md § Exit codes.
        assert_eq!(ExitCodeHint::FlashError.exit_code(), 1);
        assert_eq!(ExitCodeHint::VerifyMismatch.exit_code(), 2);
        assert_eq!(ExitCodeHint::ProtectionViolation.exit_code(), 3);
        assert_eq!(ExitCodeHint::DeviceNotFound.exit_code(), 4);
        assert_eq!(ExitCodeHint::WrpNotApplied.exit_code(), 7);
        assert_eq!(ExitCodeHint::InputFileError.exit_code(), 8);
        assert_eq!(ExitCodeHint::AdapterMissing.exit_code(), 9);
        // 130 = 128 + SIGINT (shell convention for Ctrl-C).
        assert_eq!(ExitCodeHint::Interrupted.exit_code(), 130);
    }
}
