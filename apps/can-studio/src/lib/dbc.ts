// Typed wrappers for the DBC backend commands. Mirrors the shapes
// in `apps/can-studio/src-tauri/src/dbc.rs`.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

// ---- Schema / status ----

export interface DbcSummary {
    path: string;
    messageCount: number;
    signalCount: number;
}

/**
 * One row of the flat signal schema. `signalKey` is a stable
 * `${messageId}|${signalName}` tuple that matches the key on
 * decoded values streaming via `bus_monitor:signals`.
 */
export interface SignalSchema {
    signalKey: string;
    messageId: number;
    messageName: string;
    signalName: string;
    unit: string;
    factor: number;
    offset: number;
    min: number;
    max: number;
}

export type DbcStatusEvent =
    | { kind: 'loaded'; path: string; messageCount: number; signalCount: number }
    | { kind: 'unloaded' }
    | { kind: 'error'; message: string };

// ---- Live decoded values ----

export interface DecodedSignal {
    signalKey: string;
    value: number;
}

// ---- Wrappers ----

export function loadDbc(path: string): Promise<DbcSummary> {
    return invoke<DbcSummary>('dbc_load', { request: { path } });
}

export function unloadDbc(): Promise<void> {
    return invoke<void>('dbc_unload');
}

export function getDbcStatus(): Promise<DbcSummary | null> {
    return invoke<DbcSummary | null>('dbc_status');
}

export function getDbcSignals(): Promise<SignalSchema[]> {
    return invoke<SignalSchema[]>('dbc_signals');
}

export function onDbcStatus(
    handler: (event: DbcStatusEvent) => void,
): Promise<UnlistenFn> {
    return listen<DbcStatusEvent>('dbc:status', (e) => handler(e.payload));
}

/**
 * Live decoded-signal stream emitted by the bus monitor when a
 * DBC is loaded. One event per matching frame, carrying every
 * signal in that frame's message.
 */
export function onDecodedSignals(
    handler: (signals: DecodedSignal[]) => void,
): Promise<UnlistenFn> {
    return listen<DecodedSignal[]>('bus_monitor:signals', (e) => handler(e.payload));
}
