<!--
    Flash history (#651) — every action that wrote to a board on this
    laptop, from the app or the CLI: who, what firmware, which board,
    when, and whether it worked. Read from the shared flash log; nothing
    here writes to a board.
-->
<script lang="ts">
    import { onMount } from 'svelte';
    import { save as saveDialog } from '@tauri-apps/plugin-dialog';

    import {
        actionLabel,
        artifactName,
        boardLabel,
        exportHistoryCsv,
        firmwareLabel,
        readHistory,
        type HistoryEntry,
    } from './history';
    import { logsReveal } from './logs';

    type BoardFilter = 'all' | 'ecu' | 'ams' | 'udv' | 'other';
    const BOARDS: ReadonlyArray<{ k: BoardFilter; label: string }> = [
        { k: 'all', label: 'All' },
        { k: 'ecu', label: 'ECU' },
        { k: 'ams', label: 'AMS' },
        { k: 'udv', label: 'uDV' },
        { k: 'other', label: 'Other' },
    ];
    const PAGE = 200;

    let entries = $state<HistoryEntry[]>([]);
    let logPath = $state<string | null>(null);
    let loading = $state<boolean>(false);
    let error = $state<string | null>(null);
    let notice = $state<string | null>(null);

    let board = $state<BoardFilter>('all');
    let failedOnly = $state<boolean>(false);
    let query = $state<string>('');
    let shown = $state<number>(PAGE);
    let expanded = $state<string | null>(null);

    async function refresh(): Promise<void> {
        loading = true;
        error = null;
        try {
            const h = await readHistory();
            entries = h.entries;
            logPath = h.path;
        } catch (e) {
            error = String(e);
        } finally {
            loading = false;
        }
    }

    onMount(() => {
        void refresh();
    });

    function matchesBoard(e: HistoryEntry): boolean {
        if (board === 'all') return true;
        if (board === 'other') return !e.role;
        return e.role === board;
    }

    function matchesQuery(e: HistoryEntry): boolean {
        const q = query.trim().toLowerCase();
        if (q === '') return true;
        return [
            e.operator,
            e.host,
            e.artifact,
            e.fwProduct,
            e.fwVersion,
            e.fwGitHash,
            e.crc32,
            e.error,
            e.deviceSerial,
            actionLabel(e.action),
        ].some((f) => f?.toLowerCase().includes(q));
    }

    const filtered = $derived(
        entries.filter((e) => matchesBoard(e) && (!failedOnly || !e.ok) && matchesQuery(e)),
    );
    const visible = $derived(filtered.slice(0, shown));
    const failedCount = $derived(entries.filter((e) => !e.ok).length);

    // A new filter starts back at the first page.
    $effect(() => {
        void board;
        void failedOnly;
        void query;
        shown = PAGE;
    });

    function rowKey(e: HistoryEntry, i: number): string {
        return `${e.ts}|${e.action}|${i}`;
    }

    function when(ts: string): { date: string; time: string } {
        const d = new Date(ts);
        if (Number.isNaN(d.getTime())) return { date: ts, time: '' };
        return {
            date: d.toLocaleDateString([], { year: 'numeric', month: 'short', day: 'numeric' }),
            time: d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' }),
        };
    }

    async function exportCsv(): Promise<void> {
        notice = null;
        const stamp = new Date().toISOString().slice(0, 10);
        const path = await saveDialog({
            defaultPath: `mingocan-flash-history-${stamp}.csv`,
            filters: [{ name: 'CSV', extensions: ['csv'] }],
        });
        if (path === null) return;
        try {
            const n = await exportHistoryCsv(path, filtered);
            notice = `Exported ${n} ${n === 1 ? 'entry' : 'entries'} to ${path}`;
        } catch (e) {
            error = String(e);
        }
    }

    async function reveal(): Promise<void> {
        if (logPath === null) return;
        try {
            await logsReveal(logPath);
        } catch (e) {
            error = String(e);
        }
    }

    function detailRows(e: HistoryEntry): Array<[string, string]> {
        const rows: Array<[string, string]> = [];
        if (e.artifact) rows.push(['File', e.artifact]);
        if (e.crc32) rows.push(['CRC32', e.crc32]);
        if (e.sizeBytes !== undefined) rows.push(['Size', `${e.sizeBytes.toLocaleString()} B`]);
        if (e.fwGitHash) rows.push(['Git hash', e.fwGitHash]);
        if (e.interface) rows.push(['Adapter', [e.interface, e.channel].filter(Boolean).join(' · ')]);
        if (e.deviceSerial) rows.push(['USB / probe serial', e.deviceSerial]);
        for (const [k, v] of Object.entries(e.details ?? {})) {
            rows.push([k, typeof v === 'string' ? v : JSON.stringify(v)]);
        }
        rows.push(['Logged by', `${e.tool} v${e.toolVersion}${e.host ? ` on ${e.host}` : ''}`]);
        if (e.exitCode !== undefined) rows.push(['Exit code', String(e.exitCode)]);
        return rows;
    }
</script>

<div class="view">
    <header class="view-header">
        <h2>Flash history</h2>
        <p class="muted">
            Everything this laptop has written to a board — firmware flashes,
            bootloader burns, node-ids, cleared DTCs — from the app or the
            command line. Newest first.
        </p>
    </header>

    {#if error}
        <div class="banner banner-danger">{error}</div>
    {/if}
    {#if notice}
        <div class="banner banner-success">{notice}</div>
    {/if}

    <section class="card">
        <div class="toolbar">
            <div class="segmented" role="tablist" aria-label="Board">
                {#each BOARDS as b (b.k)}
                    <button
                        type="button"
                        role="tab"
                        class="seg"
                        class:active={board === b.k}
                        aria-selected={board === b.k}
                        onclick={() => (board = b.k)}
                    >
                        {b.label}
                    </button>
                {/each}
            </div>
            <label class="check small">
                <input type="checkbox" bind:checked={failedOnly} />
                Failed only{failedCount > 0 ? ` (${failedCount})` : ''}
            </label>
            <input
                class="input search"
                type="search"
                placeholder="Search file, version, operator, error…"
                bind:value={query}
            />
            <span class="spacer"></span>
            <button type="button" class="btn btn-sm btn-ghost" onclick={refresh} disabled={loading}>
                {loading ? 'Loading…' : 'Refresh'}
            </button>
            <button
                type="button"
                class="btn btn-sm"
                onclick={exportCsv}
                disabled={filtered.length === 0}
                title="Save the rows shown below as a spreadsheet"
            >
                Export CSV
            </button>
        </div>

        {#if entries.length === 0 && !loading}
            <div class="empty">
                <h3>Nothing yet</h3>
                <p class="muted">
                    Flash a board, burn a bootloader or set a node-id and it
                    shows up here.
                </p>
            </div>
        {:else if filtered.length === 0}
            <p class="muted empty">No entries match these filters.</p>
        {:else}
            <table class="history-table">
                <thead>
                    <tr>
                        <th>When</th>
                        <th>Action</th>
                        <th>Board</th>
                        <th>Firmware</th>
                        <th>Result</th>
                        <th>Who</th>
                    </tr>
                </thead>
                <tbody>
                    {#each visible as e, i (rowKey(e, i))}
                        {@const key = rowKey(e, i)}
                        {@const w = when(e.ts)}
                        <tr
                            class="hrow"
                            class:failed={!e.ok}
                            class:open={expanded === key}
                            onclick={() => (expanded = expanded === key ? null : key)}
                        >
                            <td class="nowrap">
                                <div>{w.date}</div>
                                <div class="muted small mono">{w.time}</div>
                            </td>
                            <td>{actionLabel(e.action)}</td>
                            <td class="nowrap">{boardLabel(e)}</td>
                            <td class="fw">
                                <div class="mono small" title={e.artifact ?? ''}>{artifactName(e)}</div>
                                {#if firmwareLabel(e)}
                                    <div class="muted small mono">{firmwareLabel(e)}</div>
                                {/if}
                            </td>
                            <td class="nowrap">
                                {#if !e.ok}
                                    <span class="pill pill-danger">Failed</span>
                                {:else if e.dryRun}
                                    <span class="pill">Dry run</span>
                                {:else}
                                    <span class="pill pill-success">OK</span>
                                    {#if e.verified}
                                        <span class="muted small" title="The board's copy was read back and matched">verified</span>
                                    {/if}
                                {/if}
                            </td>
                            <td class="small">
                                <div>{e.operator ?? '—'}</div>
                                <div class="muted">{e.tool === 'cli' ? 'command line' : 'app'}</div>
                            </td>
                        </tr>
                        {#if expanded === key}
                            <tr class="detail">
                                <td colspan="6">
                                    {#if e.error}
                                        <p class="err mono small">{e.error}</p>
                                    {/if}
                                    <dl>
                                        {#each detailRows(e) as [k, v] (k)}
                                            <dt class="muted small">{k}</dt>
                                            <dd class="mono small">{v}</dd>
                                        {/each}
                                    </dl>
                                </td>
                            </tr>
                        {/if}
                    {/each}
                </tbody>
            </table>
            {#if filtered.length > shown}
                <div class="more">
                    <button type="button" class="btn btn-sm btn-ghost" onclick={() => (shown += PAGE)}>
                        Show {Math.min(PAGE, filtered.length - shown)} more
                    </button>
                    <span class="muted small">{shown} of {filtered.length}</span>
                </div>
            {/if}
        {/if}

        {#if logPath}
            <p class="muted small footer">
                Saved in <span class="mono">{logPath}</span> ·
                <button type="button" class="linkish" onclick={reveal}>Show file</button>
            </p>
        {/if}
    </section>
</div>

<style>
    .toolbar {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-3);
        margin-bottom: var(--space-3);
    }
    .spacer {
        flex: 1;
    }
    .search {
        min-width: 16rem;
        flex: 0 1 22rem;
    }
    .check {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        cursor: pointer;
    }
    .segmented {
        display: inline-flex;
        gap: 2px;
        padding: 2px;
        background: var(--bg);
        border: 1px solid var(--border);
        border-radius: var(--radius-md);
    }
    .seg {
        appearance: none;
        border: none;
        background: transparent;
        color: var(--text-muted);
        font: inherit;
        font-size: var(--text-sm);
        padding: var(--space-1) var(--space-3);
        border-radius: calc(var(--radius-md) - 2px);
        cursor: pointer;
    }
    .seg:hover {
        color: var(--text);
    }
    .seg.active {
        background: var(--accent);
        color: var(--accent-contrast, #fff);
        font-weight: 600;
    }

    .history-table {
        width: 100%;
        border-collapse: collapse;
        font-size: var(--text-sm);
    }
    .history-table th {
        padding: var(--space-2) var(--space-3);
        border-bottom: 1px solid var(--border);
        text-align: left;
        font-weight: 500;
        font-size: var(--text-xs);
        color: var(--text-muted);
    }
    .history-table td {
        padding: var(--space-2) var(--space-3);
        border-bottom: 1px solid var(--border);
        vertical-align: top;
    }
    .hrow {
        cursor: pointer;
    }
    .hrow:hover td,
    .hrow.open td {
        background: var(--bg-soft);
    }
    .hrow.failed td:first-child {
        box-shadow: inset 3px 0 0 var(--danger);
    }
    .nowrap {
        white-space: nowrap;
    }
    .fw {
        max-width: 22rem;
        overflow: hidden;
        text-overflow: ellipsis;
    }
    .pill + .small {
        margin-left: var(--space-2);
    }
    .detail td {
        background: var(--bg);
        padding: var(--space-3) var(--space-4);
    }
    .detail dl {
        display: grid;
        grid-template-columns: max-content 1fr;
        gap: var(--space-1) var(--space-4);
        margin: 0;
    }
    .detail dd {
        margin: 0;
        word-break: break-all;
    }
    .err {
        color: var(--danger);
        margin: 0 0 var(--space-3);
        white-space: pre-wrap;
    }
    .empty {
        padding: var(--space-6) var(--space-2);
        text-align: center;
    }
    .empty h3 {
        margin: 0 0 var(--space-2);
    }
    .more {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        margin-top: var(--space-3);
    }
    .footer {
        margin: var(--space-4) 0 0;
    }
    .linkish {
        appearance: none;
        border: none;
        background: none;
        padding: 0;
        color: var(--accent);
        font: inherit;
        cursor: pointer;
    }
    .linkish:hover {
        text-decoration: underline;
    }
</style>
