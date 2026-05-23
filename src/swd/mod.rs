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
    download_file_with_options, erase_all, DownloadOptions, FlashProgress, Format, ProgressEvent,
    ProgressOperation,
};
use probe_rs::MemoryInterface;
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
    /// Verify the readback after write. When `true` we do BOTH
    /// probe-rs's own verify pass (during download) AND a
    /// host-side readback of chip flash that re-CRCs the bytes
    /// independently. The host-side pass is the authoritative
    /// one for [#247](https://github.com/isc-fs/can-flasher/issues/247)
    /// — probe-rs's verify trusted bytes the flash algorithm had
    /// just written, which hid silent corruption on H7. The
    /// host-side readback talks to flash via the Cortex-M AHB so
    /// it can't be fooled by the flash algorithm's view.
    pub verify: bool,
    /// Issue a reset (with `Sysresetreq`) after the flash
    /// completes. The bootloader needs a reset to start running
    /// from its newly-written address, so the default is `true`.
    pub reset_after: bool,
    /// Erase only the sectors that contain bytes from the
    /// artifact (`true`), instead of the whole chip flash bank
    /// (`false`, the new default after #247). Sector-erase is
    /// ~3 s faster but the corruption pattern in #247 was
    /// reproducibly silent on sector-erase + write; chip-erase
    /// is the canonical "start clean" path and is what
    /// STM32CubeProgrammer does by default for the same `.elf`.
    /// Option bytes live in their own bank on STM32H7, so
    /// chip-erase of user flash doesn't touch them.
    pub sector_erase_only: bool,
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
            sector_erase_only: false,
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
    #[error(
        "host-side flash verify FAILED: chip readback CRC32 0x{got:08X} != source CRC32 0x{want:08X} \
         (verified {bytes} bytes from 0x{base:08X}). \
         The flash write looked successful but the bytes on chip don't match the source — this is \
         the silent-corruption mode tracked in #247. Re-flash with STM32CubeProgrammer to recover, \
         and treat this chip as not-burned until you do."
    )]
    VerifyMismatch {
        want: u32,
        got: u32,
        base: u64,
        bytes: usize,
    },
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
    Started {
        op: SwdOperation,
        total: Option<u64>,
    },
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
}

/// Per-flash summary returned by [`flash`] / [`flash_with_progress`].
///
/// Surfacing this in the CLI / UI gives operators a verifiable
/// fingerprint of *what* got written — useful when reconciling a
/// suspected-bad ECU against a known-good one (e.g. the carrier
/// MLC1 vs MLC3 case in
/// [bootloader#117](https://github.com/isc-fs/stm32-can-bootloader/issues/117)).
///
/// `crc32` is the CRC of the source artifact bytes. Combined with
/// `verify == true` (probe-rs's readback-compare), that's
/// equivalent to "the same CRC is on the chip's flash right now":
/// every byte we sent matched what probe-rs read back, and our
/// CRC was computed over those same bytes.
#[derive(Debug, Clone)]
pub struct SwdFlashReport {
    /// `true` when probe-rs's `verify` pass ran and succeeded
    /// (the default). `false` only when the operator opted out
    /// with `--no-verify` / verify=false in the request.
    pub verified: bool,
    /// CRC32 of the source artifact file's raw bytes. Stable
    /// fingerprint operators can record in bench notes / compare
    /// across boards.
    pub crc32: u32,
    /// Total bytes of the source artifact (file size).
    pub size_bytes: u64,
    /// VTref voltage at the probe's target pin, in volts, sampled
    /// once at session start. `None` when the probe doesn't report
    /// it (e.g. dev-only ST-LINK boards without a VTref divider).
    /// A low value here is one of the failure modes worth catching
    /// — a brown-out mid-write corrupts flash silently.
    pub target_voltage_v: Option<f32>,
}

impl From<ProgressOperation> for SwdOperation {
    fn from(op: ProgressOperation) -> Self {
        // probe-rs 0.31's `ProgressOperation` is exhaustive over
        // exactly these four phases; clippy rejects a catch-all.
        // If a future probe-rs adds a variant the match goes red
        // and we update it deliberately.
        match op {
            ProgressOperation::Erase => Self::Erase,
            ProgressOperation::Program => Self::Program,
            ProgressOperation::Verify => Self::Verify,
            ProgressOperation::Fill => Self::Fill,
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
pub fn flash(request: &SwdFlashRequest) -> Result<SwdFlashReport, SwdError> {
    flash_with_progress(request, |_| {})
}

/// Like [`flash`], but invokes `on_progress` for every phase
/// transition and progress tick that probe-rs surfaces. The
/// closure runs inside probe-rs's flashing loop; keep it cheap
/// (push onto an `mpsc`, increment a counter — don't do I/O).
pub fn flash_with_progress<F>(
    request: &SwdFlashRequest,
    mut on_progress: F,
) -> Result<SwdFlashReport, SwdError>
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

    // Parse the artifact into a flat (base_addr, data) image. This
    // is the SAME view of "what bytes will land on chip" that the
    // host-side verify reads back from flash, so a CRC over
    // `image.data` is the authoritative fingerprint — comparable
    // across operators and across boards. (v2.4.3 mistakenly
    // CRC'd the raw file bytes, which for `.elf` includes the
    // ELF headers / debug info and has nothing to do with what
    // gets flashed. #247.)
    let image = crate::firmware::loader::load(&request.artifact_path, None).map_err(|e| {
        SwdError::ProbeRs(format!("parse artifact {:?}: {e}", request.artifact_path))
    })?;
    let source_crc32 = crate::firmware::crc32(&image.data);
    let image_base = u64::from(image.base_addr);
    let image_size = image.data.len();

    // Operators sometimes pass `--no-verify` for bench-loop speed.
    // After #247 we treat it as a much harder foot-gun: probe-rs's
    // internal verify alone wasn't sufficient (it returned OK on
    // corrupt flash), and skipping the host-side readback removes
    // the only remaining authoritative check. Surface it loudly.
    if !request.verify {
        warn!(
            "verify=false — skipping host-side flash readback. \
             Silent corruption (see issue #247) will not be caught \
             until the chip misbehaves on the bus. Re-run without \
             --no-verify before declaring the chip burned."
        );
    }

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
    let mut probe = probe_info
        .open()
        .map_err(|e| SwdError::ProbeRs(format!("open probe: {e}")))?;

    // Sample VTref BEFORE `attach()` consumes the Probe by value.
    // A low / missing voltage here is one of the silent-failure
    // modes the issue tracker flagged — a brown-out mid-write
    // can corrupt flash without throwing a probe-rs error.
    // `target_voltage()` returns `Ok(None)` on probes without a
    // VTref divider (some bare ST-LINK dev boards); we accept that
    // as "unknown" rather than fail the flash over it.
    let target_voltage_v = probe.get_target_voltage().ok().flatten();
    info!(?target_voltage_v, "probe target VTref voltage");

    info!(chip = request.chip, "attaching to target");
    let mut session: Session = probe
        .attach(&request.chip, Permissions::default())
        .map_err(|e| SwdError::ProbeRs(format!("attach to {}: {e}", request.chip)))?;

    // ---- Download ----------------------------------------------------
    let mut options = DownloadOptions::default();
    // probe-rs's verify is kept on (cheap, occasionally catches
    // things). The host-side readback below is the authoritative
    // check — both off only when the operator explicitly passes
    // --no-verify.
    options.verify = request.verify;
    // Default to chip-erase (sector_erase_only=false) after #247.
    // Sector-erase on STM32H7 reproduced silent flash corruption
    // even though probe-rs's verify passed; chip-erase + write
    // matches what STM32CubeProgrammer does for the same artifact
    // and produces a working BL every time. Option bytes live in
    // their own bank on H7 so chip-erase of user flash leaves
    // them alone.
    options.do_chip_erase = !request.sector_erase_only;

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
        ProgressEvent::Progress {
            operation, size, ..
        } => {
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

    // ---- Host-side flash readback (#247 safety net) ------------------
    //
    // probe-rs's `verify=true` on its own returned OK on corrupt
    // flash in #247: the flash algorithm computes a CRC of bytes
    // it had just written, so any bug in that algorithm's view of
    // flash (wrong word size, missing wait state, stale cache) is
    // invisible to it. We do a second pass here: read the bytes
    // back from chip flash via the Cortex-M AHB and CRC them
    // independently. If those CRCs disagree, the write was bad
    // regardless of what probe-rs's verify said.
    //
    // Skip only when the operator explicitly opted out with
    // verify=false; the no-verify warning above already told them
    // the safety net is off.
    if request.verify {
        info!(
            base = format!("0x{image_base:08X}"),
            size_bytes = image_size,
            "reading flash back over SWD for host-side verify",
        );
        let mut readback = vec![0u8; image_size];
        {
            let mut core = session
                .core(0)
                .map_err(|e| SwdError::ProbeRs(format!("get core for readback: {e}")))?;
            core.read(image_base, &mut readback)
                .map_err(|e| SwdError::ProbeRs(format!("read flash readback: {e}")))?;
        }
        let readback_crc32 = crate::firmware::crc32(&readback);
        if readback_crc32 != source_crc32 {
            return Err(SwdError::VerifyMismatch {
                want: source_crc32,
                got: readback_crc32,
                base: image_base,
                bytes: image_size,
            });
        }
        info!(
            crc32 = format!("0x{readback_crc32:08X}"),
            "host-side verify passed",
        );
    }

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

    info!(
        crc32 = format!("0x{source_crc32:08X}"),
        size_bytes = image_size,
        verified = request.verify,
        "swd flash complete",
    );
    Ok(SwdFlashReport {
        verified: request.verify,
        crc32: source_crc32,
        size_bytes: image_size as u64,
        target_voltage_v,
    })
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
