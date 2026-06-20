//! Application-level CAN conventions the host speaks to *running*
//! firmware — distinct from the bootloader wire protocol. Today this
//! is the reboot-to-bootloader trigger.
//!
//! A running ECU application doesn't speak the bootloader protocol,
//! so getting it into the bootloader for a flash needs a frame the
//! *application* firmware listens for. On an exact payload match the
//! app opens its HV relays and resets into the CAN bootloader. The
//! AMS, ECU, and uDV share this convention.
//!
//! Source of truth: IFS08-CE-AMS `Core/Inc/app/ams_config.hpp`
//! (`BlBootReqCanId` / `BlBootReqPayload` / `BlBootReqDlc`) and
//! `Core/Inc/app/bootloader.hpp::matches_trigger`.

/// 11-bit CAN ID the application listens on for the reboot-to-BL
/// trigger. Very high arbitration priority; same for AMS / ECU / uDV.
pub const REBOOT_TO_BL_ID: u16 = 0x002;

/// Exact 4-byte payload that must match for the app to reboot into the
/// bootloader. A magic so a stray same-ID frame can't reset the car —
/// the firmware `memcmp`s all four bytes.
pub const REBOOT_TO_BL_PAYLOAD: [u8; 4] = [0xB0, 0x07, 0xAD, 0x11];

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
