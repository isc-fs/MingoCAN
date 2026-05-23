//! `cf provision <role>` — assign a bootloader's node-id by role name.
//!
//! The team's three boards on a shared CAN bus each get a known
//! 4-bit node-id: ECU=1, AMS=2, uDV=3. Without this subcommand the
//! operator has to remember those numbers and run:
//!
//! ```text
//! cf --node-id 0xF config nvm write node-id 0x02 --reset
//! ```
//!
//! …which is fine when you remember "AMS = 2" but exactly the kind
//! of magic-number footgun that loses a bench afternoon. `provision`
//! wraps that with a role registry so the operator types the role
//! by name and the host fills in the rest:
//!
//! ```text
//! cf provision ams           # writes node-id key=0x0001 value=0x02 + resets
//! cf provision ecu --no-reset  # write only, don't reboot
//! ```
//!
//! Targeting the right board on a shared bus is the operator's job
//! — pass `--node-id 0xF` (broadcast, default) for a freshly-flashed
//! chip, or the existing node-id when re-provisioning a board that
//! already has one. The subcommand inherits the global `--node-id`
//! / `--interface` / `--channel` flags so the existing playbook for
//! "talk to this board" carries over unchanged.

use anyhow::{anyhow, Result};
use clap::Args;

use crate::cli::config::run_nvm_write;
use crate::cli::GlobalFlags;

#[derive(Debug, Args)]
pub struct ProvisionArgs {
    /// Role for the target board. Maps to the canonical node-id
    /// number. Accepts `ecu`, `ams`, `udv` (case-insensitive).
    pub role: String,

    /// Skip the post-write `CMD_RESET [Bootloader]`. Provisioning
    /// normally wants a reset so the new node-id takes effect on
    /// the next boot — only pass `--no-reset` when chaining
    /// multiple writes (the final one should reset).
    #[arg(long, default_value_t = false)]
    pub no_reset: bool,
}

pub async fn run(args: ProvisionArgs, global: &GlobalFlags) -> Result<()> {
    let node_id = resolve_role(&args.role).ok_or_else(|| {
        anyhow!(
            "unknown role {:?}; expected one of: {}",
            args.role,
            ROLES
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
                .join(", "),
        )
    })?;

    // Reuse run_nvm_write so the wire-protocol path is shared with
    // `cf config nvm write` — same connect → write → (optional)
    // reset → disconnect dance, same error mapping, same JSON
    // output shape. Round-tripping the byte as a `"0x.."` string
    // costs one parse but keeps the call site honest.
    run_nvm_write(
        BL_NVM_KEY_NODE_ID,
        format!("0x{node_id:02X}"),
        !args.no_reset,
        global,
    )
    .await
}

/// The host's name for the bootloader's `BL_NVM_KEY_NODE_ID`.
const BL_NVM_KEY_NODE_ID: u16 = 0x0001;

/// Role → node-id registry. Update this when the team adds a new
/// board to the shared CAN bus.
///
/// Names are kebab-friendly (lowercase, ASCII letters / digits)
/// because operators type them at the shell. `resolve_role`
/// compares case-insensitively so `AMS` and `ams` both resolve.
const ROLES: &[(&str, u8)] = &[
    ("ecu", 0x01),
    ("ams", 0x02),
    ("udv", 0x03),
];

fn resolve_role(raw: &str) -> Option<u8> {
    let trimmed = raw.trim();
    for (alias, id) in ROLES {
        if alias.eq_ignore_ascii_case(trimmed) {
            return Some(*id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_ecu_ams_udv() {
        assert_eq!(resolve_role("ecu"), Some(0x01));
        assert_eq!(resolve_role("ams"), Some(0x02));
        assert_eq!(resolve_role("udv"), Some(0x03));
    }

    #[test]
    fn role_lookup_is_case_insensitive() {
        // Operators paste from team docs / GitHub issues where role
        // names land in all sorts of casing. Accept anything that
        // matches ASCII-insensitive.
        assert_eq!(resolve_role("AMS"), Some(0x02));
        assert_eq!(resolve_role("uDV"), Some(0x03));
        assert_eq!(resolve_role("Ecu"), Some(0x01));
    }

    #[test]
    fn whitespace_is_trimmed() {
        assert_eq!(resolve_role("  ams  "), Some(0x02));
    }

    #[test]
    fn unknown_roles_return_none() {
        // Typos and not-yet-added boards land here. The caller
        // surfaces the expected-roles list in the error message.
        assert_eq!(resolve_role("master"), None);
        assert_eq!(resolve_role("ecu-1"), None);
        assert_eq!(resolve_role(""), None);
    }

    #[test]
    fn role_ids_are_unique() {
        // Defence against future entries colliding on the same
        // node-id — would silently confuse the bus.
        for (i, (_, a)) in ROLES.iter().enumerate() {
            for (_, b) in &ROLES[i + 1..] {
                assert_ne!(a, b, "node-id 0x{a:02X} mapped to two roles");
            }
        }
    }

    #[test]
    fn role_ids_fit_in_4_bits() {
        // The BL node-id field is 4 bits (0x0..0xF). 0xF is
        // reserved for broadcast — don't accidentally provision a
        // board to the broadcast slot.
        for (name, id) in ROLES {
            assert!(*id < 0xF, "role {name:?} = 0x{id:02X} would collide with broadcast 0xF");
        }
    }
}
