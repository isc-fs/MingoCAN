<!--
    Bus monitor — Tier 1.

    Opens the selected adapter in promiscuous mode and streams every
    frame on the bus to the UI. Two view modes:

      - "Live frames" — flat scrolling log, candump-style, one row
        per frame. Bounded buffer (settings.busMonitor.maxRows) so
        the DOM stays sane at 1kHz+.
      - "By ID"       — aggregated rows, one per unique ID. Each
        row tracks count + rolling Hz + latest data + last-seen
        timestamp. The "what's actually on this bus?" view.

    Both modes honour an ID filter (comma-separated hex prefixes,
    "0x" optional). Pause stops appending to the visible buffers
    but keeps the backend stream running — resume just starts
    showing new frames again, no reconnection.

    Adapter selection comes from the central `settings.adapter`
    store (same as Flash / Diagnostics / Live data).
-->
<script lang="ts">
    import { onDestroy } from 'svelte';
    import type { UnlistenFn } from '@tauri-apps/api/event';

    import {
        formatData,
        formatId,
        formatTs,
        onBusMonitorFrame,
        onBusMonitorStatus,
        startBusMonitor,
        stopBusMonitor,
        type BusMonitorFrame,
        type BusMonitorRequest,
    } from './bus_monitor';
    import { settings } from './settings.svelte';

    // ---- Adapter gate ----

    const adapterReady = $derived(
        settings.adapter.interface !== null &&
            (settings.adapter.interface === 'virtual' ||
                settings.adapter.channel.length > 0),
    );

    // ---- Runtime state ----

    type Status = 'idle' | 'starting' | 'running' | 'paused' | 'stopping' | 'error';
    let status = $state<Status>('idle');
    let error = $state<string | null>(null);

    // Live frame ring buffer. We append in batches (see flushQueued)
    // because Svelte 5 re-renders on every assignment — at 1kHz+
    // re-rendering for every frame would tank the UI. The queue is
    // a non-reactive scratch buffer drained on a rAF tick.
    let liveFrames = $state<BusMonitorFrame[]>([]);
    let queued: BusMonitorFrame[] = [];
    let flushScheduled = false;

    // By-ID aggregation. Keep this as a plain Map (non-reactive) and
    // promote to a sorted array via $state at flush time — same
    // batching motivation as live frames.
    interface IdStat {
        id: number;
        count: number;
        rateHz: number;
        lastTsMs: number;
        lastDlc: number;
        lastData: number[];
        // For rolling rate: a ring of recent ts to compute a
        // 1-second Hz. Bounded at 32 entries per ID — plenty for
        // signals up to ~32Hz; faster signals get an approximated
        // rate based on the last 32 samples regardless of window.
        recentTs: number[];
    }
    const idStats = new Map<number, IdStat>();
    let idStatsView = $state<IdStat[]>([]);

    let totalFrames = $state<number>(0);
    let droppedFrames = $state<number>(0);

    let unlistenFrame: UnlistenFn | null = null;
    let unlistenStatus: UnlistenFn | null = null;

    // ---- ID filter ----

    // Parse the comma-separated filter into a Set of exact IDs.
    // Empty filter → null (match everything). Each token is a hex
    // literal with optional `0x` prefix. Operators who need
    // ranges/wildcards can pass multiple comma-separated values.
    const filterIds = $derived.by<Set<number> | null>(() => {
        const raw = settings.busMonitor.idFilter.trim();
        if (raw.length === 0) return null;
        const out = new Set<number>();
        for (const part of raw.split(',')) {
            const token = part.trim().replace(/^0x/i, '');
            if (token.length === 0) continue;
            const value = parseInt(token, 16);
            if (Number.isNaN(value)) continue;
            out.add(value & 0x7ff);
        }
        return out.size === 0 ? null : out;
    });

    function passesFilter(id: number): boolean {
        if (filterIds === null) return true;
        return filterIds.has(id);
    }

    // ---- Frame ingestion (batched) ----

    function ingest(frame: BusMonitorFrame): void {
        totalFrames++;
        // Always update by-ID aggregation, even while paused — the
        // operator usually wants the running picture to reflect
        // reality. Pause only affects the visible live-frame log.
        const stat = idStats.get(frame.id);
        if (stat === undefined) {
            idStats.set(frame.id, {
                id: frame.id,
                count: 1,
                rateHz: 0,
                lastTsMs: frame.tsMs,
                lastDlc: frame.dlc,
                lastData: frame.data,
                recentTs: [frame.tsMs],
            });
        } else {
            stat.count++;
            stat.lastTsMs = frame.tsMs;
            stat.lastDlc = frame.dlc;
            stat.lastData = frame.data;
            stat.recentTs.push(frame.tsMs);
            if (stat.recentTs.length > 32) stat.recentTs.shift();
            // Rolling Hz from the recent timestamp window.
            if (stat.recentTs.length >= 2) {
                const dt = stat.recentTs[stat.recentTs.length - 1] - stat.recentTs[0];
                stat.rateHz = dt > 0 ? ((stat.recentTs.length - 1) * 1000) / dt : 0;
            }
        }

        if (status === 'paused') return;
        if (!passesFilter(frame.id)) return;
        queued.push(frame);
        scheduleFlush();
    }

    function scheduleFlush(): void {
        if (flushScheduled) return;
        flushScheduled = true;
        requestAnimationFrame(() => {
            flushScheduled = false;
            flushQueued();
        });
    }

    function flushQueued(): void {
        if (queued.length === 0 && idStats.size === idStatsView.length) return;
        // Live frames — append then trim.
        if (queued.length > 0) {
            const batch = queued;
            queued = [];
            const max = settings.busMonitor.maxRows;
            const next = liveFrames.concat(batch);
            if (next.length > max) {
                droppedFrames += next.length - max;
                liveFrames = next.slice(next.length - max);
            } else {
                liveFrames = next;
            }
        }
        // By-ID view — promote sorted Map snapshot.
        idStatsView = Array.from(idStats.values()).sort((a, b) => a.id - b.id);
    }

    // ---- Control flow ----

    async function start(): Promise<void> {
        if (!adapterReady || settings.adapter.interface === null) {
            error = 'Pick an adapter in the Adapters view first.';
            return;
        }
        if (status === 'running' || status === 'starting') return;

        error = null;
        status = 'starting';
        clearBuffers();

        try {
            unlistenFrame = await onBusMonitorFrame(ingest);
            unlistenStatus = await onBusMonitorStatus((s) => {
                if (s.kind === 'started') {
                    status = 'running';
                } else if (s.kind === 'stopped') {
                    status = 'idle';
                } else if (s.kind === 'error') {
                    error = s.message;
                    status = 'error';
                }
            });

            const payload: BusMonitorRequest = {
                interface: settings.adapter.interface,
                channel:
                    settings.adapter.channel.length > 0
                        ? settings.adapter.channel
                        : null,
                bitrate: settings.adapter.bitrate,
                pollTimeoutMs: 50,
            };
            await startBusMonitor(payload);
        } catch (err) {
            error = err instanceof Error ? err.message : String(err);
            status = 'error';
            await cleanupListeners();
        }
    }

    async function stop(): Promise<void> {
        if (status === 'idle' || status === 'stopping') return;
        status = 'stopping';
        try {
            await stopBusMonitor();
        } catch (err) {
            error = err instanceof Error ? err.message : String(err);
        }
        await cleanupListeners();
        status = 'idle';
    }

    async function cleanupListeners(): Promise<void> {
        if (unlistenFrame !== null) {
            unlistenFrame();
            unlistenFrame = null;
        }
        if (unlistenStatus !== null) {
            unlistenStatus();
            unlistenStatus = null;
        }
    }

    function togglePause(): void {
        if (status === 'running') status = 'paused';
        else if (status === 'paused') status = 'running';
    }

    function clearBuffers(): void {
        liveFrames = [];
        idStats.clear();
        idStatsView = [];
        queued = [];
        totalFrames = 0;
        droppedFrames = 0;
    }

    onDestroy(async () => {
        if (status !== 'idle') await stop();
    });
</script>

<div class="view">
    <header>
        <h2>Bus monitor</h2>
        <p class="muted">
            Live capture of every frame on the selected CAN bus.
            Two views: <em>By&nbsp;ID</em> for the at-a-glance picture
            (one row per unique ID, rolling Hz, latest payload) and
            <em>Live frames</em> for the candump-style scrolling log.
        </p>
    </header>

    {#if !adapterReady}
        <div class="warning">
            <strong>No adapter selected.</strong> Pick one in the
            <em>Adapters</em> view first — the monitor needs an
            <code>--interface</code>/<code>--channel</code> pair.
        </div>
    {/if}

    <div class="controls">
        <div class="actions">
            {#if status === 'idle' || status === 'error'}
                <button
                    type="button"
                    class="primary"
                    disabled={!adapterReady}
                    onclick={start}
                >
                    Start
                </button>
            {:else}
                <button
                    type="button"
                    disabled={status === 'starting' || status === 'stopping'}
                    onclick={stop}
                >
                    Stop
                </button>
                <button
                    type="button"
                    disabled={status === 'starting' || status === 'stopping'}
                    onclick={togglePause}
                >
                    {status === 'paused' ? 'Resume' : 'Pause'}
                </button>
            {/if}
            <button
                type="button"
                disabled={liveFrames.length === 0 && idStatsView.length === 0}
                onclick={clearBuffers}
            >
                Clear
            </button>
        </div>

        <div class="filter">
            <label for="idfilter">Filter by ID</label>
            <input
                id="idfilter"
                type="text"
                placeholder="e.g. 0x1A, 0x200"
                bind:value={settings.busMonitor.idFilter}
            />
        </div>

        <div class="stats">
            <span class="stat">
                <strong class="status-dot" class:running={status === 'running'} class:paused={status === 'paused'} class:error={status === 'error'}>●</strong>
                <span>{status}</span>
            </span>
            <span class="stat">frames&nbsp;<strong>{totalFrames}</strong></span>
            <span class="stat">unique&nbsp;IDs&nbsp;<strong>{idStats.size}</strong></span>
            {#if droppedFrames > 0}
                <span class="stat dropped" title="Oldest rows trimmed because the buffer exceeded maxRows">
                    dropped&nbsp;<strong>{droppedFrames}</strong>
                </span>
            {/if}
        </div>
    </div>

    {#if error !== null}
        <div class="error">{error}</div>
    {/if}

    <div class="tabs">
        <button
            type="button"
            class="tab"
            class:active={settings.busMonitor.activeTab === 'byId'}
            onclick={() => (settings.busMonitor.activeTab = 'byId')}
        >
            By ID ({idStatsView.length})
        </button>
        <button
            type="button"
            class="tab"
            class:active={settings.busMonitor.activeTab === 'live'}
            onclick={() => (settings.busMonitor.activeTab = 'live')}
        >
            Live frames ({liveFrames.length})
        </button>
    </div>

    {#if settings.busMonitor.activeTab === 'byId'}
        <div class="table-wrap">
            <table class="frame-table by-id">
                <thead>
                    <tr>
                        <th class="col-id">ID</th>
                        <th class="col-count">Count</th>
                        <th class="col-rate">Hz</th>
                        <th class="col-dlc">DLC</th>
                        <th class="col-data">Last data</th>
                        <th class="col-ts">Last seen</th>
                    </tr>
                </thead>
                <tbody>
                    {#each idStatsView as stat (stat.id)}
                        {#if passesFilter(stat.id)}
                            <tr>
                                <td class="col-id mono">{formatId(stat.id)}</td>
                                <td class="col-count mono">{stat.count}</td>
                                <td class="col-rate mono">{stat.rateHz.toFixed(1)}</td>
                                <td class="col-dlc mono">{stat.lastDlc}</td>
                                <td class="col-data mono">{formatData(stat.lastData, stat.lastDlc)}</td>
                                <td class="col-ts mono">{formatTs(stat.lastTsMs)}s</td>
                            </tr>
                        {/if}
                    {/each}
                    {#if idStatsView.length === 0}
                        <tr><td colspan="6" class="empty">No frames yet. Click <strong>Start</strong>.</td></tr>
                    {/if}
                </tbody>
            </table>
        </div>
    {:else}
        <div class="table-wrap">
            <table class="frame-table live">
                <thead>
                    <tr>
                        <th class="col-ts">Time</th>
                        <th class="col-id">ID</th>
                        <th class="col-dlc">DLC</th>
                        <th class="col-data">Data</th>
                    </tr>
                </thead>
                <tbody>
                    {#each liveFrames as frame, i (i)}
                        <tr>
                            <td class="col-ts mono">{formatTs(frame.tsMs)}s</td>
                            <td class="col-id mono">{formatId(frame.id)}</td>
                            <td class="col-dlc mono">{frame.dlc}</td>
                            <td class="col-data mono">{formatData(frame.data, frame.dlc)}</td>
                        </tr>
                    {/each}
                    {#if liveFrames.length === 0}
                        <tr><td colspan="4" class="empty">No frames yet. Click <strong>Start</strong>.</td></tr>
                    {/if}
                </tbody>
            </table>
        </div>
    {/if}
</div>

<style>
    .view {
        display: flex;
        flex-direction: column;
        gap: 14px;
        padding: 24px 28px;
        overflow: hidden;
        height: 100%;
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
    .controls {
        display: flex;
        flex-wrap: wrap;
        gap: 14px;
        align-items: end;
        padding: 12px 14px;
        border: 1px solid var(--border);
        background: var(--surface);
        border-radius: 8px;
    }
    .actions { display: flex; gap: 8px; }
    .filter {
        display: flex;
        flex-direction: column;
        gap: 4px;
        flex: 1;
        min-width: 200px;
    }
    .filter label { font-size: 0.78rem; color: var(--text-muted); }
    .filter input {
        background: var(--bg);
        color: var(--text);
        border: 1px solid var(--border);
        border-radius: 4px;
        padding: 6px 8px;
        font-family: var(--font-mono);
        font-size: 0.85rem;
    }
    .filter input:focus { outline: none; border-color: var(--accent); }
    .stats {
        display: flex;
        gap: 14px;
        font-size: 0.8rem;
        color: var(--text-muted);
        font-family: var(--font-mono);
    }
    .stat { display: inline-flex; gap: 4px; align-items: center; }
    .stat strong { color: var(--text); font-weight: 600; }
    .stat.dropped strong { color: var(--error); }
    .status-dot { color: var(--text-muted); }
    .status-dot.running { color: #06d6a0; animation: pulse 1.2s ease-in-out infinite; }
    .status-dot.paused { color: var(--accent); }
    .status-dot.error { color: var(--error); }
    @keyframes pulse {
        0%, 100% { opacity: 1; }
        50% { opacity: 0.4; }
    }
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
    .error {
        padding: 10px 14px;
        border: 1px solid var(--error);
        color: var(--error);
        border-radius: 6px;
        background: rgba(255, 115, 115, 0.08);
        font-size: 0.85rem;
    }
    .tabs {
        display: flex;
        gap: 4px;
        border-bottom: 1px solid var(--border);
        margin-bottom: -1px;
    }
    .tab {
        background: transparent;
        border: 1px solid transparent;
        border-bottom: none;
        border-radius: 6px 6px 0 0;
        padding: 6px 14px;
        color: var(--text-muted);
        font-size: 0.85rem;
    }
    .tab:hover:not(:disabled) {
        color: var(--text);
        border-color: transparent;
    }
    .tab.active {
        background: var(--surface);
        border-color: var(--border);
        color: var(--text);
    }
    .table-wrap {
        flex: 1;
        min-height: 0;
        overflow: auto;
        border: 1px solid var(--border);
        border-radius: 0 6px 6px 6px;
        background: var(--bg);
    }
    .frame-table {
        width: 100%;
        border-collapse: collapse;
        font-size: 0.82rem;
    }
    .frame-table thead {
        position: sticky;
        top: 0;
        background: var(--surface);
        z-index: 1;
    }
    .frame-table th, .frame-table td {
        padding: 6px 10px;
        text-align: left;
        border-bottom: 1px solid rgba(255, 255, 255, 0.04);
    }
    .frame-table th {
        font-weight: 600;
        font-size: 0.72rem;
        text-transform: uppercase;
        letter-spacing: 0.05em;
        color: var(--text-muted);
    }
    .mono { font-family: var(--font-mono); }
    .col-id { width: 80px; }
    .col-count { width: 80px; text-align: right; }
    .col-rate { width: 70px; text-align: right; }
    .col-dlc { width: 50px; text-align: right; }
    .col-ts { width: 100px; }
    .col-data { white-space: nowrap; }
    .empty {
        padding: 20px;
        text-align: center;
        color: var(--text-muted);
    }
</style>
