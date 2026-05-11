// Wrapper around `can-flasher discover --json`. Returns the list of
// bootloader-mode devices currently visible on whichever bus the
// active adapter speaks to.

import type { Config } from './config';
import { buildGlobalArgv } from './config';
import { runJson } from './cli';

// ---- Wire types (mirror src/cli/discover.rs::DiscoverRow) ----

export interface DiscoverRow {
    node_id: number;
    proto_major: number;
    proto_minor: number;

    fw_version?: string;
    git_hash?: string;
    product?: string;
    fw_error?: string;

    wrp_protected?: boolean;
    reset_cause?: string;
    health_error?: string;
}

// ---- Fetch ----

export async function fetchDevices(cfg: Config, cwd: string): Promise<DiscoverRow[]> {
    // Discover doesn't need a real session — the CLI handles the
    // broadcast + enrichment internally. We pass through the
    // configured adapter settings + a generous discover-window.
    const argv = [...buildGlobalArgv(cfg), 'discover', '--timeout-ms', '500'];
    const result = await runJson<DiscoverRow[]>(cfg.canFlasherPath, argv, cwd);
    return result.value ?? [];
}

// ---- Display helpers ----

/** `"0x3"` for the standard 4-bit node-id representation. */
export function formatNodeId(id: number): string {
    return `0x${id.toString(16).toUpperCase()}`;
}

/** `"v1.2.0"` or empty string if no firmware-info record was readable. */
export function formatFwVersion(row: DiscoverRow): string {
    if (row.fw_version !== undefined && row.fw_version.length > 0) {
        return `v${row.fw_version}`;
    }
    return '';
}

/** One-line summary suitable for a tree-item description / tooltip. */
export function formatRowDetail(row: DiscoverRow): string {
    const parts: string[] = [];
    if (row.product !== undefined) {
        parts.push(row.product);
    }
    const fw = formatFwVersion(row);
    if (fw.length > 0) {
        parts.push(fw);
    }
    if (row.fw_error !== undefined) {
        parts.push(`fw: ${row.fw_error}`);
    }
    if (row.wrp_protected === true) {
        parts.push('WRP');
    }
    if (row.health_error !== undefined) {
        parts.push(`health: ${row.health_error}`);
    }
    return parts.join(' · ');
}
