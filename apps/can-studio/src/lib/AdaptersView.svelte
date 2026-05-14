<!--
    Adapters view — lists every CAN adapter the host can see, plus
    the always-available virtual loopback. Clicking an entry writes
    the selection into `settings.adapter`; the autosave effect in
    App.svelte flushes to disk.

    No background polling: every fetch is operator-initiated, same
    rule as the VS Code extension's device tree.
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
            class="refresh"
            onclick={refresh}
            disabled={loading}
            title="Refresh adapter list"
        >
            {loading ? '…' : '⟳'}
        </button>
    </header>

    {#if error !== null}
        <div class="error">
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

    <ul class="list">
        {#each adapters as entry (`${entry.interface}:${entry.channel}`)}
            {@const active = settings.adapter.interface !== null && isSameAdapter(
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
                    class="row"
                    class:active
                    onclick={() => select(entry)}
                >
                    <div class="row-main">
                        <div class="row-title">
                            <span class="iface" data-iface={entry.interface}>
                                {entry.interface}
                            </span>
                            <span class="label">{entry.label}</span>
                            {#if active}
                                <span class="active-pill">active</span>
                            {/if}
                        </div>
                        <div class="row-detail muted small">
                            <span class="channel">
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
    .view {
        display: flex;
        flex-direction: column;
        gap: 16px;
        padding: 28px 32px;
        overflow: auto;
    }

    header {
        display: flex;
        align-items: flex-start;
        justify-content: space-between;
        gap: 16px;
    }

    h2 {
        margin: 0;
        font-size: 1.3rem;
    }

    .muted {
        color: var(--text-muted);
    }

    header .muted {
        margin: 4px 0 0;
        font-size: 0.9rem;
        max-width: 60ch;
    }

    .refresh {
        appearance: none;
        background: var(--surface);
        border: 1px solid var(--border);
        color: var(--text);
        width: 36px;
        height: 36px;
        border-radius: 6px;
        font: inherit;
        font-size: 1rem;
        cursor: pointer;
    }

    .refresh:hover:not(:disabled) {
        border-color: var(--accent);
        color: var(--accent);
    }

    .refresh:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }

    .error {
        padding: 12px 16px;
        border-radius: 6px;
        background: rgba(255, 115, 115, 0.1);
        border: 1px solid var(--error);
        color: var(--error);
    }

    .empty {
        padding: 24px;
        border: 1px dashed var(--border);
        border-radius: 8px;
        background: var(--surface);
    }

    .empty p {
        margin: 4px 0;
    }

    .small {
        font-size: 0.85rem;
    }

    .list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: 6px;
    }

    .row {
        width: 100%;
        appearance: none;
        background: var(--surface);
        border: 1px solid var(--border);
        color: var(--text);
        font: inherit;
        text-align: left;
        padding: 12px 16px;
        border-radius: 8px;
        cursor: pointer;
        transition: border-color 80ms ease, background 80ms ease;
    }

    .row:hover {
        border-color: var(--text-muted);
    }

    .row.active {
        border-color: var(--accent);
        background: rgba(242, 178, 51, 0.06);
    }

    .row-main {
        display: flex;
        flex-direction: column;
        gap: 4px;
    }

    .row-title {
        display: flex;
        align-items: center;
        gap: 10px;
    }

    .iface {
        font-family: var(--font-mono);
        font-size: 0.75rem;
        text-transform: uppercase;
        letter-spacing: 0.05em;
        padding: 1px 6px;
        border-radius: 3px;
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

    .label {
        font-weight: 500;
    }

    .active-pill {
        margin-left: auto;
        font-size: 0.7rem;
        background: var(--accent);
        color: #1a1a1a;
        padding: 1px 8px;
        border-radius: 10px;
        text-transform: uppercase;
        letter-spacing: 0.04em;
        font-weight: 600;
    }

    .row-detail {
        display: flex;
        gap: 6px;
        flex-wrap: wrap;
    }

    .channel {
        font-family: var(--font-mono);
    }
</style>
