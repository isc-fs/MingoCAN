<!--
    Persistent selected-adapter strip. Sits at the top of every workflow
    view so the interface / channel / bitrate every action is scoped to
    is always visible — on a shared bench with several adapters, this is
    the always-on confirmation of which adapter you'll flash through.

    It shows the *selected* adapter (from settings), NOT a live link — the
    app doesn't hold the bus open between actions, so there's no honest
    "connected" state to show here. Only rendered when an adapter is
    selected (the empty state is each view's actionable "no adapter"
    banner); `Change` jumps back to Adapters.
-->
<script lang="ts">
    import { onMount } from 'svelte';

    import type { ViewId } from './stores';
    import { settings } from './settings.svelte';
    import { discoverAdapters, flattenReport, isSameAdapter } from './cli';

    interface Props {
        navigateTo: (id: ViewId) => void;
    }
    const { navigateTo }: Props = $props();

    const kbit = $derived(
        `${(settings.adapter.bitrate / 1000).toLocaleString()} kbit/s`,
    );

    // Some backends (PCAN) use the channel string as the label, so
    // showing both would just repeat "PCAN_USBBUS1 PCAN_USBBUS1". Only
    // show the label when it adds something over the channel.
    const showLabel = $derived(
        settings.adapter.label.length > 0 &&
            settings.adapter.label !== settings.adapter.channel,
    );

    // Presence: the saved selection persists across restarts + unplugs,
    // so on entering a view we re-detect and confirm the selected adapter
    // is actually on this machine — otherwise the strip would claim an
    // adapter that isn't there. Checked on mount (a navigation moment, so
    // never mid-flash — probing opens channels, which would collide with
    // an in-flight operation holding the same one). `virtual` needs no
    // hardware. Optimistic until the first check resolves.
    let presence = $state<'checking' | 'present' | 'absent'>('checking');

    async function checkPresence(): Promise<void> {
        if (settings.adapter.interface === 'virtual') {
            presence = 'present';
            return;
        }
        try {
            const entries = flattenReport(await discoverAdapters());
            const here = entries.some((e) =>
                isSameAdapter(
                    e,
                    settings.adapter.interface!,
                    settings.adapter.channel,
                ),
            );
            presence = here ? 'present' : 'absent';
        } catch {
            presence = 'absent';
        }
    }

    onMount(checkPresence);
</script>

{#if presence === 'absent'}
    <div class="statusbar absent">
        <span class="warn">No adapter detected</span>
        <span class="detail">Connect a CAN adapter, then select it</span>
        <button
            type="button"
            class="change"
            onclick={() => navigateTo('adapters')}
        >
            Select adapter →
        </button>
    </div>
{:else}
    <div class="statusbar">
        <span class="tag">Adapter</span>
        <span class="iface" data-iface={settings.adapter.interface}>
            {settings.adapter.interface}
        </span>
        {#if showLabel}
            <span class="label">{settings.adapter.label}</span>
        {/if}
        {#if settings.adapter.channel.length > 0}
            <code class="channel">{settings.adapter.channel}</code>
        {/if}
        <span class="sep" aria-hidden="true">·</span>
        <span class="bitrate">{kbit}</span>
        <button
            type="button"
            class="change"
            onclick={() => navigateTo('adapters')}
        >
            Change
        </button>
    </div>
{/if}

<style>
    .statusbar {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-2) var(--space-5);
        background: var(--bg-soft);
        border-bottom: 1px solid var(--border);
        flex: none;
    }
    /* Absent state — the selected adapter isn't detected. Warning rail
       so it reads as "attention needed", not a normal chrome strip. */
    .statusbar.absent {
        border-left: 3px solid var(--warning);
    }
    .absent .warn {
        font-weight: 600;
        font-size: var(--text-sm);
        color: var(--warning);
    }
    .absent .detail {
        font-family: var(--font-mono);
        font-size: var(--text-sm);
        color: var(--text-muted);
    }

    /* Muted "Adapter" caption — anchors the strip and frames the row as
       a *selection*, not a live connection (no status LED that would
       imply the bus is up). */
    .tag {
        font-size: var(--text-xs);
        letter-spacing: 0.04em;
        text-transform: uppercase;
        color: var(--text-muted);
        flex: none;
    }
    .label {
        font-weight: 500;
        font-size: var(--text-sm);
        color: var(--text);
    }
    .channel {
        font-family: var(--font-mono);
        font-size: var(--text-sm);
        color: var(--text-secondary);
    }
    .sep {
        color: var(--border-strong);
    }
    .bitrate {
        font-family: var(--font-mono);
        font-size: var(--text-sm);
        color: var(--text-secondary);
    }
    .change {
        margin-left: auto;
        appearance: none;
        font: inherit;
        font-size: var(--text-sm);
        color: var(--text-secondary);
        background: transparent;
        border: 1px solid var(--border-strong);
        border-radius: var(--radius-md);
        padding: var(--space-1) var(--space-3);
        cursor: pointer;
        transition:
            background var(--motion-fast),
            color var(--motion-fast);
    }
    .change:hover {
        background: var(--hover);
        color: var(--text);
    }
    .change:focus-visible {
        outline: 2px solid var(--accent);
        outline-offset: 1px;
    }

    /* Per-interface color tag — mirrors AdaptersView so the same
       backend reads the same color everywhere. */
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
        flex: none;
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
</style>
