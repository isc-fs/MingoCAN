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

    Three view modes, toggled by the tab strip:

      - "Signals"     — the DBC-decoded table (default). One row per
        signal defined in the loaded DBC, with its live physical
        value. Needs a DBC loaded (Settings → DBC) and the monitor
        running for live values; shows the schema with placeholder
        values otherwise.
      - "By ID"       — raw aggregated rows, one per unique ID.
      - "Live frames" — raw candump-style scrolling log.

    The "By ID" / "Live frames" raw modes honour an ID filter
    (comma-separated hex prefixes, "0x" optional). Pause stops
    appending to the visible buffers but keeps the backend stream
    running — resume just starts showing new frames again, no
    reconnection.

    Adapter selection comes from the central `settings.adapter`
    store (same as Flash / Diagnostics).
-->
<script lang="ts">
    import { onDestroy, onMount } from 'svelte';
    import type { UnlistenFn } from '@tauri-apps/api/event';
    import { save as saveDialog } from '@tauri-apps/plugin-dialog';

    import {
        formatData,
        formatId,
        formatTs,
        onBusMonitorCapture,
        onBusMonitorFrame,
        onBusMonitorStatus,
        startBusMonitor,
        startBusMonitorCapture,
        stopBusMonitor,
        stopBusMonitorCapture,
        type BusMonitorFrame,
        type BusMonitorRequest,
    } from './bus_monitor';
    import {
        getDbcSignals,
        getDbcStatus,
        onDbcStatus,
        onDecodedSignals,
        type DbcSummary,
        type SignalSchema,
    } from './dbc';
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
    // Rows evicted from the *visible* Live-frames buffer once it
    // exceeds maxRows. This is display-only bookkeeping — NOT frames
    // lost from the bus: the decoded Signals view, By-ID aggregation,
    // and capture-to-file all see every frame regardless. (Real RX
    // overruns would need per-transport driver support, which the
    // backend doesn't surface yet.)
    let trimmedFrames = $state<number>(0);

    let unlistenFrame: UnlistenFn | null = null;
    let unlistenStatus: UnlistenFn | null = null;
    let unlistenCapture: UnlistenFn | null = null;

    // Capture-to-file state. `path` non-null means a capture is
    // active; the backend drives transitions via `bus_monitor:capture`
    // events. `frames` updates on a debounced Progress cadence.
    let captureActive = $state<boolean>(false);
    let capturePath = $state<string | null>(null);
    let captureFrames = $state<number>(0);
    let captureError = $state<string | null>(null);

    // ---- DBC decode (Signals mode) ----
    //
    // The decoded-signals stream is emitted by the same backend
    // monitor when a DBC is loaded. We subscribe persistently (on
    // mount, not on Start) so the schema table is visible the moment
    // a DBC is loaded; live values flow only while the monitor runs.
    let dbcSummary = $state<DbcSummary | null>(null);
    let schema = $state<SignalSchema[]>([]);
    // Live values keyed by signalKey. Plain Map for fast point
    // updates; promoted to a render tick via rAF so we don't re-render
    // the table on every incoming signal.
    const sigValues = new Map<string, number>();
    let sigValuesTick = $state<number>(0);
    let signalFilter = $state<string>('');
    let unlistenDbcStatus: UnlistenFn | null = null;
    let unlistenDecoded: UnlistenFn | null = null;
    let sigRafPending = false;

    onMount(async () => {
        unlistenDbcStatus = await onDbcStatus(async (evt) => {
            if (evt.kind === 'loaded') {
                dbcSummary = {
                    path: evt.path,
                    messageCount: evt.messageCount,
                    signalCount: evt.signalCount,
                };
                schema = await getDbcSignals();
                sigValues.clear();
                sigValuesTick++;
            } else if (evt.kind === 'unloaded') {
                dbcSummary = null;
                schema = [];
                sigValues.clear();
                sigValuesTick++;
            }
        });
        unlistenDecoded = await onDecodedSignals((decoded) => {
            for (const sig of decoded) sigValues.set(sig.signalKey, sig.value);
            if (!sigRafPending) {
                sigRafPending = true;
                requestAnimationFrame(() => {
                    sigRafPending = false;
                    sigValuesTick++;
                });
            }
        });
        // Cold-start: reflect a DBC already loaded before mount.
        dbcSummary = await getDbcStatus();
        if (dbcSummary !== null) schema = await getDbcSignals();
    });

    // Filtered signal schema — case-insensitive substring across
    // signal name / message name / unit. Reads the values tick so the
    // table re-renders as decoded values stream in.
    //
    // It MUST return a fresh array every recompute: the live values
    // live in a plain (non-reactive) Map that the row template reads
    // via `formatValue`, so the only thing that re-runs the {#each}
    // (and thus re-reads the Map) is a change in this derived's
    // identity. Returning the same `schema` reference when the filter
    // is empty made Svelte 5 treat the derived as unchanged on each
    // tick — so values stayed at "—" until a tab switch tore down and
    // rebuilt the block. `.slice()` gives a new reference each tick.
    const filteredSchema = $derived.by(() => {
        // eslint-disable-next-line @typescript-eslint/no-unused-expressions
        sigValuesTick;
        const f = signalFilter.trim().toLowerCase();
        if (f.length === 0) return schema.slice();
        return schema.filter(
            (s) =>
                s.signalName.toLowerCase().includes(f) ||
                s.messageName.toLowerCase().includes(f) ||
                s.unit.toLowerCase().includes(f),
        );
    });

    function formatValue(key: string): string {
        const v = sigValues.get(key);
        if (v === undefined) return '—';
        if (Math.abs(v) >= 1000) return v.toFixed(0);
        if (Math.abs(v) >= 10) return v.toFixed(1);
        if (Math.abs(v) >= 1) return v.toFixed(2);
        return v.toFixed(4);
    }

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
                trimmedFrames += next.length - max;
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
            unlistenCapture = await onBusMonitorCapture((evt) => {
                if (evt.kind === 'started') {
                    captureActive = true;
                    capturePath = evt.path;
                    captureFrames = 0;
                    captureError = null;
                } else if (evt.kind === 'stopped') {
                    captureActive = false;
                    capturePath = evt.path;
                    captureFrames = evt.frames;
                } else if (evt.kind === 'progress') {
                    capturePath = evt.path;
                    captureFrames = evt.frames;
                } else if (evt.kind === 'error') {
                    captureError = evt.message;
                    captureActive = false;
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
        if (unlistenCapture !== null) {
            unlistenCapture();
            unlistenCapture = null;
        }
    }

    // ---- Capture-to-file ----

    async function toggleCapture(): Promise<void> {
        if (captureActive) {
            try {
                await stopBusMonitorCapture();
            } catch (err) {
                captureError = err instanceof Error ? err.message : String(err);
            }
            return;
        }
        // Open the native save dialog. Default filename includes
        // the current timestamp so operators don't accidentally
        // clobber a previous capture.
        const now = new Date();
        const pad = (n: number): string => n.toString().padStart(2, '0');
        const stamp = `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}-${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`;
        const defaultName = `can-capture-${stamp}.log`;
        let picked: string | null = null;
        try {
            picked = await saveDialog({
                title: 'Save CAN capture',
                defaultPath: defaultName,
                filters: [
                    { name: 'candump log', extensions: ['log'] },
                    { name: 'All files', extensions: ['*'] },
                ],
            });
        } catch (err) {
            captureError = err instanceof Error ? err.message : String(err);
            return;
        }
        if (picked === null) return; // user cancelled

        try {
            await startBusMonitorCapture(picked);
        } catch (err) {
            captureError = err instanceof Error ? err.message : String(err);
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
        trimmedFrames = 0;
        sigValues.clear();
        sigValuesTick++;
    }

    onDestroy(async () => {
        if (unlistenDbcStatus !== null) unlistenDbcStatus();
        if (unlistenDecoded !== null) unlistenDecoded();
        if (status !== 'idle') await stop();
    });
</script>

<div class="view">
    <header>
        <div>
            <h2>Bus monitor</h2>
            <p class="muted">
                Live capture of the selected CAN bus. Defaults to
                <em>Signals</em> — the DBC-decoded table of named values
                (load a DBC in <em>Settings&nbsp;→&nbsp;DBC&nbsp;files</em>).
                Toggle to <em>By&nbsp;ID</em> or <em>Live&nbsp;frames</em> for
                the raw view of an undocumented bus.
            </p>
        </div>
    </header>

    {#if !adapterReady}
        <div class="banner banner-warning">
            <strong>No adapter selected.</strong> Pick one in the
            <em>Adapters</em> view first — the monitor needs an
            <code>--interface</code>/<code>--channel</code> pair.
        </div>
    {/if}

    <div class="controls card card-tight">
        <div class="actions">
            {#if status === 'idle' || status === 'error'}
                <button
                    type="button"
                    class="btn btn-primary"
                    disabled={!adapterReady}
                    onclick={start}
                >
                    Start
                </button>
            {:else}
                <button
                    type="button"
                    class="btn"
                    disabled={status === 'starting' || status === 'stopping'}
                    onclick={stop}
                >
                    Stop
                </button>
                <button
                    type="button"
                    class="btn"
                    disabled={status === 'starting' || status === 'stopping'}
                    onclick={togglePause}
                >
                    {status === 'paused' ? 'Resume' : 'Pause'}
                </button>
            {/if}
            <button
                type="button"
                class="btn"
                disabled={liveFrames.length === 0 && idStatsView.length === 0}
                onclick={clearBuffers}
            >
                Clear
            </button>
            <button
                type="button"
                class="btn capture-btn"
                class:active={captureActive}
                disabled={status === 'idle' || status === 'error'
                    || status === 'starting' || status === 'stopping'}
                onclick={toggleCapture}
                title={captureActive
                    ? `Stop saving (writing to ${capturePath})`
                    : 'Save every received frame to a candump-format file'}
            >
                {captureActive ? '⏺ Stop saving' : 'Save…'}
            </button>
        </div>

        <div class="filter">
            {#if settings.busMonitor.activeTab === 'signals'}
                <label for="sigfilter">Filter</label>
                <input
                    id="sigfilter"
                    class="input mono"
                    type="text"
                    placeholder="signal / message / unit"
                    bind:value={signalFilter}
                />
            {:else}
                <label for="idfilter">Filter by ID</label>
                <input
                    id="idfilter"
                    class="input mono"
                    type="text"
                    placeholder="e.g. 0x1A, 0x200"
                    bind:value={settings.busMonitor.idFilter}
                />
            {/if}
        </div>

        <div class="stats">
            <span class="stat">
                <strong class="status-dot" class:running={status === 'running'} class:paused={status === 'paused'} class:error={status === 'error'}>●</strong>
                <span>{status}</span>
            </span>
            <span class="stat">frames&nbsp;<strong>{totalFrames}</strong></span>
            <span class="stat">unique&nbsp;IDs&nbsp;<strong>{idStats.size}</strong></span>
            {#if trimmedFrames > 0 && settings.busMonitor.activeTab === 'live'}
                <span
                    class="stat trimmed"
                    title="Older rows scrolled out of the Live-frames view to stay under maxRows ({settings.busMonitor.maxRows}). Not lost — the Signals decode, By-ID view, and Save… capture all see every frame."
                >
                    trimmed&nbsp;<strong>{trimmedFrames}</strong>
                </span>
            {/if}
            {#if captureActive}
                <span class="stat capturing" title={capturePath ?? undefined}>
                    <strong class="status-dot running">⏺</strong>
                    capturing&nbsp;<strong>{captureFrames}</strong>
                </span>
            {/if}
        </div>
    </div>

    {#if captureError !== null}
        <div class="banner banner-danger">Capture: {captureError}</div>
    {/if}

    {#if error !== null}
        <div class="banner banner-danger">{error}</div>
    {/if}

    <div class="tabs">
        <button
            type="button"
            class="tab"
            class:active={settings.busMonitor.activeTab === 'signals'}
            onclick={() => (settings.busMonitor.activeTab = 'signals')}
        >
            Signals ({schema.length})
        </button>
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

    {#if settings.busMonitor.activeTab === 'signals'}
        {#if dbcSummary === null}
            <div class="empty-state">
                <strong>No DBC loaded for this adapter.</strong>
                <p>
                    Pick a <code>.dbc</code> file in
                    <em>Settings&nbsp;→&nbsp;DBC&nbsp;files</em> and switch
                    back here. The signal list populates the moment a DBC
                    is loaded; live values stream in once you click
                    <strong>Start</strong>. To watch an undocumented bus,
                    use the <em>By&nbsp;ID</em> or <em>Live&nbsp;frames</em>
                    tabs instead.
                </p>
            </div>
        {:else}
            <div class="table-wrap">
                <table class="signal-table">
                    <thead>
                        <tr>
                            <th class="col-message">Message</th>
                            <th class="col-name">Signal</th>
                            <th class="col-value">Value</th>
                            <th class="col-unit">Unit</th>
                            <th class="col-range">Range</th>
                        </tr>
                    </thead>
                    <tbody>
                        {#each filteredSchema as sig (sig.signalKey)}
                            <tr class:has-value={sigValues.has(sig.signalKey)}>
                                <td class="col-message mono">
                                    <span class="msg-name">{sig.messageName}</span>
                                    <span class="msg-id mono">0x{sig.messageId.toString(16).toUpperCase().padStart(3, '0')}</span>
                                </td>
                                <td class="col-name">{sig.signalName}</td>
                                <td class="col-value mono">{formatValue(sig.signalKey)}</td>
                                <td class="col-unit">{sig.unit}</td>
                                <td class="col-range mono">{sig.min}–{sig.max}</td>
                            </tr>
                        {/each}
                        {#if filteredSchema.length === 0}
                            <tr>
                                <td colspan="5" class="empty">
                                    {signalFilter.length > 0
                                        ? 'No signals match the filter.'
                                        : 'DBC loaded but has no signals.'}
                                </td>
                            </tr>
                        {/if}
                    </tbody>
                </table>
            </div>
        {/if}
    {:else if settings.busMonitor.activeTab === 'byId'}
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
    /* Shared chrome (.view, .card, .banner-*, .btn, .input,
       .muted, .mono) comes from app.css. Local styles cover the
       controls row layout, the tabbed table chrome, the frame
       table itself, and the status-dot indicator. */

    /* This view fills the viewport and scrolls its table — override
       the design system's .view that scrolls the whole page. */
    .view {
        overflow: hidden;
        height: 100%;
    }

    /* Controls row — actions on the left, ID filter in the middle,
       run-stats on the right. Wraps to a second row at narrow
       widths. */
    .controls {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-4);
        align-items: end;
    }
    .actions {
        display: flex;
        gap: var(--space-2);
    }
    .filter {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        flex: 1;
        min-width: 200px;
    }
    .filter label {
        font-size: var(--text-sm);
        color: var(--text-muted);
    }

    /* Stats row — mono-font cluster of run-time counters. Each
       .stat is a label + value pair; .strong picks up --text so
       the counter pops out of the muted label. */
    .stats {
        display: flex;
        gap: var(--space-4);
        font-size: var(--text-sm);
        color: var(--text-muted);
        font-family: var(--font-mono);
    }
    .stat {
        display: inline-flex;
        gap: var(--space-1);
        align-items: center;
    }
    .stat strong {
        color: var(--text);
        font-weight: 600;
    }
    /* Trimmed = Live-frames scrollback eviction; neutral/muted, not
       an alarm — it's bookkeeping, not data loss. */
    .stat.trimmed strong {
        color: var(--text-muted);
    }
    .stat.capturing strong {
        color: var(--danger);
    }

    /* Run-status dot — color encodes state. Pulse animation while
       running so the operator can tell the stream is live without
       reading the label. */
    .status-dot {
        color: var(--text-muted);
    }
    .status-dot.running {
        color: var(--success);
        animation: pulse 1.2s ease-in-out infinite;
    }
    .status-dot.paused {
        color: var(--accent);
    }
    .status-dot.error {
        color: var(--danger);
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

    /* Active-capture indicator — swap the .btn outline to danger
       so the operator can tell capture is on without parsing the
       label. */
    .btn.capture-btn.active {
        border-color: var(--danger);
        color: var(--danger);
    }
    .btn.capture-btn.active:hover:not(:disabled) {
        background: var(--danger-soft);
        border-color: var(--danger);
        color: var(--danger);
    }

    /* Tab strip — two flat buttons that sit above the table; the
       active tab merges visually into the table's top edge by
       sharing border + background. Negative margin pulls the
       table up against the tab strip's bottom border. */
    .tabs {
        display: flex;
        gap: var(--space-1);
        border-bottom: 1px solid var(--border);
        margin-bottom: -1px;
    }
    .tab {
        background: transparent;
        border: 1px solid transparent;
        border-bottom: none;
        border-radius: var(--radius-md) var(--radius-md) 0 0;
        padding: var(--space-2) var(--space-3);
        color: var(--text-muted);
        font-size: var(--text-sm);
        font: inherit;
        font-size: var(--text-sm);
        cursor: pointer;
    }
    .tab:hover:not(:disabled) {
        color: var(--text);
    }
    .tab.active {
        background: var(--surface);
        border-color: var(--border);
        color: var(--text);
    }

    /* Frame table — flex-fills the remaining height with a sticky
       thead. The table-wrap matches the .tab.active background so
       it visually continues from the active tab. */
    .table-wrap {
        flex: 1;
        min-height: 0;
        overflow: auto;
        border: 1px solid var(--border);
        border-radius: 0 var(--radius-md) var(--radius-md) var(--radius-md);
        background: var(--surface);
    }
    .frame-table {
        width: 100%;
        border-collapse: collapse;
        font-size: var(--text-sm);
    }
    .frame-table thead {
        position: sticky;
        top: 0;
        background: var(--surface);
        z-index: 1;
    }
    .frame-table th,
    .frame-table td {
        padding: var(--space-2) var(--space-3);
        text-align: left;
        border-bottom: 1px solid var(--border);
    }
    .frame-table th {
        font-weight: 500;
        font-size: var(--text-xs);
        color: var(--text-muted);
    }
    .col-id {
        width: 80px;
    }
    .col-count {
        width: 80px;
        text-align: right;
    }
    .col-rate {
        width: 70px;
        text-align: right;
    }
    .col-dlc {
        width: 50px;
        text-align: right;
    }
    .col-ts {
        width: 100px;
    }
    .col-data {
        white-space: nowrap;
    }
    .empty {
        padding: var(--space-5);
        text-align: center;
        color: var(--text-muted);
    }

    /* ---- Signals (decoded) mode ---- */

    /* Empty-state — shown in Signals mode when no DBC is loaded.
       Dashed border signals "no data here yet, point at a DBC". */
    .empty-state {
        padding: var(--space-5);
        border: 1px dashed var(--border-strong);
        background: var(--surface);
        border-radius: var(--radius-lg);
        color: var(--text-muted);
    }
    .empty-state strong {
        color: var(--text);
    }
    .empty-state p {
        margin: var(--space-2) 0 0;
    }
    .empty-state code {
        font-family: var(--font-mono);
        padding: 1px var(--space-1);
        background: rgba(255, 255, 255, 0.04);
        border-radius: var(--radius-sm);
    }

    /* Decoded-signal table — shares .table-wrap chrome with the raw
       frame tables; the column layout is signal-specific. */
    .signal-table {
        width: 100%;
        border-collapse: collapse;
        font-size: var(--text-sm);
    }
    .signal-table thead {
        position: sticky;
        top: 0;
        background: var(--surface);
        z-index: 1;
    }
    .signal-table th,
    .signal-table td {
        padding: var(--space-2) var(--space-3);
        text-align: left;
        border-bottom: 1px solid var(--border);
    }
    .signal-table th {
        font-weight: 500;
        font-size: var(--text-xs);
        color: var(--text-muted);
    }
    .col-message {
        width: 180px;
        display: flex;
        gap: var(--space-2);
        align-items: baseline;
    }
    .col-message .msg-name {
        font-weight: 500;
    }
    .col-message .msg-id {
        color: var(--text-muted);
        font-size: var(--text-xs);
    }
    .col-name {
        width: 220px;
    }
    .col-value {
        width: 100px;
        text-align: right;
        color: var(--text-muted);
    }
    /* Live row — green value cell once this signal has streamed at
       least once during the current monitor session. */
    tr.has-value .col-value {
        color: var(--success);
        font-weight: 600;
    }
    .col-unit {
        width: 80px;
        color: var(--text-muted);
    }
    .col-range {
        color: var(--text-muted);
        font-size: var(--text-xs);
    }
</style>
