//! Firmware image handling — parse ELF / Intel HEX / raw .bin into a
//! normalised [`Image`], compute the triple
//! `(crc32, size, packed_version)` the bootloader's
//! `CMD_FLASH_VERIFY` expects.
//!
//! Bootloader-side semantics (from `bl_flash.c` / `bl_memmap.h` at
//! v1.0.0):
//!
//! - App region: `BL_APP_BASE`..=`BL_APP_END` = `0x08020000`..=`0x080DFFFF` (768 KB).
//! - `CMD_FLASH_VERIFY` args: `[expected_crc_le32, expected_size_le32,
//!   expected_version_le32]`. The device CRCs `size` bytes starting at
//!   `BL_APP_BASE`; if the host-provided CRC matches, it programmes the
//!   metadata FLASHWORD at `0x080FFFE0` and ACKs. Mismatch →
//!   `NACK(CRC_MISMATCH)`.
//! - CRC variant: CRC-32/ISO-HDLC (init `0xFFFFFFFF`, reflected,
//!   final XOR `0xFFFFFFFF`). Matches the reflected-byte-loop hand
//!   implementation in `bl_flash_crc32()` and STM32 HAL's reflected
//!   default configuration.
//!
//! Host-side: callers build an [`Image`] via [`loader::load`], validate
//! it fits the app region, and pass it to whichever consumer needs the
//! numeric triple (today: `verify`; feat/17 will add `flash`).
//!
//! ## What this module does NOT do
//!
//! - **Write firmware to hardware.** That's the flash pipeline
//!   ([`feat/16-flash-manager`]), which consumes an [`Image`] and
//!   drives `CMD_FLASH_ERASE` / `CMD_FLASH_WRITE` per-sector.
//! - **Signing.** Deferred to v2 / bootloader Phase 5.

pub mod loader;

use crc::{Crc, CRC_32_ISO_HDLC};

use crate::protocol::records::FirmwareInfo;

/// First byte of STM32H7 internal flash. Sector 0 starts here.
pub const BL_FLASH_BASE: u32 = 0x0800_0000;

/// First byte of the app region on the STM32H733 — byte 0 of flash
/// sector 1. Matches `BL_APP_BASE` in the bootloader.
pub const BL_APP_BASE: u32 = 0x0802_0000;

/// Inclusive last byte of the app region (start of the NVM sector
/// onwards is off-limits). Matches `BL_APP_END`.
pub const BL_APP_END: u32 = 0x080D_FFFF;

/// Maximum firmware size the host tool will send to the bootloader.
pub const BL_APP_MAX_SIZE: u32 = BL_APP_END - BL_APP_BASE + 1;

/// STM32H7 flash sector size — 128 KB on this variant. Each of the
/// 8 sectors is independently erasable.
pub const BL_SECTOR_SIZE: u32 = 0x0002_0000;

/// Inclusive last byte of sector 0 (the bootloader's own sector).
/// Any firmware segment touching `[BL_FLASH_BASE..=BL_BOOTLOADER_SECTOR_END]`
/// would require overwriting the bootloader itself — rejected before
/// any frame hits the wire.
pub const BL_BOOTLOADER_SECTOR_END: u32 = BL_APP_BASE - 1;

/// Inclusive last byte of the metadata sector (sector 7). The
/// FLASHWORD at `0x080FFFE0` is written by the bootloader on
/// successful `CMD_FLASH_VERIFY`; user firmware must stop at
/// `BL_APP_END` (end of sector 6).
pub const BL_FLASH_END: u32 = 0x080F_FFFF;

/// Offset from `BL_APP_BASE` at which the application's
/// `__firmware_info` record lives. Same as `BL_FWINFO_OFFSET`.
pub const FW_INFO_OFFSET: u32 = 0x400;

/// Absolute address of the `__firmware_info` record.
pub const FW_INFO_ADDR: u32 = BL_APP_BASE + FW_INFO_OFFSET;

// ---- Sector-map helpers ----

/// Map a flash address to its STM32H7 sector number (`0..=7`).
/// Returns `None` for anything outside `[BL_FLASH_BASE..=BL_FLASH_END]`
/// so callers can tell "not a flash address" apart from "sector 0".
///
/// The flash manager (feat/16) uses this to walk an [`Image`] one
/// sector at a time when deciding what to erase / diff-CRC / write.
pub fn sector_of_addr(addr: u32) -> Option<u8> {
    if !(BL_FLASH_BASE..=BL_FLASH_END).contains(&addr) {
        return None;
    }
    Some(((addr - BL_FLASH_BASE) / BL_SECTOR_SIZE) as u8)
}

// ---- Image ----

/// Normalised firmware image. Segments from the source format
/// (ELF / HEX / BIN) have been composed into a single contiguous
/// byte vector, `0xFF`-padded over any gaps between them, and
/// anchored to `base_addr`.
#[derive(Debug, Clone)]
pub struct Image {
    /// Lowest byte address covered by this image. For firmware
    /// destined for the bootloader app region this must equal
    /// [`BL_APP_BASE`].
    pub base_addr: u32,
    /// Contiguous bytes from `base_addr` onwards. `data.len()` ==
    /// `size()`.
    pub data: Vec<u8>,
    /// Parsed `__firmware_info` record if the image covers the
    /// `0x08020400` region with a valid record; `None` otherwise.
    pub fw_info: Option<FirmwareInfo>,
}

impl Image {
    /// Image size in bytes — what `CMD_FLASH_VERIFY` sees as
    /// `expected_size`.
    pub fn size(&self) -> u32 {
        self.data.len() as u32
    }

    /// First address past the end of the image
    /// (`base_addr + size`). Used for range checks.
    pub fn end_addr(&self) -> u32 {
        self.base_addr.saturating_add(self.size())
    }

    /// CRC-32/ISO-HDLC over the full `data` buffer — matches the
    /// bootloader's `bl_flash_crc32`. This is the value that goes
    /// into `CMD_FLASH_VERIFY`'s `expected_crc` field.
    pub fn crc32(&self) -> u32 {
        crc32(&self.data)
    }

    /// Version field for `CMD_FLASH_VERIFY` packed as
    /// `(major << 16) | (minor << 8) | patch`, all clamped to a byte.
    /// If no `__firmware_info` record is present, returns `0`.
    pub fn packed_version(&self) -> u32 {
        match &self.fw_info {
            Some(fw) => pack_version(
                fw.fw_version_major,
                fw.fw_version_minor,
                fw.fw_version_patch,
            ),
            None => 0,
        }
    }

    /// Validates the image fits inside the app region — base at or
    /// above `BL_APP_BASE`, end at or below `BL_APP_END + 1`.
    /// Callers typically call this before issuing any flash or
    /// verify command so a linker-script mistake surfaces before
    /// frames go on the wire.
    pub fn validate_fits_app_region(&self) -> Result<(), ImageError> {
        if self.base_addr < BL_APP_BASE {
            return Err(ImageError::BelowAppBase {
                base: self.base_addr,
                app_base: BL_APP_BASE,
            });
        }
        let end = self.end_addr();
        if end > BL_APP_END + 1 {
            return Err(ImageError::AboveAppEnd {
                end,
                app_end_plus_one: BL_APP_END + 1,
            });
        }
        Ok(())
    }

    /// Inclusive span of flash sectors this image occupies, e.g.
    /// `1..=6` for a full-size app binary based at `BL_APP_BASE`.
    /// Returns `None` if the image is not entirely within the flash
    /// region — callers should `validate_fits_app_region()` first
    /// (or rely on the loader having done so).
    ///
    /// The flash manager in feat/16 walks this range to drive
    /// per-sector `CMD_FLASH_ERASE` / `CMD_FLASH_READ_CRC` (diff
    /// mode) / `CMD_FLASH_WRITE`.
    pub fn sector_range(&self) -> Option<std::ops::RangeInclusive<u8>> {
        let size = self.size();
        if size == 0 {
            return None;
        }
        let start = sector_of_addr(self.base_addr)?;
        // `end_addr()` is one past the last byte; the last byte
        // written lives at `end_addr - 1`. Use that for sector
        // lookup so a segment ending exactly on a sector boundary
        // doesn't claim the next sector.
        let last_addr = self.end_addr().saturating_sub(1);
        let end = sector_of_addr(last_addr)?;
        Some(start..=end)
    }
}

/// CRC-32/ISO-HDLC — same variant the bootloader uses.
pub fn crc32(bytes: &[u8]) -> u32 {
    Crc::<u32>::new(&CRC_32_ISO_HDLC).checksum(bytes)
}

/// Pack `(major, minor, patch)` into the 24-bit layout the
/// bootloader's metadata word expects. Values above 255 are
/// silently clamped to 255 — that matches what the bootloader sees
/// since every field is stored in a single byte.
pub fn pack_version(major: u32, minor: u32, patch: u32) -> u32 {
    let m = major.min(255);
    let n = minor.min(255);
    let p = patch.min(255);
    (m << 16) | (n << 8) | p
}

/// Errors validating a parsed [`Image`] against the bootloader's
/// memory map. Loader-level failures (bad format, malformed bytes)
/// use [`loader::LoaderError`] instead.
///
/// All variants here are a **protection violation** — the host tool
/// classifies them with [`crate::cli::ExitCodeHint::ProtectionViolation`]
/// (exit 3) so CI pipelines can tell a bad linker script apart from
/// a genuine verify mismatch.
#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("image base address 0x{base:08X} is below BL_APP_BASE (0x{app_base:08X})")]
    BelowAppBase { base: u32, app_base: u32 },

    #[error("image ends at 0x{end:08X} — past BL_APP_END + 1 (0x{app_end_plus_one:08X})")]
    AboveAppEnd { end: u32, app_end_plus_one: u32 },

    /// A per-segment check: one of the input file's segments sits
    /// partly or wholly inside the bootloader's own flash sector
    /// (`[BL_FLASH_BASE..=BL_BOOTLOADER_SECTOR_END]`). Firing this
    /// before compose avoids allocating a sparse `0xFF`-padded
    /// buffer for an input we're going to refuse anyway, and gives
    /// the user a precise "segment N at 0x…" pointer instead of the
    /// post-compose BelowAppBase message, which only reports the
    /// lowest segment's base.
    #[error(
        "segment {segment_index} at 0x{addr:08X}..0x{end:08X} overlaps the bootloader sector \
         (0x08000000..=0x0801FFFF); your firmware would overwrite the bootloader itself — \
         check the linker script"
    )]
    TouchesBootloaderSector {
        segment_index: usize,
        addr: u32,
        end: u32,
    },

    /// A per-segment check: one of the input file's segments ends
    /// past `BL_APP_END + 1` (i.e. would clobber the metadata
    /// sector or run off the end of flash entirely).
    #[error(
        "segment {segment_index} at 0x{addr:08X}..0x{end:08X} extends past BL_APP_END + 1 \
         (0x{app_end_plus_one:08X}); reserve the metadata sector for the bootloader"
    )]
    BeyondAppRegion {
        segment_index: usize,
        addr: u32,
        end: u32,
        app_end_plus_one: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_version_lays_out_bytes() {
        assert_eq!(pack_version(1, 4, 2), 0x0001_0402);
        assert_eq!(pack_version(0, 0, 0), 0);
    }

    #[test]
    fn pack_version_clamps_to_byte() {
        assert_eq!(pack_version(256, 0, 0), 0x00FF_0000);
        assert_eq!(pack_version(999, 999, 999), 0x00FF_FFFF);
    }

    #[test]
    fn crc32_matches_reference_empty_buffer() {
        // CRC-32/ISO-HDLC of the empty byte string is 0.
        assert_eq!(crc32(&[]), 0);
    }

    #[test]
    fn crc32_matches_reference_ascii() {
        // "123456789" — the standard CRC test vector.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn image_size_and_end_addr_consistent() {
        let img = Image {
            base_addr: 0x0802_0000,
            data: vec![0xAA; 1024],
            fw_info: None,
        };
        assert_eq!(img.size(), 1024);
        assert_eq!(img.end_addr(), 0x0802_0400);
    }

    #[test]
    fn validate_rejects_base_below_app_base() {
        let img = Image {
            base_addr: 0x0800_0000,
            data: vec![0; 16],
            fw_info: None,
        };
        assert!(matches!(
            img.validate_fits_app_region(),
            Err(ImageError::BelowAppBase { .. })
        ));
    }

    #[test]
    fn validate_rejects_end_above_app_end() {
        let img = Image {
            base_addr: 0x080D_FFF0,
            data: vec![0; 32], // end at 0x080E_0010, past BL_APP_END + 1
            fw_info: None,
        };
        assert!(matches!(
            img.validate_fits_app_region(),
            Err(ImageError::AboveAppEnd { .. })
        ));
    }

    #[test]
    fn validate_accepts_maximum_size_image() {
        let img = Image {
            base_addr: BL_APP_BASE,
            data: vec![0; BL_APP_MAX_SIZE as usize],
            fw_info: None,
        };
        assert!(img.validate_fits_app_region().is_ok());
    }

    #[test]
    fn packed_version_returns_zero_without_fw_info() {
        let img = Image {
            base_addr: BL_APP_BASE,
            data: vec![0; 16],
            fw_info: None,
        };
        assert_eq!(img.packed_version(), 0);
    }

    // ---- sector_of_addr ----

    #[test]
    fn sector_of_addr_rejects_below_flash() {
        assert_eq!(sector_of_addr(0x0000_0000), None);
        assert_eq!(sector_of_addr(0x07FF_FFFF), None);
    }

    #[test]
    fn sector_of_addr_rejects_above_flash_end() {
        assert_eq!(sector_of_addr(0x0810_0000), None);
        assert_eq!(sector_of_addr(0xFFFF_FFFF), None);
    }

    #[test]
    fn sector_of_addr_maps_each_sector_boundary() {
        // Sector 0: 0x08000000..=0x0801FFFF
        assert_eq!(sector_of_addr(0x0800_0000), Some(0));
        assert_eq!(sector_of_addr(0x0801_FFFF), Some(0));
        // Sector 1: 0x08020000..=0x0803FFFF
        assert_eq!(sector_of_addr(0x0802_0000), Some(1));
        assert_eq!(sector_of_addr(0x0803_FFFF), Some(1));
        // Sector 6: 0x080C0000..=0x080DFFFF (last of app region)
        assert_eq!(sector_of_addr(0x080C_0000), Some(6));
        assert_eq!(sector_of_addr(BL_APP_END), Some(6));
        // Sector 7: 0x080E0000..=0x080FFFFF (metadata)
        assert_eq!(sector_of_addr(0x080E_0000), Some(7));
        assert_eq!(sector_of_addr(BL_FLASH_END), Some(7));
    }

    // ---- Image::sector_range ----

    #[test]
    fn sector_range_single_sector_image() {
        let img = Image {
            base_addr: BL_APP_BASE,
            data: vec![0; 1024],
            fw_info: None,
        };
        assert_eq!(img.sector_range(), Some(1..=1));
    }

    #[test]
    fn sector_range_spans_multiple_sectors() {
        // Base at sector 1, end at sector 3 (partway in).
        let img = Image {
            base_addr: BL_APP_BASE,
            data: vec![0; (BL_SECTOR_SIZE * 2 + 1024) as usize],
            fw_info: None,
        };
        assert_eq!(img.sector_range(), Some(1..=3));
    }

    #[test]
    fn sector_range_exact_boundary_does_not_overrun() {
        // Image ends exactly at the sector 2 boundary — sector_range
        // must return 1..=1, not 1..=2.
        let img = Image {
            base_addr: BL_APP_BASE,
            data: vec![0; BL_SECTOR_SIZE as usize],
            fw_info: None,
        };
        assert_eq!(img.sector_range(), Some(1..=1));
    }

    #[test]
    fn sector_range_full_app_region() {
        let img = Image {
            base_addr: BL_APP_BASE,
            data: vec![0; BL_APP_MAX_SIZE as usize],
            fw_info: None,
        };
        assert_eq!(img.sector_range(), Some(1..=6));
    }

    #[test]
    fn sector_range_empty_image_returns_none() {
        let img = Image {
            base_addr: BL_APP_BASE,
            data: Vec::new(),
            fw_info: None,
        };
        assert_eq!(img.sector_range(), None);
    }
}
