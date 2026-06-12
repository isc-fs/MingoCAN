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
        <div>
            <h2>Signals</h2>
            <p class="muted">
                Every signal defined in the loaded DBC, with its live
                decoded value driven by the Bus monitor. Per-adapter
                DBC associations live in <em>Settings → DBC files</em>.
            </p>
        </div>
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
        <div class="loaded-bar card card-tight">
            <span class="path mono">{summary.path}</span>
            <span class="stat">
                <strong>{summary.messageCount}</strong> messages
            </span>
            <span class="stat">
                <strong>{summary.signalCount}</strong> signals
            </span>
        </div>

        <div class="filter-row">
            <label for="sigfilter">Filter</label>
            <input
                id="sigfilter"
                class="input mono"
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
    /* Bespoke layout for this view — the table needs sticky-header
       + flex-fill scroll behavior that the design system doesn't
       ship. Empty-state, loaded-bar, filter-row, and the signal
       table itself live here. */

    /* This view fills the viewport and scrolls its own table —
       override the design system's .view that scrolls the whole
       page. */
    .view {
        overflow: hidden;
        height: 100%;
    }

    /* Empty-state — dashed border tells the operator "no real
       data here yet, but it'll fill in when you point at a DBC". */
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

    /* Loaded-bar — tight summary card (DBC path + counts). Builds
       on .card .card-tight; this just lays out the inner contents
       inline with the path stretching to fill. */
    .loaded-bar {
        display: flex;
        gap: var(--space-4);
        align-items: center;
        font-size: var(--text-sm);
        color: var(--text-muted);
    }
    .loaded-bar .path {
        flex: 1;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        color: var(--text);
    }
    .stat strong {
        color: var(--text);
    }

    /* Filter row — label + free-text input that flexes. */
    .filter-row {
        display: flex;
        align-items: center;
        gap: var(--space-3);
    }
    .filter-row label {
        font-size: var(--text-sm);
        color: var(--text-muted);
    }
    .filter-row .input {
        flex: 1;
    }

    /* Signal table — flex-fills the remaining height with a
       sticky thead. Body scrolls on Y. */
    .table-wrap {
        flex: 1;
        min-height: 0;
        overflow: auto;
        border: 1px solid var(--border);
        border-radius: var(--radius-md);
        background: var(--surface);
    }
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
    /* Live row — green value cell when this signal has streamed
       at least once during the current monitor session. */
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
    .empty {
        padding: var(--space-5);
        text-align: center;
        color: var(--text-muted);
    }
</style>
