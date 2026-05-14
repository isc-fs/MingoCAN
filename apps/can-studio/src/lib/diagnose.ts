// Typed wrappers for the diagnose Tauri commands. Mirrors the
// DiagnoseRequest / HealthSnapshot / DtcSnapshot structs in
// `apps/can-studio/src-tauri/src/diagnose.rs`.

import { invoke } from '@tauri-apps/api/core';

import type { InterfaceType } from './types';

export interface DiagnoseRequest {
    interface: InterfaceType;
    channel: string | null;
    bitrate: number;
    nodeId: number | null;
    timeoutMs: number;
}

export interface HealthSnapshot {
    uptimeSeconds: number;
    resetCause: string;
    resetCauseRaw: number;
    flashWriteCount: number;
    dtcCount: number;
    lastDtcCode: number;
    sessionActive: boolean;
    validAppPresent: boolean;
    wrpProtected: boolean;
    rawFlags: number;
}

export interface DtcSnapshot {
    code: number;
    severity: string;
    severityRaw: number;
    occurrenceCount: number;
    firstSeenUptimeSeconds: number;
    lastSeenUptimeSeconds: number;
    contextData: number;
}

// ---- Wrappers ----

export function getHealth(req: DiagnoseRequest): Promise<HealthSnapshot> {
    return invoke<HealthSnapshot>('health', { request: req });
}

export function readDtcs(req: DiagnoseRequest): Promise<DtcSnapshot[]> {
    return invoke<DtcSnapshot[]>('read_dtcs', { request: req });
}

export function clearDtcs(req: DiagnoseRequest): Promise<void> {
    return invoke<void>('clear_dtcs', { request: req });
}

// ---- Display helpers ----

export function formatUptime(seconds: number): string {
    const h = Math.floor(seconds / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    const s = seconds % 60;
    if (h > 0) return `${h}h${String(m).padStart(2, '0')}m${String(s).padStart(2, '0')}s`;
    if (m > 0) return `${m}m${String(s).padStart(2, '0')}s`;
    return `${s}s`;
}

export function severityClass(severity: string): string {
    switch (severity) {
        case 'FATAL':
        case 'ERROR':
            return 'sev-error';
        case 'WARN':
            return 'sev-warn';
        case 'INFO':
            return 'sev-info';
        default:
            return 'sev-unknown';
    }
}
