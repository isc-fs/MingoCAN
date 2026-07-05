// Typed wrappers for the `provision_node_id` Tauri command and
// the role-from-filename inference the Flash tab uses to drive
// the "provision after flash" toggle.
//
// Mirrors `src/cli/provision.rs` in the can-flasher crate and the
// Tauri-side `apps/can-studio/src-tauri/src/provision.rs` so the
// three role registries stay in lockstep. They're short — three
// entries today — and updates land alongside bootloader additions.

import { invoke } from '@tauri-apps/api/core';

import type { InterfaceType } from './types';

export interface Role {
    /** Canonical role name (lowercase): `ecu` | `ams` | `udv`. */
    name: 'ecu' | 'ams' | 'udv';
    /** 4-bit node-id this role maps to. */
    nodeId: number;
}

/** Canonical role → node-id table, shared by the Flash tab's target-role
 *  picker and the artifact-name inference below. Order is the display
 *  order of the role control. */
export const ROLES: ReadonlyArray<Role> = [
    { name: 'ecu', nodeId: 0x01 },
    { name: 'ams', nodeId: 0x02 },
    { name: 'udv', nodeId: 0x03 },
];

const FIRMWARE_EXTS: ReadonlyArray<string> = ['elf', 'hex', 'bin'];

/**
 * Infer a role from a firmware-artifact path or basename. Returns
 * null when the basename stem doesn't match a known role *or* when
 * the extension isn't firmware-shaped — `ams.txt` deliberately
 * does NOT resolve, same rule as the CLI's path-aware
 * `resolve_role_or_path`.
 *
 * Used by the Flash tab to auto-detect whether to offer the
 * provision-after-flash toggle.
 */
export function inferRoleFromArtifact(artifactPath: string): Role | null {
    if (artifactPath.length === 0) return null;
    // Take the basename — segment after the last slash or backslash.
    const sepIdx = Math.max(
        artifactPath.lastIndexOf('/'),
        artifactPath.lastIndexOf('\\'),
    );
    const basename =
        sepIdx >= 0 ? artifactPath.slice(sepIdx + 1) : artifactPath;
    if (basename.length === 0) return null;

    const lastDot = basename.lastIndexOf('.');
    // <= 0 catches both "no extension" and hidden-dotfile-style
    // entries like `.elf` with no body.
    if (lastDot <= 0) return null;

    const ext = basename.slice(lastDot + 1).toLowerCase();
    if (!FIRMWARE_EXTS.includes(ext)) return null;

    const stem = basename.slice(0, lastDot).toLowerCase();
    return ROLES.find((r) => r.name === stem) ?? null;
}

export interface ProvisionRequest {
    /** `ecu` | `ams` | `udv` (case-insensitive). */
    role: string;
    interface: InterfaceType;
    channel: string | null;
    bitrate: number;
    /** Defaults to broadcast (0x3) when null. */
    nodeId: number | null;
    timeoutMs: number;
}

/**
 * Provision the target board's node-id over CAN: writes the
 * `node-id` NVM key + fires `CMD_RESET[Bootloader]`. The chip
 * comes back up with the new node-id resolved from NVM.
 *
 * Fire-and-forget on the reset — the chip reboots before sending
 * an ACK, so the host doesn't wait for one. Verify by running
 * `discover` after the call resolves.
 */
export function provisionNodeId(request: ProvisionRequest): Promise<void> {
    return invoke<void>('provision_node_id', { request });
}
