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
    import PlaceholderView from './lib/PlaceholderView.svelte';

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
            <PlaceholderView
                title="Flash"
                tier="Tier 0b"
                summary="One-button Build + Flash. Runs the configured build command, resolves the firmware artifact, spawns the can-flasher flash pipeline against the selected adapter, and surfaces per-phase progress."
            />
        {:else if activeView === 'diagnostics'}
            <PlaceholderView
                title="Diagnostics"
                tier="Tier 0c"
                summary="DTC viewer + clear, session health snapshot. One-shot commands that wrap the diagnose subcommand."
            />
        {:else if activeView === 'liveData'}
            <PlaceholderView
                title="Live data"
                tier="Tier 0d"
                summary="Streaming snapshot view — frames/sec, session age, NACK counters, sliding-window chart. Port of the VS Code extension's live-data webview."
            />
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
