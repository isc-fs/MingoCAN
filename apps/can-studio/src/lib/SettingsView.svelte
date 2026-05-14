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
    import { open as openDialog } from '@tauri-apps/plugin-dialog';

    import { settings, currentDbcKey } from './settings.svelte';
    import type { ViewId } from './stores';
    import {
        getDbcStatus,
        loadDbc,
        unloadDbc,
        type DbcSummary,
    } from './dbc';

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

    // DBC state ----------------------------------------------------
    let dbcSummary = $state<DbcSummary | null>(null);
    let dbcError = $state<string | null>(null);

    // The settings.dbc.paths map is the source of truth on disk.
    // The view shows the path for the current adapter and lets the
    // operator Browse… or Clear. Loading happens via the backend.
    const currentKey = $derived(currentDbcKey());
    const currentPath = $derived.by(() => {
        if (currentKey === null) return null;
        return settings.dbc.paths[currentKey] ?? null;
    });

    async function pickDbc(): Promise<void> {
        if (currentKey === null) {
            dbcError = 'Pick an adapter first.';
            return;
        }
        const picked = await openDialog({
            title: 'Pick a DBC file',
            multiple: false,
            directory: false,
            filters: [
                { name: 'DBC', extensions: ['dbc'] },
                { name: 'All files', extensions: ['*'] },
            ],
        });
        if (typeof picked !== 'string' || picked.length === 0) return;

        dbcError = null;
        try {
            const summary = await loadDbc(picked);
            settings.dbc.paths[currentKey] = picked;
            dbcSummary = summary;
        } catch (err) {
            dbcError = err instanceof Error ? err.message : String(err);
        }
    }

    async function clearDbc(): Promise<void> {
        if (currentKey === null) return;
        dbcError = null;
        try {
            await unloadDbc();
        } catch (err) {
            dbcError = err instanceof Error ? err.message : String(err);
        }
        delete settings.dbc.paths[currentKey];
        dbcSummary = null;
    }

    // Reflect status events fired by the central auto-load effect
    // (in App.svelte / settings.svelte.ts) so this view shows the
    // current loaded summary without each-view re-doing the load.
    $effect(() => {
        let cancelled = false;
        (async () => {
            try {
                const status = await getDbcStatus();
                if (!cancelled) dbcSummary = status;
            } catch (err) {
                if (!cancelled) {
                    dbcError = err instanceof Error ? err.message : String(err);
                }
            }
        })();
        // Re-poll when the adapter changes (the auto-load effect
        // in App.svelte triggers a load/unload, then `dbc_status`
        // returns the new state).
        // eslint-disable-next-line @typescript-eslint/no-unused-expressions
        currentKey;
        return () => {
            cancelled = true;
        };
    });

    onMount(async () => {
        try {
            canFlasherVersion = await invoke<string>('can_flasher_version');
        } catch {
            canFlasherVersion = 'unknown';
        }
        // Cold-start: if a DBC was already loaded by the auto-load
        // effect on app startup, reflect it here.
        try {
            dbcSummary = await getDbcStatus();
        } catch {
            // no-op
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

    <!-- DBC files (per-adapter) -->
    <section class="card">
        <header>
            <h3>DBC files (per-adapter)</h3>
        </header>
        <p class="muted small">
            Pick a <code>.dbc</code> for the current adapter; the bus
            monitor decodes every matching frame into named physical
            signals (visible in the <em>Signals</em> view). Each
            (interface, channel) pair carries its own DBC, so a
            powertrain bus and a body bus can coexist without you
            re-picking on every switch.
        </p>

        {#if !adapterSet}
            <p class="muted small">
                No adapter selected. Open the <em>Adapters</em> view to
                pick one first.
            </p>
        {:else}
            <div class="dbc-row">
                <div class="dbc-info">
                    <div class="dbc-key mono">{currentKey}</div>
                    {#if dbcSummary !== null}
                        <div class="dbc-loaded">
                            <code class="mono dbc-path">{dbcSummary.path}</code>
                            <span class="muted small">
                                {dbcSummary.messageCount} messages ·
                                {dbcSummary.signalCount} signals
                            </span>
                        </div>
                    {:else if currentPath !== null}
                        <div class="dbc-pending">
                            <code class="mono dbc-path">{currentPath}</code>
                            <span class="muted small">(reloading…)</span>
                        </div>
                    {:else}
                        <span class="muted small">No DBC associated.</span>
                    {/if}
                </div>
                <div class="dbc-actions">
                    <button type="button" onclick={pickDbc}>Browse…</button>
                    <button
                        type="button"
                        disabled={currentPath === null}
                        onclick={clearDbc}
                    >
                        Clear
                    </button>
                </div>
            </div>
            {#if dbcError !== null}
                <div class="error small">{dbcError}</div>
            {/if}
        {/if}
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

    .dbc-row {
        display: flex;
        gap: 12px;
        align-items: flex-start;
        margin-top: 6px;
    }
    .dbc-info {
        flex: 1;
        min-width: 0;
        display: flex;
        flex-direction: column;
        gap: 4px;
    }
    .dbc-key {
        font-size: 0.78rem;
        color: var(--text-muted);
        text-transform: uppercase;
        letter-spacing: 0.05em;
    }
    .dbc-loaded, .dbc-pending {
        display: flex;
        flex-direction: column;
        gap: 2px;
        min-width: 0;
    }
    .dbc-path {
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-size: 0.82rem;
    }
    .dbc-actions {
        display: flex;
        gap: 6px;
    }
    .error.small {
        margin-top: 8px;
        padding: 6px 10px;
        border: 1px solid var(--error);
        background: rgba(255, 115, 115, 0.08);
        color: var(--error);
        border-radius: 4px;
        font-size: 0.82rem;
    }
    .mono { font-family: var(--font-mono); }
</style>
