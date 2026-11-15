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
#[cfg(feature = "swd")]
pub mod dfu_flash;
pub mod diagnose;
pub mod discover;
pub mod flash;
pub mod logs;
pub mod pit_diag;
pub mod provision;
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
/// `exit_code_of` walks the error chain looking for this
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
/// ("verification mismatch"). `main::exit_code_of` walks
/// the chain via `downcast_ref::<ExitCodeHint>` and picks the code.
pub fn exit_err(hint: ExitCodeHint, message: impl std::fmt::Display) -> anyhow::Error {
    anyhow::Error::new(hint).context(message.to_string())
}

/// Interactive y/N confirmation on stderr. Returns `true` only on an
/// explicit "y"/"yes". A closed/non-TTY stdin (EOF) returns `false` —
/// i.e. destructive ops fail closed under piping / CI unless the
/// caller passed an explicit `--yes`. Shared by `config` (OB writes)
/// and `flash` (pre-flight gate, FMEA #271 G3).
pub(crate) fn confirm_prompt(question: &str) -> bool {
    use std::io::Write;
    eprint!("{question} [y/N]: ");
    std::io::stderr().flush().ok();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
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

    /// Assign a board's node-id by role name (ECU=1, AMS=2, uDV=3).
    /// Sugar over `config nvm write node-id` + reset — operator
    /// types the role, the host fills in the number.
    Provision(provision::ProvisionArgs),

    /// List / pull the microSD data logs off a node over CAN, and decode .BIN logs to CSV
    Logs(logs::LogsArgs),

    /// Telemetry observer (AMS / ECU / uDV) — `listen` passively, or
    /// arm / disarm / stream the diagnostic stream. Only `listen` is
    /// send-silent and therefore safe on a live car.
    PitDiag(pit_diag::PitDiagArgs),

    /// List detected CAN adapters on this machine
    Adapters,

    /// (`--features swd`) Flash the bootloader to a bare chip via
    /// SWD (ST-LINK). Solves the "first boot" problem for ECUs
    /// that haven't run the CAN bootloader yet.
    #[cfg(feature = "swd")]
    SwdFlash(swd_flash::SwdFlashArgs),

    /// (`--features swd`) Burn the bootloader over USB DFU — no
    /// ST-LINK. The board must be in its ROM bootloader: hold BOOT0 high
    /// and reset / power it with USB connected.
    #[cfg(feature = "swd")]
    DfuFlash(dfu_flash::DfuFlashArgs),

    /// (`--features swd`) List STM32 boards waiting in USB DFU mode
    #[cfg(feature = "swd")]
    DfuList,
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

    /// Target node ID (hex `0x0A` or decimal `10`). Required by `flash`
    /// (no default — it won't guess which board to overwrite); other
    /// commands fall back to their own per-subcommand default.
    #[arg(long = "node-id", global = true, value_parser = parse_node_id)]
    pub node_id: Option<u8>,

    /// Reply timeout in milliseconds — applies per command, covering the
    /// whole reassembled ISO-TP message (not per CAN frame)
    #[arg(long = "timeout", default_value_t = 500, global = true)]
    pub timeout_ms: u32,

    /// Machine-readable JSON output on stdout
    #[arg(long = "json", default_value_t = false, global = true)]
    pub json: bool,

    /// Flash-log file (JSONL). Every command that writes to a board
    /// appends one line. Default: the per-user MingoCAN data dir
    /// (`mingocan/flash-log.jsonl`); not written for `--interface virtual`
    /// unless this flag is given
    #[arg(long = "log", global = true)]
    pub log_path: Option<PathBuf>,

    /// Trace-level logging
    #[arg(long = "verbose", default_value_t = false, global = true)]
    pub verbose: bool,

    /// Operator name recorded in the flash log (default: the OS user)
    #[arg(long = "operator", global = true)]
    pub operator: Option<String>,

    /// The flash-log entry being filled in while a write command runs.
    /// Not a flag: `main` opens it for write commands (see
    /// [`audit_start`]), subcommands add what they learn.
    #[arg(skip)]
    pub audit: AuditSlot,
}

// ---- Flash log (#651) ----

/// Holds the [`audit::Entry`](crate::audit::Entry) for the running
/// command. Empty (every call a no-op) unless `main` opened it, so
/// subcommands can record unconditionally.
#[derive(Debug, Default, Clone)]
pub struct AuditSlot(std::sync::Arc<std::sync::Mutex<Option<crate::audit::Entry>>>);

impl AuditSlot {
    /// Start recording `entry`.
    pub fn begin(&self, entry: crate::audit::Entry) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(entry);
        }
    }

    /// Add to the open entry, if any.
    pub fn update(&self, f: impl FnOnce(&mut crate::audit::Entry)) {
        if let Ok(mut slot) = self.0.lock() {
            if let Some(entry) = slot.as_mut() {
                f(entry);
            }
        }
    }

    /// Drop the entry: the operator declined a confirmation, so nothing
    /// was written and there's nothing to log.
    pub fn cancel(&self) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = None;
        }
    }

    /// Take the entry to write it out.
    pub fn take(&self) -> Option<crate::audit::Entry> {
        self.0.lock().ok().and_then(|mut slot| slot.take())
    }
}

/// If `command` writes to a board, the flash-log entry to start and the
/// file it goes to. `None` for read-only commands, and for the virtual
/// bus unless `--log` names a file (tests and demos stay out of the
/// operator's history).
pub fn audit_start(
    command: &Command,
    global: &GlobalFlags,
) -> Option<(crate::audit::Entry, PathBuf)> {
    use crate::audit::Entry;

    // (action, is it a CAN action)
    let (action, over_can) = match command {
        Command::Flash(_) => ("flash", true),
        Command::Provision(_) => ("provision", true),
        Command::Diagnose(d) => match d.action {
            diagnose::DiagnoseAction::ClearDtc { .. } => ("clear-dtc", true),
            _ => return None,
        },
        Command::Config(c) => match &c.action {
            config::ConfigAction::Ob {
                action: config::ObAction::ApplyWrp { .. },
            } => ("ob-apply-wrp", true),
            config::ConfigAction::Nvm { action } => match action {
                config::NvmAction::Write { .. } => ("nvm-write", true),
                config::NvmAction::Erase { .. } => ("nvm-erase", true),
                config::NvmAction::Format { .. } => ("nvm-format", true),
                config::NvmAction::Read { .. } => return None,
            },
            config::ConfigAction::Ob { .. } => return None,
        },
        #[cfg(feature = "swd")]
        Command::SwdFlash(_) => ("swd-flash", false),
        #[cfg(feature = "swd")]
        Command::DfuFlash(_) => ("dfu-flash", false),
        _ => return None,
    };

    let path = match &global.log_path {
        Some(p) => p.clone(),
        None if over_can && global.interface == InterfaceType::Virtual => return None,
        None => crate::audit::default_path()?,
    };

    let mut entry = Entry::new("cli", action);
    if let Some(op) = global.operator.as_deref().map(str::trim) {
        if !op.is_empty() {
            entry.operator = Some(op.to_string());
        }
    }
    if over_can {
        entry.interface = global
            .interface
            .to_possible_value()
            .map(|v| v.get_name().to_string());
        entry.channel = global.channel.clone();
        // `flash` refuses to run without --node-id; the others fall back
        // to 0x3 like their subcommands do.
        match (command, global.node_id) {
            (_, Some(id)) => entry.set_node(id),
            (Command::Flash(_), None) => {}
            (_, None) => entry.set_node(0x3),
        }
    }
    match command {
        Command::Flash(args) => {
            entry.set_artifact(&args.firmware);
            entry.dry_run = args.dry_run;
        }
        Command::Provision(args) => {
            // The board is reached at --node-id (default 0x3) and leaves
            // as the role's id: log the new id, keep the old as a detail.
            let reached = global.node_id.unwrap_or(0x3);
            entry.detail("previousNodeId", format!("0x{reached:X}"));
            if let Some((id, _)) = provision::resolve_role_or_path(&args.role) {
                entry.set_node(id);
            }
        }
        _ => {}
    }
    if let Command::Config(c) = command {
        if let config::ConfigAction::Nvm { action } = &c.action {
            match action {
                config::NvmAction::Write { key, value, .. } => {
                    entry.detail("key", format!("0x{key:04X}"));
                    entry.detail("value", value.clone());
                }
                config::NvmAction::Erase { key } => {
                    entry.detail("key", format!("0x{key:04X}"));
                }
                _ => {}
            }
        }
    }
    #[cfg(feature = "swd")]
    if let Command::SwdFlash(args) = command {
        if let Some(a) = &args.artifact {
            entry.set_artifact(a);
        }
        entry.detail("chip", args.chip.clone());
        entry.device_serial = args.probe_serial.clone();
    }
    #[cfg(feature = "swd")]
    if let Command::DfuFlash(args) = command {
        if let Some(a) = &args.artifact {
            entry.set_artifact(a);
        }
        entry.device_serial = args.serial.clone();
    }
    Some((entry, path))
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
