// ISC STM32 CAN Flasher — VS Code extension entry point.
//
// This is the v0 sketch: every command is wired into the VS Code
// command palette and the Explorer view container is registered, but
// none of them do real work yet — they each pop a "not implemented"
// toast. Subsequent PRs replace the stub handlers one at a time:
//
//   Tier A (Build + Flash):
//     iscFs.flash, iscFs.flashWithoutBuild
//   Tier B (Device awareness):
//     iscFs.discover, iscFs.refreshDevices, iscFs.selectAdapter,
//     iscFs.devices (tree data provider)
//   Tier C (Diagnostics, post-MVP):
//     iscFs.readDtcs, iscFs.clearDtcs, iscFs.health
//
// All real work shells out to the `can-flasher` CLI (`--json` mode
// for structured output). The extension never speaks the bootloader
// protocol directly — `can-flasher` is the single source of truth
// for wire-format and exit-code semantics.

import * as vscode from 'vscode';

const COMMANDS: ReadonlyArray<readonly [string, string]> = [
    ['iscFs.flash', 'Build & Flash'],
    ['iscFs.flashWithoutBuild', 'Flash (skip build)'],
    ['iscFs.discover', 'Discover devices'],
    ['iscFs.selectAdapter', 'Select adapter'],
    ['iscFs.refreshDevices', 'Refresh device list'],
    ['iscFs.readDtcs', 'Read DTCs'],
    ['iscFs.clearDtcs', 'Clear DTCs'],
    ['iscFs.health', 'Session health'],
];

export function activate(context: vscode.ExtensionContext): void {
    for (const [id, label] of COMMANDS) {
        context.subscriptions.push(
            vscode.commands.registerCommand(id, () => notImplemented(label)),
        );
    }

    const devices = new StubDeviceTreeProvider();
    context.subscriptions.push(
        vscode.window.registerTreeDataProvider('iscFs.devices', devices),
    );
}

export function deactivate(): void {
    // Nothing to tear down in the sketch. Real handlers that spawn
    // long-running child processes (log streaming, live-data) will
    // register disposables here in later PRs.
}

function notImplemented(label: string): void {
    void vscode.window.showInformationMessage(
        `ISC CAN — ${label}: not implemented yet. ` +
            `This is the v0 sketch; the next PR wires it to can-flasher.`,
    );
}

/**
 * Placeholder so the `iscFs.devices` view renders an empty state
 * (with the welcome view's default "no devices detected" message)
 * rather than throwing. Real implementation in Tier B PR.
 */
class StubDeviceTreeProvider implements vscode.TreeDataProvider<never> {
    getTreeItem(_element: never): vscode.TreeItem {
        throw new Error('StubDeviceTreeProvider has no items');
    }

    getChildren(_element?: never): Thenable<never[]> {
        return Promise.resolve([]);
    }
}
