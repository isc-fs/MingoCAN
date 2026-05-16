// Activity-bar sidebar webview that puts every operator-facing
// command one click away. Same shape as PlatformIO's left-rail
// "Quick Access" panel — the ISC MingoCAN icon on the activity
// bar reveals a sidebar with two views:
//
//   - Tools    (this file, type=webview)
//   - Devices  (the existing iscFs.devices tree)
//
// The webview's `exec` messages are allowlist-gated against the
// `iscFs.*` command surface, same as the editor-tab panel that
// predates this view. Both UIs share zero state; the sidebar is
// the canonical surface from v2.3.4 onward.

import * as vscode from 'vscode';

import { readConfig } from './config';

const ALLOWED_COMMANDS = new Set<string>([
    'iscFs.flash',
    'iscFs.flashWithoutBuild',
    'iscFs.discover',
    'iscFs.refreshDevices',
    'iscFs.selectAdapter',
    'iscFs.health',
    'iscFs.readDtcs',
    'iscFs.clearDtcs',
    'iscFs.liveData',
]);

export class ToolsViewProvider implements vscode.WebviewViewProvider {
    public static readonly viewType = 'iscFs.tools';

    private view: vscode.WebviewView | undefined;
    private readonly disposables: vscode.Disposable[] = [];

    constructor(context: vscode.ExtensionContext) {
        // Adapter pill is driven by `iscFs.*` configuration changes,
        // so the sidebar updates as soon as the operator switches
        // adapter via the status-bar picker / palette / settings.
        //
        // We don't retain `context` ourselves; the constructor's
        // only use of it is to register our `onDidChangeConfiguration`
        // disposable into the extension host's lifetime.
        this.disposables.push(
            vscode.workspace.onDidChangeConfiguration((event) => {
                if (event.affectsConfiguration('iscFs')) {
                    this.postAdapterStatus();
                }
            }),
        );
        context.subscriptions.push(...this.disposables);
    }

    resolveWebviewView(
        webviewView: vscode.WebviewView,
        _context: vscode.WebviewViewResolveContext,
        _token: vscode.CancellationToken,
    ): void {
        this.view = webviewView;
        webviewView.webview.options = {
            enableScripts: true,
            // Sidebar webviews don't load any extension-bundled
            // assets — the HTML/CSS/JS is inlined below — so
            // leave localResourceRoots empty.
            localResourceRoots: [],
        };
        webviewView.webview.html = this.buildHtml();
        webviewView.webview.onDidReceiveMessage(
            (msg) => this.handleMessage(msg),
            null,
            this.disposables,
        );
        webviewView.onDidDispose(
            () => {
                this.view = undefined;
            },
            null,
            this.disposables,
        );
    }

    private handleMessage(msg: unknown): void {
        if (typeof msg !== 'object' || msg === null || !('type' in msg)) {
            return;
        }
        const m = msg as { type: string; command?: string };
        switch (m.type) {
            case 'ready':
                this.postAdapterStatus();
                return;
            case 'exec':
                if (m.command !== undefined && ALLOWED_COMMANDS.has(m.command)) {
                    void vscode.commands.executeCommand(m.command);
                }
                return;
        }
    }

    private postAdapterStatus(): void {
        if (this.view === undefined) return;
        const cfg = readConfig();
        const hasAdapter = cfg.channel.length > 0 || cfg.interface === 'virtual';
        void this.view.webview.postMessage({
            type: 'adapter',
            hasAdapter,
            interface: cfg.interface,
            channel: cfg.channel.length > 0 ? cfg.channel : null,
            nodeId: cfg.nodeId.length > 0 ? cfg.nodeId : null,
        });
    }

    private buildHtml(): string {
        const nonce = randomNonce();
        return /* html */ `<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8" />
    <meta http-equiv="Content-Security-Policy"
          content="default-src 'none'; style-src 'unsafe-inline'; script-src 'nonce-${nonce}';" />
    <title>ISC MingoCAN Tools</title>
    <style>
        :root { color-scheme: dark light; }
        body {
            font-family: var(--vscode-font-family);
            color: var(--vscode-foreground);
            background: var(--vscode-sideBar-background);
            margin: 0;
            padding: 10px 12px 16px;
            font-size: 0.85rem;
        }
        .adapter {
            display: flex;
            align-items: center;
            gap: 8px;
            padding: 8px 10px;
            background: var(--vscode-editorWidget-background);
            border: 1px solid var(--vscode-widget-border, transparent);
            border-radius: 4px;
            margin-bottom: 12px;
        }
        .adapter .pill {
            font-family: var(--vscode-editor-font-family);
            font-size: 0.75rem;
            padding: 1px 6px;
            border-radius: 3px;
            background: var(--vscode-badge-background);
            color: var(--vscode-badge-foreground);
            overflow: hidden;
            text-overflow: ellipsis;
            white-space: nowrap;
            max-width: 100%;
        }
        .adapter .pill.warning {
            background: var(--vscode-inputValidation-warningBackground,
                rgba(255, 200, 100, 0.18));
            color: var(--vscode-inputValidation-warningForeground,
                var(--vscode-foreground));
        }
        .adapter button {
            margin-left: auto;
            flex-shrink: 0;
        }
        h2 {
            font-size: 0.7rem;
            text-transform: uppercase;
            letter-spacing: 0.06em;
            color: var(--vscode-descriptionForeground);
            margin: 12px 0 4px;
            font-weight: 600;
        }
        section.group {
            display: flex;
            flex-direction: column;
            gap: 4px;
        }
        button {
            appearance: none;
            background: var(--vscode-button-secondaryBackground,
                var(--vscode-button-background));
            color: var(--vscode-button-secondaryForeground,
                var(--vscode-button-foreground));
            border: 1px solid var(--vscode-button-border, transparent);
            border-radius: 3px;
            padding: 6px 10px;
            font: inherit;
            font-size: 0.85rem;
            text-align: left;
            cursor: pointer;
            display: flex;
            align-items: center;
            gap: 6px;
        }
        button:hover:not(:disabled) {
            background: var(--vscode-button-secondaryHoverBackground,
                var(--vscode-button-hoverBackground));
        }
        button.primary {
            background: var(--vscode-button-background);
            color: var(--vscode-button-foreground);
        }
        button.primary:hover:not(:disabled) {
            background: var(--vscode-button-hoverBackground);
        }
        button.danger {
            color: var(--vscode-errorForeground);
            border-color: var(--vscode-errorForeground);
        }
    </style>
</head>
<body>
    <div class="adapter">
        <span class="pill" id="adapter-pill" title="">…</span>
        <button id="btn-adapter" title="Switch CAN adapter">⇄</button>
    </div>

    <h2>Flash</h2>
    <section class="group">
        <button class="primary" data-cmd="iscFs.flash">⚡ Build + Flash</button>
        <button data-cmd="iscFs.flashWithoutBuild">Flash without build</button>
    </section>

    <h2>Devices</h2>
    <section class="group">
        <button data-cmd="iscFs.discover">Discover devices</button>
        <button data-cmd="iscFs.refreshDevices">Refresh device list</button>
    </section>

    <h2>Diagnostics</h2>
    <section class="group">
        <button data-cmd="iscFs.health">Session health</button>
        <button data-cmd="iscFs.readDtcs">Read DTCs</button>
        <button class="danger" data-cmd="iscFs.clearDtcs">Clear DTCs</button>
        <button data-cmd="iscFs.liveData">Live data…</button>
    </section>

    <script nonce="${nonce}">
        const vscode = acquireVsCodeApi();

        for (const btn of document.querySelectorAll('button[data-cmd]')) {
            btn.addEventListener('click', () => {
                vscode.postMessage({ type: 'exec', command: btn.dataset.cmd });
            });
        }
        document.getElementById('btn-adapter').addEventListener('click', () => {
            vscode.postMessage({ type: 'exec', command: 'iscFs.selectAdapter' });
        });

        window.addEventListener('message', (event) => {
            const msg = event.data;
            if (msg && msg.type === 'adapter') {
                const pill = document.getElementById('adapter-pill');
                if (msg.hasAdapter) {
                    const node = msg.nodeId ? \` → \${msg.nodeId}\` : '';
                    const label = \`\${msg.interface}: \${msg.channel ?? '—'}\${node}\`;
                    pill.textContent = label;
                    pill.title = label;
                    pill.classList.remove('warning');
                } else {
                    pill.textContent = 'no adapter';
                    pill.title = 'No adapter selected — click ⇄';
                    pill.classList.add('warning');
                }
            }
        });

        vscode.postMessage({ type: 'ready' });
    </script>
</body>
</html>`;
    }
}

function randomNonce(): string {
    const bytes = new Uint8Array(16);
    for (let i = 0; i < bytes.length; i += 1) {
        bytes[i] = Math.floor(Math.random() * 256);
    }
    return Buffer.from(bytes).toString('base64').replace(/=+$/, '');
}
