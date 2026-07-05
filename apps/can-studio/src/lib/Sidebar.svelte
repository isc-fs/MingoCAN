<!--
    Left-rail navigation. One row per view; rows for views that
    aren't implemented yet show a "soon" badge so the user knows
    what's coming. Active row is highlighted by the accent colour.
-->
<script lang="ts">
    import { onMount } from 'svelte';
    import { getVersion } from '@tauri-apps/api/app';

    import type { ViewId, ViewMeta } from './stores';
    import { VIEWS, NAV_SECTIONS } from './stores';

    interface Props {
        activeView: ViewId;
        onSelect: (id: ViewId) => void;
    }

    const { activeView, onSelect }: Props = $props();

    // Pinned rows render outside the sections: Adapters at the top
    // (start here), Settings at the bottom.
    const topViews = VIEWS.filter((v) => v.id === 'adapters');
    const bottomViews = VIEWS.filter((v) => v.id === 'settings');

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

    {#snippet navRow(view: ViewMeta)}
        <button
            type="button"
            class="nav-item"
            class:active={activeView === view.id}
            title={view.description}
            onclick={() => onSelect(view.id)}
        >
            <span class="nav-text">
                <span class="label">{view.label}</span>
                <span class="desc">{view.description}</span>
            </span>
            {#if view.status === 'soon'}
                <span class="pill soon-pill">soon</span>
            {/if}
        </button>
    {/snippet}

    <nav>
        {#each topViews as view (view.id)}
            {@render navRow(view)}
        {/each}

        {#each NAV_SECTIONS as sec (sec.section)}
            <div class="nav-section">{sec.label}</div>
            {#each VIEWS.filter((v) => v.section === sec.section) as view (view.id)}
                {@render navRow(view)}
            {/each}
        {/each}

        <div class="nav-spacer"></div>

        {#each bottomViews as view (view.id)}
            {@render navRow(view)}
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
        flex: 1;
        min-height: 0;
    }

    /* Section header — quiet uppercase mono label with a trailing
       hairline, separating Program from Observe. */
    .nav-section {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        margin-top: var(--space-3);
        padding: 0 var(--space-3) var(--space-1);
        font-family: var(--font-mono);
        font-size: 10px;
        letter-spacing: 0.16em;
        text-transform: uppercase;
        color: var(--text-muted);
    }
    .nav-section::after {
        content: '';
        flex: 1;
        height: 1px;
        background: var(--border);
    }

    /* Pushes the pinned Settings row to the bottom of the rail. */
    .nav-spacer {
        flex: 1;
        min-height: var(--space-4);
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

    .nav-text {
        flex: 1;
        display: flex;
        flex-direction: column;
        gap: 1px;
        min-width: 0;
    }
    .nav-item .label {
        line-height: 1.25;
    }
    /* Plain-language subline — de-jargons "Burn bootloader" / "Pit diag"
       without needing a hover. Muted so the label still reads first;
       stays muted on the active row (only the label goes accent). */
    .nav-item .desc {
        font-size: 11px;
        font-weight: 400;
        color: var(--text-muted);
        line-height: 1.25;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
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
