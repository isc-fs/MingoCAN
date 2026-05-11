// `iscFs.selectAdapter` — quick-pick across every detected adapter.
//
// Writes `iscFs.interface` + `iscFs.channel` to **workspace**
// settings by default (so the choice moves with the project and
// teammates pulling the repo inherit the same default). A secondary
// action on each item ($(gear)) saves to **user** settings instead,
// for the single-developer-with-one-CANable case.

import * as vscode from 'vscode';

import { fetchAdapters, isActive, type AdapterEntry } from './adapters';
import { readConfig } from './config';
import { getOutputChannel } from './output';

interface AdapterQuickPickItem extends vscode.QuickPickItem {
    entry?: AdapterEntry;
}

export async function selectAdapter(): Promise<void> {
    const workspace = vscode.workspace.workspaceFolders?.[0];
    if (workspace === undefined) {
        void vscode.window.showErrorMessage(
            'ISC CAN: open a workspace folder before selecting an adapter — the choice is saved to `.vscode/settings.json`.',
        );
        return;
    }
    const cwd = workspace.uri.fsPath;
    const cfg = readConfig();
    const out = getOutputChannel();

    const entries = await vscode.window.withProgress(
        {
            location: vscode.ProgressLocation.Window,
            title: 'ISC CAN: detecting adapters',
        },
        () => fetchAdapters(cfg, cwd),
    );

    const items: AdapterQuickPickItem[] = entries.map((entry) => ({
        label: entry.label,
        description: `${entry.interface} · ${entry.channel}`,
        detail: entry.detail,
        picked: isActive(entry, cfg),
        entry,
    }));

    // Always offer "Virtual" — it isn't enumerated by `adapters` (it's
    // an in-process loopback) but it's the canonical no-hardware
    // smoke-test target.
    items.push({
        label: 'Virtual (in-process loopback)',
        description: 'virtual',
        detail: 'No hardware — runs against an in-process stub bootloader for smoke tests',
        picked: cfg.interface === 'virtual',
        entry: { interface: 'virtual', channel: '', label: 'Virtual', detail: undefined },
    });

    if (items.length === 1) {
        // Only the virtual fallback — nothing was detected.
        const retry = 'Refresh';
        const openSettings = 'Open settings';
        const pick = await vscode.window.showWarningMessage(
            'ISC CAN: no hardware adapters detected. Plug one in and refresh, or configure manually in settings.',
            retry,
            openSettings,
        );
        if (pick === retry) {
            return selectAdapter();
        }
        if (pick === openSettings) {
            await vscode.commands.executeCommand(
                'workbench.action.openSettings',
                'iscFs.interface',
            );
        }
        return;
    }

    const choice = await vscode.window.showQuickPick(items, {
        title: 'ISC CAN: pick a CAN adapter',
        placeHolder: 'Currently active adapter is pre-selected',
        matchOnDescription: true,
        matchOnDetail: true,
    });
    if (choice?.entry === undefined) {
        return;
    }

    const scope = await pickScope();
    if (scope === null) {
        return;
    }

    await applyAdapter(choice.entry, scope);
    out.appendLine(
        `[info] saved adapter selection to ${scopeName(scope)} settings: ` +
            `--interface ${choice.entry.interface} --channel ${choice.entry.channel}`,
    );
    void vscode.window.showInformationMessage(
        `ISC CAN: adapter set to ${choice.entry.label} (${scopeName(scope)} scope).`,
    );
}

// ---- Scope picker ----

async function pickScope(): Promise<vscode.ConfigurationTarget | null> {
    const workspace = 'Save to workspace settings (recommended)';
    const user = 'Save to user (global) settings';
    const pick = await vscode.window.showQuickPick(
        [
            {
                label: workspace,
                detail: 'Persists in .vscode/settings.json — moves with the project',
            },
            {
                label: user,
                detail: 'Persists across every VS Code window for this user',
            },
        ],
        {
            title: 'Where should this selection be saved?',
            placeHolder: 'Workspace is the default for team workflows',
        },
    );
    if (pick === undefined) {
        return null;
    }
    return pick.label === workspace
        ? vscode.ConfigurationTarget.Workspace
        : vscode.ConfigurationTarget.Global;
}

function scopeName(target: vscode.ConfigurationTarget): string {
    return target === vscode.ConfigurationTarget.Workspace ? 'workspace' : 'user';
}

// ---- Settings writer ----

export async function applyAdapter(
    entry: AdapterEntry,
    scope: vscode.ConfigurationTarget,
): Promise<void> {
    const cfg = vscode.workspace.getConfiguration('iscFs');
    await cfg.update('interface', entry.interface, scope);
    await cfg.update('channel', entry.channel, scope);
}
