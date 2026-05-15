// Tauri commands for the SWD flash view. Thin wrappers around
// `can_flasher::swd` — the heavy lifting (probe-rs target attach,
// sector erase, write, verify, reset) lives in the library crate
// so the CLI binary and Studio share one implementation.
//
// Three surfaces:
//
//   - `swd_list_probes`        — enumerate attached debug probes
//   - `swd_flash`              — run one flash operation (streams
//                                 progress on the `swd-flash:event`
//                                 Tauri channel)
//   - `swd_erase`              — wipe the whole chip clean
//
// `swd_flash` is sync inside probe-rs, so we wrap it in
// `tokio::task::spawn_blocking` to keep the Tauri runtime
// responsive (same pattern as the CLI subcommand).

use std::path::PathBuf;
use std::sync::mpsc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use can_flasher::bootloader_fetch::{self, BootloaderFormat};
use can_flasher::swd::{self, SwdEraseRequest, SwdFlashRequest, SwdOperation, SwdProgress};

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwdEraseArgs {
    pub chip: Option<String>,
    pub probe_serial: Option<String>,
}

/// One streamed progress event, emitted as the `swd-flash:event`
/// Tauri channel payload. Mirrors `can_flasher::swd::SwdProgress`
/// but flattened into a discriminated union the frontend can
/// pattern-match without unwrapping nested enums.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SwdStreamEvent {
    Started {
        op: &'static str,
        total: Option<u64>,
    },
    Progress {
        op: &'static str,
        delta: u64,
    },
    Finished {
        op: &'static str,
    },
    Failed {
        op: &'static str,
    },
}

fn op_name(op: SwdOperation) -> &'static str {
    match op {
        SwdOperation::Erase => "erase",
        SwdOperation::Program => "program",
        SwdOperation::Verify => "verify",
        SwdOperation::Fill => "fill",
    }
}

impl From<SwdProgress> for SwdStreamEvent {
    fn from(p: SwdProgress) -> Self {
        match p {
            SwdProgress::Started { op, total } => Self::Started {
                op: op_name(op),
                total,
            },
            SwdProgress::Progress { op, delta } => Self::Progress {
                op: op_name(op),
                delta,
            },
            SwdProgress::Finished { op } => Self::Finished { op: op_name(op) },
            SwdProgress::Failed { op } => Self::Failed { op: op_name(op) },
        }
    }
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

/// Result of a `swd_fetch_bootloader` call. The path is the
/// already-on-disk file the frontend should hand back to
/// `swd_flash`; `downloaded` lets the UI distinguish "fresh
/// pull" from "served cached" in the status text.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchedBootloader {
    pub tag: String,
    pub path: String,
    pub downloaded: bool,
}

#[tauri::command]
pub async fn swd_fetch_bootloader(tag: Option<String>) -> Result<FetchedBootloader, String> {
    // Treat `None` and `Some("")`/`Some("latest")` as "give me the
    // latest release" — keeps the JS side from having to special-case.
    let tag_for_api = match tag.as_deref() {
        None | Some("") | Some("latest") => None,
        Some(t) => Some(t.to_string()),
    };
    let cached = tokio::task::spawn_blocking(move || {
        bootloader_fetch::fetch(tag_for_api.as_deref(), BootloaderFormat::Elf)
    })
    .await
    .map_err(|e| format!("fetch task join: {e}"))?
    .map_err(|e| e.to_string())?;

    Ok(FetchedBootloader {
        tag: cached.tag,
        path: cached.path.to_string_lossy().into_owned(),
        downloaded: cached.downloaded,
    })
}

#[tauri::command]
pub async fn swd_flash(app: AppHandle, args: SwdFlashArgs) -> Result<(), String> {
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

    // probe-rs's progress closure runs on the blocking thread we're
    // about to spawn. Send the events back to the async runtime via
    // an mpsc channel so we can `app.emit` from a tokio context.
    // A bounded(64) is plenty — the flash phase generates a few
    // hundred events total, and the channel auto-drops if the UI
    // can't keep up (UI doesn't need every tick, just enough to
    // animate).
    let (tx, rx) = mpsc::channel::<SwdStreamEvent>();
    let flash_handle = tokio::task::spawn_blocking(move || {
        swd::flash_with_progress(&request, |progress| {
            // Best-effort send. If the receiver was dropped (window
            // closed mid-flash), keep flashing — discarding the
            // event is fine.
            let _ = tx.send(progress.into());
        })
    });

    // Drain progress events while the flash is in-flight. We do
    // this on the tokio runtime so the UI thread sees updates
    // immediately rather than batched at the end.
    let app_for_drain = app.clone();
    let drain_handle = tokio::task::spawn_blocking(move || {
        while let Ok(event) = rx.recv() {
            let _ = app_for_drain.emit("swd-flash:event", &event);
        }
    });

    let flash_result = flash_handle
        .await
        .map_err(|e| format!("swd-flash task join: {e}"))?;
    // Wait for the drainer to finish flushing any tail events.
    let _ = drain_handle.await;

    flash_result.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn swd_erase(args: SwdEraseArgs) -> Result<(), String> {
    let mut request = SwdEraseRequest::new();
    if let Some(chip) = args.chip {
        request.chip = chip;
    }
    request.probe_serial = args.probe_serial;

    tokio::task::spawn_blocking(move || swd::erase_chip(&request))
        .await
        .map_err(|e| format!("swd-erase task join: {e}"))?
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

    #[test]
    fn op_names_are_stable() {
        // Pinned strings; frontend matches on these.
        assert_eq!(op_name(SwdOperation::Erase), "erase");
        assert_eq!(op_name(SwdOperation::Program), "program");
        assert_eq!(op_name(SwdOperation::Verify), "verify");
        assert_eq!(op_name(SwdOperation::Fill), "fill");
    }
}
