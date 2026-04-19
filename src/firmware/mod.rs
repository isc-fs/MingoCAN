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

/// First byte of the app region on the STM32H733 — byte 0 of flash
/// sector 1. Matches `BL_APP_BASE` in the bootloader.
pub const BL_APP_BASE: u32 = 0x0802_0000;

/// Inclusive last byte of the app region (start of the NVM sector
/// onwards is off-limits). Matches `BL_APP_END`.
pub const BL_APP_END: u32 = 0x080D_FFFF;

/// Maximum firmware size the host tool will send to the bootloader.
pub const BL_APP_MAX_SIZE: u32 = BL_APP_END - BL_APP_BASE + 1;

/// Offset from `BL_APP_BASE` at which the application's
/// `__firmware_info` record lives. Same as `BL_FWINFO_OFFSET`.
pub const FW_INFO_OFFSET: u32 = 0x400;

/// Absolute address of the `__firmware_info` record.
pub const FW_INFO_ADDR: u32 = BL_APP_BASE + FW_INFO_OFFSET;

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
#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("image base address 0x{base:08X} is below BL_APP_BASE (0x{app_base:08X})")]
    BelowAppBase { base: u32, app_base: u32 },

    #[error("image ends at 0x{end:08X} — past BL_APP_END + 1 (0x{app_end_plus_one:08X})")]
    AboveAppEnd { end: u32, app_end_plus_one: u32 },
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
}
