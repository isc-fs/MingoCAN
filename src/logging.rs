//! Tracing subscriber bootstrap. One entry point, called from `main`
//! before any subcommand runs.
//!
//! Behaviour:
//!
//! - `--verbose` sets the default filter to `trace` for our own crate
//!   and `debug` for everything else. That's enough to tell "noisy
//!   can-flasher" from "noisy third-party crate" without drowning the
//!   terminal.
//! - Without `--verbose` we default to `info`. `RUST_LOG` always wins
//!   — if the user set an explicit env filter, `--verbose` is ignored
//!   so CI runners can keep their override stable.
//! - Output is pretty (ANSI colours + aligned levels) when stderr is a
//!   terminal, compact otherwise.

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

pub fn init(verbose: bool) {
    let default_filter = if verbose {
        "can_flasher=trace,debug"
    } else {
        "info"
    };

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false))
        .init();
}
