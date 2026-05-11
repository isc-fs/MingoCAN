// ISC STM32 CAN Flasher — VS Code extension entry point.
//
// Tier A is live: `iscFs.flash` and `iscFs.flashWithoutBuild` run
// real `can-flasher` invocations with progress reporting and exit-
// code-aware error toasts. The rest of the commands are still v0
// stubs and surface a "not implemented yet" toast — they land in
// follow-up Tier B / Tier C PRs.
//
//   Tier A (live):
//     iscFs.flash, iscFs.flashWithoutBuild
//   Tier B (next):
//     iscFs.discover, iscFs.refreshDevices, iscFs.selectAdapter,
//     iscFs.devices (tree data provider)
//   Tier C (later):
//     iscFs.readDtcs, iscFs.clearDtcs, iscFs.health
//
// All real work shells out to the `can-flasher` CLI in `--json`
// mode; we never speak the bootloader protocol directly.

import * as vscode from 'vscode';

import { runFlash } from './flash';

interface StubCommand {
    readonly id: string;
    readonly label: string;
}

const STUB_COMMANDS: ReadonlyArray<StubCommand> = [
    { id: 'iscFs.discover', label: 'Discover devices' },
    { id: 'iscFs.selectAdapter', label: 'Select adapter' },
    { id: 'iscFs.refreshDevices', label: 'Refresh device list' },
    { id: 'iscFs.readDtcs', label: 'Read DTCs' },
    { id: 'iscFs.clearDtcs', label: 'Clear DTCs' },
    { id: 'iscFs.health', label: 'Session health' },
];

export function activate(context: vscode.ExtensionContext): void {
    // Tier A — real handlers.
    context.subscriptions.push(
        vscode.commands.registerCommand('iscFs.flash', () =>
            runFlash({ skipBuild: false }),
        ),
        vscode.commands.registerCommand('iscFs.flashWithoutBuild', () =>
            runFlash({ skipBuild: true }),
        ),
    );

    // Tier B + C — still stubs.
    for (const cmd of STUB_COMMANDS) {
        context.subscriptions.push(
            vscode.commands.registerCommand(cmd.id, () => notImplemented(cmd.label)),
        );
    }

    // Stub tree until Tier B replaces it with a live device list.
    context.subscriptions.push(
        vscode.window.registerTreeDataProvider(
            'iscFs.devices',
            new StubDeviceTreeProvider(),
        ),
    );
}

export function deactivate(): void {
    // Output channel disposal is handled via `context.subscriptions`
    // (the OutputChannel created in output.ts is implicitly disposed
    // when the extension host unloads).
}

function notImplemented(label: string): void {
    void vscode.window.showInformationMessage(
        `ISC CAN — ${label}: not implemented yet. ` +
            `Tier B (device awareness) and Tier C (diagnostics) land in follow-up PRs.`,
    );
}

class StubDeviceTreeProvider implements vscode.TreeDataProvider<never> {
    getTreeItem(_element: never): vscode.TreeItem {
        throw new Error('StubDeviceTreeProvider has no items');
    }

    getChildren(_element?: never): Thenable<never[]> {
        return Promise.resolve([]);
    }
}
