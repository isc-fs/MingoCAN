// Controller that owns a long-running `can-flasher diagnose
// live-data --json` process. Parses each line of stdout as a
// `LiveDataJson` snapshot and emits via callback. Designed to be
// driven by the WebviewPanel — `start()` spawns, `stop()` kills,
// `dispose()` cleans up.

import { spawn, type ChildProcessWithoutNullStreams } from 'child_process';
import * as vscode from 'vscode';

import { buildGlobalArgv, type Config } from './config';
import { getOutputChannel } from './output';

// ---- Wire type (mirror src/cli/diagnose.rs::LiveDataJson) ----

export interface LiveDataSnapshot {
    uptime_ms: number;
    frames_rx: number;
    frames_tx: number;
    nacks_sent: number;
    dtc_count: number;
    last_dtc_code: number;
    flags: number;
    last_opcode: number;
    last_flash_addr: number;
    isotp_rx_progress: number;
    session_age_ms: number;
    session_active: boolean;
    valid_app_present: boolean;
    log_streaming: boolean;
    livedata_streaming: boolean;
    wrp_protected: boolean;
}

export type ControllerStatus = 'idle' | 'running' | 'stopped' | 'error';

export interface LiveDataControllerEvents {
    onSnapshot(snapshot: LiveDataSnapshot): void;
    onStatus(status: ControllerStatus, message?: string): void;
}

export interface StartOptions {
    cfg: Config;
    cwd: string;
    rateHz: number;
}

/**
 * Owns at most one `diagnose live-data` child at a time. Concurrent
 * `start()` calls are de-duplicated — the second one is a no-op while
 * the first is still running.
 */
export class LiveDataController implements vscode.Disposable {
    private child: ChildProcessWithoutNullStreams | null = null;
    private stdoutPending = '';
    private stderrPending = '';
    private startupTimer: NodeJS.Timeout | null = null;

    constructor(private readonly events: LiveDataControllerEvents) {}

    get isRunning(): boolean {
        return this.child !== null;
    }

    start(opts: StartOptions): void {
        if (this.child !== null) {
            return;
        }

        const out = getOutputChannel();
        const rate = clamp(opts.rateHz, 1, 50);
        const argv = [
            ...buildGlobalArgv(opts.cfg),
            'diagnose',
            'live-data',
            '--rate-hz',
            String(rate),
        ];

        out.appendLine('');
        out.appendLine(`---- live-data ${new Date().toISOString()} ----`);
        out.appendLine(
            `$ ${opts.cfg.canFlasherPath} ${argv
                .map((a) => (a.includes(' ') ? `'${a}'` : a))
                .join(' ')}`,
        );

        try {
            this.child = spawn(opts.cfg.canFlasherPath, argv, {
                cwd: opts.cwd,
                shell: false,
                windowsHide: true,
            });
        } catch (err) {
            this.child = null;
            this.events.onStatus(
                'error',
                err instanceof Error ? err.message : String(err),
            );
            return;
        }

        this.stdoutPending = '';
        this.stderrPending = '';

        const child = this.child;
        child.stdout.setEncoding('utf8');
        child.stderr.setEncoding('utf8');

        child.stdout.on('data', (chunk: string) => this.handleStdout(chunk));
        child.stderr.on('data', (chunk: string) => this.handleStderr(chunk));

        child.on('error', (err) => {
            out.appendLine(`[error] ${err.message}`);
            this.events.onStatus('error', err.message);
        });

        child.on('close', (code) => {
            if (this.startupTimer !== null) {
                clearTimeout(this.startupTimer);
                this.startupTimer = null;
            }
            this.flushPending();
            this.child = null;
            if (code === 0 || code === null) {
                this.events.onStatus('stopped', 'live-data stream ended');
                out.appendLine('[info] live-data process exited cleanly');
            } else {
                this.events.onStatus(
                    'error',
                    `live-data exited with code ${code}. See ISC CAN output channel.`,
                );
                out.appendLine(`[error] live-data exited with code ${code}`);
            }
        });

        // Optimistic status — flip back to 'error' if `close` fires
        // before the first snapshot lands.
        this.events.onStatus('running', `streaming @ ${rate} Hz`);
    }

    stop(): void {
        if (this.child === null) {
            return;
        }
        this.child.kill();
        // The 'close' handler will fire the final 'stopped' status.
    }

    dispose(): void {
        if (this.startupTimer !== null) {
            clearTimeout(this.startupTimer);
            this.startupTimer = null;
        }
        this.stop();
    }

    // ---- Stdout / stderr handling ----

    private handleStdout(chunk: string): void {
        this.stdoutPending += chunk;
        const lines = this.stdoutPending.split('\n');
        this.stdoutPending = lines.pop() ?? '';
        const out = getOutputChannel();
        for (const line of lines) {
            this.processStdoutLine(line, out);
        }
    }

    private handleStderr(chunk: string): void {
        this.stderrPending += chunk;
        const lines = this.stderrPending.split('\n');
        this.stderrPending = lines.pop() ?? '';
        const out = getOutputChannel();
        for (const line of lines) {
            if (line.length > 0) {
                out.appendLine(line);
            }
        }
    }

    private flushPending(): void {
        const out = getOutputChannel();
        if (this.stdoutPending.length > 0) {
            this.processStdoutLine(this.stdoutPending, out);
            this.stdoutPending = '';
        }
        if (this.stderrPending.length > 0) {
            out.appendLine(this.stderrPending);
            this.stderrPending = '';
        }
    }

    private processStdoutLine(line: string, out: vscode.OutputChannel): void {
        const trimmed = line.trim();
        if (trimmed.length === 0 || !trimmed.startsWith('{')) {
            if (trimmed.length > 0) {
                out.appendLine(line);
            }
            return;
        }
        let parsed: unknown;
        try {
            parsed = JSON.parse(trimmed);
        } catch {
            out.appendLine(line);
            return;
        }
        if (looksLikeSnapshot(parsed)) {
            this.events.onSnapshot(parsed);
        } else {
            out.appendLine(line);
        }
    }
}

// ---- Snapshot validation ----

function looksLikeSnapshot(value: unknown): value is LiveDataSnapshot {
    if (typeof value !== 'object' || value === null) {
        return false;
    }
    const required = [
        'uptime_ms',
        'frames_rx',
        'frames_tx',
        'session_active',
        'valid_app_present',
        'wrp_protected',
    ];
    for (const key of required) {
        if (!(key in value)) {
            return false;
        }
    }
    return true;
}

function clamp(n: number, lo: number, hi: number): number {
    return Math.min(Math.max(n, lo), hi);
}
