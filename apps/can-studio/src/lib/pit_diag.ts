// Typed wrappers + display helpers for the AMS pit-diag observer
// (Tier 2). Mirrors PitDiagRequest / PitDiagEvent / PitDiagStatus in
// `apps/can-studio/src-tauri/src/pit_diag.rs`.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import type { InterfaceType } from './types';

// ---- Pack geometry ----
// Mirrors the AMS_* constants in `can_flasher::pit_diag`. Hardcoded
// here for the grid layout maths; the values will move to a shared
// types file once VCU/UDV profiles land in slice 5.

export const AMS_NUM_MODULES = 5;
export const AMS_CELLS_PER_MODULE = 19;
export const AMS_NUM_CELLS = AMS_NUM_MODULES * AMS_CELLS_PER_MODULE; // 95
export const AMS_NTC_PER_MODULE = 40;
export const AMS_NUM_NTCS = AMS_NUM_MODULES * AMS_NTC_PER_MODULE; // 200
/** Sentinel value emitted in slots past the last real cell. */
export const AMS_CELLV_SENTINEL = 0xffff;
/** Sentinel value for an unwired / shorted NTC channel (INT8_MIN). */
export const AMS_NTC_SENTINEL = -128;

// ---- Request ----

export interface PitDiagRequest {
    interface: InterfaceType;
    channel: string | null;
    bitrate: number;
    /**
     * Which ECU profile to arm. Slice 1 only supports `"ams"`; field
     * exists from day one so slice 5 doesn't need a wire bump.
     */
    profile: 'ams';
}

// ---- Streamed events ----

/**
 * Lifecycle status on `pit-diag:status`. Lets the UI render
 * armed / waiting / failed without parsing per-frame events.
 */
export type PitDiagStatus =
    | { kind: 'armed'; profile: string }
    | { kind: 'stopped' }
    | { kind: 'error'; message: string };

/**
 * Per-frame event on `pit-diag:frame`. Tagged union — discriminator
 * is `kind`, payload depends on the variant. Mirrors the Rust enum
 * `PitDiagEvent` field-for-field.
 */
export type PitDiagEvent =
    | { kind: 'ack'; enabled: boolean }
    | {
          kind: 'cellVoltage';
          frameIdx: number;
          firstCell: number;
          voltagesMv: [number, number, number, number];
      }
    | {
          kind: 'ntcTemp';
          frameIdx: number;
          firstNtc: number;
          tempsC: [number, number, number, number, number, number, number, number];
      }
    | {
          kind: 'diag';
          id: number;
          data: number[];
          dlc: number;
      };

// ---- Wrappers ----

export function pitDiagEnable(request: PitDiagRequest): Promise<void> {
    return invoke<void>('pit_diag_enable', { request });
}

export function pitDiagDisable(request: PitDiagRequest): Promise<void> {
    return invoke<void>('pit_diag_disable', { request });
}

export function onPitDiagFrame(
    handler: (event: PitDiagEvent) => void,
): Promise<UnlistenFn> {
    return listen<PitDiagEvent>('pit-diag:frame', (e) => handler(e.payload));
}

export function onPitDiagStatus(
    handler: (status: PitDiagStatus) => void,
): Promise<UnlistenFn> {
    return listen<PitDiagStatus>('pit-diag:status', (e) => handler(e.payload));
}

// ---- Display helpers ----

/**
 * Snap a cell's index to its (module, slot-within-module) coords.
 * Module 0 = cells 0..18, module 1 = cells 19..37, etc.
 */
export function cellCoords(cellIdx: number): { module: number; slot: number } {
    return {
        module: Math.floor(cellIdx / AMS_CELLS_PER_MODULE),
        slot: cellIdx % AMS_CELLS_PER_MODULE,
    };
}

/** Same idea for NTCs. */
export function ntcCoords(ntcIdx: number): { module: number; slot: number } {
    return {
        module: Math.floor(ntcIdx / AMS_NTC_PER_MODULE),
        slot: ntcIdx % AMS_NTC_PER_MODULE,
    };
}

/**
 * Pack the four-element voltage tuple from a CellVoltage frame back
 * into the pack-wide array, skipping the sentinel. Returns the
 * cells actually written (1..=4) so the caller can update its
 * count of "frames received this scan".
 */
export function writeCellsInto(
    cells: (number | null)[],
    frame: Extract<PitDiagEvent, { kind: 'cellVoltage' }>,
): number {
    let written = 0;
    for (let i = 0; i < 4; i++) {
        const cellIdx = frame.firstCell + i;
        if (cellIdx >= AMS_NUM_CELLS) break;
        const mv = frame.voltagesMv[i];
        if (mv === AMS_CELLV_SENTINEL) break;
        cells[cellIdx] = mv;
        written += 1;
    }
    return written;
}

/** Same idea for NTCs — no sentinel-by-value, but the unwired
 *  channels report INT8_MIN, which we map to `null` for the UI. */
export function writeNtcsInto(
    ntcs: (number | null)[],
    frame: Extract<PitDiagEvent, { kind: 'ntcTemp' }>,
): number {
    let written = 0;
    for (let i = 0; i < 8; i++) {
        const ntcIdx = frame.firstNtc + i;
        if (ntcIdx >= AMS_NUM_NTCS) break;
        const c = frame.tempsC[i];
        ntcs[ntcIdx] = c === AMS_NTC_SENTINEL ? null : c;
        written += 1;
    }
    return written;
}
