// Auto-update wrappers around `@tauri-apps/plugin-updater` +
// `@tauri-apps/plugin-process`, mirroring the thin-wrapper convention
// in `swd.ts` / `provision.ts`.
//
// The app checks the GitHub release `latest.json` (configured in
// tauri.conf.json → plugins.updater.endpoints) against the running
// version. `checkForUpdate()` is best-effort: it swallows every error
// — offline, no manifest published yet (pre-activation), an invalid
// placeholder pubkey, or running outside the Tauri runtime (a plain
// `vite preview`) — and returns null so the UI simply shows nothing.

import { check, type Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';

export interface AvailableUpdate {
    /** The newer version offered by the manifest. */
    version: string;
    /** The version currently running. */
    currentVersion: string;
    /** Release notes body from the manifest (may be empty). */
    notes: string;
    /** Publish date string from the manifest, if present. */
    date: string | null;
}

/** Download progress, surfaced to the banner while installing. */
export type DownloadPhase =
    | { kind: 'started'; total: number | null }
    | { kind: 'progress'; downloaded: number; total: number | null }
    | { kind: 'finished' };

// The live `Update` handle from the last successful `check()`, kept so
// `downloadInstallAndRelaunch()` can act on the same update the banner
// is showing without re-fetching the manifest.
let staged: Update | null = null;

/**
 * Check the configured endpoint for a newer version. Returns the
 * update info when one is available, or null (no update / any error —
 * see module note). Never throws.
 */
export async function checkForUpdate(): Promise<AvailableUpdate | null> {
    try {
        const update = await check();
        if (update === null) {
            staged = null;
            return null;
        }
        staged = update;
        return {
            version: update.version,
            currentVersion: update.currentVersion,
            notes: update.body ?? '',
            date: update.date ?? null,
        };
    } catch {
        staged = null;
        return null;
    }
}

/**
 * Download + install the update staged by the last `checkForUpdate()`,
 * then relaunch into the new version. Throws if no update is staged or
 * if the download/install fails (the caller surfaces the error).
 */
export async function downloadInstallAndRelaunch(
    onPhase?: (phase: DownloadPhase) => void,
): Promise<void> {
    if (staged === null) {
        throw new Error('no update is staged — run checkForUpdate() first');
    }
    let downloaded = 0;
    let total: number | null = null;
    await staged.downloadAndInstall((event) => {
        switch (event.event) {
            case 'Started':
                total = event.data.contentLength ?? null;
                onPhase?.({ kind: 'started', total });
                break;
            case 'Progress':
                downloaded += event.data.chunkLength;
                onPhase?.({ kind: 'progress', downloaded, total });
                break;
            case 'Finished':
                onPhase?.({ kind: 'finished' });
                break;
        }
    });
    // Install complete — restart into the freshly-installed version.
    await relaunch();
}

/** GitHub release page for a given version tag (for "release notes"). */
export function releaseNotesUrl(version: string): string {
    return `https://github.com/isc-fs/can-flasher/releases/tag/v${version}`;
}
