<!--
    Target-role picker for a CAN node-id.

    The node-id selects which board the reboot-to-bootloader magic is
    aimed at — and the ECU and AMS use DIFFERENT payloads (v2.6.10), so a
    mismatched node-id silently fails to enter the bootloader. Fronting
    the raw 0–0xF number with the role table makes the common targets one
    click and hard to fat-finger; `Custom` reveals the raw field for
    anything off-scheme. Shared by the Flash tab and Settings so the two
    stay in lockstep. `value` is the 4-bit node-id, two-way bound.
-->
<script lang="ts">
    import { ROLES } from './provision';

    interface Props {
        /** 4-bit node-id, or `null` when unset (shown as an empty
         *  Custom field). */
        value: number | null;
    }
    let { value = $bindable() }: Props = $props();

    const roleForNode = $derived(ROLES.find((r) => r.nodeId === value) ?? null);
    let customNode = $state<boolean>(false);
    const showCustom = $derived(customNode || roleForNode === null);

    function pickRole(nodeId: number): void {
        value = nodeId;
        customNode = false;
    }
    function enableCustom(): void {
        customNode = true;
    }
    function roleHex(nodeId: number): string {
        return `0x${nodeId.toString(16).toUpperCase()}`;
    }
</script>

<div class="role-row">
    <div class="segmented" role="group" aria-label="Target role">
        {#each ROLES as r (r.name)}
            <button
                type="button"
                class="seg"
                class:active={!showCustom && roleForNode?.name === r.name}
                aria-pressed={!showCustom && roleForNode?.name === r.name}
                onclick={() => pickRole(r.nodeId)}
            >
                {r.name.toUpperCase()}<span class="seg-id">{roleHex(r.nodeId)}</span
                >
            </button>
        {/each}
        <button
            type="button"
            class="seg"
            class:active={showCustom}
            aria-pressed={showCustom}
            onclick={enableCustom}
        >
            Custom
        </button>
    </div>
    {#if showCustom}
        <input
            class="input mono node-custom"
            type="number"
            min="0"
            max="15"
            aria-label="Custom node id (0–0xF)"
            bind:value
        />
    {/if}
</div>

<style>
    .role-row {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-2);
    }
    /* Segmented track — mirrors the Release/Debug control's look so the
       whole app speaks one segmented-control language. */
    .segmented {
        display: inline-flex;
        flex-wrap: wrap;
        gap: 2px;
        padding: 2px;
        background: var(--bg);
        border: 1px solid var(--border);
        border-radius: var(--radius-md);
    }
    .seg {
        appearance: none;
        border: none;
        background: transparent;
        color: var(--text-muted);
        font: inherit;
        font-size: var(--text-sm);
        padding: var(--space-1) var(--space-4);
        border-radius: calc(var(--radius-md) - 2px);
        cursor: pointer;
        transition:
            background var(--motion-base),
            color var(--motion-base);
    }
    .seg:hover {
        color: var(--text);
    }
    .seg.active {
        background: var(--accent);
        color: var(--accent-contrast, #fff);
        font-weight: 600;
    }
    /* Dim hex after each role name — present, but the role reads first. */
    .seg-id {
        margin-left: var(--space-1);
        font-family: var(--font-mono);
        font-size: var(--text-sm);
        opacity: 0.65;
    }
    .seg.active .seg-id {
        opacity: 0.8;
    }
    .node-custom {
        width: 6rem;
    }
</style>
