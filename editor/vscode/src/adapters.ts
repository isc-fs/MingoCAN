// Wrapper around `can-flasher adapters --json`. Flattens the
// per-backend arrays into a single list of `AdapterEntry`s for the
// tree view + adapter picker. The native JSON shape stays keyed by
// backend so the consumer can group / filter; we expose both forms.

import type { Config, InterfaceType } from './config';
import { runJson } from './cli';

// ---- Wire types (mirror src/cli/adapters.rs) ----

export interface AdapterReport {
    slcan: SlcanEntry[];
    socketcan: SocketCanEntry[];
    pcan: PcanEntry[];
    vector: VectorEntry[];
}

interface SlcanEntry {
    channel: string;
    description: string;
    vid?: string;
    pid?: string;
}
interface SocketCanEntry {
    interface: string;
}
interface PcanEntry {
    channel: string;
    channel_byte: string;
}
interface VectorEntry {
    channel: string;
    name: string;
    transceiver: string;
}

// ---- Flattened entry for tree / picker ----

/**
 * One row in the tree's adapter section. `interface + channel`
 * uniquely identifies the adapter from `can-flasher`'s POV — those
 * are exactly the strings that go into `--interface` + `--channel`.
 */
export interface AdapterEntry {
    /** Backend kind — matches `InterfaceType` and `--interface`. */
    interface: InterfaceType;
    /** Channel string — exact value for `--channel`. */
    channel: string;
    /** Operator-facing label (e.g. "VN1610 Channel 1", "CANable 2.0"). */
    label: string;
    /** Optional detail line (USB VID:PID, transceiver name, hex byte). */
    detail?: string;
}

// ---- Fetch ----

export async function fetchAdapters(cfg: Config, cwd: string): Promise<AdapterEntry[]> {
    // `adapters` ignores `--interface` / `--channel`; we still pass
    // through `--json` so the output is machine-parseable.
    const result = await runJson<AdapterReport>(
        cfg.canFlasherPath,
        ['--json', 'adapters'],
        cwd,
    );
    if (result.value === null) {
        return [];
    }
    return flatten(result.value);
}

export function flatten(report: AdapterReport): AdapterEntry[] {
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
 * Return `true` when `entry` corresponds to the currently-active
 * `iscFs.interface` + `iscFs.channel` settings — used by the tree
 * to highlight the active adapter and by the picker to mark the
 * default selection.
 */
export function isActive(entry: AdapterEntry, cfg: Config): boolean {
    return entry.interface === cfg.interface && entry.channel === cfg.channel;
}
