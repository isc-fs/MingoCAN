//! Provisioning seed — the host↔bootloader contract for setting a
//! board's node-id over **SWD**, in the same step that burns the
//! bootloader, with no CAN round-trip.
//!
//! The node-id normally lives in the bootloader's log-structured KV
//! NVM (sector 7), whose record format is private to `bl_nvm` and
//! evolves. Rather than teach the host that layout, the SWD tool
//! writes a fixed record at a reserved flash address; the bootloader
//! reads it on first boot and translates it into a proper NVM entry
//! via its own `bl_nvm_write`. One-shot: the BL only acts on the seed
//! when its NVM has no node-id yet — which is always the case right
//! after a chip-erase burn.
//!
//! **The layout here is a byte-for-byte mirror of `bl_provision_seed_t`**
//! in stm32-can-bootloader (`Core/Inc/bl_provision.h`, PR #184 / issue
//! #183). If the two ever disagree the bootloader silently rejects the
//! seed and the board boots unprovisioned, so the golden vectors in the
//! tests below were computed independently (Python `zlib.crc32`) rather
//! than with this crate's own CRC.
//!
//! A bootloader without seed support ignores the word entirely, which
//! would also leave the board unprovisioned with no error anywhere. So
//! [`bootloader_supports_seed`] checks the image *before* anything
//! touches the chip, and the SWD burn refuses to provision with an
//! image it can't prove supports seeding.

use std::path::Path;

/// Reserved flash address for the seed — one STM32H7 256-bit flashword
/// just below the app-metadata word (`0x080FFFE0`), at the top of the
/// NVM sector. Mirrors `BL_PROVISION_SEED_ADDR`.
pub const SEED_ADDR: u64 = 0x080F_FFC0;

/// Magic marking a valid seed (vs. erased `0xFFFFFFFF` flash or
/// garbage). Little-endian in the record. Mirrors
/// `BL_PROVISION_SEED_MAGIC`.
pub const SEED_MAGIC: u32 = 0xB007_0D1D;

/// Size of the seed record — a full H7 flashword (write-once between
/// erases, so the whole word is programmed at once). Mirrors
/// `BL_PROVISION_SEED_SIZE`.
pub const SEED_LEN: usize = 32;

/// Bytes the CRC covers: everything before the `crc32` field.
const CRC_COVERED: usize = 8;

/// Largest assignable node-id (0xF is the broadcast/host-reserved ID;
/// 0x0 is the host). Real boards are `1..=0xE`.
const MAX_NODE_ID: u8 = 0x0E;

/// The bootloader function that consumes the seed at boot. Its presence
/// in a bootloader ELF's symbol table is how we know the image supports
/// seeding: it has external linkage, lives in its own translation unit,
/// and the bootloader is built without LTO, so a seed-capable build
/// always carries it (verified against the release ELFs, which keep
/// their full symbol tables).
pub const SEED_CONSUMER_SYMBOL: &str = "bl_provision_consume_seed";

/// Build the 32-byte seed flashword for `node_id`.
///
/// Layout (all little-endian), exactly `bl_provision_seed_t`:
///
/// | offset | field | value |
/// |---|---|---|
/// | 0  | `magic`         | [`SEED_MAGIC`] |
/// | 4  | `node_id`       | `0x1..=0xE` |
/// | 5  | `node_id_check` | `!node_id` |
/// | 6  | `reserved`      | `0xFFFF` |
/// | 8  | `crc32`         | CRC-32/ISO-HDLC over bytes `[0..8)` |
/// | 12 | `padding`       | `0xFF` × 20 |
///
/// Returns `Err` for an unassignable node-id (must be `1..=0xE`).
pub fn build_seed_record(node_id: u8) -> Result<[u8; SEED_LEN], String> {
    if node_id == 0 || node_id > MAX_NODE_ID {
        return Err(format!(
            "node-id 0x{node_id:X} is not assignable (must be 0x1..=0x{MAX_NODE_ID:X}; \
             0x0 is the host, 0xF is broadcast)"
        ));
    }
    let mut rec = [0xFFu8; SEED_LEN];
    rec[0..4].copy_from_slice(&SEED_MAGIC.to_le_bytes());
    rec[4] = node_id;
    rec[5] = !node_id;
    // rec[6..8] stays 0xFFFF (reserved).
    let crc = crate::firmware::crc32(&rec[..CRC_COVERED]);
    rec[8..12].copy_from_slice(&crc.to_le_bytes());
    Ok(rec)
}

/// Read back the node-id a seed record encodes, applying the same
/// checks the bootloader does (magic, complement, range, CRC). `None`
/// when the bytes aren't a seed the bootloader would accept — erased
/// flash, a torn write, or a record built to the wrong layout.
pub fn parse_seed_record(rec: &[u8]) -> Option<u8> {
    if rec.len() < CRC_COVERED + 4 {
        return None;
    }
    let magic = u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]]);
    if magic != SEED_MAGIC {
        return None;
    }
    let node_id = rec[4];
    if rec[5] != !node_id {
        return None;
    }
    if node_id == 0 || node_id > MAX_NODE_ID {
        return None;
    }
    let stored = u32::from_le_bytes([rec[8], rec[9], rec[10], rec[11]]);
    if crate::firmware::crc32(&rec[..CRC_COVERED]) != stored {
        return None;
    }
    Some(node_id)
}

/// Whether a bootloader image can adopt a provisioning seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedSupport {
    /// The image carries [`SEED_CONSUMER_SYMBOL`].
    Supported,
    /// An ELF without the consumer — a bootloader that predates seed
    /// support. A seed written with it would be silently ignored.
    NotSupported,
    /// Can't tell from this file (a `.hex` / `.bin` has no symbols).
    Unverifiable(String),
}

/// Inspect a bootloader image for seed support, without touching any
/// hardware. Only an ELF can answer; `.hex` / `.bin` come back
/// [`SeedSupport::Unverifiable`].
pub fn bootloader_supports_seed(path: &Path) -> Result<SeedSupport, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if ext != "elf" {
        return Ok(SeedSupport::Unverifiable(format!(
            "a .{ext} image has no symbol table, so its seed support can't be checked"
        )));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    elf_supports_seed(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))
}

/// [`bootloader_supports_seed`] on ELF bytes already in memory.
pub fn elf_supports_seed(bytes: &[u8]) -> Result<SeedSupport, String> {
    use object::{Object, ObjectSymbol};

    let file = object::File::parse(bytes).map_err(|e| e.to_string())?;
    if file.symbols().next().is_none() {
        // A stripped ELF is as blind as a .bin.
        return Ok(SeedSupport::Unverifiable(
            "the ELF has no symbol table (stripped), so its seed support can't be checked".into(),
        ));
    }
    let found = file
        .symbols()
        .any(|s| s.name() == Ok(SEED_CONSUMER_SYMBOL) && s.is_definition());
    Ok(if found {
        SeedSupport::Supported
    } else {
        SeedSupport::NotSupported
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// First 12 bytes (magic | id | check | reserved | crc) for a few
    /// node-ids, computed with Python's `zlib.crc32` — an implementation
    /// independent of this crate — over the same 8 bytes the bootloader's
    /// `crc32_buf` covers.
    const GOLDEN: &[(u8, [u8; 12])] = &[
        (
            0x01,
            [
                0x1D, 0x0D, 0x07, 0xB0, 0x01, 0xFE, 0xFF, 0xFF, 0xBF, 0x89, 0x49, 0x57,
            ],
        ),
        (
            0x02,
            [
                0x1D, 0x0D, 0x07, 0xB0, 0x02, 0xFD, 0xFF, 0xFF, 0x08, 0x98, 0xBA, 0x47,
            ],
        ),
        (
            0x03,
            [
                0x1D, 0x0D, 0x07, 0xB0, 0x03, 0xFC, 0xFF, 0xFF, 0x5A, 0x95, 0xC4, 0xFE,
            ],
        ),
        (
            0x0E,
            [
                0x1D, 0x0D, 0x07, 0xB0, 0x0E, 0xF1, 0xFF, 0xFF, 0xD4, 0xDE, 0x76, 0x04,
            ],
        ),
    ];

    #[test]
    fn seed_matches_the_bootloader_layout_byte_for_byte() {
        for (id, head) in GOLDEN {
            let rec = build_seed_record(*id).unwrap();
            assert_eq!(&rec[..12], head, "node-id 0x{id:X}");
            assert!(
                rec[12..].iter().all(|&b| b == 0xFF),
                "padding is erased flash"
            );
        }
    }

    #[test]
    fn build_then_parse_roundtrips() {
        for id in 1..=MAX_NODE_ID {
            let rec = build_seed_record(id).unwrap();
            assert_eq!(parse_seed_record(&rec), Some(id));
        }
    }

    #[test]
    fn rejects_reserved_ids() {
        assert!(build_seed_record(0x0).is_err());
        assert!(build_seed_record(0xF).is_err());
        assert!(build_seed_record(0x10).is_err());
    }

    #[test]
    fn parse_rejects_what_the_bootloader_rejects() {
        assert_eq!(parse_seed_record(&[0xFF; SEED_LEN]), None, "erased");

        let good = build_seed_record(0x03).unwrap();

        let mut bad = good;
        bad[5] = 0x00;
        assert_eq!(parse_seed_record(&bad), None, "complement");

        let mut bad = good;
        bad[0] ^= 0x01;
        assert_eq!(parse_seed_record(&bad), None, "magic");

        let mut bad = good;
        bad[8] ^= 0x01;
        assert_eq!(parse_seed_record(&bad), None, "crc");

        // The pre-fix host layout: right magic/id/check, no CRC. The
        // bootloader rejects it, so our readback must too — this is the
        // exact bug that would have left every board unprovisioned.
        let mut no_crc = good;
        no_crc[8..12].copy_from_slice(&[0xFF; 4]);
        assert_eq!(parse_seed_record(&no_crc), None, "missing crc");
    }

    #[test]
    fn non_elf_images_are_unverifiable() {
        let dir = std::env::temp_dir().join(format!("cf-seed-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for ext in ["bin", "hex"] {
            let p = dir.join(format!("CAN_BL.{ext}"));
            std::fs::write(&p, b"not an elf").unwrap();
            assert!(matches!(
                bootloader_supports_seed(&p).unwrap(),
                SeedSupport::Unverifiable(_)
            ));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A minimal ARM ELF carrying the named function symbols, like a
    /// real bootloader build does.
    fn synth_bootloader_elf(symbols: &[&str]) -> Vec<u8> {
        use object::write::{Object, Symbol, SymbolSection};
        use object::{
            Architecture, BinaryFormat, Endianness, SymbolFlags, SymbolKind, SymbolScope,
        };

        let mut obj = Object::new(BinaryFormat::Elf, Architecture::Arm, Endianness::Little);
        let text = obj.section_id(object::write::StandardSection::Text);
        let off = obj.append_section_data(text, &[0u8; 64], 4);
        for (i, name) in symbols.iter().enumerate() {
            obj.add_symbol(Symbol {
                name: name.as_bytes().to_vec(),
                value: off + (i as u64) * 4,
                size: 4,
                kind: SymbolKind::Text,
                scope: SymbolScope::Linkage,
                weak: false,
                section: SymbolSection::Section(text),
                flags: SymbolFlags::None,
            });
        }
        obj.write().unwrap()
    }

    #[test]
    fn elf_with_the_seed_consumer_is_supported() {
        let elf = synth_bootloader_elf(&["main", "bl_nvm_init", SEED_CONSUMER_SYMBOL]);
        assert_eq!(elf_supports_seed(&elf).unwrap(), SeedSupport::Supported);
    }

    #[test]
    fn elf_without_the_seed_consumer_is_not_supported() {
        // What every bootloader release up to v1.6.2 looks like.
        let elf = synth_bootloader_elf(&["main", "bl_nvm_init", "bl_node_id_init_from_nvm"]);
        assert_eq!(elf_supports_seed(&elf).unwrap(), SeedSupport::NotSupported);
    }

    #[test]
    fn a_lookalike_symbol_does_not_count() {
        let elf = synth_bootloader_elf(&[
            "bl_provision_consume_seed_v2",
            "my_bl_provision_consume_seed",
        ]);
        assert_eq!(elf_supports_seed(&elf).unwrap(), SeedSupport::NotSupported);
    }

    #[test]
    fn garbage_elf_is_an_error_not_a_yes() {
        assert!(elf_supports_seed(b"\x7fELF but not really").is_err());
    }
}
