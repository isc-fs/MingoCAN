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

export function swdFlash(args: SwdFlashArgs): Promise<void> {
    return invoke<void>('swd_flash', { args });
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
