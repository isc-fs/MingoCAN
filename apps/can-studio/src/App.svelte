<!--
    ISC CAN Studio — root layout.

    Two-column shell: left sidebar with view selector, right main
    area that swaps between views based on `activeView`. Selected
    adapter state lifted here so Tier 0b's Flash view can read it
    without a global store yet.
-->
<script lang="ts">
    import { defaultAppState, type ViewId } from './lib/stores';
    import type { AdapterEntry } from './lib/types';

    import Sidebar from './lib/Sidebar.svelte';
    import AdaptersView from './lib/AdaptersView.svelte';
    import FlashView from './lib/FlashView.svelte';
    import DiagnosticsView from './lib/DiagnosticsView.svelte';
    import LiveDataView from './lib/LiveDataView.svelte';

    const initial = defaultAppState();
    let activeView = $state<ViewId>(initial.activeView);
    let selectedAdapter = $state<AdapterEntry | null>(initial.selectedAdapter);

    function selectView(id: ViewId): void {
        activeView = id;
    }

    function selectAdapter(entry: AdapterEntry): void {
        selectedAdapter = entry;
    }
</script>

<div class="shell">
    <Sidebar {activeView} onSelect={selectView} />

    <main>
        {#if activeView === 'adapters'}
            <AdaptersView {selectedAdapter} onSelect={selectAdapter} />
        {:else if activeView === 'flash'}
            <FlashView {selectedAdapter} />
        {:else if activeView === 'diagnostics'}
            <DiagnosticsView {selectedAdapter} />
        {:else if activeView === 'liveData'}
            <LiveDataView {selectedAdapter} />
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
</style>
