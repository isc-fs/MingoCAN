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
    import type { ViewId } from './stores';
    import { settings } from './settings.svelte';

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
</script>

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
    <button type="button" class="change" onclick={() => navigateTo('adapters')}>
        Change
    </button>
</div>

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
