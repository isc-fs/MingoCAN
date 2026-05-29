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
/**
 * Total frames the AMS emits per 1 Hz scan when armed.
 *
 * Source of truth: `docs/CAN_MAP.md` in IFS08-CE-AMS, which
 * documents 24 cell-V + 25 NTC + 2 diag frames = 51, and confirms
 * with the bus-cost note "51 frames × ~12 bytes-on-wire".
 *
 * The #252 issue body lists more diag frames (balance / boot /
 * crash / firmware-ID) — those are forward-looking; this constant
 * tracks what the firmware *currently* emits. When the AMS team
 * ships additional diag-block frames, bump the Rust
 * `AMS_EXPECTED_FRAMES_PER_SCAN` constant and mirror it here.
 *
 * The view banners a "schema drift suspected" warning if the
 * observed scan rate diverges from this value by more than ±2 —
 * that's the canary for "the spec has changed since this constant
 * was last verified".
 *
 * 53 = 24 cell-V + 25 NTC + 4 diag (FSM 0x6C0, poll 0x6C1, and the
 * two per-IC PEC frames 0x6C7/0x6C8 added by AMS #258).
 */
export const AMS_EXPECTED_FRAMES_PER_SCAN = 53;
/** Monitor ICs in the pack — 2 per module × 5 modules. */
export const AMS_NUM_ICS = 10;

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
          /** 0x6C0 — FSM extended status. */
          kind: 'fsmStatus';
          /** Stringified FSM state from the firmware enum:
           *  "start" | "precharge" | "transition" | "run" | "charge"
           *  | "error" | "unknown(0xNN)". */
          state: string;
          /** Mode lock as a string: "undecided" | "car" | "charger"
           *  | "unknown(0xNN)". */
          modeLocked: string;
          tsms: boolean;
          dashChg: boolean;
          amsOk: boolean;
          pecErrorTotal: number;
          /** Latched-ERROR predicate branch (#276): "none" when
           *  healthy, else "bmsStale" / "cellOverVoltage" / … /
           *  "fsmError" / "unknown(0xNN)". */
          faultReason: string;
          /** Context for faultReason: module index (bmsStale),
           *  module_online_mask (bmsModuleOffline), or 0. */
          faultDetail: number;
      }
    | {
          /** 0x6C1 — V-poll latency + T-sweep failure mask. */
          kind: 'pollTiming';
          lastVPollMs: number;
          worstVPollMs: number;
          tSweepFailMask: number;
      }
    | {
          /** 0x6C7 / 0x6C8 — per-IC PEC error counts. `firstIc` is 0
           *  for 0x6C7 (ICs 0..7) or 8 for 0x6C8 (ICs 8..9). Only the
           *  first `valid` entries of `counts` are real ICs. */
          kind: 'perIcPec';
          firstIc: number;
          valid: number;
          counts: number[];
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
