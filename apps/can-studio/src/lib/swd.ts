// Typed wrappers for the `swd_*` Tauri commands. Mirrors the
// shapes in `apps/can-studio/src-tauri/src/swd.rs`.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export interface ProbeInfo {
    identifier: string;
    serialNumber: string | null;
    vendorId: number;
    productId: number;
}

export interface SwdFlashArgs {
    artifactPath: string;
    chip: string | null;
    probeSerial: string | null;
    /** "0x08000000" hex string, or null to take the library default. */
    base: string | null;
    verify: boolean;
    resetAfter: boolean;
    /**
     * When `true`, fall back to sector-erase. Default `false`
     * (chip-erase) after #247 — sector-erase reproduced silent
     * flash corruption on STM32H7 even with verify enabled. The
     * backend defaults this when the field is omitted, so most
     * callers can leave it out.
     */
    sectorEraseOnly?: boolean;
}

export function defaultSwdFlashArgs(): SwdFlashArgs {
    return {
        artifactPath: '',
        chip: 'STM32H733ZGTx',
        probeSerial: null,
        base: '0x08000000',
        verify: true,
        resetAfter: true,
    };
}

export function listSwdProbes(): Promise<ProbeInfo[]> {
    return invoke<ProbeInfo[]>('swd_list_probes');
}

/**
 * Per-flash summary returned by `swd_flash`. Mirrors
 * `SwdFlashReportDto` in apps/can-studio/src-tauri/src/swd.rs.
 *
 * `crc32Hex` is the CRC32 of the source artifact's raw bytes.
 * Combined with `verified: true` (probe-rs's readback-compare,
 * default-on), it's a verifiable fingerprint of what's on the
 * chip's flash right now — useful for reconciling a suspected-bad
 * ECU against a known-good one (e.g. carrier MLC1 vs MLC3 in
 * isc-fs/stm32-can-bootloader#117).
 */
export interface SwdFlashReport {
    verified: boolean;
    crc32Hex: string;
    sizeBytes: number;
    targetVoltageV: number | null;
}

export function swdFlash(args: SwdFlashArgs): Promise<SwdFlashReport> {
    return invoke<SwdFlashReport>('swd_flash', { args });
}

/** A bootloader artefact resolved on the local disk. */
export interface FetchedBootloader {
    /** Resolved release tag, e.g. `v1.2.0`. */
    tag: string;
    /** Absolute path to the on-disk `.elf` ready to flash. */
    path: string;
    /** `true` for a fresh download, `false` for cache hit. */
    downloaded: boolean;
}

/**
 * Download (or read from cache) the prebuilt bootloader from the
 * isc-fs/stm32-can-bootloader release page.
 *
 * @param tag GitHub release tag, e.g. `"v1.2.0"`. Pass `null` for
 *            the latest release.
 */
export function swdFetchBootloader(tag: string | null): Promise<FetchedBootloader> {
    return invoke<FetchedBootloader>('swd_fetch_bootloader', { tag });
}

/** Probe-rs phase identifier. Matches `SwdOperation` in swd.rs. */
export type SwdOp = 'erase' | 'program' | 'verify' | 'fill';

/**
 * One progress event emitted while `swd_flash` is in flight.
 * Discriminated by `kind`; mirrors `SwdStreamEvent` in swd.rs.
 */
export type SwdFlashEvent =
    | { kind: 'started'; op: SwdOp; total: number | null }
    | { kind: 'progress'; op: SwdOp; delta: number }
    | { kind: 'finished'; op: SwdOp }
    | { kind: 'failed'; op: SwdOp };

/**
 * Subscribe to `swd-flash:event` progress payloads. The returned
 * function unsubscribes; call it when the view unmounts.
 */
export function onSwdFlashEvent(
    handler: (event: SwdFlashEvent) => void,
): Promise<UnlistenFn> {
    return listen<SwdFlashEvent>('swd-flash:event', (e) => handler(e.payload));
}

export interface SwdEraseArgs {
    chip: string | null;
    probeSerial: string | null;
}

/**
 * Wipe the entire flash. Destructive — chip comes out empty.
 */
export function swdErase(args: SwdEraseArgs): Promise<void> {
    return invoke<void>('swd_erase', { args });
}
