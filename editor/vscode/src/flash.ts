// Implementation of `iscFs.flash` and `iscFs.flashWithoutBuild`.
//
// Shape of one flash run:
//
//   1. Read config + validate (artifact path, workspace folder)
//   2. (optional) Run the configured build command via the shell;
//      stream its output to the ISC CAN channel; abort on non-zero.
//   3. Resolve the firmware artifact path — exact path or glob;
//      prompt if a glob matches more than one file.
//   4. Spawn `can-flasher <global flags> flash <flags> <artifact>`
//      with `--json` and parse the event stream into a progress
//      bar's `report({ message })` calls.
//   5. On clean exit, surface duration + sectors-touched in a
//      success toast; on non-zero, exit-code-aware error toast that
//      maps to REQUIREMENTS.md's exit-code table.
//
// Every operation routes its raw stdout / stderr through the
// shared ISC CAN output channel — operators get a deterministic
// record of what was run when something goes wrong on the bench.

import * as path from 'path';
import * as vscode from 'vscode';

import { buildGlobalArgv, type Config, readConfig } from './config';
import {
    type FlashEvent,
    type FlashReport,
    parseFlashEvent,
    parseFlashReport,
    spawnCommand,
} from './cli';
import { getOutputChannel, showOutputChannel } from './output';

export interface FlashOptions {
    /** When true, skip the configured build command. */
    skipBuild: boolean;
}

/**
 * Entry point wired from `extension.ts` for `iscFs.flash` /
 * `iscFs.flashWithoutBuild`. Never throws — caller-facing errors
 * surface as toasts and structured output-channel logs.
 */
export async function runFlash(options: FlashOptions): Promise<void> {
    const out = getOutputChannel();
    showOutputChannel();

    const workspace = vscode.workspace.workspaceFolders?.[0];
    if (workspace === undefined) {
        void vscode.window.showErrorMessage(
            'ISC CAN: open a workspace folder before flashing.',
        );
        return;
    }
    const cwd = workspace.uri.fsPath;
    const cfg = readConfig();

    if (cfg.firmwareArtifact.trim().length === 0) {
        const open = 'Open settings';
        const pick = await vscode.window.showErrorMessage(
            'ISC CAN: set `iscFs.firmwareArtifact` to the .elf/.hex/.bin to flash.',
            open,
        );
        if (pick === open) {
            await vscode.commands.executeCommand(
                'workbench.action.openSettings',
                'iscFs.firmwareArtifact',
            );
        }
        return;
    }

    await vscode.window.withProgress(
        {
            location: vscode.ProgressLocation.Notification,
            title: 'ISC CAN: flashing firmware',
            cancellable: true,
        },
        async (progress, token) => {
            // Stage 1: build
            if (!options.skipBuild && cfg.buildCommand.trim().length > 0) {
                progress.report({ message: 'building…' });
                const buildOk = await runBuildStep(cfg.buildCommand, cwd, token);
                if (token.isCancellationRequested) {
                    return;
                }
                if (!buildOk) {
                    void vscode.window.showErrorMessage(
                        'ISC CAN: build failed. See ISC CAN output channel for details.',
                    );
                    return;
                }
            } else {
                out.appendLine(
                    options.skipBuild
                        ? '[skip] iscFs.flashWithoutBuild: build step skipped'
                        : '[skip] iscFs.buildCommand is empty; skipping build',
                );
            }

            // Stage 2: resolve artifact
            progress.report({ message: 'resolving artifact…' });
            const artifact = await resolveArtifact(cfg.firmwareArtifact, cwd);
            if (artifact === null) {
                return;
            }

            // Stage 3: flash
            progress.report({ message: 'opening session…' });
            await runFlashStep(cfg, artifact, cwd, progress, token);
        },
    );
}

// ---- Stage helpers ----

async function runBuildStep(
    command: string,
    cwd: string,
    token: vscode.CancellationToken,
): Promise<boolean> {
    const out = getOutputChannel();
    out.appendLine('');
    out.appendLine(`---- build ${new Date().toISOString()} ----`);

    // Spawn through the user's shell so the configured command can
    // contain pipes / chained commands / quoted args without us
    // having to parse it. `/bin/sh -c` is fine for the team's
    // dev workflow today; if Windows operators need cmd.exe we can
    // detect platform later.
    const shell = process.platform === 'win32' ? 'cmd.exe' : '/bin/sh';
    const shellArgs = process.platform === 'win32' ? ['/c', command] : ['-c', command];

    const result = await spawnCommand(shell, shellArgs, {
        cwd,
        cancellation: token,
        onStdoutLine: (line) => out.appendLine(line),
        onStderrLine: (line) => out.appendLine(line),
    });

    if (result.cancelled) {
        out.appendLine('[cancelled] build interrupted by user');
        return false;
    }
    if (result.exitCode !== 0) {
        out.appendLine(`[error] build exited with code ${result.exitCode}`);
        return false;
    }
    return true;
}

async function resolveArtifact(
    pattern: string,
    cwd: string,
): Promise<string | null> {
    // Use VS Code's own findFiles for glob support — works
    // consistently with the workspace's file-watcher exclusions and
    // avoids pulling in a separate `glob` dependency. If the
    // pattern has no glob meta-characters, findFiles returns the
    // single exact match (if it exists).
    const out = getOutputChannel();
    const isAbsolute = path.isAbsolute(pattern);
    if (isAbsolute) {
        // Trust an absolute path — no glob expansion. The caller
        // typed an absolute path knowing what they wanted.
        return pattern;
    }

    const matches = await vscode.workspace.findFiles(
        pattern,
        '**/node_modules/**',
        10,
    );
    if (matches.length === 0) {
        const open = 'Open settings';
        const pick = await vscode.window.showErrorMessage(
            `ISC CAN: no firmware artifact matched \`${pattern}\` under ${cwd}.`,
            open,
        );
        if (pick === open) {
            await vscode.commands.executeCommand(
                'workbench.action.openSettings',
                'iscFs.firmwareArtifact',
            );
        }
        return null;
    }
    if (matches.length === 1) {
        return matches[0].fsPath;
    }
    // Multiple matches — let the operator pick. Sorted alphabetically
    // for stable ordering across runs.
    const items = matches
        .map((uri) => ({
            label: vscode.workspace.asRelativePath(uri),
            uri,
        }))
        .sort((a, b) => a.label.localeCompare(b.label));
    const picked = await vscode.window.showQuickPick(items, {
        title: 'ISC CAN: pick a firmware artifact',
        matchOnDescription: true,
    });
    out.appendLine(
        picked === undefined
            ? '[cancelled] firmware-artifact pick cancelled by user'
            : `[info] using firmware artifact: ${picked.uri.fsPath}`,
    );
    return picked?.uri.fsPath ?? null;
}

async function runFlashStep(
    cfg: Config,
    artifactPath: string,
    cwd: string,
    progress: vscode.Progress<{ message?: string; increment?: number }>,
    token: vscode.CancellationToken,
): Promise<void> {
    const out = getOutputChannel();
    out.appendLine('');
    out.appendLine(`---- flash ${new Date().toISOString()} ----`);

    const argv = [
        ...buildGlobalArgv(cfg),
        'flash',
        ...flashFlags(cfg),
        artifactPath,
    ];

    const events: FlashEvent[] = [];
    const result = await spawnCommand(cfg.canFlasherPath, argv, {
        cwd,
        cancellation: token,
        onStdoutLine: (line) => {
            out.appendLine(line);
            const ev = parseFlashEvent(line);
            if (ev !== null) {
                events.push(ev);
                progress.report({ message: progressMessage(ev) });
            }
        },
        onStderrLine: (line) => out.appendLine(line),
    });

    if (result.cancelled) {
        void vscode.window.showWarningMessage(
            'ISC CAN: flash cancelled. Device may be in an intermediate state — re-run to recover.',
        );
        return;
    }
    if (result.exitCode === 0) {
        const report = parseFlashReport(result.stdout);
        announceSuccess(report, artifactPath);
        return;
    }
    announceFailure(result.exitCode, result.stderr);
}

// ---- Argv + UX helpers ----

function flashFlags(cfg: Config): string[] {
    const out: string[] = [];
    if (cfg.requireWrp) out.push('--require-wrp');
    if (cfg.applyWrp) out.push('--apply-wrp');
    if (cfg.profile) out.push('--profile');
    if (cfg.jumpAfterFlash) {
        out.push('--jump');
    } else {
        out.push('--no-jump');
    }
    return out;
}

function progressMessage(ev: FlashEvent): string {
    switch (ev.event) {
        case 'planning':
            return `planning sector ${ev.sector} (${ev.role})`;
        case 'erased':
            return `erased sector ${ev.sector}`;
        case 'written': {
            const pct = ev.total === 0 ? 0 : Math.floor((ev.bytes * 100) / ev.total);
            return `writing sector ${ev.sector}: ${pct}% (${ev.bytes}/${ev.total} B)`;
        }
        case 'verified':
            return `verified sector ${ev.sector} (crc=${ev.crc})`;
        case 'committing':
            return 'committing';
        case 'done':
            return `done in ${ev.duration_ms} ms`;
    }
}

function announceSuccess(report: FlashReport | null, artifactPath: string): void {
    const base = path.basename(artifactPath);
    if (report === null) {
        // Exit 0 but no parseable report — likely a dry-run or a
        // future report-schema change. Still a success; just less
        // informative.
        void vscode.window.showInformationMessage(
            `ISC CAN: flashed ${base} ✓ (no JSON report parsed; see output channel)`,
        );
        return;
    }
    const sectors = report.sectors_written.length;
    const skipped = report.sectors_skipped.length;
    void vscode.window.showInformationMessage(
        `ISC CAN: flashed ${base} ✓  ` +
            `${sectors} sector(s) written, ${skipped} skipped, ${report.duration_ms} ms.`,
    );
}

function announceFailure(exitCode: number | null, stderr: string): void {
    // Exit-code table from REQUIREMENTS.md § Output and CI integration.
    // Keep in sync with src/cli/mod.rs::ExitCodeHint.
    const hint = exitCodeHint(exitCode);
    const firstLine = stderr.split('\n').find((l) => l.trim().length > 0) ?? '';
    const detail = firstLine.length > 0 ? `\n${firstLine}` : '';
    void vscode.window.showErrorMessage(
        `ISC CAN: flash failed (exit ${exitCode ?? 'killed'}: ${hint}).${detail}  ` +
            `See ISC CAN output channel.`,
    );
}

function exitCodeHint(code: number | null): string {
    switch (code) {
        case 0:
            return 'ok';
        case 1:
            return 'flash error';
        case 2:
            return 'verification mismatch';
        case 3:
            return 'protection violation';
        case 4:
            return 'device not found / timeout';
        case 7:
            return 'WRP not applied';
        case 8:
            return 'input file error';
        case 9:
            return 'adapter not found';
        case 99:
            return 'generic error';
        case 130:
            return 'interrupted';
        default:
            return code === null ? 'killed' : `code ${code}`;
    }
}
