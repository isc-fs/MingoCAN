// Passive app-mode detection via `cf pit-diag listen`.
//
// `discover` only sees bootloader-mode boards. But both the ECU (0x704,
// 1 Hz) and the AMS (0x6CA, 1 Hz) broadcast a firmware-health frame
// ungated, so a short, send-silent `pit-diag listen --profile all`
// window tells us which apps are running and how healthy they are —
// without arming anything or perturbing a live car.
//
// The pit-diag frames are fixed per-board CAN IDs with no node-id field,
// so the frame kind identifies the board: `ecu*` → ECU (node 0x1),
// `amsHealth` → AMS (node 0x2). We fold the window into one AppNode per
// board heard. (Both boards' health reuses the same NDJSON field names,
// so the folding + rollup is board-agnostic.)

import { buildGlobalArgv, type Config } from './config';
import { spawnCommand } from './cli';
import { getOutputChannel } from './output';

/** Team node-id scheme — the frame kind tells us which board we heard. */
const ECU_NODE_ID = 0x1;
const AMS_NODE_ID = 0x2;

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
 * Run one short passive-listen window and fold the frames into an AppNode
 * per board heard (ECU and/or AMS). Returns `[]` when nothing was heard
 * (no app running, or the bus/adapter is quiet). Best-effort — a
 * spawn/parse failure yields `[]`, never throws; the tree stays a
 * discover-only view.
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

    // `--profile all` decodes both the ECU (0x704) and AMS (0x6CA) ungated
    // health in one window — their IDs never overlap.
    const argv = [
        ...buildGlobalArgv(cfg),
        'pit-diag',
        'listen',
        '--profile',
        'all',
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

/** Parse the NDJSON listen output into one AppNode per board heard
 *  (ECU and/or AMS). Exported for unit testing. */
export function foldFrames(stdout: string): AppNode[] {
    // A board only materialises once we hear one of its frames.
    let ecu: AppNode | undefined;
    let ams: AppNode | undefined;
    const ensureEcu = (): AppNode =>
        (ecu ??= { nodeId: ECU_NODE_ID, role: 'ECU', health: 'ok', reasons: [] });
    const ensureAms = (): AppNode =>
        (ams ??= { nodeId: AMS_NODE_ID, role: 'AMS', health: 'ok', reasons: [] });

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
            // Health frames — same field names for both boards (ecuHealth /
            // amsHealth), so one applier serves both.
            case 'ecuHealth':
                applyHealth(ensureEcu(), ev);
                break;
            case 'amsHealth':
                applyHealth(ensureAms(), ev);
                break;
            case 'ecuFwInfo':
                applyFwInfo(ensureEcu(), ev);
                break;
            // Any other decoded frame just proves that board's app is alive.
            case 'ecuStatus':
            case 'ecuPedals':
            case 'ecuInverter':
            case 'ecuBrake':
            case 'ecuInverterTemps':
            case 'ecuDv':
                ensureEcu();
                break;
            default:
                break;
        }
    }

    const nodes: AppNode[] = [];
    for (const node of [ecu, ams]) {
        if (node !== undefined) {
            rollUpHealth(node);
            nodes.push(node);
        }
    }
    return nodes;
}

/** Apply a health frame (ecuHealth / amsHealth — identical shape). */
function applyHealth(node: AppNode, ev: Record<string, unknown>): void {
    node.resetCause = str(ev.resetCause);
    node.uptimeS = num(ev.uptimeS);
    node.lastFault = num(ev.lastFault);
    node.tasks = {
        control: bool(ev.taskControl),
        canRx: bool(ev.taskCanRx),
        canTx: bool(ev.taskCanTx),
        diag: bool(ev.taskDiag),
    };
}

/** Apply an ecuFwInfo frame (ECU only — the AMS ungated frame carries no
 *  firmware identity). */
function applyFwInfo(node: AppNode, ev: Record<string, unknown>): void {
    node.fwVersion = `${num(ev.fwMajor) ?? 0}.${num(ev.fwMinor) ?? 0}.${num(ev.fwPatch) ?? 0}`;
    node.gitHash = hexHash(ev.gitHash);
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
