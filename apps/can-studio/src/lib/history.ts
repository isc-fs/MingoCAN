// Flash history (#651) — the per-user flash log the CLI and this app
// both append to (`can_flasher::audit`). One entry per action that
// wrote to a board.

import { invoke } from '@tauri-apps/api/core';

/** Mirrors `can_flasher::audit::Entry` (camelCase). */
export interface HistoryEntry {
    v: number;
    /** UTC, RFC 3339. */
    ts: string;
    operator?: string;
    host?: string;
    /** `cli` or `app`. */
    tool: string;
    toolVersion: string;
    /** `flash`, `swd-flash`, `swd-erase`, `dfu-flash`, `provision`,
     *  `ob-apply-wrp`, `nvm-write`, `nvm-erase`, `nvm-format`, `clear-dtc`. */
    action: string;
    nodeId?: number;
    /** `ecu` / `ams` / `udv`. */
    role?: string;
    interface?: string;
    channel?: string;
    deviceSerial?: string;
    artifact?: string;
    crc32?: string;
    sizeBytes?: number;
    fwProduct?: string;
    fwVersion?: string;
    fwGitHash?: string;
    verified?: boolean;
    dryRun?: boolean;
    details?: Record<string, unknown>;
    ok: boolean;
    error?: string;
    exitCode?: number;
}

export interface History {
    /** Where the log file lives (may not exist yet). */
    path: string | null;
    /** Newest first. */
    entries: HistoryEntry[];
}

export function readHistory(): Promise<History> {
    return invoke<History>('history_read');
}

/** Write `entries` as CSV to `path`; resolves to the row count. */
export function exportHistoryCsv(path: string, entries: HistoryEntry[]): Promise<number> {
    return invoke<number>('history_export_csv', { path, entries });
}

const ACTION_LABELS: Record<string, string> = {
    flash: 'Flash app',
    'swd-flash': 'Burn bootloader (SWD)',
    'dfu-flash': 'Burn bootloader (USB)',
    'swd-erase': 'Erase chip (SWD)',
    provision: 'Set node-id',
    'clear-dtc': 'Clear DTCs',
    'ob-apply-wrp': 'Apply write protection',
    'nvm-write': 'NVM write',
    'nvm-erase': 'NVM erase',
    'nvm-format': 'NVM format',
};

export function actionLabel(action: string): string {
    return ACTION_LABELS[action] ?? action;
}

/** `AMS (0x2)`, `node 0x5`, or `—`. */
export function boardLabel(e: HistoryEntry): string {
    if (e.nodeId === undefined) return '—';
    const hex = `0x${e.nodeId.toString(16).toUpperCase()}`;
    return e.role ? `${e.role === 'udv' ? 'uDV' : e.role.toUpperCase()} (${hex})` : `node ${hex}`;
}

/** File name of the artifact, without its folder. */
export function artifactName(e: HistoryEntry): string {
    if (!e.artifact) return '';
    const parts = e.artifact.split(/[\\/]/);
    return parts[parts.length - 1] ?? e.artifact;
}

/** `ams v1.4.2 · 3f2a9c1` from the image's firmware-info record. */
export function firmwareLabel(e: HistoryEntry): string {
    const bits: string[] = [];
    if (e.fwProduct) bits.push(e.fwProduct);
    if (e.fwVersion) bits.push(`v${e.fwVersion}`);
    if (e.fwGitHash) bits.push(e.fwGitHash.slice(0, 7));
    return bits.join(' ');
}
