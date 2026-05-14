// `iscFs.liveData` — webview panel showing a live chart of
// `diagnose live-data --json` snapshots.
//
// One panel per (interface, channel) pair, keyed in `byKey`. Each
// panel captures its adapter identity at creation time, so a
// running stream stays locked to its adapter even if the operator
// later switches `iscFs.interface` / `iscFs.channel` to look at a
// different board. This is how the two-board workflow works:
// open one panel for CANable A, switch settings, open a second
// for CANable B, both stream independently.
//
// Lifecycle:
//   command:iscFs.liveData
//      → captures current adapter from settings
//      → createOrShow(context, interface, channel)
//          → if a panel for that adapter exists: reveal it
//          → else: spawn a new one keyed under byKey
//   user clicks "Start" in the webview
//      → controller.start() spawns can-flasher with the *captured*
//        adapter (overrides the current settings)
//      → snapshots stream → postMessage → chart updates
//   user clicks "Stop"
//      → controller.stop() kills the child
//   panel closed
//      → controller.dispose() (kills any in-flight child)
//      → panel removed from byKey
//   extension deactivates
//      → context.subscriptions disposes every panel, which kills
//        any in-flight child via its dispose() chain.

import * as vscode from 'vscode';

import { type Config, type InterfaceType, readConfig } from './config';
import {
    LiveDataController,
    type ControllerStatus,
    type LiveDataSnapshot,
} from './liveData';

const VIEW_TYPE = 'iscFs.liveData';

export class LiveDataPanel {
    private static readonly byKey: Map<string, LiveDataPanel> = new Map();

    private readonly panel: vscode.WebviewPanel;
    private readonly controller: LiveDataController;
    private readonly extensionUri: vscode.Uri;
    private readonly disposables: vscode.Disposable[] = [];
    private readonly key: string;

    static createOrShow(
        context: vscode.ExtensionContext,
        capturedInterface: InterfaceType,
        capturedChannel: string,
    ): void {
        const key = panelKey(capturedInterface, capturedChannel);
        const existing = LiveDataPanel.byKey.get(key);
        if (existing !== undefined) {
            existing.panel.reveal();
            return;
        }
        const title = panelTitle(capturedInterface, capturedChannel);
        const panel = vscode.window.createWebviewPanel(
            VIEW_TYPE,
            title,
            vscode.ViewColumn.Active,
            {
                enableScripts: true,
                retainContextWhenHidden: true,
                localResourceRoots: [vscode.Uri.joinPath(context.extensionUri, 'media')],
            },
        );
        const instance = new LiveDataPanel(
            panel,
            context,
            capturedInterface,
            capturedChannel,
            key,
        );
        LiveDataPanel.byKey.set(key, instance);
    }

    private constructor(
        panel: vscode.WebviewPanel,
        context: vscode.ExtensionContext,
        private readonly capturedInterface: InterfaceType,
        private readonly capturedChannel: string,
        key: string,
    ) {
        this.panel = panel;
        this.extensionUri = context.extensionUri;
        this.key = key;

        this.controller = new LiveDataController({
            onSnapshot: (snapshot) => this.postSnapshot(snapshot),
            onStatus: (status, message) => this.postStatus(status, message),
        });

        this.panel.webview.html = this.buildHtml(this.panel.webview);

        this.disposables.push(
            this.panel.webview.onDidReceiveMessage((msg) => this.handleMessage(msg)),
            this.panel.onDidDispose(() => this.dispose()),
            vscode.workspace.onDidChangeConfiguration((event) => {
                if (event.affectsConfiguration('iscFs.liveDataWindowSeconds')) {
                    this.postConfig();
                }
            }),
        );
    }

    private dispose(): void {
        this.controller.dispose();
        for (const d of this.disposables) {
            d.dispose();
        }
        this.disposables.length = 0;
        if (LiveDataPanel.byKey.get(this.key) === this) {
            LiveDataPanel.byKey.delete(this.key);
        }
    }

    // ---- Webview ↔ host messages ----

    private handleMessage(msg: unknown): void {
        if (typeof msg !== 'object' || msg === null || !('type' in msg)) {
            return;
        }
        const m = msg as { type: string };
        switch (m.type) {
            case 'ready':
                this.postConfig();
                this.postStatus('idle');
                break;
            case 'start':
                this.startStream();
                break;
            case 'stop':
                this.controller.stop();
                break;
        }
    }

    private startStream(): void {
        const workspace = vscode.workspace.workspaceFolders?.[0];
        if (workspace === undefined) {
            this.postStatus(
                'error',
                'Open a workspace folder before starting the live-data stream.',
            );
            return;
        }
        if (
            this.capturedChannel.length === 0 &&
            this.capturedInterface !== 'virtual'
        ) {
            this.postStatus(
                'error',
                'No adapter selected when this panel was opened. Close it and re-open after selecting one.',
            );
            return;
        }
        // Lock the controller to the adapter we captured at panel
        // creation, regardless of whether the operator has since
        // switched the global setting to a different board. Every
        // other field is read fresh so rate / timeouts / nodeId
        // changes still take effect on the next Start.
        const liveCfg = readConfig();
        const lockedCfg: Config = {
            ...liveCfg,
            interface: this.capturedInterface,
            channel: this.capturedChannel,
        };
        const rateHz = vscode.workspace
            .getConfiguration('iscFs')
            .get<number>('liveDataRateHz', 10);
        // Tell the webview to clear any stale points from a prior
        // session so the chart isn't confused by a discontinuity.
        this.panel.webview.postMessage({ type: 'reset' });
        this.controller.start({
            cfg: lockedCfg,
            cwd: workspace.uri.fsPath,
            rateHz,
        });
    }

    private postConfig(): void {
        const cfg = vscode.workspace.getConfiguration('iscFs');
        void this.panel.webview.postMessage({
            type: 'config',
            windowSeconds: cfg.get<number>('liveDataWindowSeconds', 60),
            rateHz: cfg.get<number>('liveDataRateHz', 10),
        });
    }

    private postStatus(status: ControllerStatus, message?: string): void {
        void this.panel.webview.postMessage({ type: 'status', status, message });
    }

    private postSnapshot(snapshot: LiveDataSnapshot): void {
        void this.panel.webview.postMessage({ type: 'snapshot', data: snapshot });
    }

    // ---- HTML template ----

    private buildHtml(webview: vscode.Webview): string {
        const nonce = randomNonce();
        const mediaRoot = vscode.Uri.joinPath(this.extensionUri, 'media');
        const chartJs = webview.asWebviewUri(
            vscode.Uri.joinPath(mediaRoot, 'chart.umd.min.js'),
        );
        const scriptUri = webview.asWebviewUri(
            vscode.Uri.joinPath(mediaRoot, 'live-data.js'),
        );
        const styleUri = webview.asWebviewUri(
            vscode.Uri.joinPath(mediaRoot, 'live-data.css'),
        );

        // CSP: only allow resources from this webview's origin + the
        // inline canvas tag's nonced styling. No external network.
        const csp = [
            `default-src 'none'`,
            `img-src ${webview.cspSource} https:`,
            `style-src ${webview.cspSource} 'unsafe-inline'`,
            `script-src 'nonce-${nonce}'`,
            `connect-src 'none'`,
        ].join('; ');

        return `<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8" />
    <meta http-equiv="Content-Security-Policy" content="${csp}" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <link rel="stylesheet" href="${styleUri.toString()}" />
    <title>ISC CAN — Live data</title>
</head>
<body>
    <div class="toolbar">
        <button id="start">Start</button>
        <button id="stop" class="secondary" disabled>Stop</button>
        <button id="clear" class="secondary">Clear chart</button>
        <span id="status" class="status idle">Idle</span>
    </div>

    <div id="indicators" class="indicators"></div>

    <div id="counters" class="counters"></div>

    <div class="chart-wrap">
        <canvas id="chart"></canvas>
    </div>

    <script nonce="${nonce}" src="${chartJs.toString()}"></script>
    <script nonce="${nonce}" src="${scriptUri.toString()}"></script>
</body>
</html>`;
    }
}

function randomNonce(): string {
    const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
    let out = '';
    for (let i = 0; i < 32; i++) {
        out += chars.charAt(Math.floor(Math.random() * chars.length));
    }
    return out;
}

/** Map-key for one panel — unique per (interface, channel) pair. */
function panelKey(iface: InterfaceType, channel: string): string {
    return `${iface}:${channel}`;
}

/** Editor-tab title — includes the adapter so the multi-panel case
 *  is visually distinguishable. */
function panelTitle(iface: InterfaceType, channel: string): string {
    if (iface === 'virtual') {
        return 'ISC CAN — Live data (virtual)';
    }
    return channel.length > 0
        ? `ISC CAN — Live data (${iface} · ${channel})`
        : `ISC CAN — Live data (${iface})`;
}
