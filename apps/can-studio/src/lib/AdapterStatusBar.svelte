<!--
    Persistent active-adapter strip. Sits at the top of every workflow
    view so the interface / channel / bitrate every action is scoped to
    is always visible — on a shared bench with several adapters, this is
    the always-on confirmation you're flashing the right channel. Only
    rendered when an adapter is selected (the empty state is each view's
    actionable "no adapter" banner); `Change` jumps back to Adapters.
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
</script>

<div class="statusbar">
    <span class="live" aria-hidden="true"></span>
    <span class="iface" data-iface={settings.adapter.interface}>
        {settings.adapter.interface}
    </span>
    <span class="label">{settings.adapter.label || '(no label)'}</span>
    <code class="channel">
        {settings.adapter.channel.length > 0
            ? settings.adapter.channel
            : '(no channel)'}
    </code>
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
    /* Steady green pip — "an adapter is selected". Not a live-link
       indicator (the app doesn't hold the bus open); it just anchors
       the strip and reads as "ready". */
    .live {
        width: 8px;
        height: 8px;
        border-radius: 50%;
        background: var(--success);
        box-shadow: 0 0 0 3px var(--success-soft);
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
