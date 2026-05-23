//! `cf provision <role>` — assign a bootloader's node-id by role name
//! or firmware-artifact path.
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
//! wraps that with a role registry so the operator types either the
//! role name or the firmware path and the host fills in the rest:
//!
//! ```text
//! cf provision ams                   # explicit role
//! cf provision build/ams.elf         # role inferred from filename
//! cf provision ecu --no-reset        # write only, don't reboot
//! ```
//!
//! Filename inference matches the artifact-naming convention the
//! team uses: `<role>.elf` / `<role>.hex` / `<role>.bin` (case-
//! insensitive). Anything that doesn't end in those extensions is
//! treated as a literal role name.
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
    /// Either a role name (`ecu`, `ams`, `udv`, case-insensitive)
    /// or a path to a firmware artifact whose basename matches a
    /// role (e.g. `build/ams.elf`, `../firmware/uDV.HEX`). When a
    /// path is given the role is inferred from the basename stem;
    /// the file isn't opened — only its name matters.
    pub role: String,

    /// Skip the post-write `CMD_RESET [Bootloader]`. Provisioning
    /// normally wants a reset so the new node-id takes effect on
    /// the next boot — only pass `--no-reset` when chaining
    /// multiple writes (the final one should reset).
    #[arg(long, default_value_t = false)]
    pub no_reset: bool,
}

pub async fn run(args: ProvisionArgs, global: &GlobalFlags) -> Result<()> {
    let (node_id, source) = resolve_role_or_path(&args.role).ok_or_else(|| {
        anyhow!(
            "could not resolve {:?} to a role; expected one of: {} (or a path \
             whose basename matches, like `build/ams.elf`)",
            args.role,
            ROLES
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
                .join(", "),
        )
    })?;

    if !global.json {
        // Tell the operator what the host inferred — surprises here
        // (e.g. they typed `cf provision build/ams.elf` and we
        // picked up the wrong role from a parent directory name)
        // are easier to catch when we say it out loud.
        match source {
            RoleSource::Explicit => {
                println!("Provisioning role {:?} → node-id 0x{node_id:02X}", args.role);
            }
            RoleSource::FromPath(role_name) => {
                println!(
                    "Provisioning role {role_name:?} (from {:?}) → node-id 0x{node_id:02X}",
                    args.role
                );
            }
        }
    }

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

/// Firmware-artifact extensions recognised when inferring a role
/// from a path. Matches `swd::flash`'s supported formats.
const FIRMWARE_EXTENSIONS: &[&str] = &["elf", "hex", "bin"];

/// Where the resolved role came from, surfaced to the operator
/// so a misleading filename can't quietly provision the wrong id.
enum RoleSource {
    /// Operator typed the role name directly (e.g. `cf provision ams`).
    Explicit,
    /// Role was inferred from a firmware path's basename stem; the
    /// inner string is the canonical role name we matched.
    FromPath(&'static str),
}

/// Resolve a `cf provision <role>` argument to a node-id.
///
/// Two shapes accepted:
///
///  - **Literal role name** — operator typed `ams`, `ECU`, `udv` etc.
///    Resolved against [`ROLES`] case-insensitively.
///  - **Firmware-artifact path** — operator typed `build/ams.elf`,
///    `../firmware/uDV.HEX`, `./out/ECU.bin` etc. The basename's
///    stem is taken and resolved against [`ROLES`]; the path itself
///    isn't opened.
///
/// A value is treated as a path when it either contains a path
/// separator (`/` or `\`) **or** has a firmware-style extension.
/// Bare strings without a separator or a matching extension fall
/// through to the literal-name branch; the registry rejects
/// non-matches with a clear error.
fn resolve_role_or_path(raw: &str) -> Option<(u8, RoleSource)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if looks_like_path(trimmed) {
        let stem = path_stem(trimmed)?;
        let (alias, id) = lookup_role(stem)?;
        return Some((id, RoleSource::FromPath(alias)));
    }
    let (_, id) = lookup_role(trimmed)?;
    Some((id, RoleSource::Explicit))
}

fn lookup_role(name: &str) -> Option<(&'static str, u8)> {
    for (alias, id) in ROLES {
        if alias.eq_ignore_ascii_case(name) {
            return Some((*alias, *id));
        }
    }
    None
}

fn looks_like_path(raw: &str) -> bool {
    if raw.contains('/') || raw.contains('\\') {
        return true;
    }
    raw.rsplit_once('.')
        .map(|(_, ext)| {
            FIRMWARE_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(ext))
        })
        .unwrap_or(false)
}

/// Extract the basename stem from a path-like string. Returns
/// `None` for paths whose basename has no stem (e.g. trailing
/// slash, or a hidden file with no body like `.elf`).
fn path_stem(raw: &str) -> Option<&str> {
    // Take the segment after the last path separator (either
    // slash works on both POSIX and Windows for our purposes —
    // we're parsing a string the operator typed, not walking the
    // filesystem).
    let basename = raw
        .rsplit(|c: char| c == '/' || c == '\\')
        .next()
        .filter(|s| !s.is_empty())?;

    // Strip the firmware extension if we recognise it. We don't
    // strip arbitrary extensions because a role name like `ams.fw`
    // shouldn't accidentally match `ams`.
    if let Some((stem, ext)) = basename.rsplit_once('.') {
        if FIRMWARE_EXTENSIONS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(ext))
            && !stem.is_empty()
        {
            return Some(stem);
        }
    }
    Some(basename)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_of(raw: &str) -> Option<u8> {
        resolve_role_or_path(raw).map(|(id, _)| id)
    }

    #[test]
    fn resolves_ecu_ams_udv() {
        assert_eq!(id_of("ecu"), Some(0x01));
        assert_eq!(id_of("ams"), Some(0x02));
        assert_eq!(id_of("udv"), Some(0x03));
    }

    #[test]
    fn role_lookup_is_case_insensitive() {
        // Operators paste from team docs / GitHub issues where role
        // names land in all sorts of casing. Accept anything that
        // matches ASCII-insensitive.
        assert_eq!(id_of("AMS"), Some(0x02));
        assert_eq!(id_of("uDV"), Some(0x03));
        assert_eq!(id_of("Ecu"), Some(0x01));
    }

    #[test]
    fn whitespace_is_trimmed() {
        assert_eq!(id_of("  ams  "), Some(0x02));
    }

    #[test]
    fn unknown_roles_return_none() {
        // Typos and not-yet-added boards land here. The caller
        // surfaces the expected-roles list in the error message.
        assert_eq!(id_of("master"), None);
        assert_eq!(id_of("ecu-1"), None);
        assert_eq!(id_of(""), None);
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
            assert!(
                *id < 0xF,
                "role {name:?} = 0x{id:02X} would collide with broadcast 0xF"
            );
        }
    }

    // ---- Filename inference ----

    #[test]
    fn bare_filename_with_role_extension_resolves() {
        // The motivating case: team builds produce `ams.elf` /
        // `ecu.elf` / `uDV.elf` next to each project. Operator
        // can paste the filename verbatim.
        assert_eq!(id_of("ams.elf"), Some(0x02));
        assert_eq!(id_of("ecu.hex"), Some(0x01));
        assert_eq!(id_of("uDV.bin"), Some(0x03));
    }

    #[test]
    fn path_with_role_extension_resolves() {
        // Real-world paste from a build dir.
        assert_eq!(id_of("build/ams.elf"), Some(0x02));
        assert_eq!(id_of("./build/ecu.hex"), Some(0x01));
        assert_eq!(id_of("../firmware/Debug/uDV.bin"), Some(0x03));
        // Windows-style separator too — operators on Windows
        // copy paths from Explorer.
        assert_eq!(id_of("build\\AMS.ELF"), Some(0x02));
    }

    #[test]
    fn path_inference_is_case_insensitive_on_both_stem_and_extension() {
        assert_eq!(id_of("build/AMS.ELF"), Some(0x02));
        assert_eq!(id_of("build/Ecu.Hex"), Some(0x01));
    }

    #[test]
    fn explicit_vs_inferred_source_is_tracked() {
        // The CLI uses the RoleSource variant to print "from
        // <path>" so the operator sees what the host inferred.
        let (_, src) = resolve_role_or_path("ams").unwrap();
        assert!(matches!(src, RoleSource::Explicit));

        let (_, src) = resolve_role_or_path("build/ams.elf").unwrap();
        assert!(matches!(src, RoleSource::FromPath("ams")));
    }

    #[test]
    fn unknown_filename_stems_return_none() {
        // A path that doesn't match a known role — fail loudly so
        // the operator sees what they typed, not a default.
        assert_eq!(id_of("build/master.elf"), None);
        assert_eq!(id_of("./output/firmware.bin"), None);
    }

    #[test]
    fn non_firmware_extensions_dont_strip() {
        // `ams.txt` should NOT resolve via filename inference —
        // we only treat .elf / .hex / .bin as firmware-shaped.
        // The bare string after the dot ("txt") isn't a role
        // either, so the whole thing is unresolvable.
        assert_eq!(id_of("ams.txt"), None);
        assert_eq!(id_of("ams.fw"), None);
    }

    #[test]
    fn path_segments_dont_leak_through() {
        // `firmware/ams/main.elf` should infer from `main`, not
        // `ams`. `main` isn't a role → None. Prevents a parent
        // directory name from silently picking the wrong role.
        assert_eq!(id_of("firmware/ams/main.elf"), None);
    }

    #[test]
    fn hidden_dotfile_with_no_stem_returns_none() {
        // Just `.elf` with no body — pathological case, no
        // basename stem to match against.
        assert_eq!(id_of(".elf"), None);
    }
}
