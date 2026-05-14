<!--
    Settings view — canonical place for cross-cutting configuration
    that's also editable in the workflow views.

    All fields here read/write the same `settings.*` store as the
    Flash + Live-data views, so changes here flow to those views
    automatically and vice versa (via the autosave $effect in
    App.svelte). The view exists because some fields don't belong
    to any single workflow:

      - Bitrate / nodeId / frame timeout (applied to every CAN op)
      - Live-data defaults (rate + window) for new sessions
      - About info (version, settings-file path)

    Adapter selection itself stays in the Adapters view — this view
    just shows the currently-selected one with a link back.
-->
<script lang="ts">
    import { onMount } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';

    import { settings } from './settings.svelte';
    import type { ViewId } from './stores';

    interface Props {
        navigateTo: (id: ViewId) => void;
    }

    const { navigateTo }: Props = $props();

    const adapterSet = $derived(
        settings.adapter.interface !== null &&
            (settings.adapter.interface === 'virtual' ||
                settings.adapter.channel.length > 0),
    );

    let canFlasherVersion = $state<string>('…');

    onMount(async () => {
        try {
            canFlasherVersion = await invoke<string>('can_flasher_version');
        } catch {
            canFlasherVersion = 'unknown';
        }
    });
</script>

<div class="view">
    <header>
        <h2>Settings</h2>
        <p class="muted">
            Defaults applied to every CAN operation. Changes save
            automatically to <code>settings.json</code> in the app-config
            directory.
        </p>
    </header>

    <!-- Selected adapter — read-only, link to Adapters view -->
    <section class="card">
        <header>
            <h3>Selected adapter</h3>
            <button type="button" onclick={() => navigateTo('adapters')}>
                Change…
            </button>
        </header>
        {#if adapterSet}
            <div class="adapter">
                <span class="iface" data-iface={settings.adapter.interface}>
                    {settings.adapter.interface}
                </span>
                <div class="adapter-detail">
                    <div class="label">{settings.adapter.label || '(no label)'}</div>
                    <code class="channel">
                        {settings.adapter.channel.length > 0
                            ? settings.adapter.channel
                            : '(no channel)'}
                    </code>
                </div>
            </div>
        {:else}
            <p class="muted small">
                No adapter selected. Open the <em>Adapters</em> view to pick one.
            </p>
        {/if}
    </section>

    <!-- Bus parameters -->
    <section class="card">
        <header>
            <h3>Bus parameters</h3>
        </header>
        <p class="muted small">
            Applied to every flash, discover, diagnose, and live-data call.
        </p>
        <div class="grid-three">
            <div>
                <label for="bitrate">Bitrate (bps)</label>
                <input
                    id="bitrate"
                    type="number"
                    min="10000"
                    max="1000000"
                    step="1000"
                    bind:value={settings.adapter.bitrate}
                />
            </div>
            <div>
                <label for="nodeId">Default node ID (0–0xF)</label>
                <input
                    id="nodeId"
                    type="number"
                    min="0"
                    max="15"
                    bind:value={settings.adapter.nodeId}
                />
            </div>
            <div>
                <label for="timeout">Frame timeout (ms)</label>
                <input
                    id="timeout"
                    type="number"
                    min="50"
                    max="60000"
                    bind:value={settings.adapter.timeoutMs}
                />
            </div>
        </div>
    </section>

    <!-- Live-data defaults -->
    <section class="card">
        <header>
            <h3>Live-data defaults</h3>
        </header>
        <p class="muted small">
            Initial values for the Live-data view. Editable per-session there
            too; this is the persisted default.
        </p>
        <div class="grid-two">
            <div>
                <label for="rateHz">Rate (Hz)</label>
                <input
                    id="rateHz"
                    type="number"
                    min="1"
                    max="50"
                    bind:value={settings.liveData.rateHz}
                />
            </div>
            <div>
                <label for="windowSeconds">Window (s)</label>
                <input
                    id="windowSeconds"
                    type="number"
                    min="5"
                    max="600"
                    bind:value={settings.liveData.windowSeconds}
                />
            </div>
        </div>
    </section>

    <!-- About -->
    <section class="card">
        <header>
            <h3>About</h3>
        </header>
        <dl class="about">
            <dt>App</dt>
            <dd>ISC CAN Studio</dd>

            <dt>Bundled <code>can-flasher</code></dt>
            <dd>v{canFlasherVersion}</dd>

            <dt>Repository</dt>
            <dd>
                <a href="https://github.com/isc-fs/can-flasher" target="_blank" rel="noreferrer">
                    isc-fs/can-flasher
                </a>
            </dd>

            <dt>Settings file</dt>
            <dd class="settings-path">
                <span class="muted small">
                    Stored under the OS app-config directory:
                    <code>com.iscracingteam.can-studio/settings.json</code>
                </span>
            </dd>
        </dl>
    </section>
</div>

<style>
    .view {
        display: flex;
        flex-direction: column;
        gap: 16px;
        padding: 24px 28px;
        overflow: auto;
    }
    h2 { margin: 0; font-size: 1.3rem; }
    .muted { color: var(--text-muted); }
    header .muted { margin: 4px 0 0; font-size: 0.9rem; }
    .small { font-size: 0.85rem; }
    code { font-family: var(--font-mono); }

    .card {
        padding: 16px 18px;
        border: 1px solid var(--border);
        border-radius: 8px;
        background: var(--surface);
    }
    .card header {
        display: flex;
        align-items: center;
        justify-content: space-between;
        margin-bottom: 8px;
    }
    .card h3 { margin: 0; font-size: 1rem; }
    .card > p { margin: 0 0 12px; }

    button {
        appearance: none;
        background: var(--bg);
        color: var(--text);
        border: 1px solid var(--border);
        font: inherit;
        padding: 6px 12px;
        border-radius: 5px;
        cursor: pointer;
        font-size: 0.85rem;
    }
    button:hover { border-color: var(--accent); color: var(--accent); }

    .adapter {
        display: flex;
        align-items: center;
        gap: 12px;
    }
    .iface {
        font-family: var(--font-mono);
        font-size: 0.75rem;
        text-transform: uppercase;
        letter-spacing: 0.05em;
        padding: 2px 8px;
        border-radius: 3px;
        background: var(--bg);
        border: 1px solid var(--border);
        color: var(--text-muted);
    }
    .iface[data-iface='vector'] { color: #ffd166; border-color: rgba(255, 209, 102, 0.4); }
    .iface[data-iface='pcan']   { color: #06d6a0; border-color: rgba(6, 214, 160, 0.4); }
    .iface[data-iface='slcan']  { color: #4cc9f0; border-color: rgba(76, 201, 240, 0.4); }
    .iface[data-iface='socketcan'] { color: #b388ff; border-color: rgba(179, 136, 255, 0.4); }
    .iface[data-iface='virtual'] { color: var(--text-muted); }

    .adapter-detail .label {
        font-weight: 500;
    }
    .adapter-detail .channel {
        color: var(--text-muted);
        font-size: 0.85rem;
    }

    .grid-three {
        display: grid;
        grid-template-columns: repeat(3, 1fr);
        gap: 12px;
    }
    .grid-two {
        display: grid;
        grid-template-columns: repeat(2, 1fr);
        gap: 12px;
    }
    label {
        display: block;
        font-size: 0.85rem;
        color: var(--text-muted);
        margin-bottom: 4px;
    }
    input[type="number"] {
        width: 100%;
        background: var(--bg);
        color: var(--text);
        border: 1px solid var(--border);
        border-radius: 4px;
        padding: 6px 8px;
        font: inherit;
        font-family: var(--font-mono);
        font-size: 0.85rem;
        box-sizing: border-box;
    }
    input:focus { outline: none; border-color: var(--accent); }

    .about {
        display: grid;
        grid-template-columns: max-content 1fr;
        gap: 6px 16px;
        margin: 0;
    }
    .about dt {
        color: var(--text-muted);
        font-size: 0.85rem;
    }
    .about dd {
        margin: 0;
        font-size: 0.9rem;
    }
    .about a { color: var(--accent); }
    .settings-path { display: flex; align-items: center; }
</style>
