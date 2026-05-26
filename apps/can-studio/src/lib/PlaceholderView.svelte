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
        <div>
            <h2>{title}</h2>
        </div>
        <span class="pill">{tier}</span>
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
    /* Dashed-border card distinct from the solid .card so it reads
       as "not real content yet". Lives in this file because no
       other view needs this treatment. */
    .placeholder {
        padding: var(--space-6);
        border: 1px dashed var(--border-strong);
        border-radius: var(--radius-lg);
        background: var(--surface);
    }
    .placeholder p {
        margin: var(--space-1) 0;
    }

    a {
        color: var(--accent);
    }
</style>
