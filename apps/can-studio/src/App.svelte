<!--
    ISC MingoCAN — root layout.

    Loads persistent settings on mount, then routes between views.
    Selected-adapter state lives in `settings.adapter` (see
    settings.svelte.ts); each view imports the store directly rather
    than receiving it via props, so prop drilling is unnecessary.
-->
<script lang="ts">
    import { onMount } from 'svelte';

    import { defaultAppState, type ViewId } from './lib/stores';
    import {
        loadSettings,
        registerAutosaveEffect,
        registerDbcAutoloadEffect,
    } from './lib/settings.svelte';
    import { loadDbc, unloadDbc } from './lib/dbc';
    import { checkForUpdate, type AvailableUpdate } from './lib/updater';

    import Sidebar from './lib/Sidebar.svelte';
    import UpdateBanner from './lib/UpdateBanner.svelte';
    import AdaptersView from './lib/AdaptersView.svelte';
    import FlashView from './lib/FlashView.svelte';
    import SwdFlashView from './lib/SwdFlashView.svelte';
    import DiagnosticsView from './lib/DiagnosticsView.svelte';
    import BusMonitorView from './lib/BusMonitorView.svelte';
    import SignalsView from './lib/SignalsView.svelte';
    import PitDiagView from './lib/PitDiagView.svelte';
    import SettingsView from './lib/SettingsView.svelte';

    let activeView = $state<ViewId>(defaultAppState().activeView);
    let settingsReady = $state<boolean>(false);
    let update = $state<AvailableUpdate | null>(null);

    function selectView(id: ViewId): void {
        activeView = id;
    }

    onMount(async () => {
        await loadSettings();
        settingsReady = true;
        // Best-effort, non-blocking: surface the update banner if a
        // newer release is available. Silent on offline / no manifest.
        checkForUpdate().then((u) => {
            update = u;
        });
    });

    // Register the autosave effect once the component is mounted —
    // safe even before loadSettings completes because the effect
    // no-ops while `loaded` is false (see settings.svelte.ts).
    registerAutosaveEffect();
    registerDbcAutoloadEffect({ load: loadDbc, unload: unloadDbc });
</script>

<div class="app">
    <UpdateBanner {update} />

    <div class="shell">
        <Sidebar {activeView} onSelect={selectView} />

        <main>
        {#if !settingsReady}
            <div class="loading">Loading settings…</div>
        {:else if activeView === 'adapters'}
            <AdaptersView />
        {:else if activeView === 'flash'}
            <FlashView />
        {:else if activeView === 'swdFlash'}
            <SwdFlashView />
        {:else if activeView === 'diagnostics'}
            <DiagnosticsView />
        {:else if activeView === 'busMonitor'}
            <BusMonitorView />
        {:else if activeView === 'signals'}
            <SignalsView />
        {:else if activeView === 'pitDiag'}
            <PitDiagView />
        {:else if activeView === 'settings'}
            <SettingsView navigateTo={selectView} />
        {/if}
        </main>
    </div>
</div>

<style>
    .app {
        display: flex;
        flex-direction: column;
        height: 100vh;
        overflow: hidden;
    }

    .shell {
        display: flex;
        flex: 1;
        min-height: 0;
        overflow: hidden;
    }

    main {
        flex: 1;
        display: flex;
        flex-direction: column;
        overflow: hidden;
    }

    .loading {
        padding: 40px;
        color: var(--text-muted);
        font-size: 0.9rem;
    }
</style>
