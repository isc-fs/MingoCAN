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
    download_file_with_options, erase_all, BinOptions, DownloadOptions, FlashProgress, Format,
    ProgressEvent, ProgressOperation,
};
use probe_rs::probe::list::Lister;
use probe_rs::probe::DebugProbeInfo;
use probe_rs::MemoryInterface;
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
    /// one for [#247](https://github.com/isc-fs/MingoCAN/issues/247)
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
    /// When `Some(id)`, provision the board as node `id` over SWD: after
    /// the burn, program a provisioning seed (see
    /// [`crate::provision_seed`]) that the bootloader adopts into its NVM
    /// on first boot — no CAN adapter, no boot round-trip.
    ///
    /// Checked before the probe is opened: the image must be an ELF built
    /// with seed support (stm32-can-bootloader#183), the chip must be an
    /// H72x/73x, and the burn must chip-erase (a sector-erase would leave
    /// the old node-id in NVM, which wins over the seed).
    pub seed_node_id: Option<u8>,
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
            seed_node_id: None,
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
         The flash write looked successful but the bytes on chip don't match the source — most \
         likely the silent-corruption mode tracked in #247: re-flash with STM32CubeProgrammer to \
         recover, and treat this chip as not-burned until you do. \
         (Less commonly, a CRC mismatch on a known-good probe + artifact can mean a host vs probe-rs \
         image-parse divergence rather than bad flash — re-fetch the artifact and retry once before \
         condemning the chip.)"
    )]
    VerifyMismatch {
        want: u32,
        got: u32,
        base: u64,
        bytes: usize,
    },
    #[error(
        "this bootloader image can't be provisioned over SWD: {reason}. Provisioning needs a \
         bootloader built with seed support (stm32-can-bootloader#183) — nothing was written \
         to the board"
    )]
    SeedUnsupported { reason: String },
    #[error(
        "SWD provisioning is only implemented for the STM32H72x/73x bootloader target, not {chip}"
    )]
    SeedChipUnsupported { chip: String },
    #[error("programming the provisioning seed over SWD failed: {0}")]
    SeedProgram(String),
    #[error(
        "the provisioning seed read back from the chip doesn't pass the bootloader's checks \
         (wanted node-id 0x{node_id:X}) — the board would boot unprovisioned. Re-run the burn."
    )]
    SeedReadbackMismatch { node_id: u8 },
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
    /// The node-id seeded over SWD (see [`SwdFlashRequest::seed_node_id`]),
    /// or `None` when seeding wasn't requested.
    pub seeded_node_id: Option<u8>,
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

    // ---- Provisioning pre-flight --------------------------------------
    //
    // Every reason provisioning could fail silently is checked HERE,
    // before the probe is opened — so a refusal never costs the operator
    // an erased chip.
    if let Some(id) = request.seed_node_id {
        if !chip_supports_seed(&request.chip) {
            return Err(SwdError::SeedChipUnsupported {
                chip: request.chip.clone(),
            });
        }
        crate::provision_seed::build_seed_record(id)
            .map_err(|reason| SwdError::SeedUnsupported { reason })?;
        if request.sector_erase_only {
            // Without a chip-erase, sector 7 keeps the old NVM node-id and
            // the bootloader's "NVM wins" rule ignores the seed: the board
            // would silently keep its previous ID.
            return Err(SwdError::SeedUnsupported {
                reason: "provisioning needs the default chip-erase — with --sector-erase the old \
                         node-id stays in NVM and the bootloader would ignore the new one"
                    .into(),
            });
        }
        match crate::provision_seed::bootloader_supports_seed(&request.artifact_path)
            .map_err(|reason| SwdError::SeedUnsupported { reason })?
        {
            crate::provision_seed::SeedSupport::Supported => {}
            crate::provision_seed::SeedSupport::NotSupported => {
                return Err(SwdError::SeedUnsupported {
                    reason: format!(
                        "{} has no `{}` — it predates seed support",
                        request.artifact_path.display(),
                        crate::provision_seed::SEED_CONSUMER_SYMBOL
                    ),
                });
            }
            crate::provision_seed::SeedSupport::Unverifiable(why) => {
                return Err(SwdError::SeedUnsupported {
                    reason: format!("{why}; burn the bootloader's .elf to provision"),
                });
            }
        }
    }

    // Parse the artifact into a flat (base_addr, data) image. This
    // is the SAME view of "what bytes will land on chip" that the
    // host-side verify reads back from flash, so a CRC over
    // `image.data` is the authoritative fingerprint — comparable
    // across operators and across boards. (v2.4.3 mistakenly
    // CRC'd the raw file bytes, which for `.elf` includes the
    // ELF headers / debug info and has nothing to do with what
    // gets flashed. #247.)
    // FMEA #271 G9 + G10:
    //  - `load_unchecked` (not `load`): the bootloader image is based
    //    at sector 0 (0x08000000), which the CAN-flash sector-0 guard
    //    rejects. SWD's whole purpose is to write that sector, so we
    //    skip the CAN-only validation here. (G9 — this used to abort
    //    every bootloader burn before any probe I/O.)
    //  - pass the `--base` hint: a raw `.bin` carries no address, so
    //    without it the load aborts with BinaryNeedsAddress and the
    //    documented `--base` flag was silently ignored. (G10.)
    let image = crate::firmware::loader::load_unchecked(
        &request.artifact_path,
        Some(request.base_addr as u32),
    )
    .map_err(|e| SwdError::ProbeRs(format!("parse artifact {:?}: {e}", request.artifact_path)))?;
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

    // probe-rs's `Format` variants take per-format options. ELF/HEX
    // carry their own addresses, so `Default::default()` is right. A
    // raw `.bin` does NOT — it needs the base address explicitly, or
    // probe-rs flashes it at 0x0 (FMEA #271 G10). Thread the same
    // `--base` the loader used so the CRC fingerprint and what
    // probe-rs writes agree.
    let format = match ext.as_str() {
        "elf" => Format::Elf(Default::default()),
        "hex" => Format::Hex,
        "bin" => Format::Bin(BinOptions {
            base_address: Some(request.base_addr),
            skip: 0,
        }),
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
            // Reset-and-halt BEFORE reading back (audit hardening on
            // top of the #247 readback). At this point the core is
            // halted in probe-rs's flash-algorithm state with caches +
            // the H7 flash ART prefetch potentially holding the bytes
            // we just wrote. A pre-reset read could be served those
            // cached bytes while the flash array itself is bad — so a
            // "passing" CRC wouldn't prove what the bootloader will
            // actually boot from. reset_and_halt returns caches + ART
            // to a clean post-reset state; the core stays HALTED (it
            // does not run the freshly-flashed app), so the readback
            // reflects true flash contents. Any real reset the operator
            // asked for (`reset_after`) still happens below.
            core.reset_and_halt(Duration::from_millis(500))
                .map_err(|e| {
                    SwdError::ProbeRs(format!("reset-and-halt before flash readback: {e}"))
                })?;
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

    // ---- Provisioning seed ---------------------------------------------
    //
    // After the image is on chip and verified, BEFORE the reset: program
    // the seed so the bootloader finds it on its first boot and writes
    // the node-id into its own NVM. Single flashword, register-level —
    // see `program_provision_seed` for why it can't go through probe-rs.
    let seeded_node_id = if let Some(id) = request.seed_node_id {
        info!(
            node_id = format!("0x{id:X}"),
            "programming provisioning seed over SWD"
        );
        program_provision_seed(&mut session, id)?;
        info!(
            node_id = format!("0x{id:X}"),
            "provisioning seed programmed + verified"
        );
        Some(id)
    } else {
        None
    };

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
        seeded_node_id,
    })
}

// ---- Single-flashword programming (provisioning seed) ---------------
//
// The seed is ONE 256-bit flashword. It must be programmed as exactly
// that — not through probe-rs's flash algorithm, whose page on the
// H72x/73x is 1 KiB (`stm32h72x-73x_1024`, page_size 0x400). A page
// write pads the other 31 flashwords of 0x080FFC00..0x080FFFFF with
// 0xFF *and programs them*, which on the H7 leaves them non-virgin while
// still reading 0xFF. That page holds the app-metadata word
// (0x080FFFE0), which the bootloader's `bl_flash_write_metadata` writes
// IN PLACE whenever it reads all-0xFF — so the first CAN app flash after
// provisioning would program a flashword twice and risk inconsistent
// ECC (the #166 ECC-brick class). The NVM log's last KiB has the same
// problem later in life.
//
// So we drive the FLASH controller directly, mirroring the bootloader's
// own `HAL_FLASH_Program(FLASH_TYPEPROGRAM_FLASHWORD, ...)` register for
// register (STM32H7 HAL `stm32h7xx_hal_flash.c`, `stm32h733xx.h`):
// unlock KEYR1 → set CR1.PG → write 8 × u32 → wait SR1.QW → check the
// HAL's error mask (clear via CCR1) → clear PG → relock.

/// `FLASH_R_BASE` = `D1_AHB1PERIPH_BASE + 0x2000`.
const H7_FLASH_BASE: u64 = 0x5200_2000;
const H7_FLASH_KEYR1: u64 = H7_FLASH_BASE + 0x04;
const H7_FLASH_CR1: u64 = H7_FLASH_BASE + 0x0C;
const H7_FLASH_SR1: u64 = H7_FLASH_BASE + 0x10;
const H7_FLASH_CCR1: u64 = H7_FLASH_BASE + 0x14;
const H7_FLASH_KEY1: u32 = 0x4567_0123;
const H7_FLASH_KEY2: u32 = 0xCDEF_89AB;
const H7_CR_LOCK: u32 = 1 << 0;
const H7_CR_PG: u32 = 1 << 1;
const H7_SR_QW: u32 = 1 << 2;
const H7_SR_EOP: u32 = 1 << 16;
/// `FLASH_FLAG_ALL_ERRORS_BANK1`: WRPERR | PGSERR | STRBERR | INCERR |
/// OPERR | RDPERR | RDSERR | SNECCERR | DBECCERR | CRCRDERR.
const H7_SR_ALL_ERRORS: u32 = (1 << 17)
    | (1 << 18)
    | (1 << 19)
    | (1 << 21)
    | (1 << 22)
    | (1 << 23)
    | (1 << 24)
    | (1 << 25)
    | (1 << 26)
    | (1 << 28);
/// `FLASH_NB_32BITWORD_IN_FLASHWORD` on the H72x/73x (256-bit words).
const H7_WORDS_PER_FLASHWORD: usize = 8;
/// Programming one flashword takes microseconds; this only bounds a
/// wedged controller.
const H7_FLASH_OP_TIMEOUT: Duration = Duration::from_secs(1);

/// The register map above is the single-bank, 256-bit-flashword
/// H72x/73x's, and the seed lives at the top of a **1 MB** bank
/// (`0x080FFFC0`). So: an H72x/73x part whose flash-size code — the
/// character after the pin-count letter, e.g. the `G` in `STM32H733ZGTx`
/// — is `G` (1 MB). That excludes the 128 KB STM32H730 value line and
/// the 512 KB (`E`) variants, where the seed address doesn't exist.
fn chip_supports_seed(chip: &str) -> bool {
    let c = chip.to_ascii_uppercase();
    (c.starts_with("STM32H72") || c.starts_with("STM32H73")) && c.as_bytes().get(10) == Some(&b'G')
}

fn wait_flash_idle(core: &mut probe_rs::Core<'_>, what: &str) -> Result<u32, SwdError> {
    let start = std::time::Instant::now();
    loop {
        let sr = core
            .read_word_32(H7_FLASH_SR1)
            .map_err(|e| SwdError::SeedProgram(format!("{what}: read FLASH_SR1: {e}")))?;
        if sr & H7_SR_QW == 0 {
            return Ok(sr);
        }
        if start.elapsed() > H7_FLASH_OP_TIMEOUT {
            return Err(SwdError::SeedProgram(format!(
                "{what}: flash controller still busy after {H7_FLASH_OP_TIMEOUT:?} (FLASH_SR1=0x{sr:08X})"
            )));
        }
    }
}

/// Program the provisioning seed as a single flashword at
/// [`crate::provision_seed::SEED_ADDR`], then read it back through the
/// same checks the bootloader applies. Resets and halts the core first
/// so the FLASH controller is in the same post-reset state the
/// bootloader programs from, and so the bootloader cannot run and
/// consume anything before we've verified the word.
fn program_provision_seed(session: &mut Session, node_id: u8) -> Result<(), SwdError> {
    use crate::provision_seed::{parse_seed_record, SEED_ADDR, SEED_LEN};

    let record = crate::provision_seed::build_seed_record(node_id)
        .map_err(|e| SwdError::SeedProgram(format!("building provisioning seed: {e}")))?;

    let mut core = session
        .core(0)
        .map_err(|e| SwdError::ProbeRs(format!("get core for seed programming: {e}")))?;
    core.reset_and_halt(Duration::from_millis(500))
        .map_err(|e| SwdError::ProbeRs(format!("reset-and-halt before seed programming: {e}")))?;

    // Never program a flashword twice. After the default chip-erase burn
    // the seed word is virgin (all 0xFF); anything else means sector 7
    // wasn't erased and the write must not happen.
    let mut current = [0u8; SEED_LEN];
    core.read(SEED_ADDR, &mut current)
        .map_err(|e| SwdError::ProbeRs(format!("read seed word before programming: {e}")))?;
    if current.iter().any(|&b| b != 0xFF) {
        return Err(SwdError::SeedProgram(format!(
            "the seed flashword at 0x{SEED_ADDR:08X} is already programmed, so sector 7 was not \
             erased — refusing to program it twice. Burn with the default chip-erase."
        )));
    }

    // HAL_FLASH_Program: wait for the last operation, bailing on any
    // stale error flag.
    let sr = wait_flash_idle(&mut core, "before seed programming")?;
    if sr & H7_SR_ALL_ERRORS != 0 {
        let _ = core.write_word_32(H7_FLASH_CCR1, sr & H7_SR_ALL_ERRORS);
        return Err(SwdError::SeedProgram(format!(
            "flash controller reported an error before programming (FLASH_SR1=0x{sr:08X})"
        )));
    }

    // HAL_FLASH_Unlock.
    let cr = core
        .read_word_32(H7_FLASH_CR1)
        .map_err(|e| SwdError::SeedProgram(format!("read FLASH_CR1: {e}")))?;
    if cr & H7_CR_LOCK != 0 {
        core.write_word_32(H7_FLASH_KEYR1, H7_FLASH_KEY1)
            .and_then(|()| core.write_word_32(H7_FLASH_KEYR1, H7_FLASH_KEY2))
            .map_err(|e| SwdError::SeedProgram(format!("unlock FLASH_CR1: {e}")))?;
        let cr = core
            .read_word_32(H7_FLASH_CR1)
            .map_err(|e| SwdError::SeedProgram(format!("read FLASH_CR1 after unlock: {e}")))?;
        if cr & H7_CR_LOCK != 0 {
            return Err(SwdError::SeedProgram(
                "FLASH_CR1 stayed locked after the unlock key sequence".into(),
            ));
        }
    }

    // The programming itself. Whatever happens, relock afterwards.
    let programmed = (|| -> Result<(), SwdError> {
        let cr = core
            .read_word_32(H7_FLASH_CR1)
            .map_err(|e| SwdError::SeedProgram(format!("read FLASH_CR1: {e}")))?;
        core.write_word_32(H7_FLASH_CR1, cr | H7_CR_PG)
            .map_err(|e| SwdError::SeedProgram(format!("set FLASH_CR1.PG: {e}")))?;

        for (i, chunk) in record
            .as_chunks::<4>()
            .0
            .iter()
            .take(H7_WORDS_PER_FLASHWORD)
            .enumerate()
        {
            let word = u32::from_le_bytes(*chunk);
            core.write_word_32(SEED_ADDR + (i as u64) * 4, word)
                .map_err(|e| SwdError::SeedProgram(format!("write seed word {i}: {e}")))?;
        }

        let sr = wait_flash_idle(&mut core, "seed programming")?;
        let errors = sr & H7_SR_ALL_ERRORS;
        if errors != 0 {
            let _ = core.write_word_32(H7_FLASH_CCR1, errors);
            return Err(SwdError::SeedProgram(format!(
                "flash controller rejected the seed write (FLASH_SR1=0x{sr:08X})"
            )));
        }
        if sr & H7_SR_EOP != 0 {
            let _ = core.write_word_32(H7_FLASH_CCR1, H7_SR_EOP);
        }
        Ok(())
    })();

    // CLEAR_BIT(CR1, PG) then HAL_FLASH_Lock — attempted even on failure.
    let relock = core
        .read_word_32(H7_FLASH_CR1)
        .and_then(|cr| core.write_word_32(H7_FLASH_CR1, (cr & !H7_CR_PG) | H7_CR_LOCK));
    programmed?;
    relock.map_err(|e| SwdError::SeedProgram(format!("relock FLASH_CR1: {e}")))?;

    // Read it back through the bootloader's own acceptance checks
    // (magic, complement, range, CRC). A torn or mis-laid-out seed is
    // silently ignored by the bootloader, so this is the last place it
    // can be caught.
    let mut readback = [0u8; SEED_LEN];
    core.read(SEED_ADDR, &mut readback)
        .map_err(|e| SwdError::ProbeRs(format!("seed readback: {e}")))?;
    match parse_seed_record(&readback) {
        Some(id) if id == node_id => Ok(()),
        _ => Err(SwdError::SeedReadbackMismatch { node_id }),
    }
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

    #[test]
    fn seed_programming_is_gated_to_the_h72x_73x() {
        // Same 256-bit flashword, single bank, same FLASH register map.
        for chip in [
            "STM32H733ZGTx",
            "stm32h733zgtx",
            "STM32H723ZGTx",
            "STM32H735IGKx",
        ] {
            assert!(chip_supports_seed(chip), "{chip}");
        }
        // Wrong family (different flashword width / bank layout), or a
        // smaller flash where 0x080FFFC0 doesn't exist: the 128 KB H730
        // value line and the 512 KB `E` variants.
        for chip in [
            "STM32H730VBTx",
            "STM32H723VETx",
            "STM32H743ZITx",
            "STM32H7A3ZITxQ",
            "STM32H7",
            "STM32F446RETx",
            "",
        ] {
            assert!(!chip_supports_seed(chip), "{chip}");
        }
    }

    #[test]
    fn flash_register_constants_match_the_stm32h733_hal() {
        // stm32h733xx.h: FLASH_R_BASE = D1_AHB1PERIPH_BASE (0x5200_0000) + 0x2000.
        assert_eq!(H7_FLASH_BASE, 0x5200_0000 + 0x2000);
        assert_eq!(
            (H7_FLASH_KEYR1, H7_FLASH_CR1, H7_FLASH_SR1, H7_FLASH_CCR1),
            (0x5200_2004, 0x5200_200C, 0x5200_2010, 0x5200_2014)
        );
        // stm32h7xx_hal_flash.h
        assert_eq!((H7_FLASH_KEY1, H7_FLASH_KEY2), (0x4567_0123, 0xCDEF_89AB));
        // FLASH_FLAG_ALL_ERRORS_BANK1 from the *_Pos defines:
        // WRPERR 17, PGSERR 18, STRBERR 19, INCERR 21, OPERR 22, RDPERR 23,
        // RDSERR 24, SNECCERR 25, DBECCERR 26, CRCRDERR 28.
        let expected = [17, 18, 19, 21, 22, 23, 24, 25, 26, 28]
            .iter()
            .fold(0u32, |m, b| m | (1 << b));
        assert_eq!(H7_SR_ALL_ERRORS, expected);
        assert_eq!(
            (H7_CR_LOCK, H7_CR_PG, H7_SR_QW, H7_SR_EOP),
            (1, 2, 4, 1 << 16)
        );
        // FLASH_NB_32BITWORD_IN_FLASHWORD, and the seed is exactly one flashword.
        assert_eq!(H7_WORDS_PER_FLASHWORD * 4, crate::provision_seed::SEED_LEN);
        assert_eq!(
            crate::provision_seed::SEED_ADDR % 32,
            0,
            "flashword-aligned"
        );
    }
}
