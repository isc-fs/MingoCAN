// Post-flash readback — close the build→flash→**verify** loop.
//
// Two independent checks, run after a successful flash:
//
//  1. Image ↔ workspace (always): the flash report already carries the
//     git-hash embedded in the .elf we just wrote. Resolve it against the
//     workspace's git history → "you flashed HEAD" vs "you flashed a
//     stale/unknown build". Answers "did I flash the commit I'm looking
//     at?" — the single most common post-flash doubt.
//
//  2. Live board ↔ image (bootloader-only): a bootloader `discover` reads
//     the installed app's git-hash back off the board. This only works
//     while the board is still in the bootloader (a `--no-jump` flash);
//     after the default jump the app is running and doesn't answer the
//     bootloader, so we skip this leg rather than cry wolf. Per-sector CRC
//     verification during the flash already guarantees integrity — this
//     leg is an extra identity confirmation, not the safety net.

import { execFile } from 'child_process';
import * as vscode from 'vscode';

import { type Config } from './config';
import { type FlashReport } from './cli';
import { fetchDevices } from './discover';
import { getOutputChannel } from './output';

/** Compare git-hashes by the 8-char short prefix. The flash report emits
 *  16 hex chars (full 8-byte hash); `discover` emits 8 (first 4 bytes) —
 *  so the shared, safe comparison width is 8. */
function shortHash(hash: string): string {
    return hash.trim().toLowerCase().replace(/^g/, '').slice(0, 8);
}

/**
 * Entry point, called from `flash.ts` after `announceSuccess`. Never
 * throws — a readback problem surfaces as a toast + output-channel note,
 * never as a failed flash (the flash already succeeded).
 */
export async function runReadback(
    cfg: Config,
    cwd: string,
    report: FlashReport | null,
    jumpedToApp: boolean,
): Promise<void> {
    const out = getOutputChannel();
    const flashedHash = report?.firmware.git_hash;
    if (flashedHash === undefined || flashedHash.trim().length === 0) {
        // Nothing to resolve (e.g. an image built without the git-hash
        // struct) — stay silent; the success toast already fired.
        return;
    }
    const flashedShort = shortHash(flashedHash);
    out.appendLine('');
    out.appendLine(`═══ Readback · flashed @${flashedShort} ═══`);

    // ---- Leg 1: image ↔ workspace ----
    const resolution = await resolveAgainstWorkspace(cwd, flashedShort);
    out.appendLine(`  workspace: ${resolution.detail}`);

    // ---- Leg 2: live board ↔ image (bootloader-only) ----
    let liveNote: string | null = null;
    let liveMismatch = false;
    if (jumpedToApp) {
        liveNote = 'board jumped to app — bootloader readback skipped (flash was CRC-verified per sector)';
        out.appendLine(`  live:      ${liveNote}`);
    } else {
        const live = await readLiveHash(cfg, cwd);
        if (live === null) {
            liveNote = 'board not found in bootloader — live readback unavailable';
        } else if (shortHash(live) === flashedShort) {
            liveNote = `board reports @${shortHash(live)} — matches ✓`;
        } else {
            liveMismatch = true;
            liveNote = `board reports @${shortHash(live)} but you flashed @${flashedShort}`;
        }
        out.appendLine(`  live:      ${liveNote}`);
    }

    // ---- Single summary toast ----
    if (liveMismatch) {
        void vscode.window.showWarningMessage(
            `ISC MingoCAN readback: ${liveNote}. Re-check the target node-id.`,
        );
        return;
    }
    if (resolution.status === 'stale' || resolution.status === 'unknown') {
        void vscode.window.showWarningMessage(
            `ISC MingoCAN readback: ${resolution.toast}`,
        );
        return;
    }
    // 'head' and 'nogit' → no toast. A routine flash already got its
    // success toast; the readback stays a silent guardian that only
    // speaks up on a mismatch. The "flashed HEAD ✓" confirmation is in
    // the output channel for anyone who looks.
}

// ---- Leg 1 helpers: resolve a hash against the workspace ----

type ResolutionStatus = 'head' | 'stale' | 'unknown' | 'nogit';

interface Resolution {
    status: ResolutionStatus;
    /** Output-channel line. */
    detail: string;
    /** Toast text (only used for head/stale/unknown). */
    toast: string;
}

async function resolveAgainstWorkspace(
    cwd: string,
    flashedShort: string,
): Promise<Resolution> {
    const head = await git(cwd, ['rev-parse', '--short=8', 'HEAD']);
    if (head === null) {
        return { status: 'nogit', detail: 'not a git repository', toast: '' };
    }
    const headShort = head.toLowerCase();
    const dirty = (await git(cwd, ['status', '--porcelain'])) ?? '';
    const dirtyTag = dirty.trim().length > 0 ? ' (working tree dirty)' : '';

    if (flashedShort === headShort) {
        return {
            status: 'head',
            detail: `flashed HEAD @${headShort}${dirtyTag}`,
            toast: `flashed HEAD @${headShort}${dirtyTag} ✓`,
        };
    }
    // Is the flashed commit anywhere in history? `--is-ancestor` exits 0
    // when flashed is an ancestor of HEAD (an older build).
    const isAncestor = await gitOk(cwd, [
        'merge-base',
        '--is-ancestor',
        flashedShort,
        'HEAD',
    ]);
    if (isAncestor) {
        return {
            status: 'stale',
            detail: `flashed @${flashedShort}, an older commit — HEAD is @${headShort}${dirtyTag}`,
            toast: `flashed @${flashedShort} (older than HEAD @${headShort}) — stale build?`,
        };
    }
    const known = await gitOk(cwd, ['cat-file', '-e', `${flashedShort}^{commit}`]);
    if (known) {
        return {
            status: 'stale',
            detail: `flashed @${flashedShort}, a commit not on HEAD's history — HEAD is @${headShort}${dirtyTag}`,
            toast: `flashed @${flashedShort}, off HEAD's branch — HEAD is @${headShort}`,
        };
    }
    return {
        status: 'unknown',
        detail: `flashed @${flashedShort} — not found in this repo (HEAD @${headShort})`,
        toast: `flashed @${flashedShort}, unknown to this repo — built elsewhere? (HEAD @${headShort})`,
    };
}

// ---- Leg 2 helper: read the live hash off a bootloader board ----

async function readLiveHash(cfg: Config, cwd: string): Promise<string | null> {
    let node: number | null = null;
    if (cfg.nodeId.trim().length > 0) {
        const s = cfg.nodeId.trim().toLowerCase();
        const n = s.startsWith('0x') ? Number.parseInt(s.slice(2), 16) : Number.parseInt(s, 10);
        node = Number.isInteger(n) ? n : null;
    }
    try {
        const rows = await fetchDevices(cfg, cwd);
        const row =
            node !== null
                ? rows.find((r) => r.node_id === node)
                : rows.length === 1
                  ? rows[0]
                  : undefined;
        return row?.git_hash ?? null;
    } catch {
        return null;
    }
}

// ---- git shell-out ----

/** Run `git <args>` in `cwd`, returning trimmed stdout, or `null` on any
 *  error (not a repo, git absent, non-zero exit). */
function git(cwd: string, args: string[]): Promise<string | null> {
    return new Promise((resolve) => {
        execFile('git', args, { cwd, timeout: 5_000 }, (err, stdout) => {
            resolve(err ? null : stdout.trim());
        });
    });
}

/** Run `git <args>` for its exit code only — `true` when it exits 0. */
function gitOk(cwd: string, args: string[]): Promise<boolean> {
    return new Promise((resolve) => {
        execFile('git', args, { cwd, timeout: 5_000 }, (err) => {
            resolve(err === null);
        });
    });
}
