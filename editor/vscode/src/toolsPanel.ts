// Dashboard webview that exposes every operator-facing command in
// one place, organised by phase of the workflow:
//
//   1. Adapter      — current adapter + change…
//   2. Flash        — build + flash, flash without build
//   3. Devices      — discover, refresh device tree
//   4. Diagnostics  — health, read DTCs, clear DTCs, live data
//
// Each button posts a message to the host, which dispatches the
// corresponding `iscFs.*` command via `vscode.commands.executeCommand`.
// All the real logic stays in the existing command handlers — this
// view is pure UX sugar over the command palette.
//
// Lifetime: singleton. `createOrShow` reveals an existing panel if
// one is open, or creates a fresh one. The panel listens to
// `iscFs.*` configuration changes so the adapter pill stays in
// sync with the status-bar item.

import * as vscode from 'vscode';

import { readConfig } from './config';

const VIEW_TYPE = 'iscFs.tools';
const PANEL_TITLE = 'ISC MingoCAN — Tools';

export class ToolsPanel {
    private static current: ToolsPanel | undefined;

    private readonly panel: vscode.WebviewPanel;
    private readonly disposables: vscode.Disposable[] = [];

    // `context` isn't used today — the panel doesn't load any media
    // resources from the extension bundle — but we keep it in the
    // signature so the call site matches the LiveDataPanel pattern
    // and so it's already wired up if we add icons/scripts later.
    // eslint-disable-next-line @typescript-eslint/no-unused-vars
    static createOrShow(_context: vscode.ExtensionContext): void {
        if (ToolsPanel.current !== undefined) {
            ToolsPanel.current.panel.reveal();
            return;
        }
        const panel = vscode.window.createWebviewPanel(
            VIEW_TYPE,
            PANEL_TITLE,
            vscode.ViewColumn.Active,
            { enableScripts: true, retainContextWhenHidden: true },
        );
        ToolsPanel.current = new ToolsPanel(panel);
    }

    private constructor(panel: vscode.WebviewPanel) {
        this.panel = panel;
        this.panel.webview.html = this.buildHtml();
        this.disposables.push(
            this.panel.webview.onDidReceiveMessage((msg) => this.handleMessage(msg)),
            this.panel.onDidDispose(() => this.dispose()),
            vscode.workspace.onDidChangeConfiguration((event) => {
                if (event.affectsConfiguration('iscFs')) {
                    this.postAdapterStatus();
                }
            }),
        );
    }

    private dispose(): void {
        for (const d of this.disposables) {
            d.dispose();
        }
        this.disposables.length = 0;
        if (ToolsPanel.current === this) {
            ToolsPanel.current = undefined;
        }
    }

    // ---- Host ↔ webview messages -----------------------------------

    private handleMessage(msg: unknown): void {
        if (typeof msg !== 'object' || msg === null || !('type' in msg)) {
            return;
        }
        const m = msg as { type: string; command?: string };
        switch (m.type) {
            case 'ready':
                // Webview is up; post initial adapter state.
                this.postAdapterStatus();
                return;
            case 'exec':
                // Allowlist-gate the commands we'll dispatch — the
                // webview is local but treat its messages as
                // untrusted to keep the surface intentional.
                if (m.command !== undefined && ALLOWED_COMMANDS.has(m.command)) {
                    void vscode.commands.executeCommand(m.command);
                }
                return;
        }
    }

    private postAdapterStatus(): void {
        const cfg = readConfig();
        const hasAdapter = cfg.channel.length > 0 || cfg.interface === 'virtual';
        void this.panel.webview.postMessage({
            type: 'adapter',
            hasAdapter,
            interface: cfg.interface,
            channel: cfg.channel.length > 0 ? cfg.channel : null,
            nodeId: cfg.nodeId.length > 0 ? cfg.nodeId : null,
        });
    }

    private buildHtml(): string {
        // CSP nonces aren't strictly required for inline scripts in
        // a local-resource-roots-less webview, but keep one anyway
        // so we don't trip future security audits.
        const nonce = randomNonce();
        return /* html */ `<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8" />
    <meta http-equiv="Content-Security-Policy"
          content="default-src 'none'; style-src 'unsafe-inline'; script-src 'nonce-${nonce}';" />
    <title>${PANEL_TITLE}</title>
    <style>
        :root {
            color-scheme: dark light;
            --gap: 12px;
        }
        body {
            font-family: var(--vscode-font-family);
            color: var(--vscode-foreground);
            background: var(--vscode-editor-background);
            margin: 0;
            padding: 20px 24px 32px;
        }
        h1 {
            margin: 0 0 4px;
            font-size: 1.2rem;
            font-weight: 600;
        }
        .lead {
            margin: 0 0 18px;
            color: var(--vscode-descriptionForeground);
            font-size: 0.85rem;
        }
        .adapter {
            display: flex;
            align-items: center;
            gap: 12px;
            padding: 10px 14px;
            background: var(--vscode-editorWidget-background);
            border: 1px solid var(--vscode-widget-border, transparent);
            border-radius: 6px;
            margin-bottom: 16px;
        }
        .adapter .pill {
            font-family: var(--vscode-editor-font-family);
            font-size: 0.85rem;
            padding: 2px 8px;
            border-radius: 4px;
            background: var(--vscode-badge-background);
            color: var(--vscode-badge-foreground);
        }
        .adapter .pill.warning {
            background: var(--vscode-inputValidation-warningBackground,
                rgba(255, 200, 100, 0.18));
            color: var(--vscode-inputValidation-warningForeground,
                var(--vscode-foreground));
        }
        .adapter button {
            margin-left: auto;
        }
        section.card {
            margin-bottom: 14px;
        }
        h2 {
            font-size: 0.8rem;
            text-transform: uppercase;
            letter-spacing: 0.06em;
            color: var(--vscode-descriptionForeground);
            margin: 0 0 8px;
            font-weight: 600;
        }
        .grid {
            display: grid;
            grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
            gap: 8px;
        }
        button {
            appearance: none;
            background: var(--vscode-button-secondaryBackground,
                var(--vscode-button-background));
            color: var(--vscode-button-secondaryForeground,
                var(--vscode-button-foreground));
            border: 1px solid var(--vscode-button-border, transparent);
            border-radius: 4px;
            padding: 10px 12px;
            font: inherit;
            font-size: 0.9rem;
            text-align: left;
            cursor: pointer;
            display: flex;
            align-items: center;
            gap: 8px;
        }
        button:hover:not(:disabled) {
            background: var(--vscode-button-secondaryHoverBackground,
                var(--vscode-button-hoverBackground));
        }
        button:disabled {
            opacity: 0.45;
            cursor: not-allowed;
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
        button .glyph {
            font-family: codicon, monospace;
        }
        .hint {
            color: var(--vscode-descriptionForeground);
            font-size: 0.78rem;
            margin: 2px 0 0;
        }
    </style>
</head>
<body>
    <h1>ISC MingoCAN — Tools</h1>
    <p class="lead">
        Every action the command palette exposes, plus the current
        adapter, in one place. Settings, log output and the device tree
        live in the activity bar / output panel as usual.
    </p>

    <div class="adapter" id="adapter-row">
        <strong>Adapter</strong>
        <span class="pill" id="adapter-pill">…</span>
        <button id="btn-adapter">Change…</button>
    </div>

    <section class="card">
        <h2>Flash</h2>
        <div class="grid">
            <button class="primary" data-cmd="iscFs.flash">
                ⚡ Build + Flash
                <span class="hint" style="margin-left:auto;">⌘P → ISC MingoCAN: Flash</span>
            </button>
            <button data-cmd="iscFs.flashWithoutBuild">
                Flash without build
            </button>
        </div>
    </section>

    <section class="card">
        <h2>Devices</h2>
        <div class="grid">
            <button data-cmd="iscFs.discover">Discover devices</button>
            <button data-cmd="iscFs.refreshDevices">Refresh device list</button>
        </div>
    </section>

    <section class="card">
        <h2>Diagnostics</h2>
        <div class="grid">
            <button data-cmd="iscFs.health">Session health</button>
            <button data-cmd="iscFs.readDtcs">Read DTCs</button>
            <button class="danger" data-cmd="iscFs.clearDtcs">Clear DTCs</button>
            <button data-cmd="iscFs.liveData">Live data…</button>
        </div>
    </section>

    <script nonce="${nonce}">
        const vscode = acquireVsCodeApi();

        // Wire every [data-cmd] button to the host's command dispatcher.
        for (const btn of document.querySelectorAll('button[data-cmd]')) {
            btn.addEventListener('click', () => {
                vscode.postMessage({ type: 'exec', command: btn.dataset.cmd });
            });
        }
        document.getElementById('btn-adapter').addEventListener('click', () => {
            vscode.postMessage({ type: 'exec', command: 'iscFs.selectAdapter' });
        });

        // Adapter pill is host-driven so it stays in sync with
        // settings changes (operator may switch adapters via the
        // status-bar picker while the panel is open).
        window.addEventListener('message', (event) => {
            const msg = event.data;
            if (msg && msg.type === 'adapter') {
                const pill = document.getElementById('adapter-pill');
                if (msg.hasAdapter) {
                    const node = msg.nodeId ? \` → \${msg.nodeId}\` : '';
                    pill.textContent = \`\${msg.interface}: \${msg.channel ?? '—'}\${node}\`;
                    pill.classList.remove('warning');
                } else {
                    pill.textContent = 'no adapter selected';
                    pill.classList.add('warning');
                }
            }
        });

        // Signal we're ready so the host posts the initial adapter state.
        vscode.postMessage({ type: 'ready' });
    </script>
</body>
</html>`;
    }
}

// Allowlist of commands the webview is permitted to dispatch.
// Mirrors the buttons in the HTML; gating here keeps the host
// authoritative — a malformed message can't trigger something
// the UI doesn't already expose.
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

function randomNonce(): string {
    // 16 bytes of base64 ≈ 22 chars — plenty for CSP nonce uniqueness.
    const bytes = new Uint8Array(16);
    for (let i = 0; i < bytes.length; i += 1) {
        bytes[i] = Math.floor(Math.random() * 256);
    }
    return Buffer.from(bytes).toString('base64').replace(/=+$/, '');
}
