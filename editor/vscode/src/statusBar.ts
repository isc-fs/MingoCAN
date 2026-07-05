// Left-aligned status-bar items giving the operator one-click
// access to the three most-used actions, without opening the
// command palette:
//
//   $(plug) <iface>:<channel> → <node>     ← click to switch adapter
//   $(zap) Build + Flash                    ← click to build then flash
//   $(tools) Tools                          ← click to open the dashboard panel
//
// Priorities are descending so they render left-to-right in that
// order. Subscribes to `iscFs.*` configuration changes so the
// adapter pill updates as soon as the operator switches adapters
// via the picker, edits settings.json, or runs
// `iscFs.selectAdapter` from the palette.

import * as vscode from 'vscode';

import {
    currentSnapshot,
    onDidChangePresence,
    type PresenceSnapshot,
} from './adapterPresence';
import { readConfig } from './config';

let adapterItem: vscode.StatusBarItem | undefined;
let flashItem: vscode.StatusBarItem | undefined;
let toolsItem: vscode.StatusBarItem | undefined;

const FLASH_TOOLTIP_BASE =
    'Run `iscFs.buildCommand` (default `cmake --build build`) and then flash ' +
    'the produced artifact to the selected device over CAN.';
const FLASH_IDLE_TEXT = '$(zap) Build + Flash';
let cliInfo = '';

/**
 * Drive the Flash status-bar item's live state during a flash run.
 * `setFlashBusy` swaps the label for a spinner + a terse stage
 * ("$(sync~spin) Writing…") so the operator can watch progress from the
 * status bar without keeping the notification toast in focus;
 * `setFlashIdle` restores the clickable "Build + Flash" label. Both
 * no-op before the item is registered. Callers MUST pair every
 * `setFlashBusy` with a `setFlashIdle` (flash.ts does this in a
 * `finally`).
 */
export function setFlashBusy(stage: string): void {
    if (flashItem !== undefined) {
        flashItem.text = `$(sync~spin) ${stage}`;
    }
}

export function setFlashIdle(): void {
    if (flashItem !== undefined) {
        flashItem.text = FLASH_IDLE_TEXT;
    }
}

/**
 * Surface which `can-flasher` binary actually resolved (the managed
 * globalStorage download silently wins over PATH — see cliPath.ts — and
 * that has bitten operators). Appended to the Flash button's tooltip.
 * Called from `bootstrapCli` once resolution settles.
 */
export function setCliInfo(info: string): void {
    cliInfo = info;
    if (flashItem !== undefined) {
        flashItem.tooltip =
            cliInfo.length > 0
                ? `${FLASH_TOOLTIP_BASE}\n\nCLI: ${cliInfo}`
                : FLASH_TOOLTIP_BASE;
    }
}

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
    flashItem.name = 'ISC MingoCAN: build + flash';
    // $(zap) is the lightning-bolt codicon — universally "flash"
    // even for non-English operators. The "Build + Flash" label is
    // explicit so nobody confuses it with `iscFs.flashWithoutBuild`.
    flashItem.text = FLASH_IDLE_TEXT;
    flashItem.tooltip = FLASH_TOOLTIP_BASE;
    flashItem.show();
    context.subscriptions.push(flashItem);
    // Re-apply any CLI info that resolved before this item existed.
    setCliInfo(cliInfo);

    // Tools panel — opens the dashboard webview with every
    // action surface side-by-side. Last so it's the rightmost.
    toolsItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 48);
    // VS Code auto-generates a `<viewId>.focus` command for every
    // contributed view, so focusing the activity-bar sidebar
    // without registering anything ourselves is just this string.
    toolsItem.command = 'iscFs.tools.focus';
    toolsItem.name = 'ISC MingoCAN: tools';
    toolsItem.text = '$(tools) Tools';
    toolsItem.tooltip = 'Reveal the ISC MingoCAN tools sidebar';
    toolsItem.show();
    context.subscriptions.push(toolsItem);

    updateAdapterPill(currentSnapshot());

    context.subscriptions.push(
        vscode.workspace.onDidChangeConfiguration((event) => {
            if (event.affectsConfiguration('iscFs')) {
                updateAdapterPill(currentSnapshot());
            }
        }),
    );

    // Presence-driven updates: when the adapterPresence service
    // (re)checks the hardware, the pill flips between "present",
    // "disconnected" (red warning), and "unknown" (no decoration).
    context.subscriptions.push(onDidChangePresence(updateAdapterPill));
}

function updateAdapterPill(presence: PresenceSnapshot): void {
    if (adapterItem === undefined) {
        return;
    }
    const cfg = readConfig();
    // No channel configured yet — operator hasn't picked an adapter
    // (or the picker run didn't finish). `$(circle-slash)` glyph,
    // warning background.
    if (cfg.channel.length === 0 && cfg.interface !== 'virtual') {
        adapterItem.text = '$(circle-slash) ISC MingoCAN: no adapter';
        adapterItem.tooltip = 'Click to pick a CAN adapter';
        adapterItem.backgroundColor = new vscode.ThemeColor(
            'statusBarItem.warningBackground',
        );
        return;
    }
    const interfaceLabel = cfg.interface === 'virtual' ? 'virtual' : cfg.interface;
    const channelLabel = cfg.channel.length > 0 ? cfg.channel : '—';
    const nodeLabel = cfg.nodeId.length > 0 ? ` → ${cfg.nodeId}` : '';

    if (presence.presence === 'disconnected') {
        // Hardware that was configured isn't on the bus anymore —
        // flag it loudly so the operator notices before the next
        // click is about to talk to nothing. `$(debug-disconnect)`
        // is the dedicated disconnected glyph in codicons.
        adapterItem.text = `$(debug-disconnect) ${interfaceLabel}: ${channelLabel} (disconnected)`;
        adapterItem.tooltip = `${interfaceLabel}: ${channelLabel} is no longer on the bus. Plug it back in or pick a different adapter.`;
        adapterItem.backgroundColor = new vscode.ThemeColor(
            'statusBarItem.warningBackground',
        );
        return;
    }

    // `present` and `unknown` both render the normal pill; the
    // `unknown` case is brief (cold start, CLI hiccup), and
    // showing a transient warning there is more confusing than helpful.
    adapterItem.text = `$(plug) ${interfaceLabel}: ${channelLabel}${nodeLabel}`;
    adapterItem.tooltip = 'Click to switch CAN adapter';
    adapterItem.backgroundColor = undefined;
}
