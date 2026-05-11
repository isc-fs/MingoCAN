// `iscFs.liveData` — webview panel showing a live chart of
// `diagnose live-data --json` snapshots. Single-panel singleton:
// re-running the command focuses the existing panel rather than
// creating a second one.
//
// Lifecycle:
//   command:iscFs.liveData
//      → createOrShow() (singleton)
//      → panel.show()
//   user clicks "Start" in the webview
//      → controller.start() spawns can-flasher
//      → snapshots stream → postMessage → chart updates
//   user clicks "Stop"
//      → controller.stop() kills the child
//   panel closed
//      → controller.dispose() (kills any in-flight child)
//      → panel disposed
//   extension deactivates
//      → context.subscriptions disposes the singleton, which kills
//        any in-flight child via its dispose() chain.

import * as vscode from 'vscode';

import { readConfig } from './config';
import {
    LiveDataController,
    type ControllerStatus,
    type LiveDataSnapshot,
} from './liveData';

const VIEW_TYPE = 'iscFs.liveData';

export class LiveDataPanel {
    private static current: LiveDataPanel | undefined;

    private readonly panel: vscode.WebviewPanel;
    private readonly controller: LiveDataController;
    private readonly extensionUri: vscode.Uri;
    private readonly disposables: vscode.Disposable[] = [];

    static createOrShow(context: vscode.ExtensionContext): void {
        if (LiveDataPanel.current !== undefined) {
            LiveDataPanel.current.panel.reveal();
            return;
        }
        const panel = vscode.window.createWebviewPanel(
            VIEW_TYPE,
            'ISC CAN — Live data',
            vscode.ViewColumn.Active,
            {
                enableScripts: true,
                retainContextWhenHidden: true,
                localResourceRoots: [vscode.Uri.joinPath(context.extensionUri, 'media')],
            },
        );
        LiveDataPanel.current = new LiveDataPanel(panel, context);
    }

    private constructor(
        panel: vscode.WebviewPanel,
        context: vscode.ExtensionContext,
    ) {
        this.panel = panel;
        this.extensionUri = context.extensionUri;

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
        LiveDataPanel.current = undefined;
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
        const cfg = readConfig();
        if (cfg.channel.length === 0 && cfg.interface !== 'virtual') {
            this.postStatus(
                'error',
                'No adapter selected. Pick one via the status bar or `ISC CAN: Select adapter…`.',
            );
            return;
        }
        const rateHz = vscode.workspace
            .getConfiguration('iscFs')
            .get<number>('liveDataRateHz', 10);
        // Tell the webview to clear any stale points from a prior
        // session so the chart isn't confused by a discontinuity.
        this.panel.webview.postMessage({ type: 'reset' });
        this.controller.start({
            cfg,
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
