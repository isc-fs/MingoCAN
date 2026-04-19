//! `discover` subcommand — broadcast `CMD_DISCOVER` and report every
//! bootloader on the bus.
//!
//! The real implementation issues a broadcast frame (dst=0xF), listens
//! for `TYPE=DISCOVER` replies for `--timeout-ms`, then optionally
//! follows up with `CMD_GET_FW_INFO` + `CMD_GET_HEALTH` per responder
//! to populate the identity / WRP / reset-cause columns.

use anyhow::{bail, Result};
use clap::Args;

use super::GlobalFlags;

#[derive(Debug, Args)]
pub struct DiscoverArgs {
    /// How long to wait for replies after the broadcast, in milliseconds
    #[arg(long = "timeout-ms", default_value_t = 500)]
    pub timeout_ms: u32,
}

pub async fn run(_args: DiscoverArgs, _global: &GlobalFlags) -> Result<()> {
    bail!(
        "`discover` is not implemented yet — pending feat branches for \
         protocol core + transport backend. See REQUIREMENTS.md § \
         discover subcommand."
    )
}
