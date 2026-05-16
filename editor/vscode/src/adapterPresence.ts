// Liveness-checks the currently-selected CAN adapter against the
// hardware actually plugged in, and broadcasts the result so the
// status bar and Tools sidebar can show a clear "disconnected"
// state when the operator yanks the cable.
//
// Without this the UI would keep showing the last-saved
// `iscFs.interface` + `iscFs.channel` as if it were live — bad
// for operators who unplug a probe between runs and don't notice
// the next click is about to talk to nothing.
//
// Strategy:
//
//  - Run `can-flasher --json adapters` and compare the configured
//    interface+channel against the detected list.
//  - The `virtual` interface is always "present" — it's a
//    software loopback, no hardware involved.
//  - Re-check on:
//      • activation
//      • `iscFs.*` config changes (operator picked a new adapter)
//      • window focus regained (typical "after unplug" trigger)
//      • a periodic timer while the window is focused (default 8 s;
//        cheap CLI call, infrequent enough to not log-spam)
//  - Re-check is skipped while the window is unfocused so we
//    don't poll a probe-less laptop in the background.
//  - The probe results are cached for the session; subscribers
//    receive the latest snapshot via `onDidChange`.

import * as os from 'os';
import * as vscode from 'vscode';

import { fetchAdapters, type AdapterEntry } from './adapters';
import { readConfig } from './config';

/** What a subscriber sees. `'unknown'` means the check hasn't
 *  resolved yet (cold start or last call errored). */
export type Presence = 'present' | 'disconnected' | 'unknown';

export interface PresenceSnapshot {
    presence: Presence;
    /** The configured interface at check time, for label rendering. */
    interfaceKind: string;
    /** The configured channel string at check time, or null. */
    channel: string | null;
    /** When the check failed, the error message; otherwise null. */
    error: string | null;
}

const POLL_INTERVAL_MS = 8_000;

let context: vscode.ExtensionContext | undefined;
let lastSnapshot: PresenceSnapshot = {
    presence: 'unknown',
    interfaceKind: 'slcan',
    channel: null,
    error: null,
};
let pollTimer: NodeJS.Timeout | null = null;
let inflight = false;

const emitter = new vscode.EventEmitter<PresenceSnapshot>();
/** Subscribe to presence changes; immediately receives the
 *  current snapshot too (subscribers don't need to query). */
export const onDidChangePresence: vscode.Event<PresenceSnapshot> = emitter.event;

/** Best snapshot the service has so far. Useful for one-shot
 *  reads from rendering code that doesn't want a subscription. */
export function currentSnapshot(): PresenceSnapshot {
    return lastSnapshot;
}

/**
 * Wire the presence service into the extension lifetime. Idempotent
 * — calling twice is a no-op. Returns nothing; subscribers use
 * `onDidChangePresence`.
 */
export function startAdapterPresenceService(ctx: vscode.ExtensionContext): void {
    if (context !== undefined) return;
    context = ctx;

    // Cold-start probe so the UI has a real answer before the
    // operator's first interaction.
    void recheck();

    // Settings change → operator may have switched adapter or
    // typed in a different channel. Always re-probe.
    ctx.subscriptions.push(
        vscode.workspace.onDidChangeConfiguration((event) => {
            if (event.affectsConfiguration('iscFs')) {
                void recheck();
            }
        }),
    );

    // Window focus regained → typical "I just plugged something
    // back in" trigger. Re-probe immediately and resume polling.
    ctx.subscriptions.push(
        vscode.window.onDidChangeWindowState((state) => {
            if (state.focused) {
                void recheck();
                startPolling();
            } else {
                stopPolling();
            }
        }),
    );

    // Initial polling state mirrors the current window focus.
    if (vscode.window.state.focused) {
        startPolling();
    }

    ctx.subscriptions.push({
        dispose: () => {
            stopPolling();
            emitter.dispose();
        },
    });
}

/**
 * Force a fresh check. Used by the status-bar pill when the
 * operator clicks it (the existing handler also pops the
 * adapter picker — this is just side-effect-free truth-up).
 */
export async function recheck(): Promise<void> {
    if (inflight) return;
    inflight = true;
    try {
        const cfg = readConfig();
        const interfaceKind = cfg.interface;
        const channel = cfg.channel.length > 0 ? cfg.channel : null;

        // Virtual interface is a software loopback — always live.
        if (interfaceKind === 'virtual') {
            update({
                presence: 'present',
                interfaceKind,
                channel,
                error: null,
            });
            return;
        }
        // No channel configured yet — nothing to check; report
        // disconnected so the UI surfaces "no adapter selected".
        if (channel === null) {
            update({
                presence: 'disconnected',
                interfaceKind,
                channel: null,
                error: null,
            });
            return;
        }

        // `fetchAdapters` shells out to the CLI; needs a cwd, but
        // `adapters` doesn't actually depend on workspace state.
        // Fall back to the operator's home so probe-only setups
        // (no workspace open) still get presence checks.
        const cwd = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? os.homedir();
        let detected: AdapterEntry[];
        try {
            detected = await fetchAdapters(cfg, cwd);
        } catch (err) {
            update({
                presence: 'unknown',
                interfaceKind,
                channel,
                error: err instanceof Error ? err.message : String(err),
            });
            return;
        }

        const match = detected.find(
            (a) => a.interface === interfaceKind && a.channel === channel,
        );
        update({
            presence: match !== undefined ? 'present' : 'disconnected',
            interfaceKind,
            channel,
            error: null,
        });
    } finally {
        inflight = false;
    }
}

function startPolling(): void {
    if (pollTimer !== null) return;
    pollTimer = setInterval(() => {
        void recheck();
    }, POLL_INTERVAL_MS);
}

function stopPolling(): void {
    if (pollTimer === null) return;
    clearInterval(pollTimer);
    pollTimer = null;
}

function update(snapshot: PresenceSnapshot): void {
    // Suppress noise: only fire if anything changed.
    if (
        snapshot.presence === lastSnapshot.presence &&
        snapshot.interfaceKind === lastSnapshot.interfaceKind &&
        snapshot.channel === lastSnapshot.channel &&
        snapshot.error === lastSnapshot.error
    ) {
        return;
    }
    lastSnapshot = snapshot;
    emitter.fire(snapshot);
}
