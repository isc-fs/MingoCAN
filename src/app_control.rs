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
//! the AMS and the ECU sit on the same physical bus and both listen on
//! `0x002`, so the payload is what selects WHICH node reboots. Sending
//! the AMS payload to the ECU is silently ignored (exact `memcmp`) —
//! that mismatch was the intermittent CONNECT timeout of IFS08-CE-ECU#107.
//!
//! Sources of truth: IFS08-CE-AMS `ams_config.hpp` (`BlBootReqPayload`
//! = ..AD 0x11) and IFS08-CE-ECU `ecu_config.hpp`
//! (`BlBootTriggerPayload` = ..AD 0x12), each with its own
//! `bootloader.hpp::matches_trigger`.

/// 11-bit CAN ID the application listens on for the reboot-to-BL
/// trigger. Very high arbitration priority; same for AMS / ECU / uDV.
pub const REBOOT_TO_BL_ID: u16 = 0x002;

/// Per-node reboot-to-BL payloads. A magic so a stray same-ID frame
/// can't reset the car — the firmware `memcmp`s all four bytes, and
/// each node matches only its own value (selective reboot on the
/// shared bus).
pub const REBOOT_TO_BL_PAYLOAD_AMS: [u8; 4] = [0xB0, 0x07, 0xAD, 0x11];
pub const REBOOT_TO_BL_PAYLOAD_ECU: [u8; 4] = [0xB0, 0x07, 0xAD, 0x12];

/// The reboot-to-BL payload for a target node: ECU (0x1) has its own
/// magic; the AMS (0x2) keeps the historical one, which is also the
/// fallback for nodes whose app-level trigger is not yet defined
/// (uDV 0x3 — extend this table when its app implements the trigger).
pub fn reboot_to_bl_payload(target_node: u8) -> [u8; 4] {
    match target_node {
        0x1 => REBOOT_TO_BL_PAYLOAD_ECU,
        _ => REBOOT_TO_BL_PAYLOAD_AMS,
    }
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
        assert_eq!(reboot_to_bl_payload(0x1), [0xB0, 0x07, 0xAD, 0x12]);
        assert_eq!(reboot_to_bl_payload(0x2), [0xB0, 0x07, 0xAD, 0x11]);
        assert_ne!(reboot_to_bl_payload(0x1), reboot_to_bl_payload(0x2));
        // Unknown nodes fall back to the historical payload.
        assert_eq!(reboot_to_bl_payload(0x3), REBOOT_TO_BL_PAYLOAD_AMS);
    }
}
