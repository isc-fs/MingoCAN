// `iscFs.devices` tree-view data provider.
//
// Two-level tree:
//
//   ADAPTER (collapsed by default — `active` adapter expanded)
//     ├── Node 0x3 · MAIN_IFS08 v1.2.0
//     └── Node 0x5 · OTHER_BOARD v0.9.1
//   ADAPTER (other, no children)
//
// The view populates lazily — `refresh()` runs `adapters --json`
// against the active adapter and `discover --json` to list devices
// on its bus. Refresh is manual (button on the view title bar or
// `iscFs.refreshDevices` from the palette); we never auto-poll the
// bus because that means transmitting frames the operator didn't
// ask for.

import * as vscode from 'vscode';

import type { AdapterEntry } from './adapters';
import { fetchAdapters, isActive } from './adapters';
import { type Config, readConfig } from './config';
import type { DiscoverRow } from './discover';
import { fetchDevices, formatNodeId, formatRowDetail } from './discover';
import { getOutputChannel } from './output';

// ---- Tree-item kinds ----

export type IscFsTreeNode = AdapterNode | DeviceNode | StatusNode;

export class AdapterNode {
    readonly kind = 'adapter';
    constructor(
        public readonly adapter: AdapterEntry,
        public readonly active: boolean,
        /** `null` while children haven't been loaded yet. */
        public readonly children: DeviceNode[] | null,
        /** Error from the last `discover` attempt for this adapter, if any. */
        public readonly error: string | null,
    ) {}
}

export class DeviceNode {
    readonly kind = 'device';
    constructor(
        public readonly row: DiscoverRow,
        public readonly adapter: AdapterEntry,
    ) {}
}

/** Free-form leaf row used for empty/error states. */
export class StatusNode {
    readonly kind = 'status';
    constructor(
        public readonly label: string,
        public readonly description?: string,
    ) {}
}

// ---- Provider ----

export class DeviceTreeProvider implements vscode.TreeDataProvider<IscFsTreeNode> {
    private readonly _onDidChangeTreeData = new vscode.EventEmitter<
        IscFsTreeNode | undefined
    >();
    readonly onDidChangeTreeData = this._onDidChangeTreeData.event;

    /** Cached adapters from the last `refresh()` call. `null` ⇒ never refreshed. */
    private adapters: AdapterNode[] | null = null;
    /** In-flight refresh — when set, subsequent `refresh()` calls await this. */
    private inflight: Promise<void> | null = null;

    /**
     * Trigger a refresh. Returns the same promise to concurrent callers
     * so a button-click + a "populate on first reveal" don't race.
     */
    refresh(): Promise<void> {
        if (this.inflight !== null) {
            return this.inflight;
        }
        this.inflight = this.doRefresh().finally(() => {
            this.inflight = null;
        });
        return this.inflight;
    }

    private async doRefresh(): Promise<void> {
        const out = getOutputChannel();
        const workspace = vscode.workspace.workspaceFolders?.[0];
        if (workspace === undefined) {
            this.adapters = [];
            this.fire();
            return;
        }
        const cwd = workspace.uri.fsPath;
        const cfg = readConfig();

        out.appendLine('');
        out.appendLine(`---- refresh ${new Date().toISOString()} ----`);

        const entries = await fetchAdapters(cfg, cwd);

        // Enrich the active adapter (and only the active adapter)
        // with a discover result. Refreshing every adapter on the
        // bus would mean opening each one in turn — too aggressive
        // on hardware. The picker handles per-adapter inspection.
        const active = entries.find((e) => isActive(e, cfg)) ?? null;
        const nodes: AdapterNode[] = [];
        for (const entry of entries) {
            if (active !== null && entry === active) {
                const { devices, error } = await refreshActiveAdapter(cfg, cwd);
                nodes.push(new AdapterNode(entry, true, devices, error));
            } else {
                nodes.push(new AdapterNode(entry, false, null, null));
            }
        }
        this.adapters = nodes;
        this.fire();
    }

    private fire(): void {
        this._onDidChangeTreeData.fire(undefined);
    }

    // ---- TreeDataProvider impl ----

    getTreeItem(node: IscFsTreeNode): vscode.TreeItem {
        switch (node.kind) {
            case 'adapter':
                return adapterTreeItem(node);
            case 'device':
                return deviceTreeItem(node);
            case 'status':
                return statusTreeItem(node);
        }
    }

    async getChildren(element?: IscFsTreeNode): Promise<IscFsTreeNode[]> {
        if (element === undefined) {
            if (this.adapters === null) {
                // First reveal — populate lazily.
                await this.refresh();
            }
            const list = this.adapters ?? [];
            if (list.length === 0) {
                return [new StatusNode('(no adapters detected)', 'click ⟳ to retry')];
            }
            return list;
        }
        if (element.kind === 'adapter') {
            if (element.error !== null) {
                return [new StatusNode('error', element.error)];
            }
            if (element.children === null) {
                return [
                    new StatusNode(
                        '(not active)',
                        'Select this adapter to discover its devices',
                    ),
                ];
            }
            if (element.children.length === 0) {
                return [
                    new StatusNode(
                        '(no devices)',
                        'No bootloader-mode devices on the bus',
                    ),
                ];
            }
            return element.children;
        }
        return [];
    }
}

async function refreshActiveAdapter(
    cfg: Config,
    cwd: string,
): Promise<{ devices: DeviceNode[]; error: string | null }> {
    try {
        const rows = await fetchDevices(cfg, cwd);
        return {
            devices: rows.map(
                (row) =>
                    new DeviceNode(row, {
                        interface: cfg.interface,
                        channel: cfg.channel,
                        label: '',
                    }),
            ),
            error: null,
        };
    } catch (err) {
        return {
            devices: [],
            error: err instanceof Error ? err.message : String(err),
        };
    }
}

// ---- TreeItem builders ----

function adapterTreeItem(node: AdapterNode): vscode.TreeItem {
    const item = new vscode.TreeItem(
        node.adapter.label,
        node.active
            ? vscode.TreeItemCollapsibleState.Expanded
            : vscode.TreeItemCollapsibleState.Collapsed,
    );
    item.description = node.active
        ? `[active] ${node.adapter.detail ?? ''}`.trim()
        : node.adapter.detail;
    item.iconPath = new vscode.ThemeIcon(node.active ? 'plug' : 'circle-large-outline');
    item.contextValue = node.active ? 'iscFsAdapter.active' : 'iscFsAdapter.inactive';
    item.tooltip = `${node.adapter.interface} · ${node.adapter.channel}`;
    return item;
}

function deviceTreeItem(node: DeviceNode): vscode.TreeItem {
    const id = formatNodeId(node.row.node_id);
    const item = new vscode.TreeItem(
        `Node ${id}`,
        vscode.TreeItemCollapsibleState.None,
    );
    item.description = formatRowDetail(node.row);
    item.iconPath = new vscode.ThemeIcon(
        node.row.fw_error !== undefined ? 'warning' : 'circuit-board',
    );
    item.contextValue = 'iscFsDevice';
    item.tooltip = [
        `Node: ${id}`,
        `Protocol: ${node.row.proto_major}.${node.row.proto_minor}`,
        node.row.product !== undefined ? `Product: ${node.row.product}` : null,
        node.row.fw_version !== undefined ? `Firmware: v${node.row.fw_version}` : null,
        node.row.git_hash !== undefined ? `Git: ${node.row.git_hash.slice(0, 12)}…` : null,
        node.row.wrp_protected !== undefined
            ? `WRP: ${node.row.wrp_protected ? 'on' : 'off'}`
            : null,
        node.row.reset_cause !== undefined ? `Reset: ${node.row.reset_cause}` : null,
    ]
        .filter((line): line is string => line !== null)
        .join('\n');
    return item;
}

function statusTreeItem(node: StatusNode): vscode.TreeItem {
    const item = new vscode.TreeItem(node.label);
    item.description = node.description;
    return item;
}
