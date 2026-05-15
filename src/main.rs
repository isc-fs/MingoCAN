//! can-flasher — host-side CLI entry point.
//!
//! This file wires `clap`'s parsed arguments up to the per-subcommand
//! entry points in [`can_flasher::cli`]. It owns the tokio runtime,
//! the tracing subscriber, and the top-level error-to-exit-code
//! mapping — nothing else.
//!
//! The module tree (`protocol`, `transport`, `cli`, `logging`) lives
//! in `src/lib.rs` so both this binary and the integration tests
//! under `tests/` reach it through the same `can_flasher::…` paths.

use std::process::ExitCode;

use clap::Parser;

use can_flasher::cli::{self, Cli, Command, ExitCodeHint};
use can_flasher::logging;

/// Top-level exit codes. Mirrors the table in REQUIREMENTS.md —
/// keep in sync with [`ExitCodeHint`] in `cli/mod.rs`.
///
/// Subcommands that want a specific exit code attach a matching
/// `ExitCodeHint` to their `anyhow::Error` via `cli::exit_err`;
/// everything else falls back to `GenericError` (99). Codes `5` and
/// `6` are reserved for Phase-5 security work (signature fail,
/// replay counter reject).
#[repr(u8)]
#[derive(Clone, Copy, Debug)]
enum ExitCodeValue {
    Ok = 0,
    GenericError = 99,
}

impl From<ExitCodeValue> for ExitCode {
    fn from(value: ExitCodeValue) -> Self {
        ExitCode::from(value as u8)
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    logging::init(cli.global.verbose);

    // Build the tokio runtime by hand rather than using `#[tokio::main]`
    // so the exit-code mapping in `run` can return `ExitCode` without
    // wrapping the whole program body in one async block.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("failed to build tokio runtime: {err}");
            return ExitCodeValue::GenericError.into();
        }
    };

    let result = runtime.block_on(run(cli));

    match result {
        Ok(()) => ExitCodeValue::Ok.into(),
        Err(err) => {
            // Pretty-print the chain. Subcommands use `anyhow::Context`
            // to attach progressively richer explanations as the call
            // stack deepens; printing the whole chain preserves that.
            eprintln!("error: {err:#}");
            map_error_to_exit_code(&err)
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Flash(args) => cli::flash::run(args, &cli.global).await,
        Command::Verify(args) => cli::verify::run(args, &cli.global).await,
        Command::Discover(args) => cli::discover::run(args, &cli.global).await,
        Command::Diagnose(args) => cli::diagnose::run(args, &cli.global).await,
        Command::Config(args) => cli::config::run(args, &cli.global).await,
        Command::Replay(args) => cli::replay::run(args, &cli.global).await,
        Command::SendRaw(args) => cli::send_raw::run(args, &cli.global).await,
        Command::Adapters => cli::adapters::run(&cli.global).await,
        #[cfg(feature = "swd")]
        Command::SwdFlash(args) => cli::swd_flash::run(args, &cli.global).await,
    }
}

/// Walk the error chain looking for an [`ExitCodeHint`] marker. If
/// found, return its mapped exit code; otherwise fall back to
/// `GenericError` (99). This is how subcommands request a specific
/// exit code without the CLI layer owning a per-subcommand error
/// taxonomy.
fn map_error_to_exit_code(err: &anyhow::Error) -> ExitCode {
    for cause in err.chain() {
        if let Some(hint) = cause.downcast_ref::<ExitCodeHint>() {
            return ExitCode::from(hint.exit_code());
        }
    }
    ExitCodeValue::GenericError.into()
}
