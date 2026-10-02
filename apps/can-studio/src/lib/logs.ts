// Data-logs (LOGFS) bridge — scan + pull the node's microSD car-data
// logs over CAN (#506). Mirrors `src-tauri/src/logs.rs`.
//
// The card has no delete, so the backend keeps a per-laptop download
// ledger and `logsScan` reports each file's status against it: `new`,
// `downloaded` (a copy is on disk at the listed size), `missing`
// (downloaded once, copy gone) or `hidden` (dismissed by hand).

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

export type FileStatus = 'new' | 'downloaded' | 'missing' | 'hidden';

export interface ScannedFile {
    index: number;
    name: string;
    size: number;
    status: FileStatus;
    /** The copy on disk (downloaded) or where it used to be (missing). */
    path: string | null;
    /** Unix seconds of that download. */
    pulledAt: number | null;
}

export interface ScanResult {
    /** Card (directory) order — sort with `newestFirst`. */
    files: ScannedFile[];
    /** A downloaded file's CRC no longer matches: reformatted or swapped
     *  card. Everything is reported as new. */
    cardChanged: boolean;
    ledgerError: string | null;
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
    /** Saved, but not recorded — it will show as new on the next scan. */
    ledgerError: string | null;
    /** A binary log (`IMUnnnn.BIN`, `CELnnnn.BIN`) decoded to a CSV beside
     *  it (#613). `null` for other files, or when decoding failed. */
    decoded: { path: string; records: number; tornBytes: number } | null;
    /** Why a binary log couldn't be decoded; the pulled file is kept. */
    decodeError: string | null;
}

/** Backend marker for an operator-cancelled pull (not a failure). */
export const CANCELLED_MSG = 'cancelled by operator';

/** Backend marker: the listed index no longer holds that file. */
export const CARD_CHANGED_MSG = 'the card changed since it was listed';

/** Progress event emitted during `logsPull`. */
export const LOGS_PROGRESS_EVENT = 'logs://progress';

export function logsScan(request: LogsRequest): Promise<ScanResult> {
    return invoke<ScanResult>('logs_scan', { request });
}

export function logsPull(
    request: LogsRequest,
    file: ScannedFile,
    destDir: string,
): Promise<PullResult> {
    return invoke<PullResult>('logs_pull', {
        request,
        index: file.index,
        expectName: file.name,
        expectSize: file.size,
        destDir,
    });
}

/** Ask an in-flight pull to stop; it aborts at the next read boundary. */
export function logsCancel(): Promise<void> {
    return invoke<void>('logs_cancel');
}

/** Record hides / unhides / forgets in this laptop's ledger. Touches
 *  neither the card nor any file on disk. */
export function logsMark(
    action: 'hide' | 'unhide' | 'forget',
    node: number,
    files: ReadonlyArray<Pick<ScannedFile, 'name' | 'size'>>,
): Promise<void> {
    return invoke<void>('logs_mark', {
        action,
        files: files.map((f) => ({ node, name: f.name, size: f.size })),
    });
}

/** `<Documents>/MingoCAN Logs` on this machine. */
export function logsDefaultRoot(): Promise<string> {
    return invoke<string>('logs_default_root');
}

/** Show a file (selected) or folder in Finder / Explorer / the file manager. */
export function logsReveal(path: string): Promise<void> {
    return invoke<void>('logs_reveal', { path });
}

export function onPullProgress(
    handler: (p: PullProgress) => void,
): Promise<UnlistenFn> {
    return listen<PullProgress>(LOGS_PROGRESS_EVENT, (e) => handler(e.payload));
}

// ---- Card file helpers ----

/** What a card file holds, from the top two bits of its LOGFS index
 *  (IFS08-CE-AMS log_names.hpp): `00` LOG (`LOGnnnn.CSV`), `10` IMU
 *  (`IMUnnnn.BIN`, `.CSV` on older cards), `01` CEL (`CELnnnn.BIN`), `11`
 *  reserved for the next stream. */
export type LogKind = 'log' | 'imu' | 'cel' | 'other';

export function kindOf(index: number): LogKind {
    switch (index & 0xc000) {
        case 0x0000:
            return 'log';
        case 0x8000:
            return 'imu';
        case 0x4000:
            return 'cel';
        default:
            return 'other';
    }
}

/** Rotation number (low 14 bits) — a new set of files every 5 min / 4 MiB,
 *  counting up, shared by the LOG, IMU and CEL files of one window. */
export function runNumber(index: number): number {
    return index & 0x3fff;
}

/** Sort comparator: newest rotation first. The card lists in directory
 *  order, which is not necessarily index order. */
export function newestFirst(a: ScannedFile, b: ScannedFile): number {
    return runNumber(b.index) - runNumber(a.index);
}

// ---- Download location ----

/** Local calendar date, `YYYY-MM-DD`. */
export function localDate(d: Date = new Date()): string {
    const p = (n: number) => n.toString().padStart(2, '0');
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/** Join path parts with the separator `root` already uses. */
export function joinPath(root: string, ...parts: string[]): string {
    const sep = root.includes('\\') && !root.includes('/') ? '\\' : '/';
    return [root.replace(/[\\/]+$/, ''), ...parts].join(sep);
}

// ---- Formatting ----

/** Human byte size — logs run to multiple MB. */
export function formatBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / (1024 * 1024)).toFixed(2)} MB`;
}

/** Classic CAN moves roughly 10–20 kB/s of log data. */
const SLOW_BPS = 10_000;
const FAST_BPS = 20_000;

export function formatDuration(seconds: number): string {
    const s = Math.max(1, Math.round(seconds));
    if (s < 60) return `${s} s`;
    const m = Math.round(s / 60);
    return `${m} min`;
}

/** Up-front estimate for `bytes`, e.g. "~2–4 min". */
export function estimate(bytes: number): string {
    const fast = formatDuration(bytes / FAST_BPS);
    const slow = formatDuration(bytes / SLOW_BPS);
    if (fast === slow) return `~${fast}`;
    const [fastN, fastUnit] = fast.split(' ');
    const [slowN, slowUnit] = slow.split(' ');
    return fastUnit === slowUnit
        ? `~${fastN}–${slowN} ${slowUnit}`
        : `~${fast}–${slow}`;
}

// ---- Transfer guard ----

// Leaving Data logs cancels a running pull (it holds the one adapter for
// minutes). App.svelte reads this to ask before navigating away.
let transferActive = false;

export function setLogsTransferActive(active: boolean): void {
    transferActive = active;
}

export function logsTransferActive(): boolean {
    return transferActive;
}

/** "today 14:31" / "3 Oct 14:31" for a ledger timestamp (unix seconds). */
export function formatPulledAt(secs: number | null): string {
    if (secs === null) return '';
    const d = new Date(secs * 1000);
    const time = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    if (localDate(d) === localDate()) return `today ${time}`;
    return `${d.toLocaleDateString([], { day: 'numeric', month: 'short' })} ${time}`;
}
