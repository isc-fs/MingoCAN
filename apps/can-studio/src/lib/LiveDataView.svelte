<!--
    Live-data view — streaming snapshot panel with a sliding-window
    frames/sec chart, state pills, and a counter grid. Mirrors the
    VS Code extension's live-data webview, executed against the
    same NOTIFY_LIVE_DATA stream.

    Two buttons:
      Start  spawns the live_data_start command. ACK lands quickly;
             snapshots stream via the `live_data:event` Tauri event.
      Stop   sends live_data_stop. The Rust task fires
             CMD_LIVE_DATA_STOP and disconnects.

    Frames RX/TX are absolute counters in each snapshot; we compute
    per-tick deltas in the chart updater so the y-axis is frames/sec.
-->
<script lang="ts">
    import { onDestroy, onMount, tick } from 'svelte';
    import type { UnlistenFn } from '@tauri-apps/api/event';
    import {
        Chart,
        type ChartConfiguration,
        type ChartDataset,
        registerables,
    } from 'chart.js';

    import {
        onLiveDataEvent,
        startLiveData,
        stopLiveData,
        type LiveDataEvent,
        type LiveDataRequest,
    } from './live_data';
    import { settings } from './settings.svelte';

    Chart.register(...registerables);

    const adapterReady = $derived(
        settings.adapter.interface !== null &&
            (settings.adapter.interface === 'virtual' ||
                settings.adapter.channel.length > 0),
    );

    let status = $state<'idle' | 'running' | 'stopped' | 'error'>('idle');
    let statusMessage = $state<string>('idle');
    let error = $state<string | null>(null);
    let latest = $state<LiveDataEvent | null>(null);

    let canvas: HTMLCanvasElement | undefined = $state();
    let chart: Chart | null = null;
    let unlisten: UnlistenFn | null = null;
    // Most recent snapshot — for computing deltas to feed the chart.
    let previous: { uptimeMs: number; framesRx: number; framesTx: number } | null = null;

    onMount(async () => {
        if (canvas === undefined) return;
        chart = new Chart(canvas, buildChartConfig());
        unlisten = await onLiveDataEvent(handleEvent);
    });

    onDestroy(async () => {
        if (unlisten !== null) {
            unlisten();
            unlisten = null;
        }
        if (chart !== null) {
            chart.destroy();
            chart = null;
        }
        // Best-effort stop — if the user navigates away mid-stream,
        // the Rust task should tear down rather than orphan a
        // session on the device.
        try {
            await stopLiveData();
        } catch {
            // ignore — stop is idempotent
        }
    });

    function buildChartConfig(): ChartConfiguration<'line', { x: number; y: number }[]> {
        const fg = cssVar('--text', '#ececec');
        const muted = cssVar('--text-muted', '#999');
        const border = cssVar('--border', '#383838');

        return {
            type: 'line',
            data: {
                datasets: [
                    {
                        label: 'frames/s RX',
                        data: [],
                        borderColor: 'rgba(76, 175, 80, 0.9)',
                        backgroundColor: 'transparent',
                        borderWidth: 2,
                        pointRadius: 0,
                        tension: 0.2,
                    },
                    {
                        label: 'frames/s TX',
                        data: [],
                        borderColor: 'rgba(33, 150, 243, 0.9)',
                        backgroundColor: 'transparent',
                        borderWidth: 2,
                        pointRadius: 0,
                        tension: 0.2,
                    },
                ] as ChartDataset<'line', { x: number; y: number }[]>[],
            },
            options: {
                responsive: true,
                maintainAspectRatio: false,
                animation: false,
                parsing: false,
                interaction: { mode: 'nearest', intersect: false },
                plugins: {
                    legend: {
                        position: 'top',
                        labels: { color: fg },
                    },
                    tooltip: {
                        callbacks: {
                            title: (items) =>
                                items.length > 0
                                    ? `t = ${(items[0] as { parsed: { x: number } }).parsed.x.toFixed(1)} s`
                                    : '',
                        },
                    },
                },
                scales: {
                    x: {
                        type: 'linear',
                        title: { display: true, text: 'uptime (s)', color: muted },
                        ticks: { color: muted },
                        grid: { color: border },
                    },
                    y: {
                        type: 'linear',
                        title: { display: true, text: 'frames / s', color: muted },
                        beginAtZero: true,
                        ticks: { color: muted },
                        grid: { color: border },
                    },
                },
            },
        };
    }

    function handleEvent(event: LiveDataEvent): void {
        if (event.kind === 'status') {
            status = event.status;
            statusMessage = event.message ?? event.status;
            if (event.status === 'error') {
                error = event.message;
            }
            return;
        }
        // snapshot
        latest = event;
        updateChart(event);
    }

    function updateChart(snap: {
        uptimeMs: number;
        framesRx: number;
        framesTx: number;
    }): void {
        if (chart === null) return;

        if (previous !== null) {
            const dt = (snap.uptimeMs - previous.uptimeMs) / 1000;
            if (dt > 0) {
                const rxRate = (snap.framesRx - previous.framesRx) / dt;
                const txRate = (snap.framesTx - previous.framesTx) / dt;
                const t = snap.uptimeMs / 1000;

                const rxData = chart.data.datasets[0].data as { x: number; y: number }[];
                const txData = chart.data.datasets[1].data as { x: number; y: number }[];
                rxData.push({ x: t, y: Math.max(0, rxRate) });
                txData.push({ x: t, y: Math.max(0, txRate) });

                // Prune anything older than the configured window.
                const cutoff = t - settings.liveData.windowSeconds;
                for (const arr of [rxData, txData]) {
                    while (arr.length > 0 && arr[0].x < cutoff) {
                        arr.shift();
                    }
                }
                chart.update('none');
            }
        }
        previous = { uptimeMs: snap.uptimeMs, framesRx: snap.framesRx, framesTx: snap.framesTx };
    }

    async function start(): Promise<void> {
        if (!adapterReady || settings.adapter.interface === null) {
            error = 'Pick an adapter in the Adapters view first.';
            return;
        }
        error = null;
        clearChart();

        const request: LiveDataRequest = {
            interface: settings.adapter.interface,
            channel:
                settings.adapter.channel.length > 0
                    ? settings.adapter.channel
                    : null,
            bitrate: settings.adapter.bitrate,
            nodeId: settings.adapter.nodeId,
            timeoutMs: settings.adapter.timeoutMs,
            rateHz: settings.liveData.rateHz,
        };

        try {
            status = 'idle';
            statusMessage = 'starting…';
            await startLiveData(request);
            // After this resolves, status switches to 'running' via
            // the `status` event from the Rust task.
        } catch (err) {
            error = err instanceof Error ? err.message : String(err);
            status = 'error';
            statusMessage = 'failed';
        }
    }

    async function stop(): Promise<void> {
        try {
            await stopLiveData();
        } catch (err) {
            error = err instanceof Error ? err.message : String(err);
        }
    }

    async function clearChart(): Promise<void> {
        if (chart === null) return;
        previous = null;
        for (const ds of chart.data.datasets) {
            ds.data.length = 0;
        }
        chart.update('none');
        await tick();
    }

    function cssVar(name: string, fallback: string): string {
        const v = getComputedStyle(document.documentElement).getPropertyValue(name);
        return v.trim().length > 0 ? v.trim() : fallback;
    }

    function fmtMs(ms: number): string {
        const total = Math.floor(ms / 1000);
        const h = Math.floor(total / 3600);
        const m = Math.floor((total % 3600) / 60);
        const s = total % 60;
        if (h > 0) return `${h}h${String(m).padStart(2, '0')}m${String(s).padStart(2, '0')}s`;
        if (m > 0) return `${m}m${String(s).padStart(2, '0')}s`;
        return `${s}.${String(ms % 1000).padStart(3, '0').slice(0, 1)}s`;
    }

    function fmtHex(value: number, width: number): string {
        return `0x${value.toString(16).toUpperCase().padStart(width, '0')}`;
    }

    // Helpers to read fields from the most-recent snapshot.
    const snap = $derived.by(() => (latest !== null && latest.kind === 'snapshot' ? latest : null));
</script>

<div class="view">
    <header>
        <div>
            <h2>Live data</h2>
            <p class="muted">
                Streaming bootloader telemetry — frames/sec, session age,
                NACK counts. One panel per (adapter, channel); pressing
                Start opens a session and subscribes to NOTIFY_LIVE_DATA.
            </p>
        </div>
    </header>

    {#if !adapterReady}
        <div class="banner banner-warning">
            <strong>No adapter selected.</strong> Pick one in the
            <em>Adapters</em> view first.
        </div>
    {/if}

    <div class="toolbar card card-tight">
        <button
            type="button"
            class="btn btn-primary"
            disabled={status === 'running' || !adapterReady}
            onclick={start}
        >
            Start
        </button>
        <button
            type="button"
            class="btn"
            disabled={status !== 'running'}
            onclick={stop}
        >
            Stop
        </button>
        <button type="button" class="btn" onclick={clearChart}>
            Clear chart
        </button>

        <div class="config">
            <label class="config-field">
                <span>Rate (Hz)</span>
                <input
                    class="input mono"
                    type="number"
                    min="1"
                    max="50"
                    bind:value={settings.liveData.rateHz}
                    disabled={status === 'running'}
                />
            </label>
            <label class="config-field">
                <span>Window (s)</span>
                <input
                    class="input mono"
                    type="number"
                    min="5"
                    max="600"
                    bind:value={settings.liveData.windowSeconds}
                />
            </label>
        </div>

        <span class="status status-{status}">
            {status === 'running' ? '●' : '○'} {statusMessage}
        </span>
    </div>

    {#if error !== null}
        <div class="banner banner-danger">{error}</div>
    {/if}

    {#if snap !== null}
        <div class="indicators">
            <span class="indicator" class:on={snap.sessionActive}>
                session active
            </span>
            <span class="indicator" class:on={snap.validAppPresent}>
                valid app
            </span>
            <span class="indicator" class:on={snap.wrpProtected}>
                WRP
            </span>
            <span class="indicator" class:on={snap.logStreaming}>
                log stream
            </span>
            <span class="indicator" class:on={snap.livedataStreaming}>
                live-data stream
            </span>
        </div>

        <div class="counters">
            <div class="counter">
                <div class="label">Uptime</div>
                <div class="value">{fmtMs(snap.uptimeMs)}</div>
            </div>
            <div class="counter">
                <div class="label">Session age</div>
                <div class="value">{fmtMs(snap.sessionAgeMs)}</div>
            </div>
            <div class="counter">
                <div class="label">DTCs</div>
                <div class="value">{snap.dtcCount}</div>
            </div>
            <div class="counter">
                <div class="label">NACKs sent</div>
                <div class="value">{snap.nacksSent}</div>
            </div>
            <div class="counter">
                <div class="label">Last DTC code</div>
                <div class="value"><code>{fmtHex(snap.lastDtcCode, 4)}</code></div>
            </div>
            <div class="counter">
                <div class="label">Last opcode</div>
                <div class="value"><code>{fmtHex(snap.lastOpcode, 2)}</code></div>
            </div>
            <div class="counter">
                <div class="label">Last flash addr</div>
                <div class="value"><code>{fmtHex(snap.lastFlashAddr, 8)}</code></div>
            </div>
            <div class="counter">
                <div class="label">ISO-TP RX bytes</div>
                <div class="value">{snap.isotpRxProgress}</div>
            </div>
        </div>
    {/if}

    <div class="chart-wrap">
        <canvas bind:this={canvas}></canvas>
    </div>
</div>

<style>
    /* Shared chrome (.view, .card, .banner-*, .btn, .input,
       .muted, .mono) comes from app.css. Local styles cover the
       toolbar layout, the on/off indicators, the counter grid,
       and the chart wrapper. */

    /* Toolbar — controls + config inputs + status text, all on
       one row with wrap. Builds on .card .card-tight. */
    .toolbar {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
    }
    .toolbar .config {
        display: flex;
        gap: var(--space-3);
        margin-left: var(--space-1);
    }
    .config-field {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        font-size: var(--text-sm);
        color: var(--text-muted);
    }
    .config-field .input {
        width: 80px;
        padding: var(--space-1) var(--space-2);
    }

    /* Status text pinned to the right edge of the toolbar. Color
       encodes state — muted by default, green when streaming,
       danger when errored. */
    .status {
        margin-left: auto;
        font-size: var(--text-sm);
        color: var(--text-muted);
        font-family: var(--font-mono);
    }
    .status.status-running {
        color: var(--success);
    }
    .status.status-error {
        color: var(--danger);
    }

    /* Boolean indicators — pill shapes that fill with --success
       when the corresponding flag is true, plain outlined when
       false. Quieter than the design system's .pill-success
       which would be loud for five flags side by side. */
    .indicators {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-2);
    }
    .indicator {
        padding: 2px var(--space-3);
        border-radius: var(--radius-pill);
        font-size: var(--text-sm);
        border: 1px solid var(--border);
        color: var(--text-muted);
        transition:
            background var(--motion-fast),
            color var(--motion-fast),
            border-color var(--motion-fast);
    }
    .indicator.on {
        background: var(--success-soft);
        color: var(--success);
        border-color: var(--success);
    }

    /* Counter grid — auto-fit columns so the layout adapts to
       sidebar-collapse without breaking. Each tile is a flat
       --surface block (no border) with a small uppercase label
       above a big mono value. */
    .counters {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
        gap: var(--space-2);
    }
    .counter {
        padding: var(--space-3) var(--space-4);
        border: 1px solid var(--border);
        border-radius: var(--radius-md);
        background: var(--surface);
    }
    .counter .label {
        font-size: var(--text-xs);
        color: var(--text-muted);
    }
    .counter .value {
        font-family: var(--font-mono);
        font-size: var(--text-xl);
        margin-top: 2px;
    }
    code {
        font-family: var(--font-mono);
    }

    /* Chart wrapper — fixed height so the canvas has somewhere
       to fill into. */
    .chart-wrap {
        position: relative;
        height: 320px;
        flex: 0 0 320px;
    }
</style>
