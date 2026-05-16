// Helpers for spawning `can-flasher` (or any other shell command)
// and consuming its output line-by-line. The flash subcommand's
// `--json` mode emits one JSON object per line per progress event,
// then a pretty-printed multi-line final report — both are
// surfaced from here.

import { spawn } from 'child_process';
import * as vscode from 'vscode';

import { getOutputChannel } from './output';

// ---- Flash event shape ----
//
// Mirrors `JsonEvent` in src/cli/flash.rs. Each variant matches one
// line of `--json` output during a flash. Kept as a tagged union so
// the caller can `switch` on `event` and TypeScript narrows the
// payload accordingly.

export type FlashEvent =
    | { event: 'planning'; sector: number; role: string }
    | { event: 'erased'; sector: number }
    | { event: 'written'; sector: number; bytes: number; total: number }
    | { event: 'verified'; sector: number; crc: string }
    | { event: 'committing' }
    | { event: 'done'; duration_ms: number };

/**
 * Final report object printed after the last event. Mirrors
 * `JsonReport` in src/cli/flash.rs (pretty-printed across many
 * lines, so the caller has to accumulate-and-parse rather than
 * splitting by newline).
 */
export interface FlashReport {
    status: string;
    firmware: {
        path: string;
        crc32: string;
        size_bytes: number;
        version: string;
        product_name?: string;
        git_hash?: string;
    };
    bootloader: {
        proto_major: number;
        proto_minor: number;
        wrp_protected: boolean;
        wrp_sector_mask: string;
    };
    sectors_erased: number[];
    sectors_written: number[];
    sectors_skipped: number[];
    duration_ms: number;
    error?: string;
}

// ---- Generic spawn ----

export interface SpawnOptions {
    cwd: string;
    /** Called once per line of stdout (excluding the trailing `\n`). */
    onStdoutLine?: (line: string) => void;
    /** Called once per line of stderr. */
    onStderrLine?: (line: string) => void;
    /** Token to abort the spawned process. */
    cancellation?: vscode.CancellationToken;
}

export interface SpawnResult {
    exitCode: number | null;
    /** Full accumulated stdout (every byte the child wrote). */
    stdout: string;
    /** Full accumulated stderr. */
    stderr: string;
    /** Set when the cancellation token fired and we killed the process. */
    cancelled: boolean;
}

/**
 * Run `command` with `args` and stream output line-by-line.
 *
 * Logs the argv to the ISC MingoCAN output channel before spawning so
 * operators can see exactly what was run. Resolves once the
 * process exits (or is killed by cancellation); never rejects on
 * non-zero exit — the caller decides what to do with the code.
 */
export function spawnCommand(
    command: string,
    args: readonly string[],
    options: SpawnOptions,
): Promise<SpawnResult> {
    return new Promise((resolve) => {
        const out = getOutputChannel();
        out.appendLine(`$ ${quoteArgv([command, ...args])}`);

        const child = spawn(command, args, {
            cwd: options.cwd,
            shell: false,
            windowsHide: true,
        });

        let stdoutBuf = '';
        let stderrBuf = '';
        let stdoutPending = '';
        let stderrPending = '';
        let cancelled = false;

        const cancelSub = options.cancellation?.onCancellationRequested(() => {
            cancelled = true;
            child.kill();
        });

        child.stdout.setEncoding('utf8');
        child.stderr.setEncoding('utf8');

        child.stdout.on('data', (chunk: string) => {
            stdoutBuf += chunk;
            stdoutPending += chunk;
            const lines = stdoutPending.split('\n');
            stdoutPending = lines.pop() ?? '';
            for (const line of lines) {
                options.onStdoutLine?.(line);
            }
        });

        child.stderr.on('data', (chunk: string) => {
            stderrBuf += chunk;
            stderrPending += chunk;
            const lines = stderrPending.split('\n');
            stderrPending = lines.pop() ?? '';
            for (const line of lines) {
                options.onStderrLine?.(line);
            }
        });

        child.on('error', (err) => {
            // Spawn-time error (binary not found, permission denied,
            // …). Stuff it into stderr so the caller's normal error
            // path handles it uniformly.
            stderrBuf += `${err.message}\n`;
            options.onStderrLine?.(err.message);
            // ENOENT is the "can-flasher CLI isn't installed (or
            // isn't on the PATH VS Code sees)" case. Show a
            // one-shot notification with actions so operators get a
            // path forward instead of a silently-empty Adapters tree.
            if ((err as NodeJS.ErrnoException).code === 'ENOENT') {
                maybeNotifyMissingCli(command);
            }
        });

        child.on('close', (code) => {
            // Flush any final partial line that wasn't terminated.
            if (stdoutPending.length > 0) {
                options.onStdoutLine?.(stdoutPending);
            }
            if (stderrPending.length > 0) {
                options.onStderrLine?.(stderrPending);
            }
            cancelSub?.dispose();
            resolve({
                exitCode: code,
                stdout: stdoutBuf,
                stderr: stderrBuf,
                cancelled,
            });
        });
    });
}

// ---- Flash-event parsing ----

/**
 * Try to parse one line of `flash --json` output as a `FlashEvent`.
 * Returns `null` for lines that are part of the trailing
 * pretty-printed report, empty lines, or anything we don't
 * recognise — caller treats those as opaque.
 */
export function parseFlashEvent(line: string): FlashEvent | null {
    const trimmed = line.trim();
    if (trimmed.length === 0 || !trimmed.startsWith('{')) {
        return null;
    }
    let parsed: unknown;
    try {
        parsed = JSON.parse(trimmed);
    } catch {
        return null;
    }
    if (typeof parsed !== 'object' || parsed === null) {
        return null;
    }
    const obj = parsed as { event?: unknown };
    if (typeof obj.event !== 'string') {
        return null;
    }
    // Trust the contract — we've defined the variant shapes to match
    // `JsonEvent` in src/cli/flash.rs. If the contract drifts the
    // caller falls back to "unknown event" UX (no progress update,
    // line still goes to the output channel).
    return parsed as FlashEvent;
}

/**
 * Extract the trailing pretty-printed `FlashReport` from the full
 * stdout of a successful flash run. Returns `null` if the tail
 * doesn't parse as a report (e.g. dry-run early exit, or a future
 * report-schema drift the extension doesn't recognise).
 *
 * Events use compact single-line JSON (`{"event":"erased",…}`); the
 * report uses `serde_json::to_string_pretty` which always starts
 * with `{\n` and indents fields. We anchor on the last `{\n`
 * occurrence and parse from there — O(n) once, no quadratic scan.
 */
export function parseFlashReport(stdout: string): FlashReport | null {
    const startIdx = stdout.lastIndexOf('{\n');
    if (startIdx < 0) {
        return null;
    }
    const slice = stdout.slice(startIdx).trim();
    try {
        const parsed: unknown = JSON.parse(slice);
        if (
            typeof parsed === 'object' &&
            parsed !== null &&
            'firmware' in parsed &&
            'bootloader' in parsed
        ) {
            return parsed as FlashReport;
        }
    } catch {
        // Not a parseable report at the tail — caller falls back to
        // a less-informative success toast.
    }
    return null;
}

// ---- One-shot JSON capture ----

export interface JsonResult<T> {
    /** Parsed JSON value, or `null` if the command exited non-zero or
     *  emitted output that didn't parse. */
    value: T | null;
    exitCode: number | null;
    stdout: string;
    stderr: string;
}

/**
 * Run a `can-flasher` invocation that emits a single JSON document
 * (object or array) on stdout — `adapters --json`, `discover --json`,
 * `config nvm read --json`, etc. Captures the whole stdout and parses
 * it as JSON.
 *
 * Long-running flash-style commands that emit a *stream* of JSON
 * events should use `spawnCommand` with `onStdoutLine` instead.
 */
export async function runJson<T>(
    canFlasherPath: string,
    args: readonly string[],
    cwd: string,
    cancellation?: vscode.CancellationToken,
): Promise<JsonResult<T>> {
    const result = await spawnCommand(canFlasherPath, args, { cwd, cancellation });
    if (result.exitCode !== 0) {
        return { value: null, ...result };
    }
    try {
        const value = JSON.parse(result.stdout) as T;
        return { value, ...result };
    } catch {
        return { value: null, ...result };
    }
}

// ---- ENOENT UX ----
//
// We surface this exactly once per VS Code session. Operators
// who pinned a custom path (and have it wrong) need to see it,
// but every spawn that ENOENTs would otherwise re-pop the toast.

let enoentNotified = false;

function maybeNotifyMissingCli(command: string): void {
    if (enoentNotified) return;
    enoentNotified = true;

    const releaseUrl = 'https://github.com/isc-fs/can-flasher/releases/latest';
    const message =
        `Couldn't run \`${command}\` — the can-flasher CLI isn't on the PATH ` +
        `VS Code sees. Download the latest release for your platform, or ` +
        `point \`iscFs.canFlasherPath\` at the absolute install path.`;

    void vscode.window
        .showErrorMessage(message, 'Download CLI', 'Open settings')
        .then((choice) => {
            if (choice === 'Download CLI') {
                void vscode.env.openExternal(vscode.Uri.parse(releaseUrl));
            } else if (choice === 'Open settings') {
                void vscode.commands.executeCommand(
                    'workbench.action.openSettings',
                    'iscFs.canFlasherPath',
                );
            }
        });
}

// ---- Argv quoting (display only) ----

function quoteArgv(argv: readonly string[]): string {
    return argv.map(quoteArg).join(' ');
}

function quoteArg(arg: string): string {
    if (arg.length === 0) {
        return "''";
    }
    if (/^[A-Za-z0-9_@%+=:,./-]+$/.test(arg)) {
        return arg;
    }
    return `'${arg.replace(/'/g, "'\\''")}'`;
}
