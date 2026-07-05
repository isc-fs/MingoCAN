// Implementation of `iscFs.flash` and `iscFs.flashWithoutBuild`.
//
// Shape of one flash run:
//
//   1. Read config + validate (artifact path, workspace folder)
//   2. (optional) Run the configured build command via the shell;
//      stream its output to the ISC CAN channel; abort on non-zero.
//   3. Resolve the firmware artifact path — exact path or glob;
//      prompt if a glob matches more than one file.
//   4. Spawn `can-flasher <global flags> flash <flags> <artifact>`
//      with `--json` and parse the event stream into a progress
//      bar's `report({ message })` calls.
//   5. On clean exit, surface duration + sectors-touched in a
//      success toast; on non-zero, exit-code-aware error toast that
//      maps to REQUIREMENTS.md's exit-code table.
//
// Every operation routes its raw stdout / stderr through the
// shared ISC CAN output channel — operators get a deterministic
// record of what was run when something goes wrong on the bench.

import * as path from 'path';
import * as vscode from 'vscode';

import { detectCmakePreset } from './cmakePresets';
import {
    buildGlobalArgv,
    DEFAULT_BUILD_COMMAND,
    DEFAULT_FIRMWARE_GLOB,
    type Config,
    readConfig,
} from './config';
import {
    type FlashEvent,
    type FlashReport,
    parseFlashEvent,
    parseFlashReport,
    spawnCommand,
} from './cli';
import { getOutputChannel, showOutputChannel } from './output';
import { currentSnapshot } from './adapterPresence';
import { setFlashBusy, setFlashIdle } from './statusBar';

export interface FlashOptions {
    /** When true, skip the configured build command. */
    skipBuild: boolean;
    /** Re-flash a previous artifact/node-id with no prompts (used by
     *  `iscFs.reflashLast`). Implies `skipBuild`. */
    reflash?: LastFlash;
}

/** The bits `iscFs.reflashLast` remembers between runs. */
export interface LastFlash {
    artifactPath: string;
    nodeId: string;
}

// Workspace-scoped memento, set once at activation, so the last flash's
// artifact + node-id survive across a re-flash (and window reloads).
let flashMemento: vscode.Memento | undefined;
const LAST_FLASH_KEY = 'iscFs.lastFlash';

/** Wire the extension's workspaceState in at activation. */
export function initFlash(memento: vscode.Memento): void {
    flashMemento = memento;
}

/**
 * `iscFs.reflashLast` — repeat the previous flash (same artifact + node)
 * with no build and no prompts. The daily inner-loop shortcut.
 */
export async function runReflashLast(): Promise<void> {
    const last = flashMemento?.get<LastFlash>(LAST_FLASH_KEY);
    if (last === undefined) {
        void vscode.window.showInformationMessage(
            'ISC MingoCAN: nothing to re-flash yet — run Build & Flash once first.',
        );
        return;
    }
    await runFlash({ skipBuild: true, reflash: last });
}

/**
 * Entry point wired from `extension.ts` for `iscFs.flash` /
 * `iscFs.flashWithoutBuild`. Never throws — caller-facing errors
 * surface as toasts and structured output-channel logs.
 */
export async function runFlash(options: FlashOptions): Promise<void> {
    const out = getOutputChannel();
    showOutputChannel();

    const workspace = vscode.workspace.workspaceFolders?.[0];
    if (workspace === undefined) {
        void vscode.window.showErrorMessage(
            'ISC MingoCAN: open a workspace folder before flashing.',
        );
        return;
    }
    const cwd = workspace.uri.fsPath;
    const cfg = readConfig();
    const reflash = options.reflash;

    // Flash requires a target node-id (the CLI refuses to guess which
    // board to overwrite). A re-flash reuses the remembered node-id; a
    // normal flash with `iscFs.nodeId` unset prompts for it (and
    // remembers it) rather than letting the CLI fail with a raw
    // "node-id required" error the operator can't act on.
    if (reflash !== undefined) {
        cfg.nodeId = reflash.nodeId;
    } else if (cfg.nodeId.trim().length === 0) {
        const picked = await promptForNodeId();
        if (picked === undefined) {
            out.appendLine('[cancelled] flash aborted — no node-id provided');
            return;
        }
        cfg.nodeId = picked;
    }

    // Auto-fill an empty `iscFs.firmwareArtifact` with the default
    // glob from package.json. Operators carrying the setting forward
    // as an empty string (saved that way by older versions, or
    // explicitly cleared) get the glob-based discovery without
    // having to touch settings. The artifact resolver below handles
    // 0 / 1 / many matches.
    if (cfg.firmwareArtifact.trim().length === 0) {
        cfg.firmwareArtifact = DEFAULT_FIRMWARE_GLOB;
        out.appendLine(
            `[info] iscFs.firmwareArtifact is empty; falling back to ${DEFAULT_FIRMWARE_GLOB}`,
        );
    }

    // STM32CubeMX-generated CMake projects ship a CMakePresets.json
    // pinning the arm-none-eabi toolchain — bare `cmake -B build`
    // either errors out or builds for the host. When a presets file
    // is present AND the operator is still on the default build
    // command, transparently swap in the `--preset` form so the
    // Flash button "just works" on those projects. Logged so the
    // operator can see which preset got picked.
    const preset = detectCmakePreset(cwd);
    if (preset !== null && cfg.buildCommand === DEFAULT_BUILD_COMMAND) {
        out.appendLine(
            `[info] detected CMakePresets.json; using preset-driven build: ${preset.command}`,
        );
        cfg.buildCommand = preset.command;
        if (
            preset.artifactGlobHint !== null &&
            cfg.firmwareArtifact === DEFAULT_FIRMWARE_GLOB
        ) {
            out.appendLine(
                `[info] narrowing artifact glob to ${preset.artifactGlobHint}`,
            );
            cfg.firmwareArtifact = preset.artifactGlobHint;
        }
    }

    // Mirror the flash lifecycle onto the status-bar Flash item (spinner
    // + terse stage) so progress is visible even when the notification
    // toast isn't focused. The `finally` guarantees the label resets to
    // "Build + Flash" no matter which stage returns/throws.
    setFlashBusy('Starting…');
    try {
    await vscode.window.withProgress(
        {
            location: vscode.ProgressLocation.Notification,
            title: 'ISC MingoCAN: flashing firmware',
            cancellable: true,
        },
        async (progress, token) => {
            // Stage 1: build
            if (!options.skipBuild && cfg.buildCommand.trim().length > 0) {
                progress.report({ message: 'building…' });
                setFlashBusy('Building…');
                const buildOk = await runBuildStep(cfg.buildCommand, cwd, token);
                if (token.isCancellationRequested) {
                    return;
                }
                if (!buildOk) {
                    // Surface the output channel so the operator can
                    // see exactly what the build printed without
                    // hunting through View → Output. Toast offers
                    // an action to jump to `iscFs.buildCommand` in
                    // case the default isn't right for this project.
                    showOutputChannel();
                    const change = 'Change build command';
                    const choice = await vscode.window.showErrorMessage(
                        'ISC MingoCAN: build failed. See the ISC MingoCAN output channel for details.',
                        change,
                    );
                    if (choice === change) {
                        await vscode.commands.executeCommand(
                            'workbench.action.openSettings',
                            'iscFs.buildCommand',
                        );
                    }
                    return;
                }
            } else if (!options.skipBuild) {
                // The operator explicitly asked for build+flash but
                // `iscFs.buildCommand` is empty. The old behaviour
                // silently skipped the build, which looked to many
                // operators like the Flash button "didn't build" —
                // surface the gap so they can fix the setting
                // (or knowingly fall through to flash-only).
                out.appendLine(
                    '[skip] iscFs.buildCommand is empty; build step skipped',
                );
                const choice = await vscode.window.showWarningMessage(
                    'ISC MingoCAN: `iscFs.buildCommand` is empty — the Flash button skipped the build step and is about to flash the existing artifact.',
                    'Set build command',
                    'Continue (flash only)',
                );
                if (choice === 'Set build command') {
                    await vscode.commands.executeCommand(
                        'workbench.action.openSettings',
                        'iscFs.buildCommand',
                    );
                    return;
                }
                // Anything else (dismiss, "Continue") → fall through.
            } else {
                out.appendLine(
                    '[skip] iscFs.flashWithoutBuild: build step skipped',
                );
            }

            // Stage 2: resolve artifact — a re-flash reuses the exact
            // path from last time (no glob, no picker); a normal flash
            // resolves the configured artifact/glob.
            let artifact: string | null;
            if (reflash !== undefined) {
                artifact = reflash.artifactPath;
                out.appendLine(`[info] re-flashing ${artifact}`);
            } else {
                progress.report({ message: 'resolving artifact…' });
                setFlashBusy('Resolving…');
                artifact = await resolveArtifact(cfg.firmwareArtifact, cwd);
            }
            if (artifact === null) {
                return;
            }

            // Stage 3: flash
            progress.report({ message: 'opening session…' });
            setFlashBusy('Opening…');
            await runFlashStep(cfg, artifact, cwd, progress, token);
        },
    );
    } finally {
        setFlashIdle();
    }
}

// ---- Node-id prompt ----

/**
 * Ask the operator for the target node-id when `iscFs.nodeId` is
 * unset. Validates against the CLI's accepted form (0x0–0xF, hex or
 * decimal) and persists the answer to workspace settings so the next
 * flash doesn't re-prompt. Returns the normalized string, or
 * `undefined` if the operator cancelled.
 */
async function promptForNodeId(): Promise<string | undefined> {
    // A role QuickPick beats free text: the three team boards are one
    // click and can't be fat-fingered (a wrong node-id silently fails to
    // enter the bootloader, since the reboot-to-BL payload is per-node).
    // "Other…" keeps the raw entry for off-scheme nodes.
    interface RoleItem extends vscode.QuickPickItem {
        nodeId?: string;
        custom?: boolean;
    }
    const hex = (n: number): string => `0x${n.toString(16).toUpperCase()}`;
    const items: RoleItem[] = [
        ...PROVISION_ROLES.map((r) => ({
            label: r.name.toUpperCase(),
            description: hex(r.nodeId),
            nodeId: hex(r.nodeId),
        })),
        {
            label: 'Other…',
            description: 'custom node-id',
            detail: 'Enter a hex (0x0–0xF) or decimal node-id by hand',
            custom: true,
        },
    ];
    const pick = await vscode.window.showQuickPick(items, {
        title: 'ISC MingoCAN: target board',
        placeHolder: 'Which board are you flashing? (sets --node-id)',
        ignoreFocusOut: true,
    });
    if (pick === undefined) {
        return undefined;
    }

    let normalized: string;
    if (pick.custom === true) {
        const value = await vscode.window.showInputBox({
            title: 'ISC MingoCAN: custom node-id',
            prompt: 'Bootloader node-id (0x0–0xF), hex (e.g. 0x4) or decimal.',
            placeHolder: 'e.g. 0x4',
            ignoreFocusOut: true,
            validateInput: validateNodeId,
        });
        if (value === undefined) {
            return undefined;
        }
        normalized = value.trim();
    } else {
        normalized = pick.nodeId as string;
    }

    // Remember it (workspace-scoped) so routine re-flashes don't
    // re-prompt. The operator can change it in Settings or via
    // "Flash this device…", which overrides per-run.
    try {
        await vscode.workspace
            .getConfiguration('iscFs')
            .update('nodeId', normalized, vscode.ConfigurationTarget.Workspace);
    } catch {
        // Non-fatal — use it for this run even if the write failed
        // (e.g. no workspace settings file writable).
    }
    return normalized;
}

/** Mirrors `parse_node_id` in src/cli/mod.rs: 0x0–0xF, hex (`0x1`) or
 *  decimal (`1`). Returns an error string for the input box, or null
 *  when valid. */
function validateNodeId(raw: string): string | null {
    const t = raw.trim();
    if (t.length === 0) {
        return 'Enter a node-id (0x0–0xF).';
    }
    const isHex = /^0x[0-9a-fA-F]+$/.test(t);
    const isDec = /^[0-9]+$/.test(t);
    if (!isHex && !isDec) {
        return 'Use hex (e.g. 0x1) or decimal (e.g. 1).';
    }
    const n = isHex ? parseInt(t.slice(2), 16) : parseInt(t, 10);
    if (Number.isNaN(n) || n < 0 || n > 0xf) {
        return 'Node-id must be 0x0–0xF (0–15).';
    }
    return null;
}

// ---- Stage helpers ----

async function runBuildStep(
    command: string,
    cwd: string,
    token: vscode.CancellationToken,
): Promise<boolean> {
    const out = getOutputChannel();
    out.appendLine('');
    out.appendLine(`---- build ${new Date().toISOString()} ----`);

    // Spawn through the user's shell so the configured command can
    // contain pipes / chained commands / quoted args without us
    // having to parse it. `/bin/sh -c` is fine for the team's
    // dev workflow today; if Windows operators need cmd.exe we can
    // detect platform later.
    const shell = process.platform === 'win32' ? 'cmd.exe' : '/bin/sh';
    const shellArgs = process.platform === 'win32' ? ['/c', command] : ['-c', command];

    const result = await spawnCommand(shell, shellArgs, {
        cwd,
        cancellation: token,
        onStdoutLine: (line) => out.appendLine(line),
        onStderrLine: (line) => out.appendLine(line),
    });

    if (result.cancelled) {
        out.appendLine('[cancelled] build interrupted by user');
        return false;
    }
    if (result.exitCode !== 0) {
        out.appendLine(`[error] build exited with code ${result.exitCode}`);
        return false;
    }
    return true;
}

async function resolveArtifact(
    pattern: string,
    cwd: string,
): Promise<string | null> {
    // Use VS Code's own findFiles for glob support — works
    // consistently with the workspace's file-watcher exclusions and
    // avoids pulling in a separate `glob` dependency. If the
    // pattern has no glob meta-characters, findFiles returns the
    // single exact match (if it exists).
    const out = getOutputChannel();
    const isAbsolute = path.isAbsolute(pattern);
    if (isAbsolute) {
        // Trust an absolute path — no glob expansion. The caller
        // typed an absolute path knowing what they wanted.
        return pattern;
    }

    const matches = await vscode.workspace.findFiles(
        pattern,
        '**/node_modules/**',
        10,
    );
    if (matches.length === 0) {
        // Before erroring on the DEFAULT glob, cast a wider net. Lots of
        // firmware projects build into a non-`build/` directory (e.g.
        // `build-fw/`, `Debug/`, `out/`) that `**/build/**` never
        // matches. If we find binaries elsewhere, let the operator pick
        // one and PERSIST it to `iscFs.firmwareArtifact` (workspace
        // scope → `.vscode/settings.json`), so the fix sticks and can be
        // committed for the whole team instead of every dev re-hitting
        // this wall.
        if (pattern === DEFAULT_FIRMWARE_GLOB) {
            const wider = await vscode.workspace.findFiles(
                '**/*.{elf,hex,bin}',
                '{**/node_modules/**,**/CMakeFiles/**,**/.git/**}',
                50,
            );
            if (wider.length > 0) {
                const items = wider
                    .map((uri) => ({
                        label: vscode.workspace.asRelativePath(uri),
                        uri,
                    }))
                    .sort((a, b) => a.label.localeCompare(b.label));
                const picked = await vscode.window.showQuickPick(items, {
                    title: 'ISC MingoCAN: no artifact matched the default — pick your firmware',
                    placeHolder:
                        'Saved to iscFs.firmwareArtifact — commit .vscode/settings.json to share it with the team',
                    matchOnDescription: true,
                });
                if (picked === undefined) {
                    out.appendLine(
                        '[cancelled] firmware-artifact auto-discovery cancelled by user',
                    );
                    return null;
                }
                try {
                    await vscode.workspace
                        .getConfiguration('iscFs')
                        .update(
                            'firmwareArtifact',
                            picked.label,
                            vscode.ConfigurationTarget.Workspace,
                        );
                    out.appendLine(
                        `[info] saved iscFs.firmwareArtifact = ${picked.label} (workspace) — commit .vscode/settings.json to share it`,
                    );
                } catch (e) {
                    // Non-fatal: no workspace folder to persist into, etc.
                    // Still flash the picked artifact this run.
                    out.appendLine(
                        `[warn] could not persist iscFs.firmwareArtifact: ${String(e)}`,
                    );
                }
                return picked.uri.fsPath;
            }
        }

        const setPath = 'Set artifact path';
        const buildFirst = 'Build first';
        const detail =
            pattern === DEFAULT_FIRMWARE_GLOB
                ? "no `.elf` / `.hex` / `.bin` found — run your build first, or set `iscFs.firmwareArtifact` to the path your toolchain produces (and commit `.vscode/settings.json` so teammates inherit it)"
                : `no file matched \`${pattern}\` under ${cwd}`;
        const pick = await vscode.window.showErrorMessage(
            `ISC MingoCAN: ${detail}.`,
            buildFirst,
            setPath,
        );
        if (pick === setPath) {
            await vscode.commands.executeCommand(
                'workbench.action.openSettings',
                'iscFs.firmwareArtifact',
            );
        } else if (pick === buildFirst) {
            // Surface the build command setting so the operator can
            // confirm it before re-running Flash. We don't auto-
            // invoke a build here — the next Flash click will do
            // both stages once the operator has produced an artifact.
            await vscode.commands.executeCommand(
                'workbench.action.openSettings',
                'iscFs.buildCommand',
            );
        }
        return null;
    }
    if (matches.length === 1) {
        return matches[0].fsPath;
    }
    // Multiple matches — let the operator pick. Sorted alphabetically
    // for stable ordering across runs.
    const items = matches
        .map((uri) => ({
            label: vscode.workspace.asRelativePath(uri),
            uri,
        }))
        .sort((a, b) => a.label.localeCompare(b.label));
    const picked = await vscode.window.showQuickPick(items, {
        title: 'ISC MingoCAN: pick a firmware artifact',
        matchOnDescription: true,
    });
    out.appendLine(
        picked === undefined
            ? '[cancelled] firmware-artifact pick cancelled by user'
            : `[info] using firmware artifact: ${picked.uri.fsPath}`,
    );
    return picked?.uri.fsPath ?? null;
}

async function runFlashStep(
    cfg: Config,
    artifactPath: string,
    cwd: string,
    progress: vscode.Progress<{ message?: string; increment?: number }>,
    token: vscode.CancellationToken,
): Promise<void> {
    const out = getOutputChannel();
    out.appendLine('');
    out.appendLine(`---- flash ${new Date().toISOString()} ----`);

    const argv = [
        ...buildGlobalArgv(cfg),
        'flash',
        ...flashFlags(cfg),
        artifactPath,
    ];

    const events: FlashEvent[] = [];
    const result = await spawnCommand(cfg.canFlasherPath, argv, {
        cwd,
        cancellation: token,
        onStdoutLine: (line) => {
            out.appendLine(line);
            const ev = parseFlashEvent(line);
            if (ev !== null) {
                events.push(ev);
                progress.report({ message: progressMessage(ev) });
                setFlashBusy(flashStageShort(ev));
            }
        },
        onStderrLine: (line) => out.appendLine(line),
    });

    if (result.cancelled) {
        void vscode.window.showWarningMessage(
            'ISC MingoCAN: flash cancelled. Device may be in an intermediate state — re-run to recover.',
        );
        return;
    }
    if (result.exitCode === 0) {
        const report = parseFlashReport(result.stdout);
        announceSuccess(report, artifactPath);
        // Remember this flash so `iscFs.reflashLast` can repeat it with
        // no build and no prompts.
        void flashMemento?.update(LAST_FLASH_KEY, {
            artifactPath,
            nodeId: cfg.nodeId,
        } satisfies LastFlash);
        // Look at the artifact filename — if it matches a known
        // role (`ams.elf`, `build/ECU.HEX`, etc.) offer to write the
        // node-id NVM key + reset, completing the
        // commission-this-board flow in one toast. Falls through to
        // a no-op when the filename doesn't match — keeps routine
        // flashes (e.g. `firmware.elf`) silent.
        await maybeOfferProvision(artifactPath, cfg, cwd);
        return;
    }
    await announceFailure(result.exitCode, result.stderr);
}

// ---- Argv + UX helpers ----

function flashFlags(cfg: Config): string[] {
    const out: string[] = [];
    // `--yes` is REQUIRED: we spawn `can-flasher` with a piped (non-TTY)
    // stdin, and the CLI's pre-flight gate (`confirm_prompt`, FMEA #271)
    // reads stdin for a y/N — on a pipe that's EOF/blocking, so without
    // `--yes` the flash stalls or fails closed at the prompt. In the GUI
    // the operator's explicit "Build & Flash" action (plus the node-id
    // prompt) IS the confirmation, so skipping the redundant CLI prompt
    // is correct. Do NOT remove this without also feeding stdin.
    out.push('--yes');
    if (cfg.requireWrp) out.push('--require-wrp');
    if (cfg.applyWrp) out.push('--apply-wrp');
    if (cfg.profile) out.push('--profile');
    if (cfg.jumpAfterFlash) {
        out.push('--jump');
    } else {
        out.push('--no-jump');
    }
    return out;
}

function progressMessage(ev: FlashEvent): string {
    switch (ev.event) {
        case 'planning':
            return `planning sector ${ev.sector} (${ev.role})`;
        case 'erased':
            return `erased sector ${ev.sector}`;
        case 'written': {
            const pct = ev.total === 0 ? 0 : Math.floor((ev.bytes * 100) / ev.total);
            return `writing sector ${ev.sector}: ${pct}% (${ev.bytes}/${ev.total} B)`;
        }
        case 'verified':
            return `verified sector ${ev.sector} (crc=${ev.crc})`;
        case 'committing':
            return 'committing';
        case 'done':
            return `done in ${ev.duration_ms} ms`;
    }
}

/** Terse one- or two-word stage label for the status-bar Flash item —
 *  the notification toast carries the detailed `progressMessage`; the
 *  status bar just needs the current phase at a glance. */
function flashStageShort(ev: FlashEvent): string {
    switch (ev.event) {
        case 'planning':
            return 'Planning…';
        case 'erased':
            return `Erasing s${ev.sector}`;
        case 'written': {
            const pct = ev.total === 0 ? 0 : Math.floor((ev.bytes * 100) / ev.total);
            return `Writing s${ev.sector} ${pct}%`;
        }
        case 'verified':
            return `Verifying s${ev.sector}`;
        case 'committing':
            return 'Committing…';
        case 'done':
            return 'Done ✓';
    }
}

// ---- Provision-after-flash hook ----
//
// Mirrors the role registry in `src/cli/provision.rs`. Kept
// duplicated rather than scraped from the CLI's --help output so
// the extension can detect the role offline and the toast wording
// is something we can iterate on without round-tripping a release.
const PROVISION_ROLES: ReadonlyArray<{ name: string; nodeId: number }> = [
    { name: 'ecu', nodeId: 0x01 },
    { name: 'ams', nodeId: 0x02 },
    { name: 'udv', nodeId: 0x03 },
];
const FIRMWARE_EXTS: ReadonlyArray<string> = ['elf', 'hex', 'bin'];

/**
 * If `artifactPath`'s basename stem matches a role from
 * `PROVISION_ROLES`, ask the operator whether to run
 * `can-flasher provision <role>` (which writes the node-id NVM
 * key + resets the bootloader). Returns silently when the filename
 * doesn't match a role — routine flashes don't get prompted.
 */
async function maybeOfferProvision(
    artifactPath: string,
    cfg: Config,
    cwd: string,
): Promise<void> {
    const role = inferRoleFromArtifact(artifactPath);
    if (role === null) {
        return;
    }
    const out = getOutputChannel();
    const accept = `Provision as ${role.name.toUpperCase()}`;
    const skip = 'Skip';
    const choice = await vscode.window.showInformationMessage(
        `ISC MingoCAN: \`${path.basename(artifactPath)}\` looks like the **${role.name.toUpperCase()}** firmware. ` +
            `Also write node-id 0x${role.nodeId.toString(16).padStart(2, '0')} to NVM and reset the device?`,
        accept,
        skip,
    );
    if (choice !== accept) {
        out.appendLine(`[info] provision-after-flash skipped for role ${role.name}`);
        return;
    }
    await runProvision(role.name, cfg, cwd);
}

/** Extract a role from the artifact path's basename stem.
 *  Mirrors `cli/provision.rs::resolve_role_or_path`'s rules for
 *  inferred-from-path inputs (firmware-shaped extensions only,
 *  case-insensitive). */
function inferRoleFromArtifact(
    artifactPath: string,
): { name: string; nodeId: number } | null {
    const base = path.basename(artifactPath);
    const lastDot = base.lastIndexOf('.');
    if (lastDot <= 0) return null; // no extension or hidden-dotfile
    const stem = base.slice(0, lastDot);
    const ext = base.slice(lastDot + 1).toLowerCase();
    if (!FIRMWARE_EXTS.includes(ext)) return null;
    const lowered = stem.toLowerCase();
    return PROVISION_ROLES.find((r) => r.name === lowered) ?? null;
}

/** Shell out to `can-flasher provision <role>` with the current
 *  global flags. Streams stdout/stderr to the output channel and
 *  surfaces a toast on success / failure. */
async function runProvision(role: string, cfg: Config, cwd: string): Promise<void> {
    const out = getOutputChannel();
    out.appendLine('');
    out.appendLine(`---- provision ${new Date().toISOString()} ----`);
    const argv = [...buildGlobalArgv(cfg), 'provision', role];
    const result = await spawnCommand(cfg.canFlasherPath, argv, {
        cwd,
        onStdoutLine: (line) => out.appendLine(line),
        onStderrLine: (line) => out.appendLine(line),
    });
    if (result.exitCode === 0) {
        void vscode.window.showInformationMessage(
            `ISC MingoCAN: provisioned as ${role.toUpperCase()}. Run \`discover\` to confirm the new node-id.`,
        );
    } else {
        void vscode.window.showErrorMessage(
            `ISC MingoCAN: provision ${role} failed (exit ${result.exitCode ?? 'killed'}). See output channel.`,
        );
    }
}

function announceSuccess(report: FlashReport | null, artifactPath: string): void {
    const base = path.basename(artifactPath);
    if (report === null) {
        // Exit 0 but no parseable report — likely a dry-run or a
        // future report-schema change. Still a success; just less
        // informative.
        void vscode.window.showInformationMessage(
            `ISC MingoCAN: flashed ${base} ✓ (no JSON report parsed; see output channel)`,
        );
        return;
    }
    // Name the board + firmware identity the CLI already reported, so a
    // successful flash is a definitive receipt ("Flashed ECU v0.2.0
    // @293db9c") rather than an anonymous "flashed firmware.elf".
    const fw = report.firmware;
    const name = fw.product_name ?? base;
    const ver = fw.version.trim().length > 0 ? ` v${fw.version}` : '';
    const git =
        fw.git_hash !== undefined && fw.git_hash.trim().length > 0
            ? ` @${fw.git_hash.slice(0, 7)}`
            : '';
    const sectors = report.sectors_written.length;
    const skipped = report.sectors_skipped.length;
    const skipStr = skipped > 0 ? `, ${skipped} skipped` : '';
    void vscode.window.showInformationMessage(
        `ISC MingoCAN: flashed ${name}${ver}${git} ✓  ` +
            `(${sectors} sector(s)${skipStr}, ${report.duration_ms} ms)`,
    );
}

async function announceFailure(
    exitCode: number | null,
    stderr: string,
): Promise<void> {
    // Exit-code table from REQUIREMENTS.md § Output and CI integration.
    // Keep in sync with src/cli/mod.rs::ExitCodeHint.
    const hint = exitCodeHint(exitCode);
    const firstLine = stderr.split('\n').find((l) => l.trim().length > 0) ?? '';
    const detail = firstLine.length > 0 ? `\n${firstLine}` : '';

    // Action labels (also used as the returned choice).
    const OPEN_LOG = 'Open log';
    const SELECT_ADAPTER = 'Select adapter…';
    const CHANGE_NODE = 'Change node-id';
    const APPLY_WRP = 'Enable --apply-wrp';
    const REFLASH = 'Re-flash';

    // If the adapter isn't currently on the bus, that's almost certainly
    // the real cause of a timeout / not-found — lead with it.
    const adapterGone = currentSnapshot().presence === 'disconnected';

    const actions: string[] = [];
    switch (exitCode) {
        case 4: // device not found / timeout
            actions.push(SELECT_ADAPTER, CHANGE_NODE, OPEN_LOG);
            break;
        case 9: // adapter not found
            actions.push(SELECT_ADAPTER, OPEN_LOG);
            break;
        case 3: // protection violation (address in a WRP'd sector)
        case 7: // WRP not applied
            actions.push(APPLY_WRP, OPEN_LOG);
            break;
        case 2: // verification mismatch
            actions.push(REFLASH, OPEN_LOG);
            break;
        default:
            actions.push(OPEN_LOG);
    }

    const message =
        adapterGone && (exitCode === 4 || exitCode === 9)
            ? `ISC MingoCAN: no adapter detected — the flash couldn't reach the bus. Connect + select an adapter, then retry.`
            : `ISC MingoCAN: flash failed (exit ${exitCode ?? 'killed'}: ${hint}).${detail}`;

    const choice = await vscode.window.showErrorMessage(message, ...actions);
    switch (choice) {
        case OPEN_LOG:
            showOutputChannel();
            break;
        case SELECT_ADAPTER:
            void vscode.commands.executeCommand('iscFs.selectAdapter');
            break;
        case CHANGE_NODE:
            void vscode.commands.executeCommand(
                'workbench.action.openSettings',
                'iscFs.nodeId',
            );
            break;
        case APPLY_WRP:
            try {
                await vscode.workspace
                    .getConfiguration('iscFs')
                    .update(
                        'applyWrp',
                        true,
                        vscode.ConfigurationTarget.Workspace,
                    );
                void vscode.window.showInformationMessage(
                    'ISC MingoCAN: enabled `--apply-wrp`. Re-run the flash.',
                );
            } catch {
                void vscode.window.showWarningMessage(
                    'ISC MingoCAN: could not write iscFs.applyWrp — set it in Settings.',
                );
            }
            break;
        case REFLASH:
            void vscode.commands.executeCommand('iscFs.flash');
            break;
        default:
            break;
    }
}

function exitCodeHint(code: number | null): string {
    switch (code) {
        case 0:
            return 'ok';
        case 1:
            return 'flash error';
        case 2:
            return 'verification mismatch';
        case 3:
            return 'protection violation';
        case 4:
            return 'device not found / timeout';
        case 7:
            return 'WRP not applied';
        case 8:
            return 'input file error';
        case 9:
            return 'adapter not found';
        case 99:
            return 'generic error';
        case 130:
            return 'interrupted';
        default:
            return code === null ? 'killed' : `code ${code}`;
    }
}
