// `iscFs.doctor` — one-shot environment triage.
//
// The extension "not always working" almost always reduces to one of a
// handful of environment faults: the CLI didn't resolve (or resolved to
// a skewed version), no adapter is selected (or the selected one fell
// off the bus), no node-id is set, or the build config points at nothing.
// Rather than making the operator discover each of these the hard way
// mid-flash, `doctor` checks them all up front and prints a single
// pass/warn/fail report with a remediation button for the first blocker.

import * as vscode from 'vscode';

import { fetchAdapters, type AdapterEntry } from './adapters';
import { cliVersion } from './cliManager';
import { DEFAULT_BARE_NAME } from './cliPath';
import { readConfig, type Config } from './config';
import { getOutputChannel, showOutputChannel } from './output';

type CheckStatus = 'ok' | 'warn' | 'fail';

interface Check {
    name: string;
    status: CheckStatus;
    detail: string;
}

/** A remediation offered in the summary toast — label + the command to
 *  run when the operator clicks it. Only the first blocker's action is
 *  surfaced, to keep the toast to a single decisive button. */
interface Remediation {
    label: string;
    command: string;
    arg?: string;
}

const GLYPH: Record<CheckStatus, string> = { ok: '✓', warn: '⚠', fail: '✗' };

// Team node-id scheme, mirroring `PROVISION_ROLES` in flash.ts and the
// `iscFs.nodeId` description in package.json.
const ROLE_BY_ID: Record<number, string> = { 0x1: 'ECU', 0x2: 'AMS', 0x3: 'uDV' };

export async function runDoctor(context: vscode.ExtensionContext): Promise<void> {
    const out = getOutputChannel();
    showOutputChannel();
    out.appendLine('');
    out.appendLine(`═══ ISC MingoCAN doctor · ${new Date().toISOString()} ═══`);

    const workspace = vscode.workspace.workspaceFolders?.[0];
    const cfg = readConfig();

    const checks: Check[] = [];
    const remediations: Remediation[] = [];

    // ---- 1. CLI binary + version ----
    const expected = extensionVersion(context);
    const configured = vscode.workspace
        .getConfiguration('iscFs')
        .get<string>('canFlasherPath', DEFAULT_BARE_NAME);
    const source =
        configured !== DEFAULT_BARE_NAME
            ? 'pinned'
            : cfg.canFlasherPath !== DEFAULT_BARE_NAME
              ? 'resolved'
              : 'PATH';
    const actual = await cliVersion(cfg.canFlasherPath);
    if (actual === null) {
        checks.push({
            name: 'CLI binary',
            status: 'fail',
            detail: `can-flasher couldn't run (${source}: ${cfg.canFlasherPath}). Install it or point iscFs.canFlasherPath at it.`,
        });
        remediations.push({
            label: 'CLI settings…',
            command: 'workbench.action.openSettings',
            arg: 'iscFs.canFlasherPath',
        });
    } else if (expected !== null && actual !== expected) {
        checks.push({
            name: 'CLI binary',
            status: 'warn',
            detail: `v${actual} (${source}: ${cfg.canFlasherPath}) — extension expects v${expected}. Flashing may hit bugs already fixed.`,
        });
        remediations.push({ label: 'Use CLI on PATH…', command: 'iscFs.useCliOnPath' });
    } else {
        const suffix = expected === null ? '' : ` (matches extension)`;
        checks.push({
            name: 'CLI binary',
            status: 'ok',
            detail: `v${actual}${suffix} — ${source}: ${cfg.canFlasherPath}`,
        });
    }

    // ---- 2. Workspace ----
    if (workspace === undefined) {
        checks.push({
            name: 'Workspace',
            status: 'fail',
            detail: 'No folder open. Flash/discover need a workspace folder as the working directory.',
        });
    } else {
        checks.push({
            name: 'Workspace',
            status: 'ok',
            detail: workspace.uri.fsPath,
        });
    }

    // ---- 3. Adapter (config + live presence) ----
    if (cfg.interface === 'virtual') {
        checks.push({
            name: 'Adapter',
            status: 'ok',
            detail: 'virtual bus (no hardware needed)',
        });
    } else if (cfg.channel.trim().length === 0) {
        checks.push({
            name: 'Adapter',
            status: 'fail',
            detail: 'No adapter selected. Pick one before flashing or diagnosing.',
        });
        remediations.push({ label: 'Select adapter…', command: 'iscFs.selectAdapter' });
    } else if (workspace === undefined || actual === null) {
        // Can't probe the bus without a workspace + working CLI; report
        // what's configured and defer the presence check.
        checks.push({
            name: 'Adapter',
            status: 'warn',
            detail: `${cfg.interface}: ${cfg.channel} configured — bus presence not checked (needs a workspace + working CLI).`,
        });
    } else {
        let present: boolean | null;
        try {
            const found = await fetchAdapters(cfg, workspace.uri.fsPath);
            present = found.some((e) => isSameAdapter(e, cfg));
        } catch {
            present = null;
        }
        if (present === true) {
            checks.push({
                name: 'Adapter',
                status: 'ok',
                detail: `${cfg.interface}: ${cfg.channel} — present on the bus`,
            });
        } else if (present === false) {
            checks.push({
                name: 'Adapter',
                status: 'warn',
                detail: `${cfg.interface}: ${cfg.channel} configured but not detected. Plug it back in or re-select.`,
            });
            remediations.push({ label: 'Select adapter…', command: 'iscFs.selectAdapter' });
        } else {
            checks.push({
                name: 'Adapter',
                status: 'warn',
                detail: `${cfg.interface}: ${cfg.channel} configured — adapter probe failed (see output channel).`,
            });
        }
    }

    // ---- 4. Target node-id ----
    const node = parseNodeId(cfg.nodeId);
    if (cfg.nodeId.trim().length === 0) {
        checks.push({
            name: 'Target node-id',
            status: 'warn',
            detail: 'Not set — you\'ll be prompted at flash time (and the answer saved). Set it to skip the prompt.',
        });
    } else if (node === null) {
        checks.push({
            name: 'Target node-id',
            status: 'warn',
            detail: `"${cfg.nodeId}" isn't a valid hex (0x0–0xF) or decimal id.`,
        });
    } else {
        const role = ROLE_BY_ID[node];
        checks.push({
            name: 'Target node-id',
            status: 'ok',
            detail: `0x${node.toString(16)}${role !== undefined ? ` (${role})` : ''}`,
        });
    }

    // ---- 5. Build config + artifact ----
    if (workspace !== undefined) {
        const buildDetail =
            cfg.buildCommand.trim().length === 0
                ? 'build step disabled (empty iscFs.buildCommand) — flashing uses a pre-built artifact'
                : `\`${cfg.buildCommand}\``;
        let artifact: vscode.Uri | null = null;
        try {
            const matches = await vscode.workspace.findFiles(
                new vscode.RelativePattern(workspace, cfg.firmwareArtifact),
                '**/{node_modules,.git}/**',
                1,
            );
            artifact = matches[0] ?? null;
        } catch {
            artifact = null;
        }
        if (artifact !== null) {
            checks.push({
                name: 'Firmware artifact',
                status: 'ok',
                detail: `${vscode.workspace.asRelativePath(artifact)} (matches \`${cfg.firmwareArtifact}\`)`,
            });
        } else if (cfg.buildCommand.trim().length === 0) {
            checks.push({
                name: 'Firmware artifact',
                status: 'warn',
                detail: `No file matches \`${cfg.firmwareArtifact}\` and the build step is off — nothing to flash.`,
            });
        } else {
            checks.push({
                name: 'Firmware artifact',
                status: 'ok',
                detail: `none yet — will be produced by the build (${buildDetail})`,
            });
        }
    }

    // ---- Render ----
    for (const c of checks) {
        out.appendLine(`  ${GLYPH[c.status]} ${c.name.padEnd(18)} ${c.detail}`);
    }

    const fails = checks.filter((c) => c.status === 'fail').length;
    const warns = checks.filter((c) => c.status === 'warn').length;
    out.appendLine('');
    out.appendLine(
        `  ${checks.length - fails - warns} ok · ${warns} warning(s) · ${fails} error(s)`,
    );

    await announceSummary(fails, warns, remediations);
}

// ---- Summary toast ----

async function announceSummary(
    fails: number,
    warns: number,
    remediations: Remediation[],
): Promise<void> {
    const action = remediations[0];
    const buttons = action !== undefined ? [action.label] : [];

    if (fails > 0) {
        const choice = await vscode.window.showErrorMessage(
            `ISC MingoCAN doctor: ${fails} blocker(s), ${warns} warning(s). See the output channel.`,
            ...buttons,
        );
        runRemediation(choice, action);
        return;
    }
    if (warns > 0) {
        const choice = await vscode.window.showWarningMessage(
            `ISC MingoCAN doctor: ready to flash, ${warns} warning(s). See the output channel.`,
            ...buttons,
        );
        runRemediation(choice, action);
        return;
    }
    void vscode.window.showInformationMessage(
        'ISC MingoCAN doctor: all checks passed — ready to flash.',
    );
}

function runRemediation(choice: string | undefined, action?: Remediation): void {
    if (action === undefined || choice !== action.label) {
        return;
    }
    if (action.arg !== undefined) {
        void vscode.commands.executeCommand(action.command, action.arg);
    } else {
        void vscode.commands.executeCommand(action.command);
    }
}

// ---- Helpers ----

function extensionVersion(context: vscode.ExtensionContext): string | null {
    const v = (context.extension.packageJSON as { version?: unknown }).version;
    return typeof v === 'string' ? v : null;
}

/** Parse `iscFs.nodeId` — hex (`0x1`) or decimal (`2`). Returns `null`
 *  for empty/garbage. Matches the CLI's `--node-id` acceptance. */
function parseNodeId(raw: string): number | null {
    const s = raw.trim();
    if (s.length === 0) {
        return null;
    }
    const n = s.toLowerCase().startsWith('0x')
        ? Number.parseInt(s.slice(2), 16)
        : Number.parseInt(s, 10);
    return Number.isInteger(n) && n >= 0 ? n : null;
}

function isSameAdapter(entry: AdapterEntry, cfg: Config): boolean {
    return entry.interface === cfg.interface && entry.channel === cfg.channel;
}
