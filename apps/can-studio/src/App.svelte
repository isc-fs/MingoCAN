<!--
    ISC CAN Studio — root layout.

    Loads persistent settings on mount, then routes between views.
    Selected-adapter state lives in `settings.adapter` (see
    settings.svelte.ts); each view imports the store directly rather
    than receiving it via props, so prop drilling is unnecessary.
-->
<script lang="ts">
    import { onMount } from 'svelte';

    import { defaultAppState, type ViewId } from './lib/stores';
    import { loadSettings, registerAutosaveEffect } from './lib/settings.svelte';

    import Sidebar from './lib/Sidebar.svelte';
    import AdaptersView from './lib/AdaptersView.svelte';
    import FlashView from './lib/FlashView.svelte';
    import DiagnosticsView from './lib/DiagnosticsView.svelte';
    import LiveDataView from './lib/LiveDataView.svelte';

    let activeView = $state<ViewId>(defaultAppState().activeView);
    let settingsReady = $state<boolean>(false);

    function selectView(id: ViewId): void {
        activeView = id;
    }

    onMount(async () => {
        await loadSettings();
        settingsReady = true;
    });

    // Register the autosave effect once the component is mounted —
    // safe even before loadSettings completes because the effect
    // no-ops while `loaded` is false (see settings.svelte.ts).
    registerAutosaveEffect();
</script>

<div class="shell">
    <Sidebar {activeView} onSelect={selectView} />

    <main>
        {#if !settingsReady}
            <div class="loading">Loading settings…</div>
        {:else if activeView === 'adapters'}
            <AdaptersView />
        {:else if activeView === 'flash'}
            <FlashView />
        {:else if activeView === 'diagnostics'}
            <DiagnosticsView />
        {:else if activeView === 'liveData'}
            <LiveDataView />
        {/if}
    </main>
</div>

<style>
    .shell {
        display: flex;
        height: 100vh;
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
