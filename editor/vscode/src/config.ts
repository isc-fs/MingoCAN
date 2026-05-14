// Type-safe reader for the `iscFs.*` configuration tree.
//
// Everything VS Code exposes via `WorkspaceConfiguration.get` is
// `unknown` until cast; centralising the reads here means the rest
// of the extension code can treat the config object as a normal
// typed record and the schema mirrors `package.json`'s
// `contributes.configuration.properties` exactly.

import * as vscode from 'vscode';

export type InterfaceType = 'slcan' | 'socketcan' | 'pcan' | 'vector' | 'virtual';

export interface Config {
    canFlasherPath: string;
    interface: InterfaceType;
    channel: string;
    bitrate: number;
    nodeId: string;
    buildCommand: string;
    firmwareArtifact: string;
    timeoutMs: number;
    requireWrp: boolean;
    applyWrp: boolean;
    profile: boolean;
    jumpAfterFlash: boolean;
}

export function readConfig(): Config {
    const cfg = vscode.workspace.getConfiguration('iscFs');
    return {
        canFlasherPath: cfg.get<string>('canFlasherPath', 'can-flasher'),
        interface: cfg.get<InterfaceType>('interface', 'slcan'),
        channel: cfg.get<string>('channel', ''),
        bitrate: cfg.get<number>('bitrate', 500_000),
        nodeId: cfg.get<string>('nodeId', ''),
        buildCommand: cfg.get<string>('buildCommand', 'cmake --build build'),
        firmwareArtifact: cfg.get<string>('firmwareArtifact', ''),
        timeoutMs: cfg.get<number>('timeoutMs', 500),
        requireWrp: cfg.get<boolean>('requireWrp', false),
        applyWrp: cfg.get<boolean>('applyWrp', false),
        profile: cfg.get<boolean>('profile', false),
        jumpAfterFlash: cfg.get<boolean>('jumpAfterFlash', true),
    };
}

/**
 * Argv passed to `can-flasher` *before* the subcommand — the global
 * flags from REQUIREMENTS.md § Global flags. Subcommands are
 * appended by the caller (`flash …`, `discover …`, etc.).
 */
export function buildGlobalArgv(cfg: Config): string[] {
    const argv: string[] = ['--interface', cfg.interface];
    if (cfg.channel.length > 0) {
        argv.push('--channel', cfg.channel);
    }
    argv.push('--bitrate', String(cfg.bitrate));
    if (cfg.nodeId.length > 0) {
        argv.push('--node-id', cfg.nodeId);
    }
    argv.push('--timeout', String(cfg.timeoutMs));
    argv.push('--json');
    return argv;
}
