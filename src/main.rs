//! can-flasher — host-side CLI for the isc-fs STM32 CAN bootloader.
//!
//! This file wires `clap`'s parsed arguments up to the per-subcommand
//! entry points in `cli::*`. It owns the tokio runtime, the tracing
//! subscriber, and the top-level error-to-exit-code mapping — nothing
//! else. Each subcommand is free to pull in whatever protocol /
//! transport machinery it needs.
//!
//! The skeleton landed in `feat/2-cargo-skeleton` deliberately stubs
//! every subcommand to a "not implemented" bail. Later feat branches
//! replace the stubs one at a time in the order defined by the
//! roadmap.

use std::process::ExitCode;

use clap::Parser;

mod cli;
mod logging;

use cli::{Cli, Command};

/// Exit codes. Mirrors the table in REQUIREMENTS.md — keep them in
/// sync.
///
/// All variants except `Ok` and `GenericError` are reserved for
/// structured-error downcasting that lands with the first real
/// subcommand implementation. `#[expect(dead_code)]` is the
/// forward-looking form: clippy will flag it as soon as a variant
/// starts being constructed, reminding us to drop the attribute.
#[repr(u8)]
#[derive(Clone, Copy, Debug)]
#[expect(
    dead_code,
    reason = "reserved exit-code variants, wired up per-subcommand from feat/3 onward"
)]
enum ExitCodeValue {
    Ok = 0,
    FlashError = 1,
    VerifyMismatch = 2,
    ProtectionViolation = 3,
    DeviceNotFound = 4,
    // 5 (signature failed) — reserved for v2 / Phase-5.
    // 6 (replay counter) — reserved for v2 / Phase-5.
    WrpNotApplied = 7,
    InputFileError = 8,
    AdapterMissing = 9,
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
            map_error_to_exit_code(&err).into()
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
        Command::Adapters => cli::adapters::run(&cli.global).await,
    }
}

/// Map an `anyhow::Error` to one of the exit codes from
/// REQUIREMENTS.md. Until the subcommands attach structured error
/// types (v2-ish), every error lands as `GenericError`. The table is
/// already in place so later branches can attach a downcastable
/// `FlasherError` and have the exit code line up automatically.
fn map_error_to_exit_code(_err: &anyhow::Error) -> ExitCodeValue {
    // TODO(feat/3+): downcast to typed error variants and return the
    // specific code. Keeping this as a single-arm today so the
    // behaviour is obvious in the skeleton.
    ExitCodeValue::GenericError
}
