//! Provisioning seed — the host↔bootloader contract for seeding a
//! board's node-id over **SWD**, during the same step that burns the
//! bootloader, with no CAN round-trip.
//!
//! The node-id normally lives in the bootloader's log-structured KV
//! NVM (sector 7), whose record format is private to `bl_nvm` and
//! evolves. Rather than teach the host that layout, the SWD tool
//! writes a trivially-simple fixed record at a reserved flash address;
//! the bootloader reads it on first boot and translates it into a
//! proper NVM entry via its own `bl_nvm_write`. One-shot: the BL only
//! acts on the seed when its NVM has no node-id yet.
//!
//! **Requires bootloader support** — tracked in
//! <https://github.com/isc-fs/stm32-can-bootloader/issues/183>. On a
//! bootloader without seed support the written word is inert (it sits
//! in erased sector-7 space the BL ignores). The constants here MUST
//! match whatever that issue settles on.

/// Reserved flash address for the seed — one STM32H7 256-bit
/// flashword just below the app-metadata word (`0x080FFFE0`), at the
/// top of the NVM sector. 32-byte aligned.
pub const SEED_ADDR: u64 = 0x080F_FFC0;

/// Magic marking a valid seed (vs. erased `0xFFFFFFFF` flash or
/// garbage). Little-endian in the record.
pub const SEED_MAGIC: u32 = 0xB007_0D1D;

/// Size of the seed record — a full H7 flashword (write-once between
/// erases, so we program the whole word at once).
pub const SEED_LEN: usize = 32;

/// Largest assignable node-id (0xF is the broadcast/host-reserved ID;
/// 0x0 is the host). Real boards are `1..=0xE`.
const MAX_NODE_ID: u8 = 0x0E;

/// Build the 32-byte seed flashword for `node_id`.
///
/// Layout: `magic(4, LE) | node_id(1) | !node_id(1) | 0xFF…padding`.
/// The 32-bit magic plus the complement byte are the integrity check —
/// no CRC needed for a 2-byte payload behind a 4-byte magic.
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
    Ok(rec)
}

/// Read back the node-id a seed record encodes, validating the magic +
/// complement. `None` when the bytes aren't a valid seed (erased
/// flash, wrong magic, failed complement, out-of-range id). Mirrors
/// the check the bootloader performs at boot — handy for a post-write
/// readback verify.
pub fn parse_seed_record(rec: &[u8]) -> Option<u8> {
    if rec.len() < 6 {
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
    Some(node_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_then_parse_roundtrips() {
        for id in 1..=MAX_NODE_ID {
            let rec = build_seed_record(id).unwrap();
            assert_eq!(rec.len(), SEED_LEN);
            assert_eq!(parse_seed_record(&rec), Some(id));
        }
    }

    #[test]
    fn magic_is_little_endian_first() {
        let rec = build_seed_record(0x02).unwrap();
        assert_eq!(&rec[0..4], &SEED_MAGIC.to_le_bytes());
        assert_eq!(rec[4], 0x02);
        assert_eq!(rec[5], 0xFD); // !0x02
        assert!(rec[6..].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn rejects_reserved_ids() {
        assert!(build_seed_record(0x0).is_err());
        assert!(build_seed_record(0xF).is_err());
        assert!(build_seed_record(0x10).is_err());
    }

    #[test]
    fn parse_rejects_erased_and_corrupt() {
        assert_eq!(parse_seed_record(&[0xFF; SEED_LEN]), None); // erased
        let mut rec = build_seed_record(0x03).unwrap();
        rec[5] = 0x00; // break the complement
        assert_eq!(parse_seed_record(&rec), None);
        rec[5] = !0x03;
        rec[0] ^= 0x01; // break the magic
        assert_eq!(parse_seed_record(&rec), None);
    }
}
