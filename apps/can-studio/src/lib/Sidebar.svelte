<!--
    Left-rail navigation. One row per view; rows for views that
    aren't implemented yet show a "soon" badge so the user knows
    what's coming. Active row is highlighted by the accent colour.
-->
<script lang="ts">
    import type { ViewId } from './stores';
    import { VIEWS } from './stores';

    interface Props {
        activeView: ViewId;
        onSelect: (id: ViewId) => void;
    }

    const { activeView, onSelect }: Props = $props();
</script>

<aside class="sidebar">
    <div class="brand">
        <img src="/icon.png" alt="" />
        <div>
            <h1>ISC CAN Studio</h1>
            <p class="muted">Tier 2 · DBC + Signals live</p>
        </div>
    </div>

    <nav>
        {#each VIEWS as view (view.id)}
            <button
                type="button"
                class="nav-item"
                class:active={activeView === view.id}
                onclick={() => onSelect(view.id)}
            >
                <span class="label">{view.label}</span>
                {#if view.status === 'soon'}
                    <span class="badge">soon</span>
                {/if}
            </button>
        {/each}
    </nav>
</aside>

<style>
    .sidebar {
        width: 220px;
        flex: 0 0 220px;
        padding: 16px 12px;
        background: var(--surface);
        border-right: 1px solid var(--border);
        display: flex;
        flex-direction: column;
        gap: 16px;
    }

    .brand {
        display: flex;
        align-items: center;
        gap: 10px;
        padding: 4px 6px 12px;
        border-bottom: 1px solid var(--border);
    }

    .brand img {
        width: 36px;
        height: 36px;
        border-radius: 8px;
    }

    .brand h1 {
        margin: 0;
        font-size: 0.95rem;
        font-weight: 600;
    }

    .brand .muted {
        margin: 2px 0 0;
        font-size: 0.7rem;
        color: var(--text-muted);
    }

    nav {
        display: flex;
        flex-direction: column;
        gap: 2px;
    }

    .nav-item {
        appearance: none;
        background: transparent;
        border: none;
        color: var(--text);
        text-align: left;
        padding: 8px 10px;
        border-radius: 6px;
        cursor: pointer;
        font: inherit;
        display: flex;
        align-items: center;
        justify-content: space-between;
    }

    .nav-item:hover {
        background: var(--hover);
    }

    .nav-item.active {
        background: var(--hover);
        color: var(--accent);
        font-weight: 600;
    }

    .nav-item.active::before {
        content: '';
        width: 3px;
        height: 18px;
        background: var(--accent);
        border-radius: 2px;
        margin-right: 8px;
        margin-left: -10px;
    }

    .nav-item .label {
        flex: 1;
    }

    .badge {
        font-size: 0.65rem;
        text-transform: uppercase;
        letter-spacing: 0.05em;
        background: var(--bg);
        border: 1px solid var(--border);
        color: var(--text-muted);
        padding: 1px 6px;
        border-radius: 4px;
    }
</style>
