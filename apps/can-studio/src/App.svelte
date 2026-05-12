<!--
    ISC CAN Studio — scaffold root component.
    Tier 0+ replaces this with a real toolbar / device tree / bus
    monitor layout. For now it just proves the Rust ↔ JS bridge is
    alive via two `#[tauri::command]` calls.
-->
<script lang="ts">
    import { invoke } from '@tauri-apps/api/core';
    import { onMount } from 'svelte';

    let studioVersion = $state<string>('loading…');
    let cliVersion = $state<string>('loading…');
    let bridgeError = $state<string | null>(null);

    onMount(async () => {
        try {
            studioVersion = await invoke<string>('studio_version');
            cliVersion = await invoke<string>('can_flasher_version');
        } catch (err) {
            bridgeError = String(err);
        }
    });
</script>

<main>
    <header>
        <img src="/icon.png" alt="" class="logo" />
        <div class="titles">
            <h1>ISC CAN Studio</h1>
            <p class="subtitle">{studioVersion}</p>
        </div>
    </header>

    <section class="status">
        {#if bridgeError !== null}
            <div class="error">
                <strong>Bridge error:</strong>
                {bridgeError}
            </div>
        {:else}
            <p>
                Bundled <code>can-flasher</code> crate: <strong>v{cliVersion}</strong>
            </p>
            <p class="muted">
                Scaffold only. Tier 0 (flash · DTC · health) lands in the next PR,
                Tier 1 (bus monitor) after that.
            </p>
        {/if}
    </section>

    <footer>
        <p class="muted">
            <a href="https://github.com/isc-fs/can-flasher" target="_blank" rel="noreferrer">
                isc-fs/can-flasher
            </a>
            — desktop companion to the can-flasher CLI.
        </p>
    </footer>
</main>

<style>
    main {
        max-width: 800px;
        margin: 0 auto;
        padding: 32px 24px;
        display: flex;
        flex-direction: column;
        gap: 24px;
    }

    header {
        display: flex;
        align-items: center;
        gap: 16px;
    }

    .logo {
        width: 64px;
        height: 64px;
        border-radius: 12px;
    }

    .titles h1 {
        margin: 0;
        font-size: 1.6rem;
        font-weight: 600;
    }

    .titles .subtitle {
        margin: 4px 0 0 0;
        color: var(--text-muted);
        font-size: 0.9rem;
    }

    .status {
        padding: 16px 20px;
        border: 1px solid var(--border);
        border-radius: 8px;
        background: var(--surface);
    }

    .status p {
        margin: 6px 0;
    }

    .status .muted {
        color: var(--text-muted);
        font-size: 0.9rem;
    }

    .status .error {
        color: var(--error);
    }

    .status code {
        font-family: var(--font-mono);
        background: var(--code-bg);
        padding: 1px 6px;
        border-radius: 3px;
    }

    footer {
        margin-top: auto;
        padding-top: 16px;
        border-top: 1px solid var(--border);
    }

    footer .muted {
        color: var(--text-muted);
        font-size: 0.85rem;
        margin: 0;
    }

    footer a {
        color: inherit;
        text-decoration: underline;
        text-decoration-color: var(--accent);
        text-underline-offset: 3px;
    }
</style>
