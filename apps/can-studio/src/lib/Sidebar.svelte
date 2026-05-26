<!--
    Left-rail navigation. One row per view; rows for views that
    aren't implemented yet show a "soon" badge so the user knows
    what's coming. Active row is highlighted by the accent colour.
-->
<script lang="ts">
    import { onMount } from 'svelte';
    import { getVersion } from '@tauri-apps/api/app';

    import type { ViewId } from './stores';
    import { VIEWS } from './stores';

    interface Props {
        activeView: ViewId;
        onSelect: (id: ViewId) => void;
    }

    const { activeView, onSelect }: Props = $props();

    // `getVersion()` reads tauri.conf.json's version field — the
    // same field `release.yml`'s verify-version gate compares the
    // git tag against, so the sidebar label always matches the
    // tag operators downloaded the bundle from.
    let appVersion = $state<string>('');
    onMount(async () => {
        try {
            appVersion = await getVersion();
        } catch {
            // Standalone preview (no Tauri runtime) — leave blank.
        }
    });
</script>

<aside class="sidebar">
    <div class="brand">
        <img src="/icon.png" alt="" />
        <div class="brand-text">
            <h1>ISC MingoCAN</h1>
            {#if appVersion.length > 0}
                <p class="version">v{appVersion}</p>
            {/if}
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
                    <span class="pill soon-pill">soon</span>
                {/if}
            </button>
        {/each}
    </nav>
</aside>

<style>
    /* Fixed-width left rail. Padding deliberately uses raw px so the
       sidebar's chrome stays compact regardless of which spacing
       tokens the views tweak. Everything else (gaps, type, colors)
       leans on design-system tokens so it stays in sync. */
    .sidebar {
        width: 220px;
        flex: 0 0 220px;
        padding: var(--space-4) var(--space-3);
        background: var(--surface);
        border-right: 1px solid var(--border);
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
    }

    /* Brand block — icon + product name + version, separated from
       the nav with a hairline. The version uses --text-xs because
       it's secondary metadata; the .pill utility would be too loud. */
    .brand {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-1) var(--space-2) var(--space-3);
        border-bottom: 1px solid var(--border);
    }

    .brand img {
        width: 36px;
        height: 36px;
        border-radius: var(--radius-lg);
    }

    .brand-text h1 {
        margin: 0;
        font-size: var(--text-base);
        font-weight: 600;
    }

    .version {
        margin: 2px 0 0;
        font-size: var(--text-xs);
        color: var(--text-muted);
    }

    nav {
        display: flex;
        flex-direction: column;
        gap: 2px;
    }

    /* Nav row — bespoke button shape (not .btn) so we can use a
       left-edge accent indicator + tighter padding than the
       generic button. Active row swaps to accent text + a 3px
       accent rail to the left of the label. */
    .nav-item {
        appearance: none;
        background: transparent;
        border: none;
        color: var(--text);
        text-align: left;
        padding: var(--space-2) var(--space-3);
        border-radius: var(--radius-md);
        cursor: pointer;
        font: inherit;
        font-size: var(--text-base);
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-2);
        transition:
            background var(--motion-fast),
            color var(--motion-fast);
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
        margin-right: var(--space-2);
        margin-left: calc(var(--space-3) * -1);
    }

    .nav-item .label {
        flex: 1;
    }

    /* "soon" tag — leans on the .pill utility but knocks the font
       size down a notch and drops the mono-font + letter-spacing
       used by status pills (this is a label, not a status read). */
    .soon-pill {
        font-family: var(--font-sans);
        font-size: var(--text-xs);
        text-transform: uppercase;
        letter-spacing: 0.05em;
        padding: 1px var(--space-2);
        color: var(--text-muted);
    }
</style>
