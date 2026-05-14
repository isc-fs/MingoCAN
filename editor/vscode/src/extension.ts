// ISC STM32 CAN Flasher — VS Code extension entry point.
//
// Every roadmap surface is now live.
//
//   Tier A — Build + Flash:
//     iscFs.flash, iscFs.flashWithoutBuild
//   Tier B — Device awareness:
//     iscFs.discover, iscFs.refreshDevices, iscFs.selectAdapter,
//     iscFs.devices (live tree data provider), iscFs.flashThisDevice
//   Tier C.1 — Diagnostics (one-shot):
//     iscFs.readDtcs, iscFs.clearDtcs, iscFs.health
//   Tier C.2 — Diagnostics (streaming):
//     iscFs.liveData (webview panel with Chart.js chart)
//
// All real work shells out to the `can-flasher` CLI in `--json`
// mode; we never speak the bootloader protocol directly.

import * as vscode from 'vscode';

import { readConfig } from './config';
import { runClearDtcs, runHealth, runReadDtcs } from './diagnose';
import { runFlash } from './flash';
import { LiveDataPanel } from './liveDataPanel';
import { selectAdapter } from './picker';
import { registerStatusBarItem } from './statusBar';
import { DeviceTreeProvider, type IscFsTreeNode } from './tree';
import { getOutputChannel, showOutputChannel } from './output';
import { formatNodeId } from './discover';

export function activate(context: vscode.ExtensionContext): void {
    // ---- Tier A (flash) ----
    context.subscriptions.push(
        vscode.commands.registerCommand('iscFs.flash', () =>
            runFlash({ skipBuild: false }),
        ),
        vscode.commands.registerCommand('iscFs.flashWithoutBuild', () =>
            runFlash({ skipBuild: true }),
        ),
    );

    // ---- Tier B (device awareness) ----
    const treeProvider = new DeviceTreeProvider();
    const treeView = vscode.window.createTreeView('iscFs.devices', {
        treeDataProvider: treeProvider,
        showCollapseAll: true,
    });
    context.subscriptions.push(treeView);

    context.subscriptions.push(
        vscode.commands.registerCommand('iscFs.refreshDevices', () =>
            treeProvider.refresh(),
        ),
        vscode.commands.registerCommand('iscFs.discover', async () => {
            // Palette `discover` reuses the tree's refresh path and
            // surfaces the output channel so keyboard-driven
            // operators get the same single source of truth.
            showOutputChannel();
            await treeProvider.refresh();
        }),
        vscode.commands.registerCommand('iscFs.selectAdapter', () => selectAdapter()),
        vscode.commands.registerCommand('iscFs.flashThisDevice', (node?: IscFsTreeNode) =>
            flashThisDevice(node),
        ),
    );

    // Status-bar item (Tier B): shows current adapter + node, click to re-pick.
    registerStatusBarItem(context);

    // ---- Tier C.1 (one-shot diagnostics) ----
    context.subscriptions.push(
        vscode.commands.registerCommand('iscFs.health', () => runHealth()),
        vscode.commands.registerCommand('iscFs.readDtcs', () => runReadDtcs()),
        vscode.commands.registerCommand('iscFs.clearDtcs', () => runClearDtcs()),
    );

    // ---- Tier C.2 (streaming diagnostics) ----
    //
    // Capture the *current* adapter at command-invocation time and
    // pass it through to the panel. The panel is keyed on
    // (interface, channel), so the operator can open the command,
    // switch the global setting to a different adapter, open the
    // command again, and end up with two panels streaming from two
    // boards independently.
    context.subscriptions.push(
        vscode.commands.registerCommand('iscFs.liveData', () => {
            const cfg = readConfig();
            LiveDataPanel.createOrShow(context, cfg.interface, cfg.channel);
        }),
    );
}

export function deactivate(): void {
    // No global cleanup — every long-lived resource (status bar
    // item, output channel, tree view) is registered in
    // `context.subscriptions` and disposed by the extension host.
}

// ---- Per-device "Flash this device…" context-menu handler ----

async function flashThisDevice(node?: IscFsTreeNode): Promise<void> {
    if (node === undefined || node.kind !== 'device') {
        void vscode.window.showInformationMessage(
            'ISC CAN: right-click a device in the ISC CAN Devices view, then choose "Flash this device…".',
        );
        return;
    }
    const id = formatNodeId(node.row.node_id);
    const cfg = vscode.workspace.getConfiguration('iscFs');
    // Stash the original node-id so we can restore it after the
    // flash completes. Workspace-scoped so we don't accidentally
    // pollute user (global) settings with a temporary override.
    const original = cfg.get<string>('nodeId', '');
    await cfg.update('nodeId', id, vscode.ConfigurationTarget.Workspace);
    getOutputChannel().appendLine(`[info] override iscFs.nodeId → ${id} for this run`);
    try {
        await runFlash({ skipBuild: false });
    } finally {
        await cfg.update(
            'nodeId',
            original.length > 0 ? original : undefined,
            vscode.ConfigurationTarget.Workspace,
        );
    }
}

