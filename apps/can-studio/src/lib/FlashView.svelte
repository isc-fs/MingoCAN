<!--
    Flash view — build + flash pipeline.

    All configuration lives in the central `settings` store and
    persists across restarts. Adapter selection comes from
    `settings.adapter` (set in the Adapters view); flash defaults
    + per-run options live in `settings.flash`.
-->
<script lang="ts">
    import { onDestroy, tick } from 'svelte';
    import type { UnlistenFn } from '@tauri-apps/api/event';
    import { ask, open as openDialog } from '@tauri-apps/plugin-dialog';

    import {
        onFlashEvent,
        readRepoFlashConfig,
        runFlash,
        type FlashEvent,
        type FlashRequest,
        type JsonReport,
    } from './flash';
    import { provisionNodeId, ROLES, type Role } from './provision';
    import NodeIdRolePicker from './NodeIdRolePicker.svelte';
    import { settings } from './settings.svelte';
    import type { ViewId } from './stores';

    interface Props {
        navigateTo: (id: ViewId) => void;
    }
    const { navigateTo }: Props = $props();

    // The Flash tab exposes exactly one build choice — Release vs.
    // Debug. The build command, working directory, and artifact path
    // are "set once" in Settings; each may contain a `{profile}`
    // placeholder we substitute with the chosen profile here.
    //
    // CMake's `--config` wants a capitalized name (`Release`/`Debug`),
    // and multi-config generators nest their output under the same
    // capitalized directory, so we substitute the capitalized form
    // into both the command and the artifact path.
    function profileLabel(): string {
        return settings.flash.buildProfile === 'debug' ? 'Debug' : 'Release';
    }
    function applyProfile(text: string): string {
        return text.replaceAll('{profile}', profileLabel());
    }

    // Build working directory — the folder the build command runs in
    // (usually the CMake project root). Lives on the Flash tab (not
    // just Settings) because it's the one build field that changes as
    // you hop between projects on the bench.
    async function browseForBuildCwd(): Promise<void> {
        const current = settings.flash.buildCwd.trim();
        const picked = await openDialog({
            title: 'Pick the build working directory',
            multiple: false,
            directory: true,
            canCreateDirectories: true,
            defaultPath: current.length > 0 ? current : undefined,
        });
        if (typeof picked === 'string' && picked.length > 0) {
            settings.flash.buildCwd = picked;
        }
    }

    const adapterReady = $derived(
        settings.adapter.interface !== null &&
            (settings.adapter.interface === 'virtual' ||
                settings.adapter.channel.length > 0),
    );

    let running = $state<boolean>(false);
    let log = $state<string[]>([]);
    let progressMessage = $state<string>('');
    let result = $state<JsonReport | null>(null);
    let error = $state<string | null>(null);

    // ---- Overall flash progress (per-sector granularity) ----
    // `sectorsPlanned` is the count of sectors the planner said
    // need writing (role='write'); `sectorsWritten` increments on
    // every successful `verified` event. The pair gives a clean
    // 0–100% bar that doesn't include skipped sectors.
    let sectorsPlanned = $state<number>(0);
    let sectorsWritten = $state<number>(0);
    // Per-sector byte progress, refreshed by each `written` event.
    // Used to fill in fractional progress between sector
    // completions, so the bar moves continuously on big sectors
    // rather than jumping in chunks.
    let currentSectorBytes = $state<number>(0);
    let currentSectorTotal = $state<number>(0);

    const overallPct = $derived(
        sectorsPlanned > 0
            ? Math.min(
                  100,
                  Math.floor(
                      ((sectorsWritten +
                          (currentSectorTotal > 0
                              ? currentSectorBytes / currentSectorTotal
                              : 0)) *
                          100) /
                          sectorsPlanned,
                  ),
              )
            : null,
    );
    // Track which side of "done" the last run landed on so the
    // status indicator can show a clear green/red badge instead
    // of relying on the operator reading the log to figure it
    // out. null means "no run yet" or "currently running".
    let lastOutcome = $state<'success' | 'failure' | null>(null);
    let copyState = $state<'idle' | 'copied'>('idle');

    // ---- Provision-after-flash ---------------------------------
    // The Target-board picker is the single source of truth for which
    // board this is (it was previously inferred from the artifact
    // filename, which could disagree with the selected target — e.g. a
    // stale `ams.elf` path while flashing the ECU). When a known role is
    // selected (ECU/AMS/uDV — not Custom), a successful flash offers to
    // commission the board as that role (write its node-id to NVM +
    // reset). A confirm dialog — not a silent pre-checked box — keeps
    // commissioning an explicit choice.
    const provisionRole = $derived<Role | null>(
        ROLES.find((r) => r.nodeId === settings.adapter.nodeId) ?? null,
    );
    let provisionState = $state<
        | { kind: 'idle' }
        | { kind: 'running' }
        | { kind: 'ok'; role: string }
        | { kind: 'error'; message: string }
    >({ kind: 'idle' });

    let unlisten: UnlistenFn | null = null;

    // Log scroll behaviour — terminal-style follow-tail. When the
    // user is already at (or within 24px of) the bottom of the
    // log, we auto-scroll on every new line so they see live
    // output. The moment they scroll up to read something, we
    // stop following so they're not yanked away mid-read.
    let logEl: HTMLDivElement | null = $state(null);
    let followTail = $state<boolean>(true);

    function onLogScroll(): void {
        if (logEl === null) return;
        const distanceFromBottom =
            logEl.scrollHeight - logEl.scrollTop - logEl.clientHeight;
        followTail = distanceFromBottom < 24;
    }

    async function maybeFollowTail(): Promise<void> {
        if (!followTail || logEl === null) return;
        await tick();
        if (logEl !== null) logEl.scrollTop = logEl.scrollHeight;
    }

    function resetLog(): void {
        log = [];
        followTail = true;
    }

    async function start(opts: { skipBuild: boolean }): Promise<void> {
        if (running) return;
        if (!adapterReady) {
            error = 'Pick an adapter in the Adapters view first.';
            return;
        }
        // A repo's committed `.vscode/settings.json` (iscFs.* keys) is the
        // source of truth when the Build directory points at one — exactly
        // like the VS Code extension. It overrides the app's stored build
        // command / artifact / node-id, so there's no per-machine setup.
        const buildCwd =
            settings.flash.buildCwd.trim().length > 0
                ? settings.flash.buildCwd
                : null;
        const repoCfg = buildCwd ? await readRepoFlashConfig(buildCwd) : null;

        // Artifact: the repo value is already an absolute path; the app's
        // own path still carries the `{profile}` placeholder to resolve.
        const artifactRaw = (
            repoCfg?.artifactPath ?? settings.flash.artifactPath
        ).trim();
        if (artifactRaw.length === 0) {
            error = 'Set a firmware artifact path in Settings first.';
            return;
        }
        const artifactPath = repoCfg?.artifactPath
            ? repoCfg.artifactPath
            : applyProfile(artifactRaw);

        // Node-id (which board) is the operator's explicit choice via the
        // Target-board picker — the committed repo config only supplies
        // *how to build* (command + artifact), not which node to hit, so
        // the picker stays the single source of truth and can't be
        // silently overridden out from under the flash / provision path.

        running = true;
        resetLog();
        if (repoCfg) {
            log = [...log, `[info] build config from ${repoCfg.source}`];
        }
        progressMessage = 'starting…';
        result = null;
        error = null;
        lastOutcome = null;
        // Reset the bar — a previous run's counters would
        // otherwise show "done" while the new flash is starting.
        sectorsPlanned = 0;
        sectorsWritten = 0;
        currentSectorBytes = 0;
        currentSectorTotal = 0;

        unlisten = await onFlashEvent((event) => {
            log = [...log, formatLogLine(event)];
            const msg = formatProgress(event);
            if (msg !== null) progressMessage = msg;
            // Drive the overall progress bar from the streamed
            // events. Counts and byte-totals stay in sync with
            // the textual `progressMessage` because both are fed
            // by the same `event` payload.
            if (event.kind === 'planning' && event.role === 'write') {
                sectorsPlanned += 1;
            } else if (event.kind === 'written') {
                currentSectorBytes = event.bytes;
                currentSectorTotal = event.total;
            } else if (event.kind === 'verified') {
                sectorsWritten += 1;
                currentSectorBytes = 0;
                currentSectorTotal = 0;
            }
            void maybeFollowTail();
        });

        try {
            const buildCommandRaw =
                repoCfg?.buildCommand ?? settings.flash.buildCommand;
            const buildCmd = opts.skipBuild
                ? null
                : buildCommandRaw.trim().length > 0
                    ? applyProfile(buildCommandRaw)
                    : null;
            const payload: FlashRequest = {
                artifactPath,
                buildCommand: buildCmd,
                buildCwd,
                interface: settings.adapter.interface!, // adapterReady guard
                channel:
                    settings.adapter.channel.length > 0
                        ? settings.adapter.channel
                        : null,
                bitrate: settings.adapter.bitrate,
                nodeId: settings.adapter.nodeId,
                timeoutMs: settings.adapter.timeoutMs,
                keepaliveMs: 5_000,
                diff: settings.flash.diff,
                dryRun: settings.flash.dryRun,
                verifyAfter: settings.flash.verifyAfter,
                finalCommit: settings.flash.finalCommit,
                jump: settings.flash.jump,
                enterBootloader: settings.flash.enterBootloader,
            };
            const report = await runFlash(payload);
            result = report;
            progressMessage = `done in ${report.duration_ms} ms`;
            lastOutcome = 'success';

            // Provision-after-flash hook. When a known role is the
            // selected target, pop a confirm dialog asking whether to
            // commission the board as that role (write node-id + reset).
            // Custom target → nothing to ask. Failures here don't roll
            // back the flash — surface the error and let the operator
            // re-run later if they need to retry.
            const role = provisionRole;
            const wantsProvision =
                role !== null &&
                (await ask(
                    `Target board: ${role.name.toUpperCase()}.\n\n` +
                        `Commission this board as ${role.name.toUpperCase()}? ` +
                        `This writes node-id 0x${role.nodeId
                            .toString(16)
                            .toUpperCase()} to NVM and resets the board.`,
                    {
                        title: 'ISC MingoCAN — Commission board',
                        kind: 'info',
                        okLabel: `Commission as ${role.name.toUpperCase()}`,
                        cancelLabel: 'Skip',
                    },
                ));
            if (role !== null && wantsProvision) {
                provisionState = { kind: 'running' };
                try {
                    await provisionNodeId({
                        role: role.name,
                        interface: settings.adapter.interface!,
                        channel:
                            settings.adapter.channel.length > 0
                                ? settings.adapter.channel
                                : null,
                        bitrate: settings.adapter.bitrate,
                        nodeId: settings.adapter.nodeId,
                        timeoutMs: settings.adapter.timeoutMs,
                    });
                    provisionState = { kind: 'ok', role: role.name };
                } catch (provErr) {
                    provisionState = {
                        kind: 'error',
                        message:
                            provErr instanceof Error
                                ? provErr.message
                                : String(provErr),
                    };
                }
            } else {
                provisionState = { kind: 'idle' };
            }
        } catch (err) {
            error = err instanceof Error ? err.message : String(err);
            progressMessage = 'failed';
            lastOutcome = 'failure';
        } finally {
            running = false;
            if (unlisten !== null) {
                unlisten();
                unlisten = null;
            }
        }
    }

    onDestroy(() => {
        if (unlisten !== null) unlisten();
    });

    // Copy the entire log buffer to the system clipboard. The
    // navigator.clipboard.writeText API resolves regardless of
    // length; failures (denied permission, e.g.) flip back to
    // 'idle' so the user can retry. The 'copied' state holds for
    // 1.5s to give visual feedback.
    async function copyLog(): Promise<void> {
        if (log.length === 0) return;
        try {
            await navigator.clipboard.writeText(log.join('\n'));
            copyState = 'copied';
            setTimeout(() => {
                copyState = 'idle';
            }, 1500);
        } catch {
            copyState = 'idle';
        }
    }

    function formatProgress(event: FlashEvent): string | null {
        switch (event.kind) {
            case 'planning':
                return `planning sector ${event.sector} (${event.role})`;
            case 'erased':
                return `erased sector ${event.sector}`;
            case 'written': {
                const pct =
                    event.total === 0
                        ? 0
                        : Math.floor((event.bytes * 100) / event.total);
                return `writing sector ${event.sector}: ${pct}% (${event.bytes}/${event.total} B)`;
            }
            case 'verified':
                return `verified sector ${event.sector} (crc=${event.crc})`;
            case 'committing':
                return 'committing';
            case 'done':
                return `done in ${event.report.duration_ms} ms`;
            case 'build_line':
            case 'build_exited':
                return null;
        }
    }

    function formatLogLine(event: FlashEvent): string {
        switch (event.kind) {
            case 'build_line':
                return `[${event.stream}] ${event.text}`;
            case 'build_exited':
                return `[build] exited with code ${event.code ?? 'killed'}`;
            case 'planning':
                return `[plan] sector ${event.sector} → ${event.role}`;
            case 'erased':
                return `[erase] sector ${event.sector}`;
            case 'written':
                return `[write] sector ${event.sector}  ${event.bytes}/${event.total} B`;
            case 'verified':
                return `[verify] sector ${event.sector}  crc=${event.crc}`;
            case 'committing':
                return '[commit] CMD_FLASH_VERIFY';
            case 'done':
                return `[done] ${event.report.size} B  crc=${event.report.crc32}  in ${event.report.duration_ms} ms`;
        }
    }
</script>

<div class="view">
    <header>
        <h2>Flash</h2>
        <p class="muted">
            Build the firmware, flash it through the selected adapter, and
            stream phase-by-phase progress here.
        </p>
    </header>

    {#if !adapterReady}
        <div class="banner banner-warning gate">
            <span>
                <strong>No adapter selected.</strong> Flashing needs an
                <code>--interface</code>/<code>--channel</code> pair.
            </span>
            <button
                type="button"
                class="btn btn-sm gate-action"
                onclick={() => navigateTo('adapters')}
            >
                Choose adapter →
            </button>
        </div>
    {/if}

    <div class="card form">
        <div class="field">
            <span class="field-label">Build profile</span>
            <div class="segmented" role="radiogroup" aria-label="Build profile">
                <button
                    type="button"
                    class="seg"
                    class:active={settings.flash.buildProfile === 'release'}
                    role="radio"
                    aria-checked={settings.flash.buildProfile === 'release'}
                    onclick={() => (settings.flash.buildProfile = 'release')}
                >
                    Release
                </button>
                <button
                    type="button"
                    class="seg"
                    class:active={settings.flash.buildProfile === 'debug'}
                    role="radio"
                    aria-checked={settings.flash.buildProfile === 'debug'}
                    onclick={() => (settings.flash.buildProfile = 'debug')}
                >
                    Debug
                </button>
            </div>
            <p class="hint">
                Substituted for <code>{'{profile}'}</code> in the build
                command + artifact path. Configure those once in
                <strong>Settings → Firmware build</strong>.
            </p>
        </div>

        <div class="field">
            <label for="buildcwd">Build directory</label>
            <div class="dir-row">
                <input
                    id="buildcwd"
                    class="input mono"
                    type="text"
                    placeholder="(defaults to the artifact's parent)"
                    bind:value={settings.flash.buildCwd}
                />
                <button type="button" class="btn btn-sm" onclick={browseForBuildCwd}>
                    Browse…
                </button>
            </div>
            <p class="hint">
                Folder the build command runs in — usually your project
                root. The build command + artifact path themselves stay in
                <strong>Settings → Firmware build</strong>.
            </p>
        </div>

        <div class="field">
            <span class="field-label">Target board</span>
            <NodeIdRolePicker bind:value={settings.adapter.nodeId} />
            <p class="hint">
                Which board you're flashing — the host aims the
                reboot-to-bootloader trigger at this node, and the ECU and
                AMS use different magic, so the role must match the board.
            </p>
        </div>

        <div class="row-two">
            <div class="field">
                <label for="bitrate">Bitrate (bps)</label>
                <input
                    id="bitrate"
                    class="input mono"
                    type="number"
                    min="10000"
                    max="1000000"
                    step="1000"
                    bind:value={settings.adapter.bitrate}
                />
            </div>
            <div class="field">
                <label for="timeout">Frame timeout (ms)</label>
                <input
                    id="timeout"
                    class="input mono"
                    type="number"
                    min="50"
                    max="60000"
                    bind:value={settings.adapter.timeoutMs}
                />
            </div>
        </div>

        <details class="advanced">
            <summary>Advanced options</summary>
            <div class="opts">
                <label
                    class="toggle"
                    title="Only rewrite sectors whose contents changed on the device — faster reflashes."
                ><input type="checkbox" bind:checked={settings.flash.diff} /> Skip
                    unchanged sectors</label>
                <label
                    class="toggle"
                    title="Read each sector's CRC back after writing and compare it."
                ><input
                        type="checkbox"
                        bind:checked={settings.flash.verifyAfter}
                    /> Verify each sector after writing</label>
                <label
                    class="toggle"
                    title="Send a final whole-image CRC check so the bootloader marks the new app valid."
                ><input
                        type="checkbox"
                        bind:checked={settings.flash.finalCommit}
                    /> Confirm the whole image at the end</label>
                <label
                    class="toggle"
                    title="Boot straight into the flashed application when the flash finishes."
                ><input type="checkbox" bind:checked={settings.flash.jump} /> Start
                    the app after flashing</label>
                <label
                    class="toggle"
                    title="If the board is running its application, send the reboot-to-bootloader trigger so it can be flashed without a manual reset."
                ><input
                        type="checkbox"
                        bind:checked={settings.flash.enterBootloader}
                    /> Reboot a running board into the bootloader</label>
                <label
                    class="toggle"
                    title="Walk the whole pipeline but send no erase/write commands — a safe rehearsal."
                ><input type="checkbox" bind:checked={settings.flash.dryRun} /> Dry
                    run — no erases or writes</label>
            </div>
        </details>
        {#if provisionRole !== null}
            <!--
                Heads-up that a confirm dialog will appear after a
                successful flash — driven by the selected Target board,
                not a control here. Skippable, for routine reflashes.
            -->
            <p class="provision-hint">
                After a successful flash you'll be asked whether to
                commission this board as
                <strong>{provisionRole.name.toUpperCase()}</strong>
                (write node-id 0x{provisionRole.nodeId
                    .toString(16)
                    .toUpperCase()} + reset) — skip it for a routine reflash.
            </p>
        {/if}
    </div>

    <div class="actions">
        <button
            type="button"
            class="btn btn-primary"
            disabled={running || !adapterReady}
            onclick={() => start({ skipBuild: false })}
        >
            Build & Flash
        </button>
        <button
            type="button"
            class="btn"
            disabled={running || !adapterReady}
            onclick={() => start({ skipBuild: true })}
        >
            Flash (skip build)
        </button>
    </div>

    {#if running || progressMessage.length > 0}
        <!--
            Status badge — three visual states, so an operator at
            the bench can tell at a glance whether the last action
            succeeded or failed without parsing the log:
              running  → amber pulse dot
              success  → green check
              failure  → red cross
              idle     → muted ring (initial state before any run)
        -->
        <div
            class="progress"
            class:running
            class:success={!running && lastOutcome === 'success'}
            class:failure={!running && lastOutcome === 'failure'}
        >
            <strong class="status-icon">
                {#if running}
                    ●
                {:else if lastOutcome === 'success'}
                    ✓
                {:else if lastOutcome === 'failure'}
                    ✕
                {:else}
                    ○
                {/if}
            </strong>
            <span>{progressMessage}</span>
            {#if running && overallPct !== null}
                <span class="overall-pct">{overallPct}%</span>
            {/if}
        </div>

        {#if running}
            <!--
                Continuous bar fed by the same event stream as the
                text status. Indeterminate stripe before the
                planner emits any sector counts, fills with the
                accent once `sectorsPlanned > 0`.
            -->
            <div
                class="bar"
                class:indeterminate={overallPct === null}
                role="progressbar"
                aria-label="Flash progress"
                aria-valuemin="0"
                aria-valuemax="100"
                aria-valuenow={overallPct ?? 0}
            >
                <div
                    class="fill"
                    style:width={overallPct !== null ? `${overallPct}%` : '100%'}
                ></div>
            </div>
        {/if}
    {/if}

    {#if error !== null}
        <div class="banner banner-danger">{error}</div>
    {/if}

    {#if result !== null}
        <div class="banner banner-success">
            <strong>Done.</strong>
            {result.sectors_written.length} sector(s) written,
            {result.sectors_skipped.length} skipped,
            {result.duration_ms} ms.
            CRC {result.crc32}, {result.size} bytes.
        </div>
    {/if}

    <!--
        Provision-after-flash status. Lives next to the flash
        result so the operator can read the whole one-click
        outcome in one glance: "flashed ✓, provisioned ✓".
        Suppressed in `idle` (no role detected or operator
        unchecked the box) — no noise in that case.
    -->
    {#if provisionState.kind === 'running'}
        <div class="banner">
            <strong>Provisioning…</strong>
            writing node-id NVM key, resetting the bootloader.
        </div>
    {:else if provisionState.kind === 'ok'}
        <div class="banner banner-success">
            <strong>Provisioned as {provisionState.role.toUpperCase()}.</strong>
            Run <code>discover</code> to confirm the new node-id.
        </div>
    {:else if provisionState.kind === 'error'}
        <div class="banner banner-danger">
            <strong>Provision failed:</strong>
            {provisionState.message}
        </div>
    {/if}

    {#if log.length > 0}
        <div class="log-wrap">
            <div class="log-header">
                <span class="log-count mono small">
                    {log.length} line{log.length === 1 ? '' : 's'}
                </span>
                <button
                    type="button"
                    class="btn btn-sm"
                    onclick={copyLog}
                    title="Copy the full log buffer to the clipboard"
                >
                    {copyState === 'copied' ? '✓ Copied' : 'Copy log'}
                </button>
            </div>
            <!--
                Per-line rendering instead of `{log.join('\n')}` in one <pre>:
                each line is its own DOM node, so the browser's diff algorithm
                only appends new children rather than rebuilding the whole
                text. That preserves scroll position during chatty builds
                (e.g. gcc with -W warnings firing dozens of lines a second).
            -->
            <div
                class="log"
                bind:this={logEl}
                onscroll={onLogScroll}
            >
                {#each log as line, i (i)}
                    <div class="log-line">{line}</div>
                {/each}
            </div>
        </div>
    {/if}
</div>

<style>
    /* The shared design system supplies: .view, .card, .field,
       .toggle, .btn (+variants), .banner (+variants), .input, .small,
       .muted, .mono. This file's local styles cover the bits that
       are genuinely Flash-specific: the form's stack rhythm + grid,
       the Release/Debug segmented control, the running-progress badge
       with its status icon, the indeterminate progress bar, and the
       log window with its copy-row header. */

    /* Form card — tighter gap than the default .stack (10px vs.
       12px) so a long field stack stays compact enough to read at
       a glance. */
    .form {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
    }

    /* Field labels — small, muted, top-stacked with the input. The
       label/input pair lives in a .field; this just sizes the
       leading <label> consistent with the rest of the design
       system's "small caption" pattern. */
    .field > label {
        font-size: var(--text-sm);
        color: var(--text-muted);
    }

    .hint {
        margin: var(--space-1) 0 0;
        font-size: var(--text-xs);
        color: var(--text-muted);
        line-height: 1.5;
    }
    .hint code {
        font-family: var(--font-mono);
        font-size: 0.95em;
        padding: 0 3px;
        border-radius: var(--radius-sm);
        background: rgba(255, 255, 255, 0.04);
    }
    .hint strong {
        color: var(--text);
        font-weight: 600;
    }

    /* Static field label for the segmented control — matches the
       muted caption look the design system's <label> elements get
       via `.field > label` above, but as a non-interactive span. */
    .field-label {
        font-size: var(--text-sm);
        color: var(--text-muted);
    }

    /* Release/Debug segmented control — two pill buttons sharing a
       track, the active one filled with the accent. Replaces the old
       build-command editor: the only build choice on the Flash tab. */
    .segmented {
        display: inline-flex;
        gap: 2px;
        padding: 2px;
        background: var(--bg);
        border: 1px solid var(--border);
        border-radius: var(--radius-md);
        align-self: flex-start;
    }
    .seg {
        appearance: none;
        border: none;
        background: transparent;
        color: var(--text-muted);
        font: inherit;
        font-size: var(--text-sm);
        padding: var(--space-1) var(--space-4);
        border-radius: calc(var(--radius-md) - 2px);
        cursor: pointer;
        transition:
            background var(--motion-base),
            color var(--motion-base);
    }
    .seg:hover {
        color: var(--text);
    }
    .seg.active {
        background: var(--accent);
        color: var(--accent-contrast, #fff);
        font-weight: 600;
    }

    /* Build-directory row — text input flexes to fill, Browse… button
       hugs its content. */
    .dir-row {
        display: flex;
        gap: var(--space-2);
    }
    .dir-row .input {
        flex: 1;
    }

    /* Two-up grid for bitrate / timeout. Drops to a single column at
       narrow widths so the sidebar-collapsed layout doesn't squish the
       inputs. (Node-id moved up into the Target-role picker.) */
    .row-two {
        display: grid;
        grid-template-columns: repeat(2, 1fr);
        gap: var(--space-3);
    }
    @media (max-width: 720px) {
        .row-two {
            grid-template-columns: 1fr;
        }
    }

    /* Advanced-options disclosure — the six per-run flags are sane by
       default, so tuck them behind a twisty instead of fronting the
       happy path with a wall of jargon checkboxes. */
    .advanced {
        margin-top: var(--space-1);
    }
    .advanced summary {
        cursor: pointer;
        user-select: none;
        list-style: none;
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        padding: var(--space-1) 0;
        font-size: var(--text-sm);
        color: var(--text-secondary);
    }
    .advanced summary::-webkit-details-marker {
        display: none;
    }
    .advanced summary::before {
        content: '▸';
        color: var(--text-muted);
        transition: transform var(--motion-fast);
    }
    .advanced[open] summary::before {
        transform: rotate(90deg);
    }
    .advanced summary:hover {
        color: var(--text);
    }

    /* Per-run options — wrapping row of checkboxes. The .toggle
       utility handles spacing + accent-color; we just lay them out
       horizontally with a wider gap than the in-card stack so they
       breathe. */
    .opts {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-4);
        margin-top: var(--space-3);
    }
    /* Commission heads-up — shown under the toggles when the
       artifact name resolves to a role; the actual choice is the
       post-flash confirm dialog. */
    .provision-hint {
        margin: var(--space-2) 0 0;
        font-size: var(--text-xs);
        color: var(--text-muted);
        line-height: 1.5;
    }
    .provision-hint strong {
        color: var(--text);
    }

    /* Buttons row under the form — gap matches the form's internal
       spacing so the page reads as a vertical rhythm. */
    .actions {
        display: flex;
        gap: var(--space-2);
    }

    /* Running-progress badge. This is bespoke (no .progress
       utility in the design system) — a status icon + textual
       message + optional overall-pct readout, in a banner that
       changes border/background tint to reflect the last outcome.
       The classes here (.running / .success / .failure) layer on
       top of a base .progress that uses the same chrome as a
       .banner but with mono-font + transition affordances. */
    .progress {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-3) var(--space-4);
        border: 1px solid var(--border);
        background: var(--surface);
        border-radius: var(--radius-md);
        font-family: var(--font-mono);
        font-size: var(--text-sm);
        transition:
            border-color var(--motion-base),
            background var(--motion-base);
    }
    .status-icon {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 18px;
        height: 18px;
        line-height: 1;
        font-size: var(--text-base);
    }
    .progress.running .status-icon {
        color: var(--accent);
        animation: pulse 1.2s ease-in-out infinite;
    }
    .progress.success {
        border-color: var(--success);
        background: var(--success-soft);
    }
    .progress.success .status-icon {
        color: var(--success);
        font-weight: 700;
    }
    .progress.failure {
        border-color: var(--danger);
        background: var(--danger-soft);
    }
    .progress.failure .status-icon {
        color: var(--danger);
        font-weight: 700;
    }
    .overall-pct {
        margin-left: auto;
        font-family: var(--font-mono);
        font-size: var(--text-sm);
        color: var(--text-muted);
    }

    /* Per-run progress bar — same look as the SWD view's bar. */
    .bar {
        height: 8px;
        background: var(--bg);
        border-radius: var(--radius-sm);
        overflow: hidden;
        border: 1px solid var(--border);
    }
    .bar .fill {
        height: 100%;
        background: var(--accent);
        transition: width 120ms ease-out;
    }
    .bar.indeterminate .fill {
        animation: bar-slide 1.2s ease-in-out infinite;
        background: linear-gradient(
            90deg,
            transparent 0%,
            var(--accent) 50%,
            transparent 100%
        );
    }
    @keyframes bar-slide {
        0% {
            transform: translateX(-100%);
        }
        100% {
            transform: translateX(100%);
        }
    }
    @keyframes pulse {
        0%,
        100% {
            opacity: 1;
        }
        50% {
            opacity: 0.4;
        }
    }

    /* Log window — terminal-style follow-tail output. Bespoke
       layout because it has a sticky-style header (line count +
       copy button) above a scrollable body. */
    .log-wrap {
        display: flex;
        flex-direction: column;
        gap: 0;
        border: 1px solid var(--border);
        border-radius: var(--radius-md);
        overflow: hidden;
    }
    .log-header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        padding: var(--space-2) var(--space-3);
        background: var(--surface);
        border-bottom: 1px solid var(--border);
        color: var(--text-muted);
    }
    .log {
        margin: 0;
        max-height: 320px;
        overflow: auto;
        padding: var(--space-3);
        background: var(--bg);
        font-family: var(--font-mono);
        font-size: var(--text-sm);
        line-height: 1.5;
    }
    .log-line {
        white-space: pre-wrap;
        word-break: break-all;
    }
</style>
