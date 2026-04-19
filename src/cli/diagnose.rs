//! `diagnose` subcommand — DTC table, log ring, live-data snapshot,
//! health report, and remote reset.
//!
//! This is the runtime-inspection surface that replaces the `debug`
//! subcommand from the pre-v1.0.0 draft — the bootloader doesn't
//! expose `CMD_MEM_READ` / `CMD_MEM_WRITE`, but it does stream the
//! log ring and the live-data snapshot at host-configurable rates,
//! which covers the same observability need.

use anyhow::{bail, Result};
use clap::{Args, Subcommand, ValueEnum};

use super::GlobalFlags;

#[derive(Debug, Args)]
pub struct DiagnoseArgs {
    #[command(subcommand)]
    pub action: DiagnoseAction,
}

#[derive(Debug, Subcommand)]
pub enum DiagnoseAction {
    /// Read stored fault codes via CMD_DTC_READ
    ReadDtc,

    /// Clear stored fault codes via CMD_DTC_CLEAR (prompts unless --yes)
    ClearDtc {
        /// Skip the interactive confirmation prompt
        #[arg(long = "yes", default_value_t = false)]
        yes: bool,
    },

    /// Stream the bootloader log ring (CMD_LOG_STREAM_START + NOTIFY_LOG)
    Log {
        /// Minimum severity to emit (0=info, 1=warn, 2=error, 3=fatal)
        #[arg(long = "severity", default_value_t = 0)]
        severity: u8,
    },

    /// Stream the 32-byte live-data snapshot (CMD_LIVE_DATA_START + NOTIFY_LIVE_DATA)
    LiveData {
        /// Emission rate in Hz (1..=50)
        #[arg(long = "rate-hz", default_value_t = 10)]
        rate_hz: u8,
    },

    /// One-shot session health report (CMD_GET_HEALTH, 32-byte record)
    Health,

    /// Reset the device via CMD_RESET
    Reset {
        /// Reset mode
        #[arg(long = "mode", value_enum, default_value_t = ResetMode::Hard)]
        mode: ResetMode,
    },
}

/// Reset mode passed to `CMD_RESET`.
///
/// Numeric mapping in the on-wire payload is documented in
/// REQUIREMENTS.md § CAN protocol specification. The CLI uses the
/// named form so the user never types a raw mode byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ResetMode {
    /// NVIC_SystemReset (mode 0)
    Hard,
    /// Equivalent to hard on this family (mode 1, no distinction in HW)
    Soft,
    /// Reset into the bootloader's listen loop (mode 2, sets RTC BKP0R magic)
    Bootloader,
    /// Direct jump to the installed application (mode 3, no reset)
    App,
}

pub async fn run(_args: DiagnoseArgs, _global: &GlobalFlags) -> Result<()> {
    bail!(
        "`diagnose` is not implemented yet — pending feat branches for \
         protocol core + transport backend. See REQUIREMENTS.md § \
         diagnose subcommand."
    )
}
