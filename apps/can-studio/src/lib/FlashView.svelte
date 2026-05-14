<!--
    Flash view — build + flash pipeline.

    All configuration lives in the central `settings` store and
    persists across restarts. Adapter selection comes from
    `settings.adapter` (set in the Adapters view); flash defaults
    + per-run options live in `settings.flash`.
-->
<script lang="ts">
    import { onDestroy } from 'svelte';
    import type { UnlistenFn } from '@tauri-apps/api/event';

    import {
        onFlashEvent,
        runFlash,
        type FlashEvent,
        type FlashRequest,
        type JsonReport,
    } from './flash';
    import { settings } from './settings.svelte';

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

    let unlisten: UnlistenFn | null = null;

    async function start(opts: { skipBuild: boolean }): Promise<void> {
        if (running) return;
        if (!adapterReady) {
            error = 'Pick an adapter in the Adapters view first.';
            return;
        }
        if (settings.flash.artifactPath.trim().length === 0) {
            error = 'Set a firmware artifact path first.';
            return;
        }

        running = true;
        log = [];
        progressMessage = 'starting…';
        result = null;
        error = null;

        unlisten = await onFlashEvent((event) => {
            log = [...log, formatLogLine(event)];
            const msg = formatProgress(event);
            if (msg !== null) progressMessage = msg;
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
                artifactPath: settings.flash.artifactPath,
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
        } catch (err) {
            error = err instanceof Error ? err.message : String(err);
            progressMessage = 'failed';
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
        <div class="warning">
            <strong>No adapter selected.</strong> Pick one in the
            <em>Adapters</em> view first — the flash command needs an
            <code>--interface</code>/<code>--channel</code> pair.
        </div>
    {/if}

    <div class="form">
        <div class="row">
            <label for="artifact">Firmware artifact</label>
            <input
                id="artifact"
                type="text"
                placeholder="/abs/path/to/firmware.elf"
                bind:value={settings.flash.artifactPath}
            />
        </div>

        <div class="row">
            <label for="buildcmd">Build command</label>
            <input
                id="buildcmd"
                type="text"
                placeholder="cmake --build build"
                bind:value={settings.flash.buildCommand}
            />
        </div>

        <div class="row">
            <label for="buildcwd">Build working directory</label>
            <input
                id="buildcwd"
                type="text"
                placeholder="(defaults to artifact's parent)"
                bind:value={settings.flash.buildCwd}
            />
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
    </div>

    {#if running || progressMessage.length > 0}
        <div class="progress" class:running>
            <strong>{running ? '●' : '○'}</strong>
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
        <pre class="log">{log.join('\n')}</pre>
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
        gap: 8px;
        padding: 10px 14px;
        border: 1px solid var(--border);
        background: var(--surface);
        border-radius: 6px;
        font-family: var(--font-mono);
        font-size: 0.85rem;
    }
    .progress.running strong { color: var(--accent); }
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
    .log {
        margin: 0;
        max-height: 320px;
        overflow: auto;
        padding: 12px;
        background: var(--bg);
        border: 1px solid var(--border);
        border-radius: 6px;
        font-family: var(--font-mono);
        font-size: 0.8rem;
        line-height: 1.5;
        white-space: pre-wrap;
        word-break: break-all;
    }
</style>
