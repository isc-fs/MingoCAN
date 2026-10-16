//! Burn the bootloader over **USB DFU** — no ST-LINK (#645).
//!
//! The STM32H72x/73x ROM bootloader speaks USB DFU (with ST's "DfuSe"
//! extensions, AN3156) on USB_OTG_FS (PA11/PA12) when the chip boots from
//! system memory: hold BOOT0 high at reset, or power a blank chip. This
//! module is the [`crate::swd`] burn — erase, write, independent read-back
//! verify, optional provisioning seed, start — over that interface instead
//! of SWD.
//!
//! Lives behind the `swd` Cargo feature alongside the SWD path (both are
//! "commissioning a new board"); USB access is `nusb`, which probe-rs
//! already pulls in.
//!
//! ## Platform notes
//! - **macOS / Linux:** no driver needed (Linux: a udev rule for
//!   `0483:df11` to avoid needing root).
//! - **Windows:** bind the WinUSB driver to the "STM32 BOOTLOADER" /
//!   `0483:DF11` device once with Zadig.

pub mod layout;
pub mod protocol;
mod usb;

use std::path::PathBuf;

use thiserror::Error;
use tracing::{info, warn};

use crate::swd::SwdProgress;
pub use layout::MemoryLayout;
pub use usb::{list_devices, DfuDeviceInfo};

/// USB IDs of the STM32 ROM bootloader in DFU mode.
pub const STM32_DFU_VID: u16 = 0x0483;
pub const STM32_DFU_PID: u16 = 0xDF11;

/// Where the CAN bootloader lives (sector 0).
pub const DEFAULT_BASE_ADDR: u32 = 0x0800_0000;

#[derive(Debug, Error)]
pub enum DfuError {
    #[error(
        "no STM32 in DFU mode found (USB 0483:DF11). Hold BOOT0 high and reset or power the \
         board with USB connected; on Windows bind WinUSB to the device with Zadig"
    )]
    NoDevice,
    #[error("several boards are in DFU mode — pick one with --serial; found: {0}")]
    AmbiguousDevice(String),
    #[error("no DFU device with serial {requested:?}; found: {available}")]
    SerialNotFound {
        requested: String,
        available: String,
    },
    #[error("USB: {0}")]
    Usb(String),
    #[error(
        "the DFU device exposes no \"Internal Flash\" region — is it an STM32 ROM bootloader?"
    )]
    NoFlashLayout,
    #[error("DFU {what} failed: status {status}, state {state}")]
    Protocol {
        what: String,
        status: String,
        state: String,
    },
    #[error("DFU {0} timed out")]
    Timeout(String),
    #[error("{0}")]
    Artifact(String),
    #[error("the image (0x{base:08X}, {len} bytes) doesn't fit the device's flash ({layout})")]
    OutOfRange {
        base: u32,
        len: usize,
        layout: String,
    },
    #[error(
        "flash verify FAILED: read back CRC32 0x{got:08X} != image CRC32 0x{want:08X} \
         ({bytes} bytes from 0x{base:08X}). The board was NOT started — re-run the burn."
    )]
    VerifyMismatch {
        want: u32,
        got: u32,
        base: u32,
        bytes: usize,
    },
    #[error(
        "this bootloader image can't be provisioned: {reason}. Provisioning needs a bootloader \
         built with seed support (v1.7.0+) — nothing was written to the board"
    )]
    SeedUnsupported { reason: String },
    #[error(
        "provisioning is only implemented for the 1 MB STM32H72x/73x; this device's flash is {0}"
    )]
    SeedChipUnsupported(String),
    #[error(
        "the provisioning seed read back from the chip doesn't pass the bootloader's checks \
         (wanted node-id 0x{node_id:X}) — re-run the burn"
    )]
    SeedReadbackMismatch { node_id: u8 },
}

/// Inputs to one DFU burn.
#[derive(Debug, Clone)]
pub struct DfuFlashRequest {
    /// `.elf`, `.hex` or `.bin` (a `.bin` goes to [`DEFAULT_BASE_ADDR`]).
    pub artifact_path: PathBuf,
    /// USB serial of the device to use when several are in DFU mode.
    pub serial: Option<String>,
    /// Read the image back and CRC-compare it (default `true`).
    pub verify: bool,
    /// Leave DFU and start the bootloader when done (default `true`).
    pub leave: bool,
    /// Provision this node-id in the same run (see `swd-flash --provision`).
    pub seed_node_id: Option<u8>,
}

impl DfuFlashRequest {
    pub fn new(artifact_path: PathBuf) -> Self {
        Self {
            artifact_path,
            serial: None,
            verify: true,
            leave: true,
            seed_node_id: None,
        }
    }
}

/// Summary of a finished burn.
#[derive(Debug, Clone)]
pub struct DfuFlashReport {
    pub verified: bool,
    /// CRC32 of the bytes written (the parsed image, not the file).
    pub crc32: u32,
    pub size_bytes: u64,
    pub seeded_node_id: Option<u8>,
    /// The ROM bootloader's version (`bcdDevice`), e.g. `0x0200`.
    pub rom_version: u16,
    pub serial: Option<String>,
}

/// Check everything that could make provisioning fail silently BEFORE the
/// device is touched — a refusal never costs an erased chip.
fn provisioning_preflight(
    request: &DfuFlashRequest,
    layout: &MemoryLayout,
) -> Result<Option<(u8, [u8; crate::provision_seed::SEED_LEN])>, DfuError> {
    use crate::provision_seed::{bootloader_supports_seed, build_seed_record, SeedSupport};
    let Some(id) = request.seed_node_id else {
        return Ok(None);
    };
    if !layout.is_h72x_73x_1mb() {
        return Err(DfuError::SeedChipUnsupported(format!(
            "{} KiB at 0x{:08X}",
            layout.size() / 1024,
            layout.start
        )));
    }
    let record = build_seed_record(id).map_err(|reason| DfuError::SeedUnsupported { reason })?;
    match bootloader_supports_seed(&request.artifact_path)
        .map_err(|reason| DfuError::SeedUnsupported { reason })?
    {
        SeedSupport::Supported => Ok(Some((id, record))),
        SeedSupport::NotSupported => Err(DfuError::SeedUnsupported {
            reason: format!(
                "{} has no `{}` — it predates seed support",
                request.artifact_path.display(),
                crate::provision_seed::SEED_CONSUMER_SYMBOL
            ),
        }),
        SeedSupport::Unverifiable(why) => Err(DfuError::SeedUnsupported {
            reason: format!("{why}; burn the bootloader's .elf to provision"),
        }),
    }
}

/// Mass-erase, write, verify, (seed), start — over USB DFU.
pub fn flash_with_progress<F>(
    request: &DfuFlashRequest,
    on_progress: F,
) -> Result<DfuFlashReport, DfuError>
where
    F: FnMut(SwdProgress),
{
    if !request.artifact_path.exists() {
        return Err(DfuError::Artifact(format!(
            "{} does not exist",
            request.artifact_path.display()
        )));
    }
    let image =
        crate::firmware::loader::load_unchecked(&request.artifact_path, Some(DEFAULT_BASE_ADDR))
            .map_err(|e| {
                DfuError::Artifact(format!("parse {}: {e}", request.artifact_path.display()))
            })?;
    if !request.verify {
        warn!("verify=false — the image will not be read back; a bad write would go unnoticed");
    }

    let mut dev = usb::open(request.serial.as_deref())?;
    let layout = dev.layout().clone();
    let seed = provisioning_preflight(request, &layout)?;
    info!(
        flash = %layout.name,
        size_kib = layout.size() / 1024,
        rom = format!("0x{:04X}", dev.rom_version()),
        transfer = protocol::DfuTransport::transfer_size(&dev),
        "DFU device opened",
    );

    let crc32 = protocol::burn(
        &mut dev,
        &layout,
        &protocol::BurnPlan {
            image_base: image.base_addr,
            image: &image.data,
            seed,
            verify: request.verify,
            leave: request.leave,
        },
        on_progress,
    )?;
    Ok(DfuFlashReport {
        verified: request.verify,
        crc32,
        size_bytes: image.data.len() as u64,
        seeded_node_id: seed.map(|(id, _)| id),
        rom_version: dev.rom_version(),
        serial: dev.serial().map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisioning_is_refused_on_other_flash_layouts_before_anything_runs() {
        let mut req = DfuFlashRequest::new(PathBuf::from("does-not-matter.elf"));
        req.seed_node_id = Some(0x2);
        let two_banks = layout::parse("@Internal Flash /0x08000000/16*128Kg").unwrap();
        assert!(matches!(
            provisioning_preflight(&req, &two_banks),
            Err(DfuError::SeedChipUnsupported(_))
        ));
        req.seed_node_id = None;
        assert!(provisioning_preflight(&req, &two_banks).unwrap().is_none());
    }

    #[test]
    fn provisioning_needs_an_elf_with_seed_support() {
        let dir = std::env::temp_dir().join(format!("mingocan-dfu-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("bl.bin");
        std::fs::write(&bin, [0u8; 64]).unwrap();
        let mut req = DfuFlashRequest::new(bin);
        req.seed_node_id = Some(0x2);
        let h7 = layout::parse("@Internal Flash  /0x08000000/8*128Kg").unwrap();
        let err = provisioning_preflight(&req, &h7).unwrap_err();
        assert!(matches!(err, DfuError::SeedUnsupported { .. }), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
