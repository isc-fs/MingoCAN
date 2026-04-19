//! `verify` subcommand — compare installed image against a binary.
//!
//! Uses `CMD_FLASH_VERIFY` once protocol core lands; until then this
//! stub documents the target shape. Exits 0 on match, 2 on mismatch.

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Args;

use super::GlobalFlags;

#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// Path to .elf, .bin, or .hex firmware file
    #[arg(value_name = "FIRMWARE")]
    pub firmware: PathBuf,
}

pub async fn run(_args: VerifyArgs, _global: &GlobalFlags) -> Result<()> {
    bail!(
        "`verify` is not implemented yet — pending feat branches for \
         protocol core + transport backend. See REQUIREMENTS.md § \
         verify subcommand."
    )
}
