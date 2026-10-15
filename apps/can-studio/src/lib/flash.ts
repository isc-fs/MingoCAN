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
    /** Reboot a running app into the bootloader if CONNECT fails. */
    enterBootloader: boolean;
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

/**
 * Flash config committed to a repo's `.vscode/settings.json` — the same
 * `iscFs.*` keys the VS Code extension reads. When the Build directory
 * points at such a repo, these take precedence over the app's stored
 * settings, so a developer gets the right build command / artifact /
 * node-id with no per-machine setup. `artifactPath` is already resolved
 * to an absolute path against the repo root. `null` when the repo has no
 * `.vscode/settings.json` or declares none of the keys.
 */
export interface RepoFlashConfig {
    buildCommand: string | null;
    artifactPath: string | null;
    nodeId: string | null;
    source: string;
}

export function readRepoFlashConfig(
    cwd: string,
): Promise<RepoFlashConfig | null> {
    return invoke<RepoFlashConfig | null>('read_repo_flash_config', { cwd });
}

export function onFlashEvent(handler: (event: FlashEvent) => void): Promise<UnlistenFn> {
    return listen<FlashEvent>('flash:event', (e) => handler(e.payload));
}
