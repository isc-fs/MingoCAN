// The ISC MingoCAN "Tools" sidebar — a native TreeView.
//
// Replaces the old hand-rolled webview (toolsView.ts: ~170 lines of HTML
// + CSP + postMessage command dispatch) with a themed, keyboard-navigable
// `TreeDataProvider`. Three groups — Flash / Devices / Diagnostics — each
// holding one-click command items with codicons. The Devices group's
// description mirrors the current adapter so the sidebar always shows what
// the bus is pointed at.

import * as vscode from 'vscode';

import { readConfig } from './config';

interface Action {
    label: string;
    command: string;
    icon: string;
    tooltip?: string;
}

interface Group {
    label: string;
    icon: string;
    actions: Action[];
}

const GROUPS: Group[] = [
    {
        label: 'Flash',
        icon: 'zap',
        actions: [
            { label: 'Build + Flash', command: 'iscFs.flash', icon: 'zap' },
            {
                label: 'Flash without build',
                command: 'iscFs.flashWithoutBuild',
                icon: 'rocket',
            },
            {
                label: 'Re-flash last',
                command: 'iscFs.reflashLast',
                icon: 'history',
                tooltip: 'Repeat the last flash — same artifact + node, no build, no prompts',
            },
        ],
    },
    {
        label: 'Devices',
        icon: 'circuit-board',
        actions: [
            {
                label: 'Select adapter…',
                command: 'iscFs.selectAdapter',
                icon: 'plug',
            },
            { label: 'Discover devices', command: 'iscFs.discover', icon: 'search' },
            {
                label: 'Refresh device list',
                command: 'iscFs.refreshDevices',
                icon: 'refresh',
            },
        ],
    },
    {
        label: 'Diagnostics',
        icon: 'pulse',
        actions: [
            {
                label: 'Doctor: check environment',
                command: 'iscFs.doctor',
                icon: 'checklist',
            },
            { label: 'Session health', command: 'iscFs.health', icon: 'heart' },
            { label: 'Read DTCs', command: 'iscFs.readDtcs', icon: 'list-flat' },
            { label: 'Clear DTCs', command: 'iscFs.clearDtcs', icon: 'trash' },
            { label: 'Live data…', command: 'iscFs.liveData', icon: 'graph' },
        ],
    },
];

type ToolsNode =
    | { kind: 'group'; group: Group }
    | { kind: 'action'; action: Action };

export class ToolsTreeProvider implements vscode.TreeDataProvider<ToolsNode> {
    public static readonly viewType = 'iscFs.tools';

    private readonly emitter = new vscode.EventEmitter<ToolsNode | undefined>();
    readonly onDidChangeTreeData = this.emitter.event;

    /** Wire config-change refresh so the Devices group's adapter
     *  description tracks the current setting. */
    register(context: vscode.ExtensionContext): vscode.TreeView<ToolsNode> {
        const view = vscode.window.createTreeView(ToolsTreeProvider.viewType, {
            treeDataProvider: this,
            showCollapseAll: false,
        });
        context.subscriptions.push(
            view,
            this.emitter,
            vscode.workspace.onDidChangeConfiguration((e) => {
                if (
                    e.affectsConfiguration('iscFs.interface') ||
                    e.affectsConfiguration('iscFs.channel') ||
                    e.affectsConfiguration('iscFs.nodeId')
                ) {
                    this.emitter.fire(undefined);
                }
            }),
        );
        return view;
    }

    getTreeItem(node: ToolsNode): vscode.TreeItem {
        if (node.kind === 'group') {
            const item = new vscode.TreeItem(
                node.group.label,
                vscode.TreeItemCollapsibleState.Expanded,
            );
            item.iconPath = new vscode.ThemeIcon(node.group.icon);
            item.contextValue = 'iscFsToolsGroup';
            if (node.group.label === 'Devices') {
                item.description = adapterSummary();
            }
            return item;
        }
        const item = new vscode.TreeItem(
            node.action.label,
            vscode.TreeItemCollapsibleState.None,
        );
        item.iconPath = new vscode.ThemeIcon(node.action.icon);
        item.tooltip = node.action.tooltip ?? node.action.label;
        item.command = {
            command: node.action.command,
            title: node.action.label,
        };
        item.contextValue = 'iscFsToolsAction';
        return item;
    }

    getChildren(node?: ToolsNode): ToolsNode[] {
        if (node === undefined) {
            return GROUPS.map((group) => ({ kind: 'group', group }));
        }
        if (node.kind === 'group') {
            return node.group.actions.map((action) => ({ kind: 'action', action }));
        }
        return [];
    }
}

/** "pcan: PCAN_USBBUS1 → 0x1" or "no adapter" — the one-line adapter
 *  state shown against the Devices group. */
function adapterSummary(): string {
    const cfg = readConfig();
    if (cfg.interface === 'virtual') {
        return 'virtual';
    }
    if (cfg.channel.trim().length === 0) {
        return 'no adapter';
    }
    const node = cfg.nodeId.trim().length > 0 ? ` → ${cfg.nodeId}` : '';
    return `${cfg.interface}: ${cfg.channel}${node}`;
}
