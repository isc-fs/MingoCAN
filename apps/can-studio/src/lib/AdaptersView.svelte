<!--
    Adapters view — lists every CAN adapter the host can see, plus
    the always-available virtual loopback. Clicking an entry writes
    the selection into `settings.adapter`; the autosave effect in
    App.svelte flushes to disk.

    No background polling: every fetch is operator-initiated, same
    rule as the VS Code extension's device tree.

    Visual: uses the shared design system in `app.css`. View-local
    CSS is limited to the adapter-row layout + the per-interface
    color tags (one shade per backend kind).
-->
<script lang="ts">
    import { onMount } from 'svelte';

    import {
        discoverAdapters,
        flattenReport,
        isSameAdapter,
        VIRTUAL_ADAPTER,
    } from './cli';
    import { settings } from './settings.svelte';
    import type { AdapterEntry } from './types';

    let adapters = $state<AdapterEntry[]>([]);
    let loading = $state<boolean>(false);
    let error = $state<string | null>(null);

    async function refresh(): Promise<void> {
        loading = true;
        error = null;
        try {
            const report = await discoverAdapters();
            adapters = [...flattenReport(report), VIRTUAL_ADAPTER];
        } catch (err) {
            error = err instanceof Error ? err.message : String(err);
        } finally {
            loading = false;
        }
    }

    function select(entry: AdapterEntry): void {
        settings.adapter.interface = entry.interface;
        settings.adapter.channel = entry.channel;
        settings.adapter.label = entry.label;
    }

    onMount(refresh);
</script>

<div class="view">
    <header>
        <div>
            <h2>Adapters</h2>
            <p class="muted">
                CAN backends detected on this machine. Pick one to scope every
                subsequent action — the choice is saved across restarts.
            </p>
        </div>
        <button
            type="button"
            class="icon-btn"
            onclick={refresh}
            disabled={loading}
            title="Refresh adapter list"
            aria-label="Refresh adapter list"
        >
            {loading ? '…' : '⟳'}
        </button>
    </header>

    {#if error !== null}
        <div class="banner banner-danger">
            <strong>Failed to discover adapters:</strong>
            {error}
        </div>
    {/if}

    {#if adapters.length === 0 && !loading && error === null}
        <div class="empty">
            <p>No adapters detected. Plug a CAN adapter in and click refresh.</p>
            <p class="muted small">
                For a hardware-free smoke test, the virtual loopback is always
                listed below — it should appear here on refresh.
            </p>
        </div>
    {/if}

    <ul class="adapter-list">
        {#each adapters as entry (`${entry.interface}:${entry.channel}`)}
            {@const active =
                settings.adapter.interface !== null &&
                isSameAdapter(
                    {
                        interface: settings.adapter.interface,
                        channel: settings.adapter.channel,
                        label: settings.adapter.label,
                    },
                    entry.interface,
                    entry.channel,
                )}
            <li>
                <button
                    type="button"
                    class="adapter-row"
                    class:active
                    onclick={() => select(entry)}
                >
                    <div class="adapter-row-main">
                        <div class="adapter-row-title">
                            <span class="iface" data-iface={entry.interface}>
                                {entry.interface}
                            </span>
                            <span class="adapter-label">{entry.label}</span>
                            {#if active}
                                <span class="active-pill">active</span>
                            {/if}
                        </div>
                        <div class="adapter-row-detail muted small">
                            <span class="mono">
                                {entry.channel.length > 0
                                    ? entry.channel
                                    : '(no channel)'}
                            </span>
                            {#if entry.detail !== undefined}
                                <span>·</span>
                                <span>{entry.detail}</span>
                            {/if}
                        </div>
                    </div>
                </button>
            </li>
        {/each}
    </ul>
</div>

<style>
    /* Empty state — dashed border to signal "this is where things
       will appear once you have hardware". Distinct from the
       solid-border .card so it doesn't look like populated content. */
    .empty {
        padding: var(--space-6);
        border: 1px dashed var(--border);
        border-radius: var(--radius-lg);
        background: var(--surface);
    }
    .empty p {
        margin: var(--space-1) 0;
    }

    .adapter-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
    }

    /* Adapter row — a clickable card. Inherits surface/border
       tokens from the design system. Active variant uses the
       accent ramp for both border and a faint accent wash. */
    .adapter-row {
        width: 100%;
        appearance: none;
        background: var(--surface);
        border: 1px solid var(--border);
        color: var(--text);
        font: inherit;
        text-align: left;
        padding: var(--space-3) var(--space-4);
        border-radius: var(--radius-lg);
        cursor: pointer;
        transition:
            border-color var(--motion-fast),
            background var(--motion-fast);
    }
    .adapter-row:hover {
        border-color: var(--border-strong);
        background: var(--hover);
    }
    .adapter-row.active {
        border-color: var(--accent);
        background: var(--accent-soft);
    }

    .adapter-row-main {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
    }
    .adapter-row-title {
        display: flex;
        align-items: center;
        gap: var(--space-3);
    }
    .adapter-row-detail {
        display: flex;
        gap: var(--space-2);
        flex-wrap: wrap;
    }

    /* Per-interface color tags. Plain --bg pill, tinted border +
       text by backend so an operator can scan the list and spot
       "the PCAN one" or "the Vector one" without reading the label. */
    .iface {
        font-family: var(--font-mono);
        font-size: var(--text-xs);
        text-transform: uppercase;
        letter-spacing: 0.05em;
        padding: 2px var(--space-2);
        border-radius: var(--radius-sm);
        background: var(--bg);
        border: 1px solid var(--border);
        color: var(--text-muted);
    }
    .iface[data-iface='vector'] {
        color: #ffd166;
        border-color: rgba(255, 209, 102, 0.4);
    }
    .iface[data-iface='pcan'] {
        color: #06d6a0;
        border-color: rgba(6, 214, 160, 0.4);
    }
    .iface[data-iface='slcan'] {
        color: #4cc9f0;
        border-color: rgba(76, 201, 240, 0.4);
    }
    .iface[data-iface='socketcan'] {
        color: #b388ff;
        border-color: rgba(179, 136, 255, 0.4);
    }
    .iface[data-iface='virtual'] {
        color: var(--text-muted);
    }

    .adapter-label {
        font-weight: 500;
    }

    /* Active pill — solid accent fill so the active row's marker
       stands out from the row's accent wash. Pinned to the right
       of the title row via margin-auto. */
    .active-pill {
        margin-left: auto;
        font-size: var(--text-xs);
        background: var(--accent);
        color: #1a1a1a;
        padding: 1px var(--space-2);
        border-radius: var(--radius-pill);
        text-transform: uppercase;
        letter-spacing: 0.04em;
        font-weight: 600;
    }
</style>
