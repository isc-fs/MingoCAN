// Data-logs (LOGFS) bridge — list + pull the node's microSD car-data
// logs over CAN (#506). Mirrors `src-tauri/src/logs.rs`.
//
// `mtimeMonotonic` is boot-relative (the AMS has no set RTC) — render it
// as an ordering / uptime value, NEVER as a calendar date.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

/** Same adapter/session shape the diagnose commands take. */
export interface LogsRequest {
    interface: string;
    channel: string | null;
    bitrate: number;
    nodeId: number | null;
    timeoutMs: number;
}

export interface LogFile {
    index: number;
    name: string;
    size: number;
    /** Boot-relative / monotonic. Not a timestamp. */
    mtimeMonotonic: number;
}

export interface PullProgress {
    index: number;
    name: string;
    received: number;
    /** 0 when the node didn't report a size up front. */
    total: number;
}

export interface PullResult {
    path: string;
    bytes: number;
    crcVerified: boolean;
}

/** Backend marker for an operator-cancelled pull (not a failure). */
export const CANCELLED_MSG = 'cancelled by operator';

/** Progress event emitted during `logsPull`. */
export const LOGS_PROGRESS_EVENT = 'logs://progress';

export function logsList(request: LogsRequest): Promise<LogFile[]> {
    return invoke<LogFile[]>('logs_list', { request });
}

export function logsPull(
    request: LogsRequest,
    index: number,
    destDir: string,
): Promise<PullResult> {
    return invoke<PullResult>('logs_pull', { request, index, destDir });
}

/**
 * Seal the log currently being written and return its new index.
 *
 * The logger only closes a file on shutdown, so without this the run you
 * just did is the one file that can't be pulled.
 */
export function logsFinalize(request: LogsRequest): Promise<number> {
    return invoke<number>('logs_finalize', { request });
}

/** Ask an in-flight pull to stop; it aborts at the next read boundary. */
export function logsCancel(): Promise<void> {
    return invoke<void>('logs_cancel');
}

export function onPullProgress(
    handler: (p: PullProgress) => void,
): Promise<UnlistenFn> {
    return listen<PullProgress>(LOGS_PROGRESS_EVENT, (e) => handler(e.payload));
}

/** Human byte size — logs run to multiple MB. */
export function formatBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / (1024 * 1024)).toFixed(2)} MB`;
}
