// Wrappers around `can-flasher diagnose …` for the three Tier C
// command handlers: read-dtc, clear-dtc, health.
//
// All three are one-shot commands that emit a single JSON document
// — perfect fit for the `runJson` helper in cli.ts. The streaming
// `live-data` and `log` subcommands are deliberately not handled
// here; those need a webview surface and ship in a follow-up.

import * as vscode from 'vscode';

import { buildGlobalArgv, type Config, readConfig } from './config';
import { runJson, spawnCommand } from './cli';
import { getOutputChannel, showOutputChannel } from './output';

// ---- Wire types (mirror src/cli/diagnose.rs) ----

export interface HealthRecord {
    uptime_seconds: number;
    reset_cause: string;
    reset_cause_raw: number;
    flash_write_count: number;
    dtc_count: number;
    last_dtc_code: number;
    session_active: boolean;
    valid_app_present: boolean;
    wrp_protected: boolean;
    raw_flags: number;
}

export interface DtcEntry {
    code: number;
    severity: string;
    severity_raw: number;
    occurrence_count: number;
    first_seen_uptime_seconds: number;
    last_seen_uptime_seconds: number;
    context_data: number;
}

// ---- Command handlers ----

export async function runHealth(): Promise<void> {
    const ctx = await prepareDiagnose('health');
    if (ctx === null) {
        return;
    }
    const { cfg, cwd, out } = ctx;
    showOutputChannel();
    out.appendLine('');
    out.appendLine(`═══ Session health · ${new Date().toISOString()} ═══`);

    const result = await runJson<HealthRecord>(
        cfg.canFlasherPath,
        [...buildGlobalArgv(cfg), 'diagnose', 'health'],
        cwd,
    );
    if (result.value === null) {
        announceDiagnoseFailure('health', result.exitCode, result.stderr);
        return;
    }
    out.appendLine(formatHealth(result.value));
}

export async function runReadDtcs(): Promise<void> {
    const ctx = await prepareDiagnose('read-dtc');
    if (ctx === null) {
        return;
    }
    const { cfg, cwd, out } = ctx;
    showOutputChannel();
    out.appendLine('');
    out.appendLine(`═══ DTCs · ${new Date().toISOString()} ═══`);

    const result = await runJson<DtcEntry[]>(
        cfg.canFlasherPath,
        [...buildGlobalArgv(cfg), 'diagnose', 'read-dtc'],
        cwd,
    );
    if (result.value === null) {
        announceDiagnoseFailure('read-dtc', result.exitCode, result.stderr);
        return;
    }
    out.appendLine(formatDtcs(result.value));
    if (result.value.length > 0) {
        const worstSeverity = pickWorstSeverity(result.value);
        const msg = `ISC MingoCAN: ${result.value.length} DTC(s) logged (worst: ${worstSeverity}).`;
        if (worstSeverity === 'ERROR' || worstSeverity === 'FATAL') {
            void vscode.window.showErrorMessage(msg);
        } else {
            void vscode.window.showWarningMessage(msg);
        }
    } else {
        void vscode.window.showInformationMessage('ISC MingoCAN: no DTCs logged.');
    }
}

export async function runClearDtcs(): Promise<void> {
    const ctx = await prepareDiagnose('clear-dtc');
    if (ctx === null) {
        return;
    }
    const { cfg, cwd, out } = ctx;

    // Modal confirmation — this destroys history on the device, so
    // a clear ack is non-negotiable. The CLI gates the same action
    // behind `--yes`; we pass that flag once the operator confirms
    // here.
    const proceed = 'Clear DTCs';
    const choice = await vscode.window.showWarningMessage(
        'ISC MingoCAN: clear every DTC entry on the device? This cannot be undone.',
        { modal: true },
        proceed,
    );
    if (choice !== proceed) {
        return;
    }

    showOutputChannel();
    out.appendLine('');
    out.appendLine(`═══ Clear DTCs · ${new Date().toISOString()} ═══`);

    const result = await spawnCommand(
        cfg.canFlasherPath,
        [...buildGlobalArgv(cfg), 'diagnose', 'clear-dtc', '--yes'],
        {
            cwd,
            onStdoutLine: (line) => out.appendLine(line),
            onStderrLine: (line) => out.appendLine(line),
        },
    );
    if (result.exitCode === 0) {
        void vscode.window.showInformationMessage('ISC MingoCAN: DTCs cleared.');
    } else {
        announceDiagnoseFailure('clear-dtc', result.exitCode, result.stderr);
    }
}

// ---- Shared setup ----

interface DiagnoseContext {
    cfg: Config;
    cwd: string;
    out: vscode.OutputChannel;
}

async function prepareDiagnose(label: string): Promise<DiagnoseContext | null> {
    const workspace = vscode.workspace.workspaceFolders?.[0];
    if (workspace === undefined) {
        void vscode.window.showErrorMessage(
            `ISC MingoCAN: open a workspace folder before running diagnose ${label}.`,
        );
        return null;
    }
    const cfg = readConfig();
    if (cfg.channel.length === 0 && cfg.interface !== 'virtual') {
        const select = 'Select adapter…';
        const choice = await vscode.window.showErrorMessage(
            `ISC MingoCAN: no adapter selected. Pick one before running diagnose ${label}.`,
            select,
        );
        if (choice === select) {
            await vscode.commands.executeCommand('iscFs.selectAdapter');
        }
        return null;
    }
    return { cfg, cwd: workspace.uri.fsPath, out: getOutputChannel() };
}

function announceDiagnoseFailure(
    label: string,
    exitCode: number | null,
    stderr: string,
): void {
    const firstLine = stderr.split('\n').find((l) => l.trim().length > 0) ?? '';
    const detail = firstLine.length > 0 ? `\n${firstLine}` : '';
    void vscode.window.showErrorMessage(
        `ISC MingoCAN: diagnose ${label} failed (exit ${exitCode ?? 'killed'}).${detail}  ` +
            `See ISC CAN output channel.`,
    );
}

// ---- Formatters ----

export function formatHealth(h: HealthRecord): string {
    const lines: string[] = [];
    lines.push(`  Uptime         : ${h.uptime_seconds} s (${formatUptime(h.uptime_seconds)})`);
    lines.push(`  Reset cause    : ${h.reset_cause}`);
    lines.push(`  Session active : ${h.session_active ? 'yes' : 'no'}`);
    lines.push(`  Valid app      : ${h.valid_app_present ? 'yes' : 'no'}`);
    lines.push(`  WRP protected  : ${h.wrp_protected ? 'yes' : 'no'}`);
    lines.push(`  Flash writes   : ${h.flash_write_count}`);
    lines.push(`  DTC count      : ${h.dtc_count}`);
    lines.push(`  Last DTC code  : 0x${h.last_dtc_code.toString(16).toUpperCase().padStart(4, '0')}`);
    lines.push(`  Raw flags      : 0x${h.raw_flags.toString(16).toUpperCase().padStart(8, '0')}`);
    return lines.join('\n');
}

export function formatDtcs(entries: DtcEntry[]): string {
    if (entries.length === 0) {
        return '  No DTCs logged.';
    }
    const lines: string[] = [];
    lines.push(`  ${entries.length} DTC(s) logged:`);
    lines.push('');
    lines.push(
        '  ' + padCols(['Code', 'Severity', 'Count', 'First seen', 'Last seen', 'Context']),
    );
    lines.push('  ' + padCols(['──────', '────────', '─────', '──────────', '──────────', '──────────']));
    for (const e of entries) {
        lines.push(
            '  ' +
                padCols([
                    `0x${e.code.toString(16).toUpperCase().padStart(4, '0')}`,
                    e.severity,
                    String(e.occurrence_count),
                    `${e.first_seen_uptime_seconds}s`,
                    `${e.last_seen_uptime_seconds}s`,
                    `0x${e.context_data.toString(16).toUpperCase().padStart(8, '0')}`,
                ]),
        );
    }
    return lines.join('\n');
}

const COL_WIDTHS = [8, 10, 7, 12, 12, 12];

function padCols(cells: readonly string[]): string {
    return cells.map((c, i) => c.padEnd(COL_WIDTHS[i] ?? 0)).join(' ');
}

function formatUptime(s: number): string {
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = s % 60;
    if (h > 0) {
        return `${h}h${String(m).padStart(2, '0')}m${String(sec).padStart(2, '0')}s`;
    }
    if (m > 0) {
        return `${m}m${String(sec).padStart(2, '0')}s`;
    }
    return `${sec}s`;
}

const SEVERITY_RANK: Record<string, number> = {
    INFO: 0,
    WARN: 1,
    ERROR: 2,
    FATAL: 3,
};

function pickWorstSeverity(entries: readonly DtcEntry[]): string {
    let worst = entries[0]?.severity ?? 'INFO';
    let worstRank = SEVERITY_RANK[worst] ?? -1;
    for (const e of entries) {
        const rank = SEVERITY_RANK[e.severity] ?? -1;
        if (rank > worstRank) {
            worst = e.severity;
            worstRank = rank;
        }
    }
    return worst;
}
