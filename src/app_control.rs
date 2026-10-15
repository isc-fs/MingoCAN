//! Application-level CAN conventions the host speaks to *running*
//! firmware — distinct from the bootloader wire protocol. Today this
//! is the reboot-to-bootloader trigger.
//!
//! A running application doesn't speak the bootloader protocol, so
//! getting it into the bootloader for a flash needs a frame the
//! *application* firmware listens for. On an exact payload match the
//! app resets into the CAN bootloader (the AMS additionally opens its
//! HV relays first).
//!
//! The trigger ID is shared, but the 4-byte payload is **per node**:
//! the ECU, the AMS and the uDV share the ACU bus and all listen on
//! `0x002`, so the payload is what selects WHICH node reboots. Sending
//! the AMS payload to the ECU is silently ignored (exact `memcmp`) —
//! that mismatch was the intermittent CONNECT timeout of IFS08-CE-ECU#107.
//! Worse, a node-id with no entry used to fall back to the AMS payload,
//! so "reboot the uDV" rebooted the AMS instead. A node without a known
//! payload is now refused ([`reboot_to_bl_payload`] returns `None`).
//!
//! Sources of truth (the last byte is NOT `0x10 | node-id` — the AMS
//! kept its historical `..11` — so each value is copied from firmware):
//! - ECU, node 0x1: IFS08-CE-ECU `ecu_config.hpp` `BlBootTriggerPayload`
//!   = `B0 07 AD 12`
//! - AMS, node 0x2: IFS08-CE-AMS `ams_config.hpp` `BlBootReqPayload`
//!   = `B0 07 AD 11` (FDCAN1 on the AMS side)
//! - uDV, node 0x3: IFS09-DV-uDV `bl_entry.h` `BL_ENTRY_PAYLOAD_*`
//!   = `B0 07 AD 13` (FDCAN2; honoured only in AS OFF)

/// 11-bit CAN ID the application listens on for the reboot-to-BL
/// trigger. Very high arbitration priority; same for AMS / ECU / uDV.
pub const REBOOT_TO_BL_ID: u16 = 0x002;

/// Per-node reboot-to-BL payloads. A magic so a stray same-ID frame
/// can't reset the car — the firmware `memcmp`s all four bytes, and
/// each node matches only its own value (selective reboot on the
/// shared bus).
pub const REBOOT_TO_BL_PAYLOAD_ECU: [u8; 4] = [0xB0, 0x07, 0xAD, 0x12];
pub const REBOOT_TO_BL_PAYLOAD_AMS: [u8; 4] = [0xB0, 0x07, 0xAD, 0x11];
pub const REBOOT_TO_BL_PAYLOAD_UDV: [u8; 4] = [0xB0, 0x07, 0xAD, 0x13];

/// Every node whose application implements the reboot-to-BL trigger,
/// as `(node_id, board, payload)`. The single table behind
/// [`reboot_to_bl_payload`] and the "known nodes" list in its error.
pub const REBOOT_TO_BL_PAYLOADS: [(u8, &str, [u8; 4]); 3] = [
    (0x1, "ECU", REBOOT_TO_BL_PAYLOAD_ECU),
    (0x2, "AMS", REBOOT_TO_BL_PAYLOAD_AMS),
    (0x3, "uDV", REBOOT_TO_BL_PAYLOAD_UDV),
];

/// The reboot-to-BL payload for a target node, or `None` when that
/// node has no known app-level trigger.
///
/// There is deliberately **no fallback**: on the shared bus another
/// board's payload reboots that other board, so an unknown node must be
/// refused rather than guessed. Add the node to
/// [`REBOOT_TO_BL_PAYLOADS`] once its firmware implements the trigger.
pub fn reboot_to_bl_payload(target_node: u8) -> Option<[u8; 4]> {
    REBOOT_TO_BL_PAYLOADS
        .iter()
        .find(|(node, _, _)| *node == target_node)
        .map(|(_, _, payload)| *payload)
}

/// Human-readable list of the known nodes and their payloads, e.g.
/// `0x1 ECU (B0 07 AD 12), 0x2 AMS (B0 07 AD 11), …`. Used in the
/// refusal message so the operator sees what *is* supported.
pub fn known_reboot_payloads() -> String {
    REBOOT_TO_BL_PAYLOADS
        .iter()
        .map(|(node, board, p)| {
            format!(
                "0x{node:X} {board} ({:02X} {:02X} {:02X} {:02X})",
                p[0], p[1], p[2], p[3]
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Default total window to poll a target into the bootloader after the
/// reboot trigger, for `Session::connect_entering_bootloader`. Sized to
/// cover a board's reset-into-BL time plus several retries (a running
/// app reboots in well under a second; the extra headroom absorbs
/// timing jitter and the odd dropped frame on a busy bus).
pub const DEFAULT_BL_ENTRY_WINDOW: std::time::Duration = std::time::Duration::from_secs(8);

/// How the flash flow should get a target into the bootloader before
/// the `CMD_CONNECT` handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum BootloaderEntry {
    /// Never send the reboot trigger — CONNECT and fail (exit 4) if
    /// the target isn't already running the bootloader.
    Never,
    /// Try CONNECT first; only if it times out, send the reboot
    /// trigger, wait for the bootloader to come up, and retry CONNECT
    /// once. The default — no extra reset when the board is already in
    /// the bootloader.
    #[default]
    Auto,
    /// Always send the reboot trigger before CONNECT, even if the
    /// target might already be in the bootloader.
    Always,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ECU#107 regression: the ECU (node 0x1) and AMS (node 0x2) payloads are
    /// DIFFERENT by design (selective reboot on the shared bus). Sending the
    /// AMS payload to the ECU is silently ignored by its firmware.
    #[test]
    fn reboot_payload_is_per_node() {
        assert_eq!(reboot_to_bl_payload(0x1), Some([0xB0, 0x07, 0xAD, 0x12]));
        assert_eq!(reboot_to_bl_payload(0x2), Some([0xB0, 0x07, 0xAD, 0x11]));
        assert_eq!(reboot_to_bl_payload(0x3), Some([0xB0, 0x07, 0xAD, 0x13]));
    }

    /// The uDV used to fall back to the AMS payload, which on the shared
    /// bus reboots the AMS instead. Every known payload must be unique.
    #[test]
    fn reboot_payloads_are_pairwise_distinct() {
        for (i, (na, _, pa)) in REBOOT_TO_BL_PAYLOADS.iter().enumerate() {
            for (nb, _, pb) in &REBOOT_TO_BL_PAYLOADS[i + 1..] {
                assert_ne!(na, nb, "duplicate node id in the table");
                assert_ne!(pa, pb, "nodes 0x{na:X} and 0x{nb:X} share a payload");
            }
        }
    }

    /// No fallback: a node with no known trigger gets `None`, never
    /// another board's payload.
    #[test]
    fn unknown_node_has_no_payload() {
        for node in [0x0_u8, 0x4, 0x7, 0xF] {
            assert_eq!(reboot_to_bl_payload(node), None, "node 0x{node:X}");
        }
    }

    #[test]
    fn known_payloads_lists_every_node() {
        let s = known_reboot_payloads();
        assert!(s.contains("0x1 ECU (B0 07 AD 12)"), "{s}");
        assert!(s.contains("0x2 AMS (B0 07 AD 11)"), "{s}");
        assert!(s.contains("0x3 uDV (B0 07 AD 13)"), "{s}");
    }
}
