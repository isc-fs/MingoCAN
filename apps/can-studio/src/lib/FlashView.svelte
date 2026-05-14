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
    // Track which side of "done" the last run landed on so the
    // status indicator can show a clear green/red badge instead
    // of relying on the operator reading the log to figure it
    // out. null means "no run yet" or "currently running".
    let lastOutcome = $state<'success' | 'failure' | null>(null);
    let copyState = $state<'idle' | 'copied'>('idle');

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

        unlisten = await onFlashEvent((event) => {
            log = [...log, formatLogLine(event)];
            const msg = formatProgress(event);
            if (msg !== null) progressMessage = msg;
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
        <div class="warning">
            <strong>No adapter selected.</strong> Pick one in the
            <em>Adapters</em> view first — the flash command needs an
            <code>--interface</code>/<code>--channel</code> pair.
        </div>
    {/if}

    <div class="form">
        <div class="row">
            <label for="artifact">Firmware artifact (file)</label>
            <div class="input-with-button">
                <input
                    id="artifact"
                    type="text"
                    placeholder="/abs/path/to/firmware.elf"
                    bind:value={settings.flash.artifactPath}
                />
                <button type="button" class="browse" onclick={browseForArtifact}>
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

        <div class="row">
            <label for="buildcmd">Build command</label>
            <div class="input-with-button templates-host">
                <input
                    id="buildcmd"
                    type="text"
                    placeholder="cmake --build build"
                    bind:value={settings.flash.buildCommand}
                />
                <button
                    type="button"
                    class="browse templates-trigger"
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

        <div class="row">
            <label for="buildcwd">Build working directory (folder)</label>
            <div class="input-with-button">
                <input
                    id="buildcwd"
                    type="text"
                    placeholder="(defaults to artifact's parent)"
                    bind:value={settings.flash.buildCwd}
                />
                <button type="button" class="browse" onclick={browseForBuildCwd}>
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
            <div>
                <label for="bitrate">Bitrate (bps)</label>
                <input
                    id="bitrate"
                    type="number"
                    min="10000"
                    max="1000000"
                    step="1000"
                    bind:value={settings.adapter.bitrate}
                />
            </div>
            <div>
                <label for="nodeId">Node ID (0–0xF)</label>
                <input
                    id="nodeId"
                    type="number"
                    min="0"
                    max="15"
                    bind:value={settings.adapter.nodeId}
                />
            </div>
            <div>
                <label for="timeout">Frame timeout (ms)</label>
                <input
                    id="timeout"
                    type="number"
                    min="50"
                    max="60000"
                    bind:value={settings.adapter.timeoutMs}
                />
            </div>
        </div>

        <div class="opts">
            <label><input type="checkbox" bind:checked={settings.flash.diff} /> Diff-skip unchanged sectors</label>
            <label><input type="checkbox" bind:checked={settings.flash.verifyAfter} /> Verify each sector</label>
            <label><input type="checkbox" bind:checked={settings.flash.finalCommit} /> Final CMD_FLASH_VERIFY commit</label>
            <label><input type="checkbox" bind:checked={settings.flash.jump} /> Jump to app after flash</label>
            <label><input type="checkbox" bind:checked={settings.flash.dryRun} /> Dry-run (no erases / writes)</label>
        </div>
    </div>

    <div class="actions">
        <button
            type="button"
            class="primary"
            disabled={running || !adapterReady}
            onclick={() => start({ skipBuild: false })}
        >
            Build & Flash
        </button>
        <button
            type="button"
            disabled={running || !adapterReady}
            onclick={() => start({ skipBuild: true })}
        >
            Flash (skip build)
        </button>
        <button
            type="button"
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
        </div>
    {/if}

    {#if error !== null}
        <div class="error">{error}</div>
    {/if}

    {#if result !== null}
        <div class="result">
            <strong>Done.</strong>
            {result.sectors_written.length} sector(s) written,
            {result.sectors_skipped.length} skipped,
            {result.duration_ms} ms.
            CRC {result.crc32}, {result.size} bytes.
        </div>
    {/if}

    {#if log.length > 0}
        <div class="log-wrap">
            <div class="log-header">
                <span class="log-count">{log.length} line{log.length === 1 ? '' : 's'}</span>
                <button
                    type="button"
                    class="copy-btn"
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
    .view {
        display: flex;
        flex-direction: column;
        gap: 14px;
        padding: 24px 28px;
        overflow: auto;
    }
    header h2 { margin: 0; font-size: 1.3rem; }
    .muted { color: var(--text-muted); font-size: 0.9rem; margin: 4px 0 0; }
    .warning {
        padding: 10px 14px;
        border: 1px solid var(--accent);
        background: rgba(242, 178, 51, 0.08);
        border-radius: 6px;
    }
    .warning code { font-family: var(--font-mono); }
    .form {
        display: flex;
        flex-direction: column;
        gap: 10px;
        padding: 16px;
        border: 1px solid var(--border);
        border-radius: 8px;
        background: var(--surface);
    }
    .row { display: flex; flex-direction: column; gap: 4px; }
    .hint {
        margin: 4px 0 0;
        font-size: 0.78rem;
        color: var(--text-muted);
        line-height: 1.5;
    }
    .hint code {
        font-family: var(--font-mono);
        font-size: 0.95em;
        padding: 0 3px;
        border-radius: 3px;
        background: rgba(255, 255, 255, 0.04);
    }
    .hint strong { color: var(--text); font-weight: 600; }
    .input-with-button {
        display: flex;
        gap: 6px;
    }
    .templates-host {
        position: relative;
    }
    .templates-dropdown {
        position: absolute;
        top: calc(100% + 4px);
        right: 0;
        z-index: 10;
        min-width: 360px;
        max-width: min(560px, calc(100vw - 60px));
        background: var(--surface);
        border: 1px solid var(--border);
        border-radius: 6px;
        box-shadow: 0 6px 24px rgba(0, 0, 0, 0.35);
        padding: 6px 0;
        max-height: 360px;
        overflow: auto;
    }
    .templates-section + .templates-section {
        border-top: 1px solid var(--border);
        margin-top: 4px;
        padding-top: 4px;
    }
    .templates-section-title {
        padding: 6px 12px 4px;
        font-size: 0.7rem;
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
        padding: 8px 12px;
        cursor: pointer;
        color: var(--text);
        border-radius: 0;
    }
    .templates-item:hover {
        background: rgba(255, 255, 255, 0.04);
        border-color: transparent;
        color: var(--text);
    }
    .templates-item.preset .templates-item-label::before {
        content: '◆ ';
        color: var(--accent);
    }
    .templates-item-label {
        font-size: 0.85rem;
        font-weight: 500;
    }
    .templates-item-cmd {
        font-family: var(--font-mono);
        font-size: 0.72rem;
        color: var(--text-muted);
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
    }
    .input-with-button input { flex: 1; }
    .browse {
        white-space: nowrap;
        padding: 6px 12px;
        background: var(--bg);
        border: 1px solid var(--border);
        color: var(--text-muted);
        border-radius: 4px;
        font-size: 0.85rem;
        cursor: pointer;
    }
    .browse:hover { border-color: var(--accent); color: var(--accent); }
    .row-three {
        display: grid;
        grid-template-columns: repeat(3, 1fr);
        gap: 10px;
    }
    label { font-size: 0.85rem; color: var(--text-muted); }
    input[type="text"], input[type="number"] {
        background: var(--bg);
        color: var(--text);
        border: 1px solid var(--border);
        border-radius: 4px;
        padding: 6px 8px;
        font: inherit;
        font-family: var(--font-mono);
        font-size: 0.85rem;
    }
    input:focus { outline: none; border-color: var(--accent); }
    .opts {
        display: flex;
        flex-wrap: wrap;
        gap: 14px;
        margin-top: 4px;
    }
    .opts label {
        color: var(--text);
        font-size: 0.85rem;
        display: flex;
        gap: 6px;
        align-items: center;
    }
    .actions { display: flex; gap: 8px; }
    button {
        appearance: none;
        background: var(--surface);
        color: var(--text);
        border: 1px solid var(--border);
        font: inherit;
        padding: 8px 16px;
        border-radius: 6px;
        cursor: pointer;
    }
    button:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
    button.primary {
        background: var(--accent);
        color: #1a1a1a;
        border-color: var(--accent);
    }
    button.primary:hover:not(:disabled) { filter: brightness(1.05); color: #1a1a1a; }
    button:disabled { opacity: 0.5; cursor: not-allowed; }
    .progress {
        display: flex;
        align-items: center;
        gap: 10px;
        padding: 10px 14px;
        border: 1px solid var(--border);
        background: var(--surface);
        border-radius: 6px;
        font-family: var(--font-mono);
        font-size: 0.85rem;
        transition: border-color 0.2s ease, background 0.2s ease;
    }
    .status-icon {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 18px;
        height: 18px;
        line-height: 1;
        font-size: 0.95rem;
    }
    .progress.running .status-icon {
        color: var(--accent);
        animation: pulse 1.2s ease-in-out infinite;
    }
    .progress.success {
        border-color: #06d6a0;
        background: rgba(6, 214, 160, 0.08);
    }
    .progress.success .status-icon { color: #06d6a0; font-weight: 700; }
    .progress.failure {
        border-color: var(--error);
        background: rgba(255, 115, 115, 0.08);
    }
    .progress.failure .status-icon { color: var(--error); font-weight: 700; }
    @keyframes pulse {
        0%, 100% { opacity: 1; }
        50% { opacity: 0.4; }
    }
    .error {
        padding: 10px 14px;
        border: 1px solid var(--error);
        color: var(--error);
        border-radius: 6px;
        background: rgba(255, 115, 115, 0.08);
    }
    .result {
        padding: 10px 14px;
        border: 1px solid #06d6a0;
        color: #06d6a0;
        border-radius: 6px;
        background: rgba(6, 214, 160, 0.08);
        font-size: 0.9rem;
    }
    .log-wrap {
        display: flex;
        flex-direction: column;
        gap: 0;
        border: 1px solid var(--border);
        border-radius: 6px;
        overflow: hidden;
    }
    .log-header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        padding: 6px 10px 6px 12px;
        background: var(--surface);
        border-bottom: 1px solid var(--border);
        font-size: 0.75rem;
        color: var(--text-muted);
    }
    .log-count {
        font-family: var(--font-mono);
    }
    .copy-btn {
        background: transparent;
        border: 1px solid var(--border);
        color: var(--text-muted);
        padding: 3px 10px;
        border-radius: 4px;
        font-size: 0.75rem;
        cursor: pointer;
        font-family: inherit;
        transition: all 0.15s ease;
    }
    .copy-btn:hover:not(:disabled) {
        border-color: var(--accent);
        color: var(--accent);
    }
    .log {
        margin: 0;
        max-height: 320px;
        overflow: auto;
        padding: 12px;
        background: var(--bg);
        font-family: var(--font-mono);
        font-size: 0.8rem;
        line-height: 1.5;
    }
    .log-line {
        white-space: pre-wrap;
        word-break: break-all;
    }
</style>
