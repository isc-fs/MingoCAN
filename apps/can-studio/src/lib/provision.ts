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

/** Canonical role → node-id table for the Flash tab's target-role picker. Order is the display order of the role control. */
export const ROLES: ReadonlyArray<Role> = [
    { name: 'ecu', nodeId: 0x01 },
    { name: 'ams', nodeId: 0x02 },
    { name: 'udv', nodeId: 0x03 },
];

export interface ProvisionRequest {
    /** `ecu` | `ams` | `udv` (case-insensitive). */
    role: string;
    interface: InterfaceType;
    channel: string | null;
    bitrate: number;
    /** Falls back to 0x3 (uDV) when null. */
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
