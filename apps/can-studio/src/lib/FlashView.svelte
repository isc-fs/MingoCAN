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
    import { open as openDialog } from '@tauri-apps/plugin-dialog';

    import {
        onFlashEvent,
        readCmakePresets,
        runBuildOnly,
        runFlash,
        type CmakePresetInfo,
        type FlashEvent,
        type FlashRequest,
        type JsonReport,
    } from './flash';
    import {
        inferRoleFromArtifact,
        provisionNodeId,
        type Role,
    } from './provision';
    import { settings } from './settings.svelte';

    // Static fallback templates — shown alongside any CMake presets
    // we discover, so projects without a CMakePresets.json still
    // get one-click fills for the most common build systems.
    interface BuildTemplate {
        label: string;
        command: string;
        note: string;
    }
    const STATIC_TEMPLATES: BuildTemplate[] = [
        {
            label: 'CMake out-of-tree (configure + build)',
            command: 'cmake -S . -B build && cmake --build build',
            note: 'Configures into ./build and compiles. Needs a toolchain file passed via -D… for cross-compiling.',
        },
        {
            label: 'CMake out-of-tree with arm-none-eabi toolchain',
            command:
                'cmake -S . -B build -DCMAKE_TOOLCHAIN_FILE=cmake/gcc-arm-none-eabi.cmake && cmake --build build',
            note: 'STM32CubeMX-style: points CMake at the bundled ARM GCC toolchain file before configuring.',
        },
        {
            label: 'Plain Make',
            command: 'make',
            note: "For projects with a top-level Makefile (no CMake).",
        },
        {
            label: 'Zephyr west build',
            command: 'west build -t flash-elf',
            note: 'Zephyr RTOS projects driven by the west meta-tool.',
        },
    ];

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
    // When the artifact filename matches a role (`ams.elf` /
    // `ecu.hex` / `udv.bin` etc.) we offer to write the matching
    // node-id NVM key + reset the board after a successful flash.
    // The toggle is operator-controlled — auto-checked when a
    // role gets detected so the common case is one-click, but
    // never magically fires without the operator's consent.
    const detectedRole = $derived<Role | null>(
        inferRoleFromArtifact(settings.flash.artifactPath.trim()),
    );
    let provisionAfterFlash = $state<boolean>(true);
    let provisionState = $state<
        | { kind: 'idle' }
        | { kind: 'running' }
        | { kind: 'ok'; role: string }
        | { kind: 'error'; message: string }
    >({ kind: 'idle' });

    // CMake build presets discovered from <cwd>/CMakePresets.json,
    // refreshed whenever the build cwd changes. Empty when there's
    // no presets file (or it's invalid) — the templates dropdown
    // still shows the static fallback list.
    let cmakePresets = $state<CmakePresetInfo[]>([]);
    let templatesOpen = $state<boolean>(false);

    // Run the discovery whenever build cwd changes. Debounced via a
    // microtask so rapid input doesn't spam the backend; cancellable
    // via a generation counter so a stale response can't clobber a
    // fresh one.
    let presetsGen = 0;
    $effect(() => {
        const cwd = settings.flash.buildCwd.trim();
        const gen = ++presetsGen;
        if (cwd.length === 0) {
            cmakePresets = [];
            return;
        }
        readCmakePresets(cwd)
            .then((list) => {
                if (gen === presetsGen) cmakePresets = list;
            })
            .catch(() => {
                if (gen === presetsGen) cmakePresets = [];
            });
    });

    function pickTemplate(template: BuildTemplate): void {
        settings.flash.buildCommand = template.command;
        templatesOpen = false;
    }

    function pickPreset(preset: CmakePresetInfo): void {
        settings.flash.buildCommand = preset.command;
        if (
            preset.artifactHint !== null &&
            settings.flash.artifactPath.trim().length === 0
        ) {
            // The hint is a directory (the configure preset's
            // binaryDir). Combine it with the cwd basename — the
            // STM32CubeMX/CMake convention is `<binaryDir>/<project>.elf`,
            // and we still don't have an authoritative way to know
            // the .elf name without compiling. Operators can fix it
            // post-build via the auto-fill in the build-only path.
            const cwd = settings.flash.buildCwd.trim().replace(/\/+$/, '');
            const sep = cwd.includes('\\') ? '\\' : '/';
            const basename = cwd.split(sep).pop() ?? '';
            if (basename.length > 0) {
                const dir = preset.artifactHint.replace(/[\\/]+$/, '');
                settings.flash.artifactPath = `${dir}${sep}${basename}.elf`;
            }
        }
        templatesOpen = false;
    }

    // Close the dropdown when clicking anywhere outside it. Bound
    // to window so we don't have to thread refs through children.
    function onWindowClick(e: MouseEvent): void {
        if (!templatesOpen) return;
        const target = e.target as HTMLElement | null;
        if (target?.closest('.templates-dropdown') !== null) return;
        if (target?.closest('.templates-trigger') !== null) return;
        templatesOpen = false;
    }
    $effect(() => {
        if (typeof window === 'undefined') return;
        window.addEventListener('click', onWindowClick);
        return () => window.removeEventListener('click', onWindowClick);
    });

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

    // STM32-CMake convention: out-of-tree build under <cwd>/build/,
    // with the .elf named after the CMake `project()` directive,
    // which by convention matches the project root's basename. We
    // can't introspect CMakeLists.txt from here, so we guess from
    // the cwd basename — operators can override by typing.
    function guessArtifactFromCwd(cwd: string): string {
        const trimmed = cwd.trim().replace(/\/+$/, '');
        if (trimmed.length === 0) return '';
        const sep = trimmed.includes('\\') ? '\\' : '/';
        const basename = trimmed.split(sep).pop() ?? '';
        if (basename.length === 0) return '';
        return `${trimmed}${sep}build${sep}${basename}.elf`;
    }

    // For "Build & Flash" the artifact only needs to exist *after*
    // the build step (it's an output, not an input). If the user
    // hasn't typed one but has a build cwd, derive a sensible
    // default and stash it in settings — they can fix it after the
    // first build if our guess was wrong.
    function ensureArtifactPath(): string {
        const current = settings.flash.artifactPath.trim();
        if (current.length > 0) return current;
        const derived = guessArtifactFromCwd(settings.flash.buildCwd);
        if (derived.length > 0) {
            settings.flash.artifactPath = derived;
            return derived;
        }
        return '';
    }

    async function start(opts: { skipBuild: boolean }): Promise<void> {
        if (running) return;
        if (!adapterReady) {
            error = 'Pick an adapter in the Adapters view first.';
            return;
        }
        // For "Flash (skip build)" the artifact must exist already.
        // For "Build & Flash" we can derive an expected path from
        // the build cwd, since the build will materialise it.
        const artifactPath = opts.skipBuild
            ? settings.flash.artifactPath.trim()
            : ensureArtifactPath();
        if (artifactPath.length === 0) {
            error = opts.skipBuild
                ? 'Set a firmware artifact path first.'
                : 'Set a firmware artifact path (or a build working directory so we can derive one).';
            return;
        }

        running = true;
        resetLog();
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
            const buildCmd = opts.skipBuild
                ? null
                : settings.flash.buildCommand.trim().length > 0
                    ? settings.flash.buildCommand
                    : null;
            const buildCwd =
                settings.flash.buildCwd.trim().length > 0
                    ? settings.flash.buildCwd
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
            };
            const report = await runFlash(payload);
            result = report;
            progressMessage = `done in ${report.duration_ms} ms`;
            lastOutcome = 'success';

            // Provision-after-flash hook. Skipped when the operator
            // unchecked the box, when there's no detectable role
            // from the artifact name, or when the adapter isn't
            // backed by an interface we can drive. Failures here
            // don't roll back the flash — surface the error and
            // let the operator re-run `provision` standalone if
            // they need to retry.
            const role = detectedRole;
            if (provisionAfterFlash && role !== null) {
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

    // Build-only — no adapter required, no firmware load, no flash.
    // Designed for the bootstrap case where `build/` doesn't exist
    // yet and the operator needs to run the configure+compile step
    // once before there's any .elf to point Flash at.
    async function buildOnly(): Promise<void> {
        if (running) return;
        const cmd = settings.flash.buildCommand.trim();
        if (cmd.length === 0) {
            error = 'Set a build command first.';
            return;
        }

        running = true;
        resetLog();
        progressMessage = 'starting build…';
        result = null;
        error = null;
        lastOutcome = null;

        unlisten = await onFlashEvent((event) => {
            log = [...log, formatLogLine(event)];
            void maybeFollowTail();
        });

        try {
            const buildCwd =
                settings.flash.buildCwd.trim().length > 0
                    ? settings.flash.buildCwd
                    : null;
            await runBuildOnly(cmd, buildCwd);
            progressMessage = 'build done';
            lastOutcome = 'success';
            // Best-effort: if the operator hasn't pointed Flash at
            // an artifact yet, derive one from the cwd so the next
            // click on Build & Flash / Flash skip-build finds it.
            if (settings.flash.artifactPath.trim().length === 0) {
                const derived = guessArtifactFromCwd(settings.flash.buildCwd);
                if (derived.length > 0) settings.flash.artifactPath = derived;
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

    // Best-effort: when the user has already typed a path into
    // the field, seed the picker there so they don't have to
    // re-navigate the tree from $HOME every time.
    function defaultPathForArtifact(): string | undefined {
        const a = settings.flash.artifactPath.trim();
        if (a.length > 0) return a;
        const cwd = settings.flash.buildCwd.trim();
        return cwd.length > 0 ? cwd : undefined;
    }

    function defaultPathForBuildCwd(): string | undefined {
        const cwd = settings.flash.buildCwd.trim();
        if (cwd.length > 0) return cwd;
        const a = settings.flash.artifactPath.trim();
        return a.length > 0 ? a : undefined;
    }

    async function browseForArtifact(): Promise<void> {
        const picked = await openDialog({
            title: 'Pick a firmware artifact',
            multiple: false,
            directory: false,
            defaultPath: defaultPathForArtifact(),
            filters: [
                { name: 'Firmware', extensions: ['elf', 'hex', 'bin'] },
                { name: 'All files', extensions: ['*'] },
            ],
        });
        if (typeof picked === 'string' && picked.length > 0) {
            settings.flash.artifactPath = picked;
        }
    }

    async function browseForBuildCwd(): Promise<void> {
        // On macOS the system folder picker shows files greyed
        // out — that's the platform-native UX, not a bug. We
        // make sure `directory: true` is set so only folders
        // are *selectable*, and `canCreateDirectories` lets the
        // operator make a fresh `build/` from inside the dialog.
        const picked = await openDialog({
            title: 'Pick a build working directory',
            multiple: false,
            directory: true,
            canCreateDirectories: true,
            defaultPath: defaultPathForBuildCwd(),
        });
        if (typeof picked === 'string' && picked.length > 0) {
            settings.flash.buildCwd = picked;
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
        <div class="banner banner-warning">
            <strong>No adapter selected.</strong> Pick one in the
            <em>Adapters</em> view first — the flash command needs an
            <code>--interface</code>/<code>--channel</code> pair.
        </div>
    {/if}

    <div class="card form">
        <div class="field">
            <label for="artifact">Firmware artifact (file)</label>
            <div class="input-with-button">
                <input
                    id="artifact"
                    class="input mono"
                    type="text"
                    placeholder="/abs/path/to/firmware.elf"
                    bind:value={settings.flash.artifactPath}
                />
                <button type="button" class="btn btn-sm" onclick={browseForArtifact}>
                    Browse…
                </button>
            </div>
            <p class="hint">
                Path to the <code>.elf</code> / <code>.hex</code> / <code>.bin</code>
                the build produces. You can type the <em>expected</em> output
                path before the build has ever run — Browse… only works on
                existing files. For first-time setup, leave this blank, set
                a build cwd + command, then click <strong>Build only</strong>;
                we'll auto-fill an STM32-style guess after the build lands.
            </p>
        </div>

        <div class="field">
            <label for="buildcmd">Build command</label>
            <div class="input-with-button templates-host">
                <input
                    id="buildcmd"
                    class="input mono"
                    type="text"
                    placeholder="cmake --build build"
                    bind:value={settings.flash.buildCommand}
                />
                <button
                    type="button"
                    class="btn btn-sm templates-trigger"
                    onclick={() => (templatesOpen = !templatesOpen)}
                    aria-expanded={templatesOpen}
                    title="Pick a build command template — auto-detects CMake presets in your build cwd"
                >
                    Templates ▾
                </button>
                {#if templatesOpen}
                    <div class="templates-dropdown" role="menu">
                        {#if cmakePresets.length > 0}
                            <div class="templates-section">
                                <div class="templates-section-title">
                                    Detected CMake presets
                                </div>
                                {#each cmakePresets as preset (preset.name)}
                                    <button
                                        type="button"
                                        class="templates-item preset"
                                        onclick={() => pickPreset(preset)}
                                        role="menuitem"
                                    >
                                        <span class="templates-item-label">
                                            {preset.name}
                                        </span>
                                        <span class="templates-item-cmd">
                                            {preset.command}
                                        </span>
                                    </button>
                                {/each}
                            </div>
                        {/if}
                        <div class="templates-section">
                            <div class="templates-section-title">
                                Common templates
                            </div>
                            {#each STATIC_TEMPLATES as template (template.label)}
                                <button
                                    type="button"
                                    class="templates-item"
                                    onclick={() => pickTemplate(template)}
                                    role="menuitem"
                                    title={template.note}
                                >
                                    <span class="templates-item-label">
                                        {template.label}
                                    </span>
                                    <span class="templates-item-cmd">
                                        {template.command}
                                    </span>
                                </button>
                            {/each}
                        </div>
                    </div>
                {/if}
            </div>
            <p class="hint">
                Shell command run from the build working directory.
                Click <strong>Templates ▾</strong> to pick a preset
                (auto-detected from your <code>CMakePresets.json</code>) or
                a common pattern for non-preset projects.
            </p>
        </div>

        <div class="field">
            <label for="buildcwd">Build working directory (folder)</label>
            <div class="input-with-button">
                <input
                    id="buildcwd"
                    class="input mono"
                    type="text"
                    placeholder="(defaults to artifact's parent)"
                    bind:value={settings.flash.buildCwd}
                />
                <button type="button" class="btn btn-sm" onclick={browseForBuildCwd}>
                    Browse…
                </button>
            </div>
            <p class="hint">
                Folder the build command runs in — usually your CMake
                project root. The macOS folder picker greys out files
                (only folders are selectable) and offers a New Folder
                button if you need to create one.
            </p>
        </div>

        <div class="row-three">
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
                <label for="nodeId">Node ID (0–0xF)</label>
                <input
                    id="nodeId"
                    class="input mono"
                    type="number"
                    min="0"
                    max="15"
                    bind:value={settings.adapter.nodeId}
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

        <div class="opts">
            <label class="toggle"><input type="checkbox" bind:checked={settings.flash.diff} /> Diff-skip unchanged sectors</label>
            <label class="toggle"><input type="checkbox" bind:checked={settings.flash.verifyAfter} /> Verify each sector</label>
            <label class="toggle"><input type="checkbox" bind:checked={settings.flash.finalCommit} /> Final CMD_FLASH_VERIFY commit</label>
            <label class="toggle"><input type="checkbox" bind:checked={settings.flash.jump} /> Jump to app after flash</label>
            <label class="toggle"><input type="checkbox" bind:checked={settings.flash.dryRun} /> Dry-run (no erases / writes)</label>
            <!--
                Provision-after-flash auto-detects the role from
                the artifact's basename. We always render the
                checkbox so its presence isn't a surprise, but
                disable + dim it when no role is recoverable from
                the filename. Tooltip explains the requirement.
            -->
            <label
                class="toggle"
                class:provision-disabled={detectedRole === null}
                title={detectedRole === null
                    ? 'Artifact filename must match a role (ams.elf / ecu.hex / udv.bin) to enable.'
                    : `After flash: write node-id 0x${detectedRole.nodeId
                          .toString(16)
                          .padStart(2, '0')} to NVM and reset.`}
            >
                <input
                    type="checkbox"
                    bind:checked={provisionAfterFlash}
                    disabled={detectedRole === null}
                />
                Provision as
                <strong>
                    {detectedRole === null ? '—' : detectedRole.name.toUpperCase()}
                </strong>
                after flash
            </label>
        </div>
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
        <button
            type="button"
            class="btn"
            disabled={running}
            onclick={buildOnly}
            title="Run the build step only — useful before the first flash, when `build/` doesn't exist yet so the .elf can't be picked."
        >
            Build only
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
       the build-templates dropdown, the running-progress badge with
       its status icon, the indeterminate progress bar, and the log
       window with its copy-row header. */

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

    /* Input + adjacent button row. The input flexes to fill, the
       button hugs its content. */
    .input-with-button {
        display: flex;
        gap: var(--space-2);
    }
    .input-with-button .input {
        flex: 1;
    }

    /* Build-templates dropdown — anchored to the templates trigger
       button. Owns its own surface so it sits cleanly above the
       form regardless of card background. */
    .templates-host {
        position: relative;
    }
    .templates-dropdown {
        position: absolute;
        top: calc(100% + var(--space-1));
        right: 0;
        z-index: 10;
        min-width: 360px;
        max-width: min(560px, calc(100vw - 60px));
        background: var(--surface-elev);
        border: 1px solid var(--border);
        border-radius: var(--radius-md);
        box-shadow: var(--shadow-md);
        padding: var(--space-2) 0;
        max-height: 360px;
        overflow: auto;
    }
    .templates-section + .templates-section {
        border-top: 1px solid var(--border);
        margin-top: var(--space-1);
        padding-top: var(--space-1);
    }
    .templates-section-title {
        padding: var(--space-2) var(--space-3) var(--space-1);
        font-size: var(--text-xs);
        text-transform: uppercase;
        letter-spacing: 0.05em;
        color: var(--text-muted);
    }
    .templates-item {
        display: flex;
        flex-direction: column;
        gap: 2px;
        width: 100%;
        text-align: left;
        background: transparent;
        border: none;
        padding: var(--space-2) var(--space-3);
        cursor: pointer;
        color: var(--text);
        border-radius: 0;
        font: inherit;
    }
    .templates-item:hover {
        background: var(--hover);
        color: var(--text);
    }
    .templates-item.preset .templates-item-label::before {
        content: '◆ ';
        color: var(--accent);
    }
    .templates-item-label {
        font-size: var(--text-sm);
        font-weight: 500;
    }
    .templates-item-cmd {
        font-family: var(--font-mono);
        font-size: var(--text-xs);
        color: var(--text-muted);
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
    }

    /* Three-up grid for bitrate / node-id / timeout. Drops to a
       single column at narrow widths so the sidebar-collapsed
       layout doesn't squish the inputs. */
    .row-three {
        display: grid;
        grid-template-columns: repeat(3, 1fr);
        gap: var(--space-3);
    }
    @media (max-width: 720px) {
        .row-three {
            grid-template-columns: 1fr;
        }
    }

    /* Per-run options — wrapping row of checkboxes. The .toggle
       utility handles spacing + accent-color; we just lay them out
       horizontally with a wider gap than the in-card stack so they
       breathe. */
    .opts {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-4);
        margin-top: var(--space-1);
    }
    .opts .toggle.provision-disabled {
        opacity: 0.45;
        cursor: help;
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
