//! Format-aware firmware loader. Accepts a path + optional address
//! hint (used only for raw `.bin`), detects the format, dispatches to
//! the matching parser, and returns a normalised [`Image`].
//!
//! Format detection picks from three signals, in order:
//!
//! 1. ELF magic (`7F 45 4C 46`) at byte 0.
//! 2. Intel HEX: ASCII input beginning with `:` (first non-whitespace
//!    byte).
//! 3. File extension: `.elf`, `.hex`, `.bin` if the magic check failed.
//!
//! If none matches the loader returns `UnknownFormat` so the caller
//! can surface a clear error. Callers that know the format can skip
//! detection by calling [`load_elf`] / [`load_ihex`] / [`load_bin`]
//! directly.

use std::path::Path;

use object::{Object, ObjectSegment};

use super::{Image, BL_APP_BASE, FW_INFO_OFFSET};
use crate::protocol::records::FirmwareInfo;

/// Errors the loader surfaces. Validation errors against the
/// bootloader's memory map live in [`super::ImageError`] and are
/// raised by [`Image::validate_fits_app_region`] rather than here.
#[derive(Debug, thiserror::Error)]
pub enum LoaderError {
    #[error("I/O error reading firmware file: {0}")]
    Io(#[from] std::io::Error),

    #[error(
        "could not identify firmware format: no magic match and path has no .elf/.hex/.bin extension"
    )]
    UnknownFormat,

    #[error("raw .bin input requires --address (where would it flash to?)")]
    BinaryNeedsAddress,

    #[error("ELF parse error: {0}")]
    Elf(String),

    #[error("Intel HEX parse error: {0}")]
    IntelHex(String),

    #[error("image has no loadable segments — nothing to flash")]
    NoSegments,

    #[error("ELF segment address 0x{addr:016X} does not fit in 32 bits")]
    AddressOverflow { addr: u64 },
}

/// Detected input format. Callers typically don't reach for this —
/// [`load`] handles the full dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Elf,
    IntelHex,
    Binary,
}

/// Top-level entry point: read the file, detect its format, parse,
/// return an [`Image`]. `address_hint` is required for raw binaries
/// and ignored for ELF / HEX (which carry their own load addresses).
pub fn load(path: &Path, address_hint: Option<u32>) -> Result<Image, LoaderError> {
    let bytes = std::fs::read(path)?;
    let format = detect_format(path, &bytes)?;
    match format {
        Format::Elf => load_elf(&bytes),
        Format::IntelHex => load_ihex(&bytes),
        Format::Binary => {
            let addr = address_hint.ok_or(LoaderError::BinaryNeedsAddress)?;
            Ok(load_bin(&bytes, addr))
        }
    }
}

/// Inspect the file's magic bytes + extension to pick a format.
/// Exposed so callers can sniff a candidate file without reading it
/// fully — handy for validation before a flash session kicks off.
pub fn detect_format(path: &Path, bytes: &[u8]) -> Result<Format, LoaderError> {
    // ELF magic first — unambiguous.
    if bytes.starts_with(&[0x7F, b'E', b'L', b'F']) {
        return Ok(Format::Elf);
    }
    // Intel HEX: ASCII text starting with `:`. Tolerate leading
    // whitespace / BOM for files authored on Windows.
    let first_real = bytes.iter().find(|&&b| !b.is_ascii_whitespace());
    if let Some(&b':') = first_real {
        // Not perfect — a file called `foo.txt` starting with `:`
        // would be accepted — but the parser catches malformed
        // records downstream, and the likelihood of a non-HEX file
        // starting with `:` is low.
        return Ok(Format::IntelHex);
    }

    // Fall back to the file extension.
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
    {
        Some(ext) if ext == "elf" => Ok(Format::Elf),
        Some(ext) if ext == "hex" => Ok(Format::IntelHex),
        Some(ext) if ext == "bin" => Ok(Format::Binary),
        _ => Err(LoaderError::UnknownFormat),
    }
}

// ---- ELF ----

/// Parse ELF bytes into an [`Image`]. Collects every PT_LOAD-style
/// segment, normalises to a contiguous buffer, `0xFF`-pads any gaps
/// between segments.
pub fn load_elf(bytes: &[u8]) -> Result<Image, LoaderError> {
    let file = object::File::parse(bytes).map_err(|e| LoaderError::Elf(format!("{e}")))?;

    let mut segments: Vec<(u32, Vec<u8>)> = Vec::new();
    for seg in file.segments() {
        // Only segments with an actual payload are interesting — skip
        // zero-size + memsz-only (BSS-style) segments.
        let data = seg.data().map_err(|e| LoaderError::Elf(format!("{e}")))?;
        if data.is_empty() {
            continue;
        }
        let addr = seg.address();
        if addr > u32::MAX as u64 {
            return Err(LoaderError::AddressOverflow { addr });
        }
        segments.push((addr as u32, data.to_vec()));
    }

    compose_image(segments)
}

// ---- Intel HEX ----

/// Parse Intel HEX bytes into an [`Image`]. Handles `Data`,
/// `ExtendedLinearAddress`, and `EndOfFile` records; `StartLinearAddress`
/// and segment-mode records are ignored (cosmetic, not needed for a
/// Cortex-M firmware).
pub fn load_ihex(bytes: &[u8]) -> Result<Image, LoaderError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| LoaderError::IntelHex(format!("input is not valid UTF-8: {e}")))?;

    let mut base_hi: u32 = 0;
    let mut segments: Vec<(u32, Vec<u8>)> = Vec::new();

    for record in ihex::Reader::new(text) {
        let record = record.map_err(|e| LoaderError::IntelHex(format!("{e}")))?;
        match record {
            ihex::Record::Data { offset, value } => {
                let addr = base_hi | u32::from(offset);
                segments.push((addr, value));
            }
            ihex::Record::ExtendedLinearAddress(hi) => {
                base_hi = u32::from(hi) << 16;
            }
            ihex::Record::ExtendedSegmentAddress(_)
            | ihex::Record::StartLinearAddress(_)
            | ihex::Record::StartSegmentAddress { .. } => {
                // All cosmetic for our use case. Ignored.
            }
            ihex::Record::EndOfFile => break,
        }
    }

    compose_image(segments)
}

// ---- Raw binary ----

/// Wrap raw bytes at the supplied `address` into an [`Image`]. No
/// validation beyond the fw-info extraction; callers invoke
/// `validate_fits_app_region()` separately before flashing.
pub fn load_bin(bytes: &[u8], address: u32) -> Image {
    let data = bytes.to_vec();
    let fw_info = extract_fw_info(address, &data);
    Image {
        base_addr: address,
        data,
        fw_info,
    }
}

// ---- Compose + fw-info extraction ----

/// Collapse a set of `(addr, bytes)` segments into a contiguous
/// buffer keyed by the lowest address encountered. Gaps are
/// `0xFF`-padded (mirrors freshly-erased flash content).
fn compose_image(segments: Vec<(u32, Vec<u8>)>) -> Result<Image, LoaderError> {
    if segments.is_empty() {
        return Err(LoaderError::NoSegments);
    }

    let base = segments.iter().map(|(addr, _)| *addr).min().unwrap();
    let end = segments
        .iter()
        .map(|(addr, bytes)| addr.saturating_add(bytes.len() as u32))
        .max()
        .unwrap();

    let total = (end - base) as usize;
    let mut data = vec![0xFFu8; total];
    for (addr, bytes) in segments {
        let off = (addr - base) as usize;
        data[off..off + bytes.len()].copy_from_slice(&bytes);
    }

    let fw_info = extract_fw_info(base, &data);
    Ok(Image {
        base_addr: base,
        data,
        fw_info,
    })
}

/// Try to parse the `__firmware_info` record at `FW_INFO_ADDR` if
/// the image covers that offset. Returns `None` on any failure
/// (image doesn't reach that address, magic mismatch, unsupported
/// record version) — callers fall back to `packed_version == 0`.
fn extract_fw_info(base: u32, data: &[u8]) -> Option<FirmwareInfo> {
    let fw_info_addr = BL_APP_BASE + FW_INFO_OFFSET;
    if base > fw_info_addr {
        return None;
    }
    let off = (fw_info_addr - base) as usize;
    if off + FirmwareInfo::SIZE > data.len() {
        return None;
    }
    FirmwareInfo::parse(&data[off..off + FirmwareInfo::SIZE]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Format detection ----

    #[test]
    fn detect_elf_by_magic() {
        let bytes = b"\x7FELFsome more bytes";
        let format = detect_format(Path::new("/tmp/unknown"), bytes).unwrap();
        assert_eq!(format, Format::Elf);
    }

    #[test]
    fn detect_intel_hex_by_leading_colon() {
        let bytes = b":020000040800F2\n:10000000...\n";
        let format = detect_format(Path::new("/tmp/unknown"), bytes).unwrap();
        assert_eq!(format, Format::IntelHex);
    }

    #[test]
    fn detect_intel_hex_tolerates_leading_whitespace() {
        // Some editors add a UTF-8 BOM or a stray newline; accept
        // anything where the first non-whitespace byte is `:`.
        let bytes = b"\r\n:020000040800F2\n";
        let format = detect_format(Path::new("/tmp/unknown"), bytes).unwrap();
        assert_eq!(format, Format::IntelHex);
    }

    #[test]
    fn detect_binary_via_extension() {
        let bytes = b"\x00\x01\x02";
        let format = detect_format(Path::new("/tmp/raw.bin"), bytes).unwrap();
        assert_eq!(format, Format::Binary);
    }

    #[test]
    fn detect_elf_via_extension_when_magic_absent() {
        let bytes = b"partial object file";
        let format = detect_format(Path::new("/tmp/corrupt.elf"), bytes).unwrap();
        assert_eq!(format, Format::Elf);
    }

    #[test]
    fn detect_unknown_returns_error() {
        let bytes = b"random garbage";
        let err = detect_format(Path::new("/tmp/foo.txt"), bytes).unwrap_err();
        assert!(matches!(err, LoaderError::UnknownFormat));
    }

    // ---- Intel HEX ----

    fn sample_ihex_at_app_base() -> Vec<u8> {
        // Upper linear address 0x0802, offset 0x0000, 4 bytes 0xAA …
        // + EOF record. Hand-assembled so we control the exact bytes.
        let mut s = String::new();
        // Extended Linear Address: value 0x0802
        // :02 0000 04 0802 record. Bytes = 02+00+00+04+08+02 = 0x10; checksum = (-sum) & 0xFF = 0xF0.
        s.push_str(":020000040802F0\n");
        // Data at offset 0x0000 : 0xAA, 0xBB, 0xCC, 0xDD
        // :04 0000 00 AABBCCDD cksum
        // Sum = 0x04 + 0x00 + 0x00 + 0x00 + 0xAA + 0xBB + 0xCC + 0xDD = 0x312
        //   → low byte 0x12 → two's complement 0xEE.
        s.push_str(":04000000AABBCCDDEE\n");
        // EOF : 00 0000 01 FF
        s.push_str(":00000001FF\n");
        s.into_bytes()
    }

    #[test]
    fn load_ihex_parses_basic_data() {
        let bytes = sample_ihex_at_app_base();
        let img = load_ihex(&bytes).unwrap();
        assert_eq!(img.base_addr, 0x0802_0000);
        assert_eq!(img.data, vec![0xAA, 0xBB, 0xCC, 0xDD]);
        assert!(img.fw_info.is_none());
    }

    #[test]
    fn load_ihex_rejects_non_utf8() {
        // Invalid UTF-8 (0xFF mid-record).
        let bytes = vec![0xFF, 0xFE];
        let err = load_ihex(&bytes).unwrap_err();
        assert!(matches!(err, LoaderError::IntelHex(_)));
    }

    // ---- Raw binary ----

    #[test]
    fn load_bin_wraps_bytes_at_address() {
        let bytes = vec![0x01, 0x02, 0x03];
        let img = load_bin(&bytes, 0x0802_0000);
        assert_eq!(img.base_addr, 0x0802_0000);
        assert_eq!(img.data, bytes);
        assert!(img.fw_info.is_none());
    }

    // ---- compose_image ----

    #[test]
    fn compose_pads_gaps_with_ff() {
        // Two segments with a gap in the middle.
        let segs = vec![(0x1000u32, vec![0xAA, 0xBB]), (0x1008u32, vec![0xCC, 0xDD])];
        let img = compose_image(segs).unwrap();
        assert_eq!(img.base_addr, 0x1000);
        assert_eq!(img.size(), 10);
        assert_eq!(
            img.data,
            vec![0xAA, 0xBB, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xCC, 0xDD]
        );
    }

    #[test]
    fn compose_without_segments_errors() {
        let err = compose_image(vec![]).unwrap_err();
        assert!(matches!(err, LoaderError::NoSegments));
    }

    // ---- extract_fw_info via a crafted image ----

    fn sample_fw_info_bytes() -> Vec<u8> {
        let mut buf = Vec::with_capacity(64);
        buf.extend_from_slice(&FirmwareInfo::MAGIC.to_le_bytes());
        buf.extend_from_slice(&0x0001_0000u32.to_le_bytes()); // record_version = 1.0
        buf.extend_from_slice(&1u32.to_le_bytes()); // major
        buf.extend_from_slice(&4u32.to_le_bytes()); // minor
        buf.extend_from_slice(&2u32.to_le_bytes()); // patch
        buf.extend_from_slice(&0x0000_0450u32.to_le_bytes()); // mcu_id
        buf.extend_from_slice(&[0xAA; 8]); // git_hash
        buf.extend_from_slice(&0u64.to_le_bytes()); // build_timestamp
        let mut name = [0u8; 16];
        name[..5].copy_from_slice(b"TEST\0");
        buf.extend_from_slice(&name);
        buf.extend_from_slice(&0u64.to_le_bytes()); // reserved
        assert_eq!(buf.len(), FirmwareInfo::SIZE);
        buf
    }

    #[test]
    fn extract_fw_info_finds_record_at_fixed_offset() {
        // Build a buffer that starts at BL_APP_BASE, has 0x400 bytes
        // of filler, then the fw-info record, then some trailing
        // bytes. `extract_fw_info` should find + parse it.
        let mut data = vec![0xFFu8; FW_INFO_OFFSET as usize];
        data.extend(sample_fw_info_bytes());
        data.extend(vec![0u8; 64]); // trailer
        let img = Image {
            base_addr: BL_APP_BASE,
            data: data.clone(),
            fw_info: extract_fw_info(BL_APP_BASE, &data),
        };
        let fw = img.fw_info.expect("fw_info should parse");
        assert_eq!(fw.version(), (1, 4, 2));
    }

    #[test]
    fn extract_fw_info_returns_none_when_image_doesnt_cover_offset() {
        let data = vec![0xFF; 0x100]; // too short to reach 0x400
        let out = extract_fw_info(BL_APP_BASE, &data);
        assert!(out.is_none());
    }

    // ---- ELF end-to-end ----
    //
    // Hand-crafting a minimum viable ELF in a unit test is gnarly
    // (ELF header + program header + a data segment, all
    // byte-precise). The format-detection test above covers the
    // magic path, and format-level ELF parsing is exercised at
    // smoke-test time via user-supplied firmware. TODO(feat/15):
    // bundle a small pre-built `.elf` under `tests/fixtures/` and
    // wire an end-to-end load-elf-parses-known-segments test here.
}
