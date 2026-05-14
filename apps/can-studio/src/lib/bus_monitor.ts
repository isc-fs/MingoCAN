// Typed wrappers + display helpers for the generic CAN bus
// monitor (Tier 1). Mirrors BusMonitorRequest / BusMonitorFrame /
// BusMonitorStatus in `apps/can-studio/src-tauri/src/bus_monitor.rs`.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import type { InterfaceType } from './types';

// ---- Request ----

export interface BusMonitorRequest {
    interface: InterfaceType;
    channel: string | null;
    bitrate: number;
    /**
     * Per-recv timeout in the backend's poll loop. Shorter values
     * lower stop-signal latency at the cost of more idle wakeups.
     * 50ms is a reasonable default.
     */
    pollTimeoutMs: number;
}

// ---- Streamed events ----

/**
 * Per-frame event emitted on `bus_monitor:frame`. `tsMs` is
 * milliseconds since the monitor session started, not Unix time.
 * `data` is always an 8-byte array; the frontend should slice to
 * `dlc` bytes for display.
 */
export interface BusMonitorFrame {
    tsMs: number;
    id: number;
    dlc: number;
    data: number[];
}

export type BusMonitorStatus =
    | { kind: 'started' }
    | { kind: 'stopped' }
    | { kind: 'error'; message: string };

// ---- Wrappers ----

export function startBusMonitor(request: BusMonitorRequest): Promise<void> {
    return invoke<void>('bus_monitor_start', { request });
}

export function stopBusMonitor(): Promise<void> {
    return invoke<void>('bus_monitor_stop');
}

export function onBusMonitorFrame(
    handler: (frame: BusMonitorFrame) => void,
): Promise<UnlistenFn> {
    return listen<BusMonitorFrame>('bus_monitor:frame', (e) => handler(e.payload));
}

export function onBusMonitorStatus(
    handler: (status: BusMonitorStatus) => void,
): Promise<UnlistenFn> {
    return listen<BusMonitorStatus>('bus_monitor:status', (e) => handler(e.payload));
}

// ---- Capture ----

export type BusMonitorCaptureEvent =
    | { kind: 'started'; path: string }
    | { kind: 'stopped'; path: string; frames: number }
    | { kind: 'progress'; path: string; frames: number }
    | { kind: 'error'; message: string };

export function startBusMonitorCapture(path: string): Promise<void> {
    return invoke<void>('bus_monitor_capture_start', { request: { path } });
}

export function stopBusMonitorCapture(): Promise<void> {
    return invoke<void>('bus_monitor_capture_stop');
}

export function onBusMonitorCapture(
    handler: (event: BusMonitorCaptureEvent) => void,
): Promise<UnlistenFn> {
    return listen<BusMonitorCaptureEvent>('bus_monitor:capture', (e) =>
        handler(e.payload),
    );
}

// ---- Display helpers ----

export function formatId(id: number): string {
    return `0x${id.toString(16).toUpperCase().padStart(3, '0')}`;
}

export function formatData(data: number[], dlc: number): string {
    return data
        .slice(0, dlc)
        .map((b) => b.toString(16).toUpperCase().padStart(2, '0'))
        .join(' ');
}

export function formatTs(ms: number): string {
    const totalSec = ms / 1000;
    const sec = Math.floor(totalSec);
    const frac = (ms - sec * 1000).toString().padStart(3, '0');
    return `${sec}.${frac}`;
}
