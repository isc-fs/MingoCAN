//! `can-flasher dfu-flash` / `dfu-list` — burn the bootloader over the
//! STM32 ROM's USB DFU, no ST-LINK (#645). Wraps [`crate::dfu`]; shares
//! `--provision`, `--release` and the progress bar with `swd-flash`.

use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::{anyhow, Result};
use clap::Args;

use super::swd_flash::{
    drive_progress_bar, format_size, parse_format, resolve_artifact, resolve_provision_node_id,
};
use crate::bootloader_fetch::BootloaderFormat;
use crate::cli::{exit_err, ExitCodeHint, GlobalFlags};
use crate::dfu::{self, DfuFlashRequest};
use crate::swd::SwdProgress;

#[derive(Debug, Args)]
pub struct DfuFlashArgs {
    /// Bootloader to burn: `.elf`, `.hex`, or `.bin` (a `.bin` goes to
    /// 0x08000000). Mutually exclusive with `--release`.
    pub artifact: Option<PathBuf>,

    /// Fetch the bootloader from its release page instead (latest, or a
    /// tag like `v1.7.0`). Cached, like `swd-flash --release`.
    #[arg(
        long,
        value_name = "TAG",
        num_args = 0..=1,
        default_missing_value = "latest",
        conflicts_with = "artifact"
    )]
    pub release: Option<String>,

    /// Format to fetch with `--release` (default `elf`).
    #[arg(long, default_value = "elf", value_parser = parse_format)]
    pub release_format: BootloaderFormat,

    /// USB serial of the board to use when several are in DFU mode
    /// (`can-flasher dfu-list` shows them).
    #[arg(long)]
    pub serial: Option<String>,

    /// Skip reading the image back and CRC-comparing it (not recommended).
    #[arg(long, default_value_t = false)]
    pub no_verify: bool,

    /// Stay in DFU after the burn instead of starting the new bootloader.
    #[arg(long, default_value_t = false)]
    pub no_leave: bool,

    /// Provision the board's CAN node-id in the same run — a role (`ecu`,
    /// `ams`, `udv`) or `0x1`..`0xE`. Same rules as `swd-flash
    /// --provision`: the bootloader `.elf` must have seed support (v1.7.0+)
    /// and the chip must be a 1 MB STM32H72x/73x; both are checked before
    /// anything is erased.
    #[arg(long, value_name = "ROLE|0xN")]
    pub provision: Option<String>,
}

pub async fn run(args: DfuFlashArgs, _global: &GlobalFlags) -> Result<()> {
    let provision_id = args
        .provision
        .as_deref()
        .map(resolve_provision_node_id)
        .transpose()?;
    let artifact_path =
        resolve_artifact(args.artifact, args.release.as_deref(), args.release_format).await?;

    let mut request = DfuFlashRequest::new(artifact_path);
    request.serial = args.serial;
    request.verify = !args.no_verify;
    request.leave = !args.no_leave;
    request.seed_node_id = provision_id;
    if let Some(id) = provision_id {
        println!("Will provision node-id 0x{id:X} in the same run.");
    }

    let shown = request.clone();
    let (tx, rx) = mpsc::channel::<SwdProgress>();
    let handle = tokio::task::spawn_blocking(move || {
        dfu::flash_with_progress(&request, |p| {
            let _ = tx.send(p);
        })
    });
    drive_progress_bar(rx);
    let report = handle
        .await
        .map_err(|e| anyhow!("dfu-flash task join: {e}"))?
        .map_err(|e| {
            let hint = match e {
                dfu::DfuError::NoDevice
                | dfu::DfuError::AmbiguousDevice(_)
                | dfu::DfuError::SerialNotFound { .. } => ExitCodeHint::DeviceNotFound,
                dfu::DfuError::Artifact(_)
                | dfu::DfuError::SeedUnsupported { .. }
                | dfu::DfuError::SeedChipUnsupported(_) => ExitCodeHint::InputFileError,
                _ => ExitCodeHint::FlashError,
            };
            exit_err(hint, anyhow!("{e}"))
        })?;

    println!(
        "✓ burned {} over USB DFU{}",
        shown.artifact_path.display(),
        report
            .serial
            .as_deref()
            .map(|s| format!(" (board {s})"))
            .unwrap_or_default()
    );
    println!(
        "    ↳ {} · CRC32=0x{:08X} · {} · ROM bootloader v{:X}.{:X}",
        format_size(report.size_bytes),
        report.crc32,
        if report.verified {
            "verified ✓"
        } else {
            "verified ✗ (--no-verify)"
        },
        report.rom_version >> 8,
        report.rom_version & 0xFF
    );
    if let Some(id) = report.seeded_node_id {
        println!("    ↳ provisioned as node-id 0x{id:X} (seed written + verified)");
    }
    if shown.leave {
        println!("    ↳ started the bootloader. Set BOOT0 back low so the next reset doesn't re-enter DFU.");
    } else {
        println!("    ↳ --no-leave: the board is still in DFU; reset it with BOOT0 low to start the bootloader.");
    }
    Ok(())
}

pub fn run_list(global: &GlobalFlags) -> Result<()> {
    let devices = dfu::list_devices().map_err(|e| anyhow!("{e}"))?;
    if global.json {
        let rows: Vec<_> = devices
            .iter()
            .map(|d| {
                serde_json::json!({
                    "serial": d.serial,
                    "product": d.product,
                    "romVersion": format!("0x{:04X}", d.rom_version),
                    "flashKib": d.flash.as_ref().map(|l| l.size() / 1024),
                    "flashStart": d.flash.as_ref().map(|l| format!("0x{:08X}", l.start)),
                    "provisionable": d.flash.as_ref().map(|l| l.is_h72x_73x_1mb()),
                    "error": d.open_error,
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(rows));
        return Ok(());
    }
    if devices.is_empty() {
        println!("No STM32 in DFU mode. Hold BOOT0 high, then reset or power the board with USB connected.");
        return Ok(());
    }
    for d in &devices {
        let flash = match (&d.flash, &d.open_error) {
            (Some(l), _) => format!(
                "{} KiB flash at 0x{:08X}{}",
                l.size() / 1024,
                l.start,
                if l.is_h72x_73x_1mb() {
                    " (H72x/73x — provisionable)"
                } else {
                    ""
                }
            ),
            (None, Some(e)) => format!("couldn't read its layout: {e}"),
            (None, None) => "no Internal Flash region".into(),
        };
        println!(
            "{}  ROM v{:X}.{:X}  {}",
            d.serial.as_deref().unwrap_or("(no serial)"),
            d.rom_version >> 8,
            d.rom_version & 0xFF,
            flash
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: DfuFlashArgs,
    }

    #[test]
    fn parses_the_documented_invocations() {
        assert!(Cli::try_parse_from(["t", "bl.elf"]).is_ok());
        assert!(Cli::try_parse_from(["t", "--release", "--provision", "ams"]).is_ok());
        assert!(Cli::try_parse_from(["t", "--release", "v1.7.0", "--serial", "2056"]).is_ok());
        assert!(Cli::try_parse_from(["t", "bl.elf", "--no-verify", "--no-leave"]).is_ok());
        assert!(
            Cli::try_parse_from(["t", "bl.elf", "--release"]).is_err(),
            "a file and --release are mutually exclusive"
        );
    }
}
