// ISC MingoCAN Flasher — VS Code extension entry point.
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

import { startAdapterPresenceService } from './adapterPresence';
import {
    clearCanFlasherPathCache,
    DEFAULT_BARE_NAME,
    resolveCanFlasherPath,
    setManagedCliPath,
} from './cliPath';
import { cliVersion, ensureManagedCli } from './cliManager';
import { readConfig } from './config';
import { runClearDtcs, runHealth, runReadDtcs } from './diagnose';
import { runFlash } from './flash';
import { LiveDataPanel } from './liveDataPanel';
import { selectAdapter } from './picker';
import { registerStatusBarItem } from './statusBar';
import { ToolsPanel } from './toolsPanel';
import { ToolsViewProvider } from './toolsView';
import { DeviceTreeProvider, type IscFsTreeNode } from './tree';
import { getOutputChannel, showOutputChannel } from './output';
import { formatNodeId } from './discover';

export function activate(context: vscode.ExtensionContext): void {
    // Keep the can-flasher binary in lockstep with this extension's
    // version: download the matching CLI on demand (preferred over a
    // possibly-stale PATH install) and warn on version skew. Async +
    // best-effort — never blocks activation or any command. See
    // cliManager.ts for the rationale (the desktop app stays in sync
    // by compiling the library in; the extension shells out, so it
    // has to manage its own version-matched binary).
    void bootstrapCli(context);

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

    // Adapter-presence service: pings `can-flasher --json adapters`
    // periodically (and on window-focus regain) so the status bar
    // and Tools sidebar flip to a "disconnected" state the moment
    // the configured probe is yanked. Must start before the status
    // bar / Tools view subscribe so the cold-start snapshot is
    // already in flight by the time they render.
    startAdapterPresenceService(context);

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

    // ---- Tools view (activity-bar sidebar) ----
    //
    // Same shape as PlatformIO's left-rail panel: the MingoCAN
    // activity-bar icon reveals a sidebar with two views — the
    // Tools webview (one-click access to every action) and the
    // Devices tree (live discovery). This is the canonical surface
    // from v2.3.4 onward.
    const toolsView = new ToolsViewProvider(context);
    context.subscriptions.push(
        vscode.window.registerWebviewViewProvider(
            ToolsViewProvider.viewType,
            toolsView,
        ),
    );

    // ---- Legacy "Open tools panel" command ----
    //
    // Editor-tab variant of the Tools view. Kept around for
    // operators who want the dashboard side-by-side with code —
    // the activity-bar sidebar is what most clicks land on.
    context.subscriptions.push(
        vscode.commands.registerCommand('iscFs.openTools', () =>
            ToolsPanel.createOrShow(context),
        ),
    );

    // Invalidate the CLI-path discovery cache whenever the operator
    // changes `iscFs.canFlasherPath` (or anything else under
    // `iscFs.*`). Without this the discovery decision sticks for the
    // whole VS Code session and the operator has to reload the
    // window to pick up a settings change.
    context.subscriptions.push(
        vscode.workspace.onDidChangeConfiguration((event) => {
            if (event.affectsConfiguration('iscFs.canFlasherPath')) {
                clearCanFlasherPathCache();
            }
            // Re-run CLI resolution when the operator pins a different
            // path or toggles auto-download, so the change takes effect
            // without a window reload.
            if (
                event.affectsConfiguration('iscFs.canFlasherPath') ||
                event.affectsConfiguration('iscFs.cliAutoDownload')
            ) {
                void bootstrapCli(context);
            }
        }),
    );
}

// ---- CLI version-sync bootstrap ----

let skewWarned = false;

/**
 * Ensure the extension runs a can-flasher whose version matches its
 * own, and warn the operator when it can't. Precedence:
 *
 *   1. Operator pinned `iscFs.canFlasherPath` → honoured verbatim;
 *      we only skew-check it (never silently override their choice).
 *   2. `iscFs.cliAutoDownload` on (default) → download the matching
 *      release binary and prefer it over PATH. Matches by
 *      construction, so no skew warning.
 *   3. Download unavailable (offline, unsupported platform, or
 *      auto-download off) → fall back to the PATH probe and warn if
 *      that binary's version disagrees with ours.
 *
 * Best-effort throughout — any failure leaves the previous PATH-based
 * behaviour intact.
 */
async function bootstrapCli(context: vscode.ExtensionContext): Promise<void> {
    const expected = (context.extension.packageJSON as { version?: string }).version;
    if (typeof expected !== 'string') {
        return;
    }
    const cfg = vscode.workspace.getConfiguration('iscFs');
    const configured = cfg.get<string>('canFlasherPath', DEFAULT_BARE_NAME);
    const autoDownload = cfg.get<boolean>('cliAutoDownload', true);

    // Operator hasn't pinned a path and wants auto-download: fetch the
    // version-matched binary and prefer it. On success it matches by
    // construction, so skip the skew check.
    if (configured === DEFAULT_BARE_NAME && autoDownload) {
        const managed = await ensureManagedCli(context, expected);
        if (managed !== null) {
            setManagedCliPath(managed);
            return;
        }
        // Download unavailable → clear any stale managed override so we
        // fall back to PATH, then skew-check below.
        setManagedCliPath(null);
    }

    // Skew-check whatever we'd actually run (pinned path, or PATH
    // probe). A `null` version means the binary couldn't run — the
    // ENOENT UX in cli.ts already handles the missing-binary case, so
    // we only warn on a definite version mismatch.
    const resolved = resolveCanFlasherPath(configured);
    const actual = await cliVersion(resolved);
    if (actual !== null && actual !== expected) {
        warnSkew(expected, actual, configured !== DEFAULT_BARE_NAME);
    }
}

/** One-shot (per session) version-skew warning with a remediation
 *  action. Phrased around the concrete versions so the operator knows
 *  exactly what to update. */
function warnSkew(expected: string, actual: string, pinned: boolean): void {
    if (skewWarned) {
        return;
    }
    skewWarned = true;
    const action = pinned ? 'Open settings' : 'How to update';
    const tail = pinned
        ? 'Point `iscFs.canFlasherPath` at a v' + expected + ' binary, or clear it to let the extension manage one.'
        : 'Enable `iscFs.cliAutoDownload` to let the extension manage a matching binary, or update your CLI.';
    void vscode.window
        .showWarningMessage(
            `ISC MingoCAN: the can-flasher CLI is v${actual}, but this extension expects v${expected}. ` +
                `Flashing may hit bugs already fixed in v${expected}. ${tail}`,
            action,
        )
        .then((choice) => {
            if (choice === 'Open settings') {
                void vscode.commands.executeCommand(
                    'workbench.action.openSettings',
                    'iscFs.canFlasherPath',
                );
            } else if (choice === 'How to update') {
                void vscode.env.openExternal(
                    vscode.Uri.parse(
                        `https://github.com/isc-fs/can-flasher/releases/tag/v${expected}`,
                    ),
                );
            }
        });
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
            'ISC MingoCAN: right-click a device in the ISC MingoCAN Devices view, then choose "Flash this device…".',
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

