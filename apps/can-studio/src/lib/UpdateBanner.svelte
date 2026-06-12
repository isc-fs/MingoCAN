<!--
    Global "update available" banner. Rendered once at the top of the
    app shell (App.svelte). App.svelte runs the best-effort check on
    launch and passes the result in; this component owns the
    download → install → relaunch interaction and dismissal.
-->
<script lang="ts">
    import {
        downloadInstallAndRelaunch,
        releaseNotesUrl,
        type AvailableUpdate,
        type DownloadPhase,
    } from './updater';

    let { update }: { update: AvailableUpdate | null } = $props();

    type InstallState =
        | { kind: 'idle' }
        | { kind: 'downloading'; downloaded: number; total: number | null }
        | { kind: 'error'; message: string };

    let installState = $state<InstallState>({ kind: 'idle' });
    let dismissed = $state<boolean>(false);

    // Show only when an update is offered and the operator hasn't
    // dismissed it this session. A re-check (Settings) that surfaces a
    // different version clears dismissal — reset on version change.
    let lastSeenVersion = $state<string | null>(null);
    $effect(() => {
        if (update !== null && update.version !== lastSeenVersion) {
            lastSeenVersion = update.version;
            dismissed = false;
            installState = { kind: 'idle' };
        }
    });

    const visible = $derived(update !== null && !dismissed);

    const pct = $derived.by(() => {
        if (
            installState.kind !== 'downloading' ||
            installState.total === null ||
            installState.total === 0
        ) {
            return null;
        }
        return Math.min(100, Math.floor((installState.downloaded * 100) / installState.total));
    });

    async function install(): Promise<void> {
        installState = { kind: 'downloading', downloaded: 0, total: null };
        try {
            await downloadInstallAndRelaunch((phase: DownloadPhase) => {
                if (phase.kind === 'started') {
                    installState = { kind: 'downloading', downloaded: 0, total: phase.total };
                } else if (phase.kind === 'progress') {
                    installState = {
                        kind: 'downloading',
                        downloaded: phase.downloaded,
                        total: phase.total,
                    };
                }
            });
            // On success the app relaunches; this line is effectively
            // unreachable. If relaunch is somehow blocked, fall through.
        } catch (err) {
            installState = {
                kind: 'error',
                message: err instanceof Error ? err.message : String(err),
            };
        }
    }
</script>

{#if visible && update !== null}
    <div class="update-banner" role="status">
        <div class="body">
            {#if installState.kind === 'downloading'}
                <span class="headline">Updating to v{update.version}…</span>
                <span class="muted small">
                    {#if pct !== null}
                        Downloading {pct}% — the app will restart when it's done.
                    {:else}
                        Downloading — the app will restart when it's done.
                    {/if}
                </span>
            {:else if installState.kind === 'error'}
                <span class="headline">Update to v{update.version} failed</span>
                <span class="muted small">{installState.message}</span>
            {:else}
                <span class="headline">
                    Update available — <strong>v{update.version}</strong>
                    <span class="muted">(you have v{update.currentVersion})</span>
                </span>
                <span class="muted small">
                    <a
                        href={releaseNotesUrl(update.version)}
                        target="_blank"
                        rel="noreferrer">What's new</a
                    >
                </span>
            {/if}
        </div>

        <div class="actions">
            {#if installState.kind === 'downloading'}
                <span class="spinner" aria-label="Updating"></span>
            {:else if installState.kind === 'error'}
                <button type="button" class="btn btn-sm" onclick={install}>Retry</button>
                <button type="button" class="btn btn-sm" onclick={() => (dismissed = true)}>
                    Dismiss
                </button>
            {:else}
                <button type="button" class="btn btn-sm btn-primary" onclick={install}>
                    Install &amp; restart
                </button>
                <button type="button" class="btn btn-sm" onclick={() => (dismissed = true)}>
                    Later
                </button>
            {/if}
        </div>
    </div>
{/if}

<style>
    /* Slim app-wide notice. Mirrors the design-system banner (plain
       surface + a coloured left rail) but laid out as a single row
       with the message on the left and actions on the right. */
    .update-banner {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-4);
        padding: var(--space-2) var(--space-4);
        background: var(--surface);
        border-bottom: 1px solid var(--border);
        border-left: 3px solid var(--accent);
    }
    .body {
        display: flex;
        flex-direction: column;
        gap: 2px;
        min-width: 0;
    }
    .headline {
        font-size: var(--text-sm);
    }
    .actions {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex-shrink: 0;
    }
    .small {
        font-size: var(--text-xs);
    }
    .spinner {
        width: 14px;
        height: 14px;
        border: 2px solid var(--border);
        border-top-color: var(--accent);
        border-radius: 50%;
        animation: spin 1s linear infinite;
        display: inline-block;
    }
    @keyframes spin {
        to {
            transform: rotate(360deg);
        }
    }
</style>
