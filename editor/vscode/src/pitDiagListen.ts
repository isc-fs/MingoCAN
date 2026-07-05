// Passive app-mode detection via `cf pit-diag listen`.
//
// `discover` only sees bootloader-mode boards. But the ECU app
// broadcasts 0x704 health ungated at 1 Hz (+ 0x703 fwinfo / 0x700
// status), so a short, send-silent `pit-diag listen` window tells us
// whether the app is running and how healthy it is — without arming
// anything or perturbing a live car.
//
// The pit-diag frames are ECU-fixed CAN IDs (0x700–0x706) with no
// node-id field, so an `ecu`-profile listen identifies exactly one
// board: the ECU (node 0x1 by the team scheme). We fold whatever frames
// arrive in the window into a single AppNode.

import { buildGlobalArgv, type Config } from './config';
import { spawnCommand } from './cli';
import { getOutputChannel } from './output';

/** ECU node-id (team scheme) — the board an `ecu`-profile listen hears. */
const ECU_NODE_ID = 0x1;

export type HealthLevel = 'ok' | 'warn';

/** A board detected running its application firmware (not the bootloader). */
export interface AppNode {
    nodeId: number;
    role: string;
    /** Overall health rollup from the liveness bits + reset/fault. */
    health: HealthLevel;
    fwVersion?: string;
    gitHash?: string;
    resetCause?: string;
    uptimeS?: number;
    lastFault?: number;
    /** Per-task liveness (stepped since the previous health frame). */
    tasks?: {
        control: boolean;
        canRx: boolean;
        canTx: boolean;
        diag: boolean;
    };
    /** Reasons the rollup is `warn` (empty when `ok`) — for tooltips. */
    reasons: string[];
}

// Reset causes that mean the app crashed/hung rather than booted cleanly.
const BAD_RESET_CAUSES = new Set(['Iwdg', 'Wwdg']);

/**
 * Run one short passive-listen window and fold the frames into at most
 * one AppNode (the ECU). Returns `[]` when nothing was heard (no app
 * running, or the bus/adapter is quiet). Best-effort — a spawn/parse
 * failure yields `[]`, never throws; the tree stays a discover-only view.
 */
export async function listenForAppNodes(
    cfg: Config,
    cwd: string,
    durationMs = 1500,
): Promise<AppNode[]> {
    // No adapter selected → nothing to listen on (virtual has no app boards).
    if (cfg.interface === 'virtual' || cfg.channel.trim().length === 0) {
        return [];
    }

    const argv = [
        ...buildGlobalArgv(cfg),
        'pit-diag',
        'listen',
        '--profile',
        'ecu',
        '--duration-ms',
        String(durationMs),
    ];

    let stdout: string;
    try {
        const result = await spawnCommand(cfg.canFlasherPath, argv, { cwd });
        if (result.exitCode !== 0) {
            return [];
        }
        stdout = result.stdout;
    } catch {
        return [];
    }

    return foldFrames(stdout);
}

/** Parse the NDJSON listen output into an AppNode list. Exported for
 *  unit testing. */
export function foldFrames(stdout: string): AppNode[] {
    let heard = false;
    const node: AppNode = {
        nodeId: ECU_NODE_ID,
        role: 'ECU',
        health: 'ok',
        reasons: [],
    };

    for (const line of stdout.split('\n')) {
        const trimmed = line.trim();
        if (trimmed.length === 0 || !trimmed.startsWith('{')) {
            continue;
        }
        let ev: Record<string, unknown>;
        try {
            const parsed: unknown = JSON.parse(trimmed);
            if (typeof parsed !== 'object' || parsed === null) {
                continue;
            }
            ev = parsed as Record<string, unknown>;
        } catch {
            continue;
        }
        const kind = ev.kind;
        if (typeof kind !== 'string') {
            continue;
        }
        switch (kind) {
            case 'ecuHealth':
                heard = true;
                node.resetCause = str(ev.resetCause);
                node.uptimeS = num(ev.uptimeS);
                node.lastFault = num(ev.lastFault);
                node.tasks = {
                    control: bool(ev.taskControl),
                    canRx: bool(ev.taskCanRx),
                    canTx: bool(ev.taskCanTx),
                    diag: bool(ev.taskDiag),
                };
                break;
            case 'ecuFwInfo':
                heard = true;
                node.fwVersion = `${num(ev.fwMajor) ?? 0}.${num(ev.fwMinor) ?? 0}.${num(ev.fwPatch) ?? 0}`;
                node.gitHash = hexHash(ev.gitHash);
                break;
            case 'ecuStatus':
            case 'ecuPedals':
            case 'ecuInverter':
            case 'ecuBrake':
            case 'ecuInverterTemps':
                // Any decoded ECU frame proves the app is alive, even if
                // we don't surface its fields on the node.
                heard = true;
                break;
            default:
                break;
        }
    }

    if (!heard) {
        return [];
    }
    rollUpHealth(node);
    return [node];
}

/** Derive the `health` level + the human reasons behind a `warn`. */
function rollUpHealth(node: AppNode): void {
    const reasons: string[] = [];
    if (node.resetCause !== undefined && BAD_RESET_CAUSES.has(node.resetCause)) {
        reasons.push(`watchdog reset (${node.resetCause})`);
    }
    if (node.lastFault !== undefined && node.lastFault !== 0) {
        reasons.push(`last fault 0x${node.lastFault.toString(16).toUpperCase()}`);
    }
    if (node.tasks !== undefined) {
        const dead = (
            [
                ['control', node.tasks.control],
                ['can-rx', node.tasks.canRx],
                ['can-tx', node.tasks.canTx],
                ['diag', node.tasks.diag],
            ] as const
        )
            .filter(([, alive]) => !alive)
            .map(([name]) => name);
        if (dead.length > 0) {
            reasons.push(`task(s) not stepping: ${dead.join(', ')}`);
        }
    }
    node.reasons = reasons;
    node.health = reasons.length > 0 ? 'warn' : 'ok';
}

// ---- tiny coercion helpers (NDJSON is loosely typed) ----

function str(v: unknown): string | undefined {
    return typeof v === 'string' ? v : undefined;
}
function num(v: unknown): number | undefined {
    return typeof v === 'number' && Number.isFinite(v) ? v : undefined;
}
function bool(v: unknown): boolean {
    return v === true;
}
function hexHash(v: unknown): string | undefined {
    if (!Array.isArray(v)) {
        return undefined;
    }
    return v
        .map((b) => (typeof b === 'number' ? b.toString(16).padStart(2, '0') : ''))
        .join('');
}

/** Emit a one-line summary of a listen result to the output channel. */
export function logAppNodes(nodes: AppNode[]): void {
    const out = getOutputChannel();
    if (nodes.length === 0) {
        out.appendLine('[listen] no app-mode boards heard');
        return;
    }
    for (const n of nodes) {
        const fw = n.fwVersion !== undefined ? ` v${n.fwVersion}` : '';
        const git = n.gitHash !== undefined ? ` @${n.gitHash.slice(0, 8)}` : '';
        const health = n.health === 'ok' ? 'healthy' : `WARN (${n.reasons.join('; ')})`;
        out.appendLine(
            `[listen] ${n.role} 0x${n.nodeId.toString(16)}${fw}${git} — app running, ${health}`,
        );
    }
}
