// Left-aligned status-bar items giving the operator one-click
// access to the three most-used actions, without opening the
// command palette:
//
//   $(plug) <iface>:<channel> → <node>     ← click to switch adapter
//   $(rocket) Flash                         ← click to build + flash
//   $(tools) Tools                          ← click to open the dashboard panel
//
// Priorities are descending so they render left-to-right in that
// order. Subscribes to `iscFs.*` configuration changes so the
// adapter pill updates as soon as the operator switches adapters
// via the picker, edits settings.json, or runs
// `iscFs.selectAdapter` from the palette.

import * as vscode from 'vscode';

import { readConfig } from './config';

let adapterItem: vscode.StatusBarItem | undefined;
let flashItem: vscode.StatusBarItem | undefined;
let toolsItem: vscode.StatusBarItem | undefined;

export function registerStatusBarItem(context: vscode.ExtensionContext): void {
    // Adapter pill — leftmost, highest priority number renders first.
    adapterItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 50);
    adapterItem.command = 'iscFs.selectAdapter';
    adapterItem.name = 'ISC MingoCAN: current adapter';
    adapterItem.tooltip = 'Click to switch CAN adapter';
    adapterItem.show();
    context.subscriptions.push(adapterItem);

    // Flash button — one click runs the same `iscFs.flash` command
    // the palette exposes (build → flash via CAN → optional jump).
    flashItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 49);
    flashItem.command = 'iscFs.flash';
    flashItem.name = 'ISC MingoCAN: flash';
    flashItem.text = '$(rocket) Flash';
    flashItem.tooltip = 'Build firmware and flash it to the selected device over CAN';
    flashItem.show();
    context.subscriptions.push(flashItem);

    // Tools panel — opens the dashboard webview with every
    // action surface side-by-side. Last so it's the rightmost.
    toolsItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 48);
    toolsItem.command = 'iscFs.openTools';
    toolsItem.name = 'ISC MingoCAN: tools';
    toolsItem.text = '$(tools) Tools';
    toolsItem.tooltip = 'Open the ISC MingoCAN tools panel';
    toolsItem.show();
    context.subscriptions.push(toolsItem);

    updateAdapterPill();

    context.subscriptions.push(
        vscode.workspace.onDidChangeConfiguration((event) => {
            if (event.affectsConfiguration('iscFs')) {
                updateAdapterPill();
            }
        }),
    );
}

function updateAdapterPill(): void {
    if (adapterItem === undefined) {
        return;
    }
    const cfg = readConfig();
    // $(plug) is a built-in codicon; $(circle-slash) signals "no
    // channel set" — operator hasn't picked an adapter yet.
    if (cfg.channel.length === 0 && cfg.interface !== 'virtual') {
        adapterItem.text = '$(circle-slash) ISC MingoCAN: no adapter';
        adapterItem.backgroundColor = new vscode.ThemeColor(
            'statusBarItem.warningBackground',
        );
        return;
    }
    const interfaceLabel = cfg.interface === 'virtual' ? 'virtual' : cfg.interface;
    const channelLabel = cfg.channel.length > 0 ? cfg.channel : '—';
    const nodeLabel = cfg.nodeId.length > 0 ? ` → ${cfg.nodeId}` : '';
    adapterItem.text = `$(plug) ${interfaceLabel}: ${channelLabel}${nodeLabel}`;
    adapterItem.backgroundColor = undefined;
}
