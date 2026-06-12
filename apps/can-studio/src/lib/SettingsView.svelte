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
    import { getVersion } from '@tauri-apps/api/app';
    import { open as openDialog } from '@tauri-apps/plugin-dialog';

    import { checkForUpdate, downloadInstallAndRelaunch } from './updater';

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

    // App version + manual update check (the launch check + banner
    // live in App.svelte; this is the "check now" path from Settings).
    let appVersion = $state<string>('…');
    let updateStatus = $state<
        | { kind: 'idle' }
        | { kind: 'checking' }
        | { kind: 'uptodate' }
        | { kind: 'available'; version: string }
        | { kind: 'installing' }
        | { kind: 'error'; message: string }
    >({ kind: 'idle' });

    async function runUpdateCheck(): Promise<void> {
        updateStatus = { kind: 'checking' };
        const available = await checkForUpdate(); // never throws
        updateStatus =
            available === null
                ? { kind: 'uptodate' }
                : { kind: 'available', version: available.version };
    }

    async function installUpdate(): Promise<void> {
        updateStatus = { kind: 'installing' };
        try {
            await downloadInstallAndRelaunch();
            // App relaunches on success; nothing to do here.
        } catch (err) {
            updateStatus = {
                kind: 'error',
                message: err instanceof Error ? err.message : String(err),
            };
        }
    }

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
        try {
            appVersion = await getVersion();
        } catch {
            appVersion = 'unknown';
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
        <div>
            <h2>Settings</h2>
            <p class="muted">
                Defaults applied to every CAN operation. Changes save
                automatically to <code>settings.json</code> in the app-config
                directory.
            </p>
        </div>
    </header>

    <!-- Selected adapter — read-only, link to Adapters view -->
    <section class="card">
        <div class="card-header">
            <h3>Selected adapter</h3>
            <button
                type="button"
                class="btn btn-sm"
                onclick={() => navigateTo('adapters')}
            >
                Change…
            </button>
        </div>
        {#if adapterSet}
            <div class="adapter">
                <span class="iface" data-iface={settings.adapter.interface}>
                    {settings.adapter.interface}
                </span>
                <div class="adapter-detail">
                    <div class="adapter-label">
                        {settings.adapter.label || '(no label)'}
                    </div>
                    <code class="adapter-channel">
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
        <div class="card-header">
            <h3>Bus parameters</h3>
        </div>
        <p class="muted small section-hint">
            Applied to every flash, discover, diagnose, and live-data call.
        </p>
        <div class="grid-three">
            <div class="field">
                <label for="bitrate">Bitrate (bps)</label>
                <input
                    id="bitrate"
                    class="input mono"
                    type="number"
                    min="10000"
                    max="1000000"
                    step="1000"
                    bind:value={settings.adapter.bitrate}
                />
            </div>
            <div class="field">
                <label for="nodeId">Default node ID (0–0xF)</label>
                <input
                    id="nodeId"
                    class="input mono"
                    type="number"
                    min="0"
                    max="15"
                    bind:value={settings.adapter.nodeId}
                />
            </div>
            <div class="field">
                <label for="timeout">Frame timeout (ms)</label>
                <input
                    id="timeout"
                    class="input mono"
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
        <div class="card-header">
            <h3>Live-data defaults</h3>
        </div>
        <p class="muted small section-hint">
            Initial values for the Live-data view. Editable per-session there
            too; this is the persisted default.
        </p>
        <div class="grid-two">
            <div class="field">
                <label for="rateHz">Rate (Hz)</label>
                <input
                    id="rateHz"
                    class="input mono"
                    type="number"
                    min="1"
                    max="50"
                    bind:value={settings.liveData.rateHz}
                />
            </div>
            <div class="field">
                <label for="windowSeconds">Window (s)</label>
                <input
                    id="windowSeconds"
                    class="input mono"
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
        <div class="card-header">
            <h3>DBC files (per-adapter)</h3>
        </div>
        <p class="muted small section-hint">
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
                    <button type="button" class="btn btn-sm" onclick={pickDbc}>
                        Browse…
                    </button>
                    <button
                        type="button"
                        class="btn btn-sm"
                        disabled={currentPath === null}
                        onclick={clearDbc}
                    >
                        Clear
                    </button>
                </div>
            </div>
            {#if dbcError !== null}
                <div class="banner banner-danger">{dbcError}</div>
            {/if}
        {/if}
    </section>

    <!-- About -->
    <section class="card">
        <div class="card-header">
            <h3>About</h3>
        </div>
        <dl class="about">
            <dt>App</dt>
            <dd>ISC MingoCAN v{appVersion}</dd>

            <dt>Bundled <code>can-flasher</code></dt>
            <dd>v{canFlasherVersion}</dd>

            <dt>Updates</dt>
            <dd>
                {#if updateStatus.kind === 'available'}
                    <span>v{updateStatus.version} available.</span>
                    <button type="button" class="btn btn-sm btn-primary" onclick={installUpdate}>
                        Install &amp; restart
                    </button>
                {:else if updateStatus.kind === 'installing'}
                    <span class="muted">Installing… the app will restart.</span>
                {:else if updateStatus.kind === 'error'}
                    <span class="muted small">Update failed: {updateStatus.message}</span>
                    <button type="button" class="btn btn-sm" onclick={runUpdateCheck}>Retry</button>
                {:else}
                    <button
                        type="button"
                        class="btn btn-sm"
                        onclick={runUpdateCheck}
                        disabled={updateStatus.kind === 'checking'}
                    >
                        {updateStatus.kind === 'checking' ? 'Checking…' : 'Check for updates'}
                    </button>
                    {#if updateStatus.kind === 'uptodate'}
                        <span class="muted small">You're on the latest version.</span>
                    {/if}
                {/if}
            </dd>

            <dt>Repository</dt>
            <dd>
                <a href="https://github.com/isc-fs/can-flasher" target="_blank" rel="noreferrer">
                    isc-fs/can-flasher
                </a>
            </dd>

            <dt>Settings file</dt>
            <dd>
                <span class="muted small">
                    Stored under the OS app-config directory:
                    <code>com.iscracingteam.can-studio/settings.json</code>
                </span>
            </dd>
        </dl>
    </section>
</div>

<style>
    /* Shared chrome (.view, .card, .card-header, .banner-*, .btn,
       .field, .input, .muted, .small, .mono) comes from app.css.
       Local styles cover: section-hint margin, the per-interface
       .iface tag (duplicated from AdaptersView since the design
       system treats backend IDs as functional color coding), the
       form grids, the About <dl>, and the DBC-row layout. */

    code {
        font-family: var(--font-mono);
    }

    .section-hint {
        margin: 0 0 var(--space-3);
    }

    /* Selected-adapter row — same iface tag + label/channel pair
       as AdaptersView. Color-coded backend IDs help operators
       scan; keeping them functional (not decorative) is why these
       styles live here instead of being collapsed to a .pill. */
    .adapter {
        display: flex;
        align-items: center;
        gap: var(--space-3);
    }
    .iface {
        font-family: var(--font-mono);
        font-size: var(--text-xs);
        text-transform: uppercase;
        letter-spacing: 0.05em;
        padding: 2px var(--space-2);
        border-radius: var(--radius-sm);
        background: transparent;
        border: 1px solid var(--border);
        color: var(--text-muted);
    }
    .iface[data-iface='vector'] {
        color: #ffd166;
        border-color: rgba(255, 209, 102, 0.4);
    }
    .iface[data-iface='pcan'] {
        color: var(--success);
        border-color: var(--success);
    }
    .iface[data-iface='slcan'] {
        color: var(--info);
        border-color: var(--info);
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
    .adapter-channel {
        color: var(--text-muted);
        font-size: var(--text-sm);
    }

    /* Form grids — three-up for bus params, two-up for live-data.
       Collapse to single column on narrow widths. */
    .grid-three {
        display: grid;
        grid-template-columns: repeat(3, 1fr);
        gap: var(--space-3);
    }
    .grid-two {
        display: grid;
        grid-template-columns: repeat(2, 1fr);
        gap: var(--space-3);
    }
    @media (max-width: 720px) {
        .grid-three,
        .grid-two {
            grid-template-columns: 1fr;
        }
    }

    /* DBC-row — info on the left (key + path + summary), action
       buttons on the right. Stacks if the path is long. */
    .dbc-row {
        display: flex;
        gap: var(--space-3);
        align-items: flex-start;
    }
    .dbc-info {
        flex: 1;
        min-width: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
    }
    .dbc-key {
        font-size: var(--text-xs);
        color: var(--text-muted);
        text-transform: uppercase;
        letter-spacing: 0.05em;
    }
    .dbc-loaded,
    .dbc-pending {
        display: flex;
        flex-direction: column;
        gap: 2px;
        min-width: 0;
    }
    .dbc-path {
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        font-size: var(--text-sm);
    }
    .dbc-actions {
        display: flex;
        gap: var(--space-2);
    }

    /* About — definition-list table. Compact two-column grid with
       muted dt + body-text dd. */
    .about {
        display: grid;
        grid-template-columns: max-content 1fr;
        gap: var(--space-2) var(--space-4);
        margin: 0;
    }
    .about dt {
        color: var(--text-muted);
        font-size: var(--text-sm);
    }
    .about dd {
        margin: 0;
        font-size: var(--text-base);
    }
    .about a {
        color: var(--accent);
    }
</style>
