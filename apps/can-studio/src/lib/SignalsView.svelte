<!--
    Signals view — Tier 2.

    Renders a flat list of every DBC-defined signal with its live
    physical value, updated by the bus monitor's signals stream
    when a DBC is loaded *and* the monitor is running.

    The view doesn't drive the monitor itself — the operator
    starts/stops capture from Bus monitor. Signals reads two
    things:
      - The schema (one-shot load from `dbc_signals`)
      - The live values stream (`bus_monitor:signals`)

    With no DBC loaded, we show a friendly "pick a DBC in
    Settings" hint. With a DBC loaded but no monitor running,
    we show the schema rows with placeholder values.
-->
<script lang="ts">
    import { onDestroy, onMount } from 'svelte';
    import type { UnlistenFn } from '@tauri-apps/api/event';

    import {
        getDbcSignals,
        getDbcStatus,
        onDbcStatus,
        onDecodedSignals,
        type DbcSummary,
        type SignalSchema,
    } from './dbc';

    let summary = $state<DbcSummary | null>(null);
    let schema = $state<SignalSchema[]>([]);
    // Live values keyed by signalKey. Plain Map for fast point
    // updates; we promote to a sorted array via $state on a
    // requestAnimationFrame tick so we don't re-render the table
    // on every incoming signal.
    const values = new Map<string, number>();
    let valuesTick = $state<number>(0);
    let filter = $state<string>('');

    let unlistenStatus: UnlistenFn | null = null;
    let unlistenSignals: UnlistenFn | null = null;
    let rafPending = false;

    onMount(async () => {
        unlistenStatus = await onDbcStatus(async (evt) => {
            if (evt.kind === 'loaded') {
                summary = {
                    path: evt.path,
                    messageCount: evt.messageCount,
                    signalCount: evt.signalCount,
                };
                schema = await getDbcSignals();
                values.clear();
                valuesTick++;
            } else if (evt.kind === 'unloaded') {
                summary = null;
                schema = [];
                values.clear();
                valuesTick++;
            }
        });
        unlistenSignals = await onDecodedSignals((decoded) => {
            for (const sig of decoded) values.set(sig.signalKey, sig.value);
            if (!rafPending) {
                rafPending = true;
                requestAnimationFrame(() => {
                    rafPending = false;
                    valuesTick++;
                });
            }
        });

        // Cold-start: fetch current state in case the DBC was loaded
        // before this view mounted.
        summary = await getDbcStatus();
        if (summary !== null) schema = await getDbcSignals();
    });

    onDestroy(() => {
        if (unlistenStatus !== null) unlistenStatus();
        if (unlistenSignals !== null) unlistenSignals();
    });

    // Filter is case-insensitive substring match across signal
    // name + message name + unit.
    const filteredSchema = $derived.by(() => {
        // Touch the values tick so the table updates as values
        // flow in without forcing the filter to re-evaluate
        // (which would be wasteful — the schema is static).
        // We use a separate $derived because Svelte 5 wants
        // every reactive read at the top of the block.
        // eslint-disable-next-line @typescript-eslint/no-unused-expressions
        valuesTick;
        const f = filter.trim().toLowerCase();
        if (f.length === 0) return schema;
        return schema.filter((s) =>
            s.signalName.toLowerCase().includes(f) ||
            s.messageName.toLowerCase().includes(f) ||
            s.unit.toLowerCase().includes(f),
        );
    });

    function formatValue(key: string): string {
        const v = values.get(key);
        if (v === undefined) return '—';
        // Show a sensible number of decimals based on the
        // factor's order of magnitude. Tiny factors usually mean
        // fixed-point signals where 4 decimals is overkill.
        if (Math.abs(v) >= 1000) return v.toFixed(0);
        if (Math.abs(v) >= 10) return v.toFixed(1);
        if (Math.abs(v) >= 1) return v.toFixed(2);
        return v.toFixed(4);
    }
</script>

<div class="view">
    <header>
        <h2>Signals</h2>
        <p class="muted">
            Every signal defined in the loaded DBC, with its live
            decoded value driven by the Bus monitor. Per-adapter
            DBC associations live in <em>Settings → DBC files</em>.
        </p>
    </header>

    {#if summary === null}
        <div class="empty-state">
            <strong>No DBC loaded for this adapter.</strong>
            <p>
                Pick a <code>.dbc</code> file in
                <em>Settings → DBC files</em> and switch back here.
                The signal list below populates the moment a DBC is
                loaded; live values stream in while Bus monitor is
                running.
            </p>
        </div>
    {:else}
        <div class="loaded-bar">
            <span class="path mono">{summary.path}</span>
            <span class="stat">
                <strong>{summary.messageCount}</strong> messages
            </span>
            <span class="stat">
                <strong>{summary.signalCount}</strong> signals
            </span>
        </div>

        <div class="filter">
            <label for="sigfilter">Filter</label>
            <input
                id="sigfilter"
                type="text"
                placeholder="signal name / message / unit"
                bind:value={filter}
            />
        </div>

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
                        <tr class:has-value={values.has(sig.signalKey)}>
                            <td class="col-message mono">
                                <span class="msg-name">{sig.messageName}</span>
                                <span class="msg-id mono">0x{sig.messageId.toString(16).toUpperCase().padStart(3, '0')}</span>
                            </td>
                            <td class="col-name">{sig.signalName}</td>
                            <td class="col-value mono">{formatValue(sig.signalKey)}</td>
                            <td class="col-unit">{sig.unit}</td>
                            <td class="col-range mono">
                                {sig.min}–{sig.max}
                            </td>
                        </tr>
                    {/each}
                    {#if filteredSchema.length === 0}
                        <tr>
                            <td colspan="5" class="empty">
                                {filter.length > 0
                                    ? 'No signals match the filter.'
                                    : 'DBC loaded but has no signals.'}
                            </td>
                        </tr>
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
    .empty-state {
        padding: 20px;
        border: 1px dashed var(--border);
        background: var(--surface);
        border-radius: 8px;
        color: var(--text-muted);
    }
    .empty-state strong { color: var(--text); }
    .empty-state p { margin: 6px 0 0; }
    .empty-state code {
        font-family: var(--font-mono);
        padding: 1px 4px;
        background: rgba(255, 255, 255, 0.04);
        border-radius: 3px;
    }
    .loaded-bar {
        display: flex;
        gap: 14px;
        align-items: center;
        padding: 8px 14px;
        border: 1px solid var(--border);
        background: var(--surface);
        border-radius: 8px;
        font-size: 0.82rem;
        color: var(--text-muted);
    }
    .loaded-bar .path {
        flex: 1;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        color: var(--text);
    }
    .stat strong { color: var(--text); }
    .filter {
        display: flex;
        align-items: center;
        gap: 10px;
    }
    .filter label { font-size: 0.78rem; color: var(--text-muted); }
    .filter input {
        flex: 1;
        background: var(--bg);
        color: var(--text);
        border: 1px solid var(--border);
        border-radius: 4px;
        padding: 6px 8px;
        font-family: var(--font-mono);
        font-size: 0.85rem;
    }
    .filter input:focus { outline: none; border-color: var(--accent); }
    .table-wrap {
        flex: 1;
        min-height: 0;
        overflow: auto;
        border: 1px solid var(--border);
        border-radius: 6px;
        background: var(--bg);
    }
    .signal-table {
        width: 100%;
        border-collapse: collapse;
        font-size: 0.85rem;
    }
    .signal-table thead {
        position: sticky;
        top: 0;
        background: var(--surface);
        z-index: 1;
    }
    .signal-table th, .signal-table td {
        padding: 6px 10px;
        text-align: left;
        border-bottom: 1px solid rgba(255, 255, 255, 0.04);
    }
    .signal-table th {
        font-weight: 600;
        font-size: 0.72rem;
        text-transform: uppercase;
        letter-spacing: 0.05em;
        color: var(--text-muted);
    }
    .mono { font-family: var(--font-mono); }
    .col-message { width: 180px; display: flex; gap: 6px; align-items: baseline; }
    .col-message .msg-name { font-weight: 500; }
    .col-message .msg-id { color: var(--text-muted); font-size: 0.78em; }
    .col-name { width: 220px; }
    .col-value { width: 100px; text-align: right; color: var(--text-muted); }
    tr.has-value .col-value { color: #06d6a0; font-weight: 600; }
    .col-unit { width: 80px; color: var(--text-muted); }
    .col-range { color: var(--text-muted); font-size: 0.78em; }
    .empty {
        padding: 20px;
        text-align: center;
        color: var(--text-muted);
    }
</style>
