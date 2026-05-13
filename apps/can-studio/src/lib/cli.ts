// Typed wrappers around the Tauri commands exposed by
// `src-tauri/src/lib.rs`. The whole frontend goes through these so
// the command names + payload shapes live in one place; if the
// Rust side renames a command, the call sites get type-checked at
// compile time.

import { invoke } from '@tauri-apps/api/core';

import type {
    AdapterEntry,
    AdapterReport,
    InterfaceType,
} from './types';

// ---- Tauri commands ----

export function getCanFlasherVersion(): Promise<string> {
    return invoke<string>('can_flasher_version');
}

export function discoverAdapters(): Promise<AdapterReport> {
    return invoke<AdapterReport>('discover_adapters');
}

// ---- Pure helpers ----

/**
 * Flatten the per-backend `AdapterReport` into one list of
 * `AdapterEntry` rows the UI iterates over. Mirrors the
 * `flatten()` helper in the VS Code extension's `adapters.ts`.
 */
export function flattenReport(report: AdapterReport): AdapterEntry[] {
    const out: AdapterEntry[] = [];

    for (const entry of report.slcan) {
        out.push({
            interface: 'slcan',
            channel: entry.channel,
            label: entry.description || entry.channel,
            detail:
                entry.vid !== undefined && entry.pid !== undefined
                    ? `USB ${entry.vid}:${entry.pid}`
                    : undefined,
        });
    }

    for (const entry of report.socketcan) {
        out.push({
            interface: 'socketcan',
            channel: entry.interface,
            label: entry.interface,
            detail: 'SocketCAN',
        });
    }

    for (const entry of report.pcan) {
        out.push({
            interface: 'pcan',
            channel: entry.channel,
            label: entry.channel,
            detail: `PCAN-Basic (${entry.channel_byte})`,
        });
    }

    for (const entry of report.vector) {
        out.push({
            interface: 'vector',
            channel: entry.channel,
            label: entry.name || `Vector channel ${entry.channel}`,
            detail: entry.transceiver,
        });
    }

    return out;
}

/**
 * Always-available virtual adapter. The CLI doesn't enumerate it
 * (it's an in-process loopback, not hardware), but the UI offers it
 * as a fallback "no hardware needed" option for smoke tests.
 */
export const VIRTUAL_ADAPTER: AdapterEntry = {
    interface: 'virtual',
    channel: '',
    label: 'Virtual (in-process loopback)',
    detail: 'No hardware — for smoke tests against the bootloader stub',
};

/**
 * Equality check used by the adapter picker to mark the
 * currently-selected entry as active. Interface + channel uniquely
 * identify an adapter from can-flasher's POV.
 */
export function isSameAdapter(
    a: AdapterEntry | null,
    iface: InterfaceType,
    channel: string,
): boolean {
    if (a === null) return false;
    return a.interface === iface && a.channel === channel;
}
