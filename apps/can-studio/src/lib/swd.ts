// Typed wrappers for the `swd_*` Tauri commands. Mirrors the
// shapes in `apps/can-studio/src-tauri/src/swd.rs`.

import { invoke } from '@tauri-apps/api/core';

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
