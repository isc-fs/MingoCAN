//! `can-flasher swd-flash` — lay the bootloader (or any firmware)
//! onto a bare STM32 via SWD. Wraps [`crate::swd`] which itself
//! wraps probe-rs. Only compiled when the `swd` Cargo feature is
//! on; see Cargo.toml.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use clap::Args;

use crate::cli::{exit_err, ExitCodeHint, GlobalFlags};
use crate::swd::{self, SwdFlashRequest};

// Bad `--base` is a parse-time problem, not a flash-time one. We
// reuse `InputFileError` (exit 8) rather than minting a new
// `ArgValidation` hint for the spike — keeps the exit-code table
// in REQUIREMENTS.md stable until the v1 SWD work lands.

/// `swd-flash` subcommand arguments. Intentionally a tight set —
/// the v1 spike covers the team's actual use case (ST-LINK + H733)
/// without growing into a general "probe-rs-as-a-service" surface.
#[derive(Debug, Args)]
pub struct SwdFlashArgs {
    /// Path to the firmware to flash. `.elf`, `.hex`, or `.bin`.
    /// For `.bin` the load address is read from `--base`; for
    /// `.elf` and `.hex` the file's own addresses are used.
    pub artifact: PathBuf,

    /// probe-rs target identifier. Defaults to the team's ECU
    /// part number; override for other STM32 variants.
    /// Examples: `STM32H733ZGTx`, `STM32H7`, `STM32F4`, `STM32G431RBTx`.
    #[arg(long, default_value_t = swd::DEFAULT_CHIP.to_string())]
    pub chip: String,

    /// Serial number of the probe to use, if multiple ST-LINKs are
    /// attached. Run `can-flasher swd-probes` to list them. With
    /// a single probe attached this can be omitted.
    #[arg(long)]
    pub probe_serial: Option<String>,

    /// Flash base address for raw `.bin` inputs. Ignored for
    /// `.elf` / `.hex`. STM32H7 main flash starts at `0x08000000`,
    /// which is also where the CAN bootloader expects to live.
    #[arg(long, default_value_t = format!("0x{:08X}", swd::DEFAULT_BASE_ADDR))]
    pub base: String,

    /// Skip the post-write read-back-and-compare. Save ~1s on the
    /// happy path; default off because a silent flash failure is
    /// the worst kind.
    #[arg(long, default_value_t = false)]
    pub no_verify: bool,

    /// Skip the post-flash reset. Defaults to issuing a reset so
    /// the freshly-written bootloader actually starts. Disable if
    /// you want to inspect the chip with a debugger before
    /// letting it run.
    #[arg(long, default_value_t = false)]
    pub no_reset: bool,
}

pub async fn run(args: SwdFlashArgs, _global: &GlobalFlags) -> Result<()> {
    let base_addr = parse_hex_u64(&args.base).ok_or_else(|| {
        exit_err(
            ExitCodeHint::InputFileError,
            format!("--base must be a 0x-prefixed hex address; got {:?}", args.base),
        )
    })?;

    let mut request = SwdFlashRequest::new(args.artifact);
    request.chip = args.chip;
    request.probe_serial = args.probe_serial;
    request.base_addr = base_addr;
    request.verify = !args.no_verify;
    request.reset_after = !args.no_reset;

    // probe-rs is blocking; run on the blocking pool so the tokio
    // runtime stays responsive (matters for future Studio / VS
    // Code shell-outs, harmless for the standalone CLI).
    let request_for_msg = request.clone();
    tokio::task::spawn_blocking(move || swd::flash(&request))
        .await
        .map_err(|e| anyhow!("swd-flash task join: {e}"))?
        .map_err(|e| exit_err(ExitCodeHint::FlashError, anyhow!("{e}")))?;

    println!(
        "✓ flashed {} to chip {} via SWD",
        request_for_msg.artifact_path.display(),
        request_for_msg.chip
    );
    Ok(())
}

/// Parse `0x`-prefixed or plain-decimal address. Returns `None`
/// on bad input rather than `Result<_, anyhow>` so the caller can
/// attach the `ExitCodeHint::ArgValidation` they want.
fn parse_hex_u64(s: &str) -> Option<u64> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u64>().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_and_decimal_addresses() {
        assert_eq!(parse_hex_u64("0x08000000"), Some(0x0800_0000));
        assert_eq!(parse_hex_u64("0X08020000"), Some(0x0802_0000));
        assert_eq!(parse_hex_u64("134217728"), Some(0x0800_0000));
        assert_eq!(parse_hex_u64("not-hex"), None);
        assert_eq!(parse_hex_u64("0xzz"), None);
    }
}
