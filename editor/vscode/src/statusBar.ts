// Left-aligned status-bar item showing the current adapter and node
// selection at a glance. Clicking it opens the adapter picker.
//
// Subscribes to `iscFs.*` configuration changes so the label
// updates as soon as the operator switches adapters via the
// picker, edits settings.json, or runs `iscFs.selectAdapter` from
// the palette.

import * as vscode from 'vscode';

import { readConfig } from './config';

let item: vscode.StatusBarItem | undefined;

export function registerStatusBarItem(context: vscode.ExtensionContext): void {
    item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 50);
    item.command = 'iscFs.selectAdapter';
    item.name = 'ISC CAN: current adapter';
    item.tooltip = 'Click to switch CAN adapter';
    update();
    item.show();

    context.subscriptions.push(item);
    context.subscriptions.push(
        vscode.workspace.onDidChangeConfiguration((event) => {
            if (event.affectsConfiguration('iscFs')) {
                update();
            }
        }),
    );
}

function update(): void {
    if (item === undefined) {
        return;
    }
    const cfg = readConfig();
    // $(plug) is a built-in codicon; $(circle-slash) signals "no
    // channel set" — operator hasn't picked an adapter yet.
    if (cfg.channel.length === 0 && cfg.interface !== 'virtual') {
        item.text = '$(circle-slash) ISC CAN: no adapter';
        item.backgroundColor = new vscode.ThemeColor('statusBarItem.warningBackground');
        return;
    }
    const interfaceLabel = cfg.interface === 'virtual' ? 'virtual' : cfg.interface;
    const channelLabel = cfg.channel.length > 0 ? cfg.channel : '—';
    const nodeLabel = cfg.nodeId.length > 0 ? ` → ${cfg.nodeId}` : '';
    item.text = `$(plug) ${interfaceLabel}: ${channelLabel}${nodeLabel}`;
    item.backgroundColor = undefined;
}
