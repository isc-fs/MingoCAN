<!--
    Placeholder for views that aren't implemented yet (Tier 0b, 0c,
    0d). Surfaces a clear "coming in PR X" message rather than an
    empty pane.
-->
<script lang="ts">
    interface Props {
        title: string;
        tier: string;
        summary: string;
    }

    const { title, tier, summary }: Props = $props();
</script>

<div class="view">
    <header>
        <h2>{title}</h2>
        <span class="badge">{tier}</span>
    </header>

    <div class="placeholder">
        <p>{summary}</p>
        <p class="muted small">
            The scaffold is in place; the real implementation lands in the
            <strong>{tier}</strong> PR. Track progress in
            <a href="https://github.com/isc-fs/can-flasher" target="_blank" rel="noreferrer">
                isc-fs/can-flasher
            </a>.
        </p>
    </div>
</div>

<style>
    .view {
        padding: 28px 32px;
        display: flex;
        flex-direction: column;
        gap: 16px;
    }

    header {
        display: flex;
        align-items: center;
        gap: 12px;
    }

    h2 {
        margin: 0;
        font-size: 1.3rem;
    }

    .badge {
        font-size: 0.7rem;
        text-transform: uppercase;
        letter-spacing: 0.06em;
        padding: 2px 8px;
        border-radius: 4px;
        background: var(--bg);
        border: 1px solid var(--border);
        color: var(--text-muted);
    }

    .placeholder {
        padding: 24px;
        border: 1px dashed var(--border);
        border-radius: 8px;
        background: var(--surface);
    }

    .placeholder p {
        margin: 4px 0;
    }

    .muted {
        color: var(--text-muted);
    }

    .small {
        font-size: 0.85rem;
    }

    a {
        color: var(--accent);
    }
</style>
