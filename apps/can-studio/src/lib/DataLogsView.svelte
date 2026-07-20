<script lang="ts">
    /*
        Data logs — list the microSD car-data logs on a node and pull them
        to a local folder over CAN (#506, firmware spec IFS08-CE-AMS#406).

        Read-only by design: v1 has no delete. Transfers run at classic-CAN
        speeds (tens of KB/s), so a multi-MB log takes minutes — every pull
        shows a live progress bar and the CRC verdict when it lands.

        `mtimeMonotonic` is boot-relative (no RTC on the AMS), so it is
        labelled "uptime" and never formatted as a date.
    */
    import { onDestroy } from 'svelte';
    import { open as openDialog } from '@tauri-apps/plugin-dialog';
    import type { UnlistenFn } from '@tauri-apps/api/event';

    import { settings } from './settings.svelte';
    import {
        logsList,
        logsPull,
        logsCancel,
        CANCELLED_MSG,
        onPullProgress,
        formatBytes,
        type LogFile,
        type LogsRequest,
    } from './logs';
    import type { ViewId } from './stores';

    interface Props {
        navigateTo: (id: ViewId) => void;
    }
    const { navigateTo }: Props = $props();

    const adapterReady = $derived(
        settings.adapter.interface !== null &&
            (settings.adapter.interface === 'virtual' ||
                settings.adapter.channel.length > 0),
    );

    let files = $state<LogFile[] | null>(null);
    let listing = $state<boolean>(false);
    let error = $state<string | null>(null);

    let destDir = $state<string | null>(null);
    let pullingIndex = $state<number | null>(null);
    let received = $state<number>(0);
    let total = $state<number>(0);
    let lastResult = $state<string | null>(null);
    let cancelling = $state<boolean>(false);

    let unlisten: UnlistenFn | null = null;
    onPullProgress((p) => {
        if (pullingIndex === null) return;
        received = p.received;
        total = p.total;
    }).then((fn) => (unlisten = fn));
    onDestroy(() => unlisten?.());

    function buildRequest(): LogsRequest | null {
        if (!adapterReady || settings.adapter.interface === null) return null;
        return {
            interface: settings.adapter.interface,
            channel:
                settings.adapter.channel.length > 0
                    ? settings.adapter.channel
                    : null,
            bitrate: settings.adapter.bitrate,
            nodeId: settings.adapter.nodeId,
            timeoutMs: settings.adapter.timeoutMs,
        };
    }

    async function refresh(): Promise<void> {
        const request = buildRequest();
        if (request === null) return;
        listing = true;
        error = null;
        lastResult = null;
        try {
            files = await logsList(request);
        } catch (err) {
            error = err instanceof Error ? err.message : String(err);
            files = null;
        } finally {
            listing = false;
        }
    }

    async function chooseFolder(): Promise<void> {
        const picked = await openDialog({ directory: true, multiple: false });
        if (typeof picked === 'string') destDir = picked;
    }

    async function cancel(): Promise<void> {
        cancelling = true;
        try {
            await logsCancel();
        } catch {
            /* the pull may have finished in the meantime — harmless */
        }
    }

    async function pull(file: LogFile): Promise<void> {
        const request = buildRequest();
        if (request === null || destDir === null) return;
        pullingIndex = file.index;
        received = 0;
        total = file.size;
        error = null;
        lastResult = null;
        cancelling = false;
        try {
            const res = await logsPull(request, file.index, destDir);
            lastResult = `Saved ${res.path} (${formatBytes(res.bytes)})${
                res.crcVerified ? ' — CRC verified' : ''
            }`;
        } catch (err) {
            const msg = err instanceof Error ? err.message : String(err);
            // A cancel is an operator choice, not a failure — say so plainly
            // and don't paint the view red.
            if (msg.includes(CANCELLED_MSG)) {
                lastResult = `Cancelled — ${file.name} was not saved.`;
            } else {
                error = msg;
            }
        } finally {
            pullingIndex = null;
            cancelling = false;
        }
    }

    const pct = $derived(
        total > 0 ? Math.min(100, Math.round((received / total) * 100)) : 0,
    );
</script>

<div class="view">
    <header class="view-header">
        <h2>Data logs</h2>
        <p class="muted">
            Pull the car-data logs off a node's microSD card over CAN — no
            card removal. Read-only.
        </p>
    </header>

    {#if !adapterReady}
        <div class="card placeholder-card">
            <h3>No adapter selected</h3>
            <p class="muted">
                Pick a CAN adapter and channel first — the log transfer uses
                the same diagnostic link as flashing.
            </p>
            <button type="button" class="btn btn-primary" onclick={() => navigateTo('adapters')}>
                Go to Adapters
            </button>
        </div>
    {:else}
        <div class="toolbar card card-tight">
            <button
                type="button"
                class="btn btn-primary"
                disabled={listing || pullingIndex !== null}
                onclick={refresh}
            >
                {listing ? 'Listing…' : 'List logs'}
            </button>
            <button
                type="button"
                class="btn"
                disabled={pullingIndex !== null}
                onclick={chooseFolder}
            >
                {destDir === null ? 'Choose folder…' : 'Change folder'}
            </button>
            {#if destDir !== null}
                <span class="stat">
                    <span>save to</span><strong class="mono">{destDir}</strong>
                </span>
            {/if}
        </div>

        {#if error !== null}
            <div class="banner banner-danger"><strong>Error:</strong> {error}</div>
        {/if}
        {#if lastResult !== null}
            <div class="banner banner-success">{lastResult}</div>
        {/if}

        {#if pullingIndex !== null}
            <section class="card">
                <div class="card-header">
                    <h3>Downloading</h3>
                    <button
                        type="button"
                        class="btn btn-danger"
                        disabled={cancelling}
                        onclick={cancel}
                    >
                        {cancelling ? 'Cancelling…' : 'Cancel'}
                    </button>
                    <span class="muted small mono">
                        {formatBytes(received)}{total > 0
                            ? ` / ${formatBytes(total)}`
                            : ''}
                    </span>
                </div>
                <div class="meter">
                    <div class="meter-fill" style="width: {pct}%"></div>
                </div>
                <p class="muted small">
                    {pct}% — classic CAN runs at tens of KB/s, so a multi-MB log
                    takes minutes. The transfer is verified against the node's
                    CRC when it finishes.
                </p>
            </section>
        {/if}

        {#if files === null}
            <div class="card placeholder-card">
                <h3>No listing yet</h3>
                <p class="muted">
                    Hit <strong>List logs</strong> to enumerate the files on the
                    card. Requires the node's log-transfer service (AMS
                    firmware) to be running.
                </p>
            </div>
        {:else if files.length === 0}
            <div class="card placeholder-card">
                <h3>No log files</h3>
                <p class="muted">The card has no sealed logs to download.</p>
            </div>
        {:else}
            <section class="card">
                <div class="card-header">
                    <h3>{files.length} log file{files.length === 1 ? '' : 's'}</h3>
                    <span class="muted small">
                        uptime is boot-relative (no RTC) — ordering only
                    </span>
                </div>
                <table class="logs-table">
                    <thead>
                        <tr>
                            <th>Name</th>
                            <th class="num">Size</th>
                            <th class="num">Uptime</th>
                            <th></th>
                        </tr>
                    </thead>
                    <tbody>
                        {#each files as f (f.index)}
                            <tr>
                                <td class="mono">{f.name}</td>
                                <td class="num mono">{formatBytes(f.size)}</td>
                                <td class="num mono">{f.mtimeMonotonic}</td>
                                <td class="num">
                                    <button
                                        type="button"
                                        class="btn"
                                        disabled={pullingIndex !== null || destDir === null}
                                        title={destDir === null
                                            ? 'Choose a destination folder first'
                                            : `Download ${f.name}`}
                                        onclick={() => pull(f)}
                                    >
                                        Download
                                    </button>
                                </td>
                            </tr>
                        {/each}
                    </tbody>
                </table>
            </section>
        {/if}
    {/if}
</div>

<style>
    .toolbar {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
    }
    .stat {
        display: inline-flex;
        gap: var(--space-2);
        align-items: center;
        font-size: var(--text-sm);
        color: var(--text-muted);
    }
    .stat strong {
        color: var(--text);
        font-weight: 600;
    }
    .placeholder-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        align-items: flex-start;
    }
    .placeholder-card h3,
    .placeholder-card p {
        margin: 0;
    }
    .logs-table {
        width: 100%;
        border-collapse: collapse;
        font-size: var(--text-sm);
    }
    .logs-table th {
        text-align: left;
        font-size: var(--text-xs);
        text-transform: uppercase;
        letter-spacing: 0.05em;
        color: var(--text-muted);
        padding: var(--space-2) var(--space-3);
        border-bottom: 1px solid var(--border);
    }
    .logs-table td {
        padding: var(--space-2) var(--space-3);
        border-bottom: 1px solid var(--border);
    }
    .logs-table .num {
        text-align: right;
    }
    /* Left-origin progress bar, reused from the pedal meters. */
    .meter {
        position: relative;
        height: 10px;
        border-radius: var(--radius-sm, 4px);
        background: var(--bg);
        border: 1px solid var(--border);
        overflow: hidden;
    }
    .meter-fill {
        height: 100%;
        background: var(--accent);
        transition: width 120ms linear;
    }
</style>
