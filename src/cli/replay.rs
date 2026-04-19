//! `replay` subcommand — record live sessions and replay them against
//! the virtual backend for regression testing.

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Args, Subcommand};

use super::GlobalFlags;

#[derive(Debug, Args)]
pub struct ReplayArgs {
    #[command(subcommand)]
    pub action: ReplayAction,
}

#[derive(Debug, Subcommand)]
pub enum ReplayAction {
    /// Record a live session to file (candump format)
    Record {
        /// Output file
        #[arg(long = "out", value_name = "FILE")]
        out: PathBuf,
    },

    /// Replay a recorded session against the virtual backend
    Run {
        /// Recorded session file
        #[arg(value_name = "FILE")]
        file: PathBuf,
    },
}

pub async fn run(_args: ReplayArgs, _global: &GlobalFlags) -> Result<()> {
    bail!(
        "`replay` is not implemented yet — pending feat branches for \
         protocol core + virtual backend. See REQUIREMENTS.md § replay \
         subcommand."
    )
}
