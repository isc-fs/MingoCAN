// Types + wrappers for the `flash` Tauri command. Mirrors the
// FlashRequest / FlashStreamEvent / JsonReport shapes in
// `apps/can-studio/src-tauri/src/flash.rs`.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import type { InterfaceType } from './types';

// ---- Request to backend ----

export interface FlashRequest {
    artifactPath: string;
    buildCommand: string | null;
    buildCwd: string | null;

    interface: InterfaceType;
    channel: string | null;
    bitrate: number;
    nodeId: number | null;

    timeoutMs: number;
    keepaliveMs: number;

    diff: boolean;
    dryRun: boolean;
    verifyAfter: boolean;
    finalCommit: boolean;
    jump: boolean;
}

export function defaultFlashRequest(
    interfaceType: InterfaceType,
    channel: string,
): FlashRequest {
    return {
        artifactPath: '',
        buildCommand: null,
        buildCwd: null,
        interface: interfaceType,
        channel: channel.length > 0 ? channel : null,
        bitrate: 500_000,
        nodeId: 0x3,
        timeoutMs: 500,
        keepaliveMs: 5_000,
        diff: true,
        dryRun: false,
        verifyAfter: true,
        finalCommit: true,
        jump: true,
    };
}

// ---- Streamed events from backend ----

export type FlashEvent =
    | { kind: 'build_line'; stream: 'stdout' | 'stderr' | 'info'; text: string }
    | { kind: 'build_exited'; code: number | null }
    | { kind: 'planning'; sector: number; role: 'write' | 'skip' }
    | { kind: 'erased'; sector: number }
    | { kind: 'written'; sector: number; bytes: number; total: number }
    | { kind: 'verified'; sector: number; crc: string }
    | { kind: 'committing' }
    | { kind: 'done'; report: JsonReport };

export interface JsonReport {
    sectors_erased: number[];
    sectors_written: number[];
    sectors_skipped: number[];
    crc32: string;
    size: number;
    version: number;
    duration_ms: number;
}

// ---- Wrappers ----

export function runFlash(request: FlashRequest): Promise<JsonReport> {
    return invoke<JsonReport>('flash', { request });
}

export function onFlashEvent(handler: (event: FlashEvent) => void): Promise<UnlistenFn> {
    return listen<FlashEvent>('flash:event', (e) => handler(e.payload));
}
