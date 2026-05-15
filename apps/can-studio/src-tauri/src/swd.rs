// Tauri commands for the SWD flash view. Thin wrappers around
// `can_flasher::swd` — the heavy lifting (probe-rs target attach,
// sector erase, write, verify, reset) lives in the library crate
// so the CLI binary and Studio share one implementation.
//
// Two surfaces:
//
//   - `swd_list_probes`        — enumerate attached debug probes
//   - `swd_flash`              — run one flash operation
//
// `swd_flash` is sync inside probe-rs, so we wrap it in
// `tokio::task::spawn_blocking` to keep the Tauri runtime
// responsive (same pattern as the CLI subcommand).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use can_flasher::swd::{self, SwdFlashRequest};

// ---- DTOs ----

/// Frontend-facing probe descriptor. Mirrors the subset of
/// `probe_rs::probe::DebugProbeInfo` that's useful in the UI;
/// keeps the IPC payload narrow + serde-friendly.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeInfo {
    /// Human-readable probe name (e.g. "STLink V3").
    pub identifier: String,
    /// USB serial number, when reported by the probe. `None` for
    /// probes that don't expose one — typically very old ST-LINKs.
    pub serial_number: Option<String>,
    pub vendor_id: u16,
    pub product_id: u16,
}

/// Inputs to `swd_flash`. `camelCase` so the frontend can pass
/// JS-style identifiers; serde renames on the way in.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwdFlashArgs {
    pub artifact_path: String,
    pub chip: Option<String>,
    pub probe_serial: Option<String>,
    /// Hex string `"0x08000000"` or plain decimal. `None` means
    /// "use the library default" (0x08000000 — start of STM32H7
    /// main flash, where the bootloader lives by convention).
    pub base: Option<String>,
    pub verify: bool,
    pub reset_after: bool,
}

// ---- Tauri commands ----

#[tauri::command]
pub fn swd_list_probes() -> Vec<ProbeInfo> {
    swd::list_probes()
        .into_iter()
        .map(|p| ProbeInfo {
            identifier: p.identifier,
            serial_number: p.serial_number,
            vendor_id: p.vendor_id,
            product_id: p.product_id,
        })
        .collect()
}

#[tauri::command]
pub async fn swd_flash(args: SwdFlashArgs) -> Result<(), String> {
    let base_addr = match args.base.as_deref().map(parse_hex_u64) {
        Some(Some(v)) => v,
        Some(None) => {
            return Err(format!(
                "--base must be 0x-prefixed hex or decimal; got {:?}",
                args.base
            ))
        }
        None => swd::DEFAULT_BASE_ADDR,
    };

    let mut request = SwdFlashRequest::new(PathBuf::from(args.artifact_path));
    if let Some(chip) = args.chip {
        request.chip = chip;
    }
    request.probe_serial = args.probe_serial;
    request.base_addr = base_addr;
    request.verify = args.verify;
    request.reset_after = args.reset_after;

    // probe-rs is blocking — keep the Tauri async runtime free.
    tokio::task::spawn_blocking(move || swd::flash(&request))
        .await
        .map_err(|e| format!("swd-flash task join: {e}"))?
        .map_err(|e| e.to_string())
}

// ---- Helpers ----

fn parse_hex_u64(s: &str) -> Option<u64> {
    let s = s.trim();
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
    fn parses_hex_and_decimal() {
        assert_eq!(parse_hex_u64("0x08000000"), Some(0x0800_0000));
        assert_eq!(parse_hex_u64("0X08020000"), Some(0x0802_0000));
        assert_eq!(parse_hex_u64("134217728"), Some(0x0800_0000));
        assert_eq!(parse_hex_u64("not-hex"), None);
    }
}
