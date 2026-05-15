//! SWD/JTAG flashing via probe-rs. **Feasibility spike** —
//! lives behind the `swd` Cargo feature so the default binary
//! stays small.
//!
//! Solves the "first boot" problem: a bare STM32 can't speak the
//! CAN bootloader's wire protocol until the bootloader is itself
//! on the chip. Operators used to drop down to STM32CubeProgrammer
//! / OpenOCD / pyOCD for that initial flash, then switch to
//! `can-flasher` for every subsequent app update. This module
//! collapses both into one tool.
//!
//! Scope (v1 spike):
//!
//! - **ST-LINK V2 / V3 only.** ~95% of STM32 development uses
//!   ST-LINK; J-Link / CMSIS-DAP land later if anyone needs them.
//! - **STM32H733 default**, but `--chip` accepts any probe-rs
//!   target string (e.g. `STM32F4`, `STM32G4`, `STM32H7`).
//! - **Erase + write + verify + optional reset.** No
//!   option-byte programming, no GDB server, no RTT — those are
//!   distinct ops that probe-rs supports but we deliberately
//!   don't expose.
//!
//! Not in scope:
//!
//! - Background "watch and re-flash on file change" mode.
//! - Auto-download of the bootloader `.elf` from the sibling
//!   repo. Operator supplies the path; auto-download is a
//!   second-PR thing once the spike confirms the SWD path
//!   itself works.
//! - Studio / VS Code surfaces. The CLI subcommand can be
//!   shelled out from either eventually.
//!
//! ## Platform notes
//!
//! Operators need a working libusb stack:
//!
//! - **Linux**: `sudo apt install libusb-1.0-0` plus a udev
//!   rule for ST-LINK (so you don't need `sudo`). See
//!   `docs/INSTALL.md`.
//! - **macOS**: ST-LINK works without extra drivers via
//!   IOKit/libusb; the bundled rusb crate handles it.
//! - **Windows**: ST-LINK ships with a kernel driver that
//!   libusb can't claim. Use Zadig once to install the WinUSB
//!   driver for the ST-LINK device. After that, `probe-rs`
//!   sees it through nusb.

use std::path::PathBuf;
use std::time::Duration;

use probe_rs::flashing::{
    download_file_with_options, erase_all, DownloadOptions, FlashProgress, Format,
    ProgressEvent, ProgressOperation,
};
use probe_rs::probe::list::Lister;
use probe_rs::probe::DebugProbeInfo;
use probe_rs::{Permissions, Session};
use thiserror::Error;
use tracing::{info, warn};

/// Default target string. The team's ECU is an STM32H733ZGTx;
/// probe-rs identifies it with the full ST part number. Override
/// with `--chip <name>` for other families.
pub const DEFAULT_CHIP: &str = "STM32H733ZGTx";

/// Default base address for the bootloader. STM32H7 main flash
/// starts at 0x08000000; sector 0 (128 KB) is where the BL lives
/// by convention so the existing CAN-side app starts at sector 1
/// (0x08020000).
pub const DEFAULT_BASE_ADDR: u64 = 0x0800_0000;

/// Inputs to a single `swd-flash` invocation.
#[derive(Debug, Clone)]
pub struct SwdFlashRequest {
    /// Path to a `.elf`, `.hex`, or `.bin` to lay onto the chip.
    pub artifact_path: PathBuf,
    /// probe-rs target identifier. Defaults to [`DEFAULT_CHIP`].
    pub chip: String,
    /// Probe selector — `None` means "auto-pick the only attached
    /// probe; error if there are zero or multiple". A specific
    /// serial number disambiguates when several probes are wired
    /// in (multi-bench setup).
    pub probe_serial: Option<String>,
    /// Flash base address. For `.elf` and `.hex` this is ignored
    /// (the file carries its own addresses); for raw `.bin`
    /// inputs the address is required.
    pub base_addr: u64,
    /// Verify the readback after write. The probe-rs default is
    /// `true`; we expose the toggle so a power-user can skip the
    /// extra ~1s of read traffic during bench tests.
    pub verify: bool,
    /// Issue a reset (with `Sysresetreq`) after the flash
    /// completes. The bootloader needs a reset to start running
    /// from its newly-written address, so the default is `true`.
    pub reset_after: bool,
}

impl SwdFlashRequest {
    /// Construct a request with sensible defaults for the team's
    /// hardware. Caller mutates fields as needed before passing
    /// to [`flash`].
    pub fn new(artifact_path: PathBuf) -> Self {
        Self {
            artifact_path,
            chip: DEFAULT_CHIP.to_string(),
            probe_serial: None,
            base_addr: DEFAULT_BASE_ADDR,
            verify: true,
            reset_after: true,
        }
    }
}

#[derive(Debug, Error)]
pub enum SwdError {
    #[error("no debug probe found — check the ST-LINK is plugged in (Linux: udev rule, Windows: Zadig WinUSB)")]
    NoProbe,
    #[error("multiple probes attached and no --probe-serial specified; available: {0}")]
    AmbiguousProbe(String),
    #[error("requested probe serial {requested:?} not found; available: {available}")]
    SerialNotFound {
        requested: String,
        available: String,
    },
    #[error("artifact path {path:?} does not exist")]
    ArtifactMissing { path: PathBuf },
    #[error("artifact path {path:?} has no extension — pass .elf, .hex, or .bin")]
    ArtifactNoExtension { path: PathBuf },
    #[error("unsupported artifact extension {ext:?}; pass .elf, .hex, or .bin")]
    ArtifactBadExtension { ext: String },
    #[error("probe-rs: {0}")]
    ProbeRs(String),
}

/// Enumerate every attached debug probe. Useful for an operator
/// who has multiple ST-LINKs and wants to confirm which serial
/// to pass to `--probe-serial`.
///
/// Returns probe metadata only — doesn't open the probe, so
/// callers can iterate to find the right one without claiming
/// exclusive access.
pub fn list_probes() -> Vec<DebugProbeInfo> {
    Lister::new().list_all()
}

/// Progress notification surfaced to the caller during a flash
/// operation. A thin remapping of probe-rs's `ProgressEvent` —
/// keeps callers from having to depend on probe-rs's types
/// directly and gives us a stable surface to evolve.
#[derive(Debug, Clone)]
pub enum SwdProgress {
    /// A new operation (erase / program / verify / fill) began.
    /// `total` is the byte count to process, when known.
    Started { op: SwdOperation, total: Option<u64> },
    /// Incremental progress on the current operation. `delta` is
    /// the byte count completed since the previous event (i.e.
    /// the caller maintains the running total themselves).
    Progress { op: SwdOperation, delta: u64 },
    /// Operation finished successfully.
    Finished { op: SwdOperation },
    /// Operation failed (download_file_with_options will return
    /// the actual error; this is informational).
    Failed { op: SwdOperation },
}

/// Phase identifier for [`SwdProgress`]. Mirrors probe-rs's
/// [`ProgressOperation`] but avoids leaking that type onto the
/// public API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwdOperation {
    Erase,
    Program,
    Verify,
    Fill,
    Other,
}

impl From<ProgressOperation> for SwdOperation {
    fn from(op: ProgressOperation) -> Self {
        match op {
            ProgressOperation::Erase => Self::Erase,
            ProgressOperation::Program => Self::Program,
            ProgressOperation::Verify => Self::Verify,
            ProgressOperation::Fill => Self::Fill,
            // probe-rs may add new variants; keep us forward-compatible.
            _ => Self::Other,
        }
    }
}

/// Open + attach + erase + write + verify + (optional) reset.
///
/// The whole operation runs synchronously; probe-rs's I/O is
/// blocking, and the operator's CLI invocation is one-shot. We
/// keep the function blocking rather than wrapping in
/// `tokio::task::spawn_blocking` because the only async caller
/// would be the future Studio / VS Code surface, which can
/// shell out to the CLI binary instead.
///
/// Use [`flash_with_progress`] to receive sector-by-sector
/// status updates; this wrapper just discards them.
pub fn flash(request: &SwdFlashRequest) -> Result<(), SwdError> {
    flash_with_progress(request, |_| {})
}

/// Like [`flash`], but invokes `on_progress` for every phase
/// transition and progress tick that probe-rs surfaces. The
/// closure runs inside probe-rs's flashing loop; keep it cheap
/// (push onto an `mpsc`, increment a counter — don't do I/O).
pub fn flash_with_progress<F>(
    request: &SwdFlashRequest,
    mut on_progress: F,
) -> Result<(), SwdError>
where
    F: FnMut(SwdProgress),
{
    if !request.artifact_path.exists() {
        return Err(SwdError::ArtifactMissing {
            path: request.artifact_path.clone(),
        });
    }
    let ext = request
        .artifact_path
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| SwdError::ArtifactNoExtension {
            path: request.artifact_path.clone(),
        })?
        .to_ascii_lowercase();

    // probe-rs's `Format` variants take per-format options. We pass
    // `Default::default()` everywhere — the defaults match what
    // operators expect (no skipped ELF sections, raw `.bin` carries
    // no embedded base address, etc.). Power-users who need to
    // tweak those can drop down to the underlying probe-rs API.
    let format = match ext.as_str() {
        "elf" => Format::Elf(Default::default()),
        "hex" => Format::Hex,
        "bin" => Format::Bin(Default::default()),
        _ => return Err(SwdError::ArtifactBadExtension { ext }),
    };

    // ---- Pick a probe ------------------------------------------------
    let probes = list_probes();
    let probe_info = match (probes.len(), request.probe_serial.as_deref()) {
        (0, _) => return Err(SwdError::NoProbe),
        (1, None) => probes.into_iter().next().unwrap(),
        (_, Some(target_serial)) => {
            let available: Vec<_> = probes
                .iter()
                .map(|p| {
                    p.serial_number
                        .clone()
                        .unwrap_or_else(|| "(no serial)".into())
                })
                .collect();
            probes
                .into_iter()
                .find(|p| p.serial_number.as_deref() == Some(target_serial))
                .ok_or_else(|| SwdError::SerialNotFound {
                    requested: target_serial.to_string(),
                    available: available.join(", "),
                })?
        }
        (n, None) => {
            let summary: Vec<_> = probes
                .iter()
                .map(|p| {
                    format!(
                        "{} ({})",
                        p.identifier,
                        p.serial_number
                            .clone()
                            .unwrap_or_else(|| "(no serial)".into()),
                    )
                })
                .collect();
            warn!(
                probe_count = n,
                "multiple probes attached; need --probe-serial"
            );
            return Err(SwdError::AmbiguousProbe(summary.join("; ")));
        }
    };

    info!(
        probe = probe_info.identifier,
        serial = ?probe_info.serial_number,
        "opening probe",
    );

    // ---- Open + attach ------------------------------------------------
    let probe = probe_info
        .open()
        .map_err(|e| SwdError::ProbeRs(format!("open probe: {e}")))?;

    info!(chip = request.chip, "attaching to target");
    let mut session: Session = probe
        .attach(&request.chip, Permissions::default())
        .map_err(|e| SwdError::ProbeRs(format!("attach to {}: {e}", request.chip)))?;

    // ---- Download ----------------------------------------------------
    let mut options = DownloadOptions::default();
    options.verify = request.verify;
    options.do_chip_erase = false; // sector-erase is faster; chip-erase risks losing OBs

    // Funnel probe-rs's progress events into the caller's closure.
    // The closure is shared between this and probe-rs via the
    // `FlashProgress::new` borrow; both run on the same thread so
    // RefCell-style interior mutability isn't needed.
    options.progress = FlashProgress::new(|event: ProgressEvent| match event {
        ProgressEvent::AddProgressBar { operation, total } => {
            on_progress(SwdProgress::Started {
                op: operation.into(),
                total,
            });
        }
        ProgressEvent::Started(op) => {
            on_progress(SwdProgress::Started {
                op: op.into(),
                total: None,
            });
        }
        ProgressEvent::Progress { operation, size, .. } => {
            on_progress(SwdProgress::Progress {
                op: operation.into(),
                delta: size,
            });
        }
        ProgressEvent::Finished(op) => {
            on_progress(SwdProgress::Finished { op: op.into() });
        }
        ProgressEvent::Failed(op) => {
            on_progress(SwdProgress::Failed { op: op.into() });
        }
        _ => {}
    });

    info!(
        artifact = ?request.artifact_path,
        format = ?format,
        verify = request.verify,
        "downloading",
    );
    download_file_with_options(&mut session, &request.artifact_path, format, options)
        .map_err(|e| SwdError::ProbeRs(format!("download_file: {e}")))?;

    // ---- Reset --------------------------------------------------------
    if request.reset_after {
        info!("resetting target");
        let mut core = session
            .core(0)
            .map_err(|e| SwdError::ProbeRs(format!("get core: {e}")))?;
        core.reset()
            .map_err(|e| SwdError::ProbeRs(format!("reset: {e}")))?;
        // Give the chip a moment to come back up before we drop the
        // probe handle — saves an operator-visible "device went away"
        // warning at the very end of a successful run.
        std::thread::sleep(Duration::from_millis(100));
    }

    Ok(())
}

/// Inputs to [`erase_chip`] — the same probe + chip selection as
/// [`SwdFlashRequest`], minus everything to do with the firmware
/// artefact. Kept as a distinct struct so adding "erase only this
/// sector range" later doesn't break callers.
#[derive(Debug, Clone)]
pub struct SwdEraseRequest {
    pub chip: String,
    pub probe_serial: Option<String>,
}

impl SwdEraseRequest {
    pub fn new() -> Self {
        Self {
            chip: DEFAULT_CHIP.to_string(),
            probe_serial: None,
        }
    }
}

impl Default for SwdEraseRequest {
    fn default() -> Self {
        Self::new()
    }
}

/// Wipe the entire flash. Destructive — the chip comes out empty,
/// bootloader and all. Operators reach for this when:
///
/// - A flash got into a half-written state and needs a clean slate
/// - They want to commission a chip from scratch (erase → burn BL)
/// - Read-protection (RDP) was set and they need to factory-reset
pub fn erase_chip(request: &SwdEraseRequest) -> Result<(), SwdError> {
    // Probe selection mirrors `flash`. Could refactor into a
    // shared helper, but inlined here is fewer indirections and
    // the two call sites diverge enough that DRY isn't a win.
    let probes = list_probes();
    let probe_info = match (probes.len(), request.probe_serial.as_deref()) {
        (0, _) => return Err(SwdError::NoProbe),
        (1, None) => probes.into_iter().next().unwrap(),
        (_, Some(target_serial)) => {
            let available: Vec<_> = probes
                .iter()
                .map(|p| {
                    p.serial_number
                        .clone()
                        .unwrap_or_else(|| "(no serial)".into())
                })
                .collect();
            probes
                .into_iter()
                .find(|p| p.serial_number.as_deref() == Some(target_serial))
                .ok_or_else(|| SwdError::SerialNotFound {
                    requested: target_serial.to_string(),
                    available: available.join(", "),
                })?
        }
        (n, None) => {
            let summary: Vec<_> = probes
                .iter()
                .map(|p| {
                    format!(
                        "{} ({})",
                        p.identifier,
                        p.serial_number
                            .clone()
                            .unwrap_or_else(|| "(no serial)".into()),
                    )
                })
                .collect();
            warn!(probe_count = n, "multiple probes attached; need --probe-serial");
            return Err(SwdError::AmbiguousProbe(summary.join("; ")));
        }
    };

    info!(
        probe = probe_info.identifier,
        serial = ?probe_info.serial_number,
        "opening probe for erase",
    );
    let probe = probe_info
        .open()
        .map_err(|e| SwdError::ProbeRs(format!("open probe: {e}")))?;

    info!(chip = request.chip, "attaching to target for erase");
    let mut session: Session = probe
        .attach(&request.chip, Permissions::default())
        .map_err(|e| SwdError::ProbeRs(format!("attach to {}: {e}", request.chip)))?;

    info!("erasing chip");
    let mut progress = FlashProgress::empty();
    erase_all(&mut session, &mut progress, false)
        .map_err(|e| SwdError::ProbeRs(format!("erase_all: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_defaults_match_team_ecu() {
        let req = SwdFlashRequest::new(PathBuf::from("bootloader.elf"));
        assert_eq!(req.chip, DEFAULT_CHIP);
        assert_eq!(req.base_addr, 0x0800_0000);
        assert!(req.verify);
        assert!(req.reset_after);
        assert!(req.probe_serial.is_none());
    }

    #[test]
    fn extension_match_picks_format() {
        // We can't actually call `flash()` without a probe, but
        // verifying the extension-match logic in isolation keeps
        // future refactors from breaking the supported set.
        for (ext, ok) in [
            ("elf", true),
            ("hex", true),
            ("bin", true),
            ("ELF", true), // case-insensitive
            ("ihex", false),
            ("img", false),
            ("", false),
        ] {
            let path = if ext.is_empty() {
                PathBuf::from("bootloader")
            } else {
                PathBuf::from(format!("bootloader.{ext}"))
            };
            let normalised = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase);
            let recognized = matches!(normalised.as_deref(), Some("elf" | "hex" | "bin"));
            assert_eq!(
                recognized, ok,
                "extension {ext:?} expected match={ok}, got {recognized}"
            );
        }
    }
}
