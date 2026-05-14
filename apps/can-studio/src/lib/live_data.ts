// Typed wrappers + display helpers for the streaming live-data
// commands. Mirrors LiveDataRequest / LiveDataStreamEvent /
// SnapshotPayload in `apps/can-studio/src-tauri/src/live_data.rs`.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import type { InterfaceType } from './types';

// ---- Request to backend ----

export interface LiveDataRequest {
    interface: InterfaceType;
    channel: string | null;
    bitrate: number;
    nodeId: number | null;
    timeoutMs: number;
    rateHz: number;
}

// ---- Streamed events from backend ----

export interface SnapshotPayload {
    uptimeMs: number;
    framesRx: number;
    framesTx: number;
    nacksSent: number;
    dtcCount: number;
    lastDtcCode: number;
    flags: number;
    lastOpcode: number;
    lastFlashAddr: number;
    isotpRxProgress: number;
    sessionAgeMs: number;
    sessionActive: boolean;
    validAppPresent: boolean;
    logStreaming: boolean;
    livedataStreaming: boolean;
    wrpProtected: boolean;
}

export type LiveDataEvent =
    | { kind: 'status'; status: 'running' | 'stopped' | 'error'; message: string | null }
    | { kind: 'snapshot'; uptimeMs: number; framesRx: number; framesTx: number; nacksSent: number; dtcCount: number; lastDtcCode: number; flags: number; lastOpcode: number; lastFlashAddr: number; isotpRxProgress: number; sessionAgeMs: number; sessionActive: boolean; validAppPresent: boolean; logStreaming: boolean; livedataStreaming: boolean; wrpProtected: boolean };

// ---- Wrappers ----

export function startLiveData(request: LiveDataRequest): Promise<void> {
    return invoke<void>('live_data_start', { request });
}

export function stopLiveData(): Promise<void> {
    return invoke<void>('live_data_stop');
}

export function onLiveDataEvent(
    handler: (event: LiveDataEvent) => void,
): Promise<UnlistenFn> {
    return listen<LiveDataEvent>('live_data:event', (e) => handler(e.payload));
}
