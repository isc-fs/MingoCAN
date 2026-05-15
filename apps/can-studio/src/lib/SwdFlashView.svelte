<!--
    Burn bootloader view — programs an STM32 over SWD with the
    custom CAN bootloader so the chip can subsequently be flashed
    over CAN from the Flash tab.

    The team's ECU is a fixed target (STM32H733 at 0x08000000), so
    the UI doesn't expose chip / base-address fields — those would
    just be footguns. Operators who need to flash a different
    family can drop down to the `can-flasher swd-flash` CLI, which
    keeps the full surface area.

    Three actions:
      - **Burn bootloader** — erase + write + verify + reset
      - **Fetch from releases** — pulls CAN_BL.elf from the BL repo
      - **Erase chip**  — destructive full-flash wipe (commissioning)
-->
<script lang="ts">
    import { onMount, onDestroy } from 'svelte';
    import { open as openDialog } from '@tauri-apps/plugin-dialog';
    import type { UnlistenFn } from '@tauri-apps/api/event';

    import {
        defaultSwdFlashArgs,
        listSwdProbes,
        onSwdFlashEvent,
        swdErase,
        swdFetchBootloader,
        swdFlash,
        type ProbeInfo,
        type SwdFlashArgs,
        type SwdFlashEvent,
        type SwdOp,
    } from './swd';

    type FlashState =
        | { kind: 'idle' }
        | { kind: 'running'; startedAt: number }
        | { kind: 'ok'; durationMs: number }
        | { kind: 'error'; message: string };

    type EraseState =
        | { kind: 'idle' }
        | { kind: 'confirming' }
        | { kind: 'running' }
        | { kind: 'ok' }
        | { kind: 'error'; message: string };

    let args = $state<SwdFlashArgs>(defaultSwdFlashArgs());
    let probes = $state<ProbeInfo[]>([]);
    let probesLoading = $state<boolean>(false);
    let probesError = $state<string | null>(null);
    let flashState = $state<FlashState>({ kind: 'idle' });
    let eraseState = $state<EraseState>({ kind: 'idle' });

    // ---- Live progress, fed by `swd-flash:event` Tauri events ----
    // `currentOp` is the operation probe-rs is in the middle of
    // (erase / program / verify / fill). `total` is the byte count
    // for that operation when probe-rs tells us; `done` accumulates
    // the per-event `delta`. Percent is derived; UI binds to it.
    let currentOp = $state<SwdOp | null>(null);
    let opDone = $state<number>(0);
    let opTotal = $state<number | null>(null);

    const pct = $derived(
        opTotal !== null && opTotal > 0
            ? Math.min(100, Math.floor((opDone * 100) / opTotal))
            : null,
    );

    let unlistenProgress: UnlistenFn | null = null;

    function handleProgress(event: SwdFlashEvent): void {
        switch (event.kind) {
            case 'started':
                currentOp = event.op;
                opDone = 0;
                opTotal = event.total;
                break;
            case 'progress':
                if (event.op === currentOp) {
                    opDone += event.delta;
                }
                break;
            case 'finished':
            case 'failed':
                // Snap to 100% on a successful finish so the bar
                // looks complete even if probe-rs's last delta
                // didn't quite total to the announced byte count.
                if (event.kind === 'finished' && event.op === currentOp && opTotal !== null) {
                    opDone = opTotal;
                }
                break;
        }
    }

    // ---- BL release fetch state ----
    // `releaseTag` is what the operator typed (blank ⇒ latest). The
    // last-fetched tag + cache-status are surfaced as muted text
    // under the button so a re-run looks idempotent.
    let releaseTag = $state<string>('');
    let fetchState = $state<
        | { kind: 'idle' }
        | { kind: 'fetching' }
        | { kind: 'done'; tag: string; downloaded: boolean }
        | { kind: 'error'; message: string }
    >({ kind: 'idle' });

    async function refreshProbes(): Promise<void> {
        probesLoading = true;
        probesError = null;
        try {
            probes = await listSwdProbes();
            // If the operator hasn't pinned a serial, reset to "auto"
            // when only one probe is attached — matches the CLI's
            // single-probe ergonomics.
            if (probes.length <= 1) {
                args.probeSerial = null;
            }
        } catch (err) {
            probesError = err instanceof Error ? err.message : String(err);
        } finally {
            probesLoading = false;
        }
    }

    async function browseForArtifact(): Promise<void> {
        const picked = await openDialog({
            title: 'Pick a firmware artifact to flash via SWD',
            multiple: false,
            directory: false,
            defaultPath:
                args.artifactPath.trim().length > 0 ? args.artifactPath : undefined,
            filters: [
                { name: 'Firmware', extensions: ['elf', 'hex', 'bin'] },
                { name: 'All files', extensions: ['*'] },
            ],
        });
        if (typeof picked === 'string' && picked.length > 0) {
            args.artifactPath = picked;
        }
    }

    async function fetchFromReleases(): Promise<void> {
        const tag = releaseTag.trim();
        fetchState = { kind: 'fetching' };
        try {
            const result = await swdFetchBootloader(tag.length > 0 ? tag : null);
            args.artifactPath = result.path;
            fetchState = {
                kind: 'done',
                tag: result.tag,
                downloaded: result.downloaded,
            };
        } catch (err) {
            fetchState = {
                kind: 'error',
                message: err instanceof Error ? err.message : String(err),
            };
        }
    }

    async function runFlash(): Promise<void> {
        if (args.artifactPath.trim().length === 0) {
            flashState = { kind: 'error', message: 'Pick a firmware artifact first.' };
            return;
        }
        // Reset the bar — leftover state from a prior run would
        // otherwise show 100% while the new flash is still erasing.
        currentOp = null;
        opDone = 0;
        opTotal = null;
        const startedAt = performance.now();
        flashState = { kind: 'running', startedAt };
        try {
            // Snapshot the args at submit-time so a mid-flight edit
            // can't poison an in-progress write.
            const submitted: SwdFlashArgs = {
                artifactPath: args.artifactPath.trim(),
                chip: args.chip?.trim() || null,
                probeSerial: args.probeSerial?.trim() || null,
                base: args.base?.trim() || null,
                verify: args.verify,
                resetAfter: args.resetAfter,
            };
            await swdFlash(submitted);
            flashState = {
                kind: 'ok',
                durationMs: Math.round(performance.now() - startedAt),
            };
        } catch (err) {
            flashState = {
                kind: 'error',
                message: err instanceof Error ? err.message : String(err),
            };
        }
    }

    function describeProbe(p: ProbeInfo): string {
        const sn = p.serialNumber ?? '(no serial)';
        return `${p.identifier} — ${sn}`;
    }

    async function startErase(): Promise<void> {
        eraseState = { kind: 'running' };
        try {
            await swdErase({
                chip: null, // library default = STM32H733ZGTx
                probeSerial: args.probeSerial?.trim() || null,
            });
            eraseState = { kind: 'ok' };
        } catch (err) {
            eraseState = {
                kind: 'error',
                message: err instanceof Error ? err.message : String(err),
            };
        }
    }

    const running = $derived(flashState.kind === 'running');
    const erasing = $derived(eraseState.kind === 'running');
    const opLabel = $derived(
        currentOp === 'erase'
            ? 'Erasing flash'
            : currentOp === 'program'
              ? 'Writing flash'
              : currentOp === 'verify'
                ? 'Verifying flash'
                : currentOp === 'fill'
                  ? 'Filling flash'
                  : 'Working',
    );

    onMount(async () => {
        await refreshProbes();
        unlistenProgress = await onSwdFlashEvent(handleProgress);
    });

    onDestroy(() => {
        if (unlistenProgress) {
            unlistenProgress();
            unlistenProgress = null;
        }
    });
</script>

<div class="view">
    <header>
        <div>
            <h2>Burn bootloader</h2>
            <p class="muted">
                The team's CAN bootloader has to be programmed onto the
                STM32H733 over SWD before any over-CAN flashing can work — a
                bare chip can't yet speak the bootloader's wire protocol.
                Burn it once when commissioning a new ECU; from then on,
                every app update uses the <strong>Flash</strong> tab over CAN.
            </p>
        </div>
        <button
            type="button"
            class="refresh"
            onclick={refreshProbes}
            disabled={probesLoading}
            title="Re-enumerate attached probes"
        >
            {probesLoading ? '…' : '⟳'}
        </button>
    </header>

    <section class="card">
        <h3>Probe</h3>
        {#if probesError !== null}
            <div class="error">
                <strong>Failed to enumerate probes:</strong>
                {probesError}
            </div>
        {:else if probes.length === 0 && !probesLoading}
            <p class="empty muted">
                No debug probes found. Plug an ST-LINK in, then refresh. On
                Linux you may need a udev rule; on Windows, Zadig to install
                the WinUSB driver. See <code>docs/INSTALL.md</code>.
            </p>
        {:else if probes.length === 1}
            <p class="single-probe">
                <span class="dot ok"></span>
                {describeProbe(probes[0])}
            </p>
        {:else}
            <label class="field">
                <span>Serial</span>
                <select bind:value={args.probeSerial} disabled={running}>
                    <option value={null}>(auto — pick the only attached probe)</option>
                    {#each probes as p (p.serialNumber ?? p.identifier)}
                        <option value={p.serialNumber}>
                            {describeProbe(p)}
                        </option>
                    {/each}
                </select>
            </label>
            <p class="muted small">
                {probes.length} probes attached — pin one by serial.
            </p>
        {/if}
    </section>

    <section class="card">
        <h3>Firmware</h3>
        <label class="field">
            <span>Artifact</span>
            <div class="row">
                <input
                    type="text"
                    bind:value={args.artifactPath}
                    placeholder="/path/to/bootloader.elf"
                    disabled={running || fetchState.kind === 'fetching'}
                />
                <button
                    type="button"
                    onclick={browseForArtifact}
                    disabled={running || fetchState.kind === 'fetching'}
                >
                    Browse…
                </button>
            </div>
        </label>
        <p class="muted small">
            <code>.elf</code>, <code>.hex</code>, or <code>.bin</code>. For raw
            <code>.bin</code> the load address comes from <em>Base address</em>;
            <code>.elf</code> and <code>.hex</code> carry their own addresses.
        </p>

        <div class="divider"></div>

        <label class="field">
            <span>or — fetch from <code>isc-fs/stm32-can-bootloader</code></span>
            <div class="row">
                <input
                    type="text"
                    bind:value={releaseTag}
                    placeholder="(latest)"
                    disabled={running || fetchState.kind === 'fetching'}
                />
                <button
                    type="button"
                    onclick={fetchFromReleases}
                    disabled={running || fetchState.kind === 'fetching'}
                >
                    {fetchState.kind === 'fetching' ? 'Fetching…' : 'Fetch'}
                </button>
            </div>
        </label>
        {#if fetchState.kind === 'done'}
            <p class="muted small">
                {fetchState.downloaded ? '↓ Downloaded' : '✓ Cached'}
                <code>CAN_BL.elf</code>
                from <strong>{fetchState.tag}</strong>; artifact path filled in
                above.
            </p>
        {:else if fetchState.kind === 'error'}
            <p class="fetch-error small">
                <strong>Fetch failed:</strong>
                {fetchState.message}
            </p>
        {:else}
            <p class="muted small">
                Blank tag pulls the latest release. Files are cached locally —
                a second flash with the same tag won't hit the network.
            </p>
        {/if}
    </section>

    <section class="card">
        <h3>Options</h3>
        <label class="toggle">
            <input type="checkbox" bind:checked={args.verify} disabled={running} />
            <span>Verify after write</span>
        </label>
        <label class="toggle">
            <input
                type="checkbox"
                bind:checked={args.resetAfter}
                disabled={running}
            />
            <span>Reset target after flash</span>
        </label>
    </section>

    <div class="actions">
        <button
            type="button"
            class="primary"
            onclick={runFlash}
            disabled={running || erasing || args.artifactPath.trim().length === 0}
        >
            {#if running}
                Burning…
            {:else}
                Burn bootloader
            {/if}
        </button>
        <button
            type="button"
            class="danger"
            onclick={() => (eraseState = { kind: 'confirming' })}
            disabled={running || erasing}
        >
            Erase chip
        </button>
    </div>

    {#if flashState.kind === 'running'}
        <div class="status running">
            <div class="progress-row">
                <span class="op-label">{opLabel}</span>
                <span class="pct">
                    {#if pct !== null}
                        {pct}%
                    {:else}
                        …
                    {/if}
                </span>
            </div>
            <div
                class="bar"
                class:indeterminate={pct === null}
                role="progressbar"
                aria-label={opLabel}
                aria-valuemin="0"
                aria-valuemax="100"
                aria-valuenow={pct ?? 0}
            >
                <div class="fill" style:width={pct !== null ? `${pct}%` : '100%'}></div>
            </div>
            <p class="muted small">Don't unplug the probe.</p>
        </div>
    {:else if flashState.kind === 'ok'}
        <div class="status ok">
            ✓ Bootloader burned in {flashState.durationMs} ms. The chip is
            ready to be flashed over CAN from the <strong>Flash</strong> tab.
        </div>
    {:else if flashState.kind === 'error'}
        <div class="status error">
            <strong>Burn failed:</strong>
            {flashState.message}
        </div>
    {/if}

    {#if eraseState.kind === 'confirming'}
        <div class="status warn">
            <strong>Erase the entire chip?</strong>
            This wipes the bootloader and any application code. The chip will
            need the bootloader burned again before CAN flashing works.
            <div class="confirm-actions">
                <button
                    type="button"
                    class="danger"
                    onclick={startErase}
                >
                    Yes, erase
                </button>
                <button
                    type="button"
                    onclick={() => (eraseState = { kind: 'idle' })}
                >
                    Cancel
                </button>
            </div>
        </div>
    {:else if eraseState.kind === 'running'}
        <div class="status running">
            <span class="spinner"></span>
            Erasing chip — don't unplug the probe.
        </div>
    {:else if eraseState.kind === 'ok'}
        <div class="status ok">
            ✓ Chip erased. Burn the bootloader next to make it CAN-flashable.
        </div>
    {:else if eraseState.kind === 'error'}
        <div class="status error">
            <strong>Erase failed:</strong>
            {eraseState.message}
        </div>
    {/if}
</div>

<style>
    .view {
        padding: 20px 24px 32px;
        display: flex;
        flex-direction: column;
        gap: 16px;
        overflow-y: auto;
    }

    header {
        display: flex;
        align-items: flex-start;
        gap: 12px;
    }

    header > div {
        flex: 1;
    }

    h2 {
        margin: 0;
        font-size: 1.2rem;
    }

    h3 {
        margin: 0 0 10px;
        font-size: 0.85rem;
        text-transform: uppercase;
        letter-spacing: 0.06em;
        color: var(--text-muted);
    }

    .muted {
        color: var(--text-muted);
    }

    .small {
        font-size: 0.8rem;
        margin: 6px 0 0;
    }

    .card {
        padding: 14px 16px;
        background: var(--surface);
        border: 1px solid var(--border);
        border-radius: 8px;
    }

    .field {
        display: flex;
        flex-direction: column;
        gap: 4px;
    }

    .field > span {
        font-size: 0.8rem;
        color: var(--text-muted);
    }

    .field input,
    .field select {
        appearance: none;
        background: var(--bg);
        border: 1px solid var(--border);
        color: var(--text);
        font: inherit;
        padding: 8px 10px;
        border-radius: 6px;
    }

    .field input:focus,
    .field select:focus {
        outline: none;
        border-color: var(--accent);
    }

    .row {
        display: flex;
        gap: 6px;
    }

    .row input {
        flex: 1;
        font-family: var(--font-mono);
    }

    .row button,
    .actions button {
        appearance: none;
        background: var(--surface);
        border: 1px solid var(--border);
        color: var(--text);
        font: inherit;
        padding: 8px 14px;
        border-radius: 6px;
        cursor: pointer;
    }

    .row button:hover:not(:disabled),
    .actions button:hover:not(:disabled) {
        border-color: var(--accent);
        color: var(--accent);
    }

    button:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }

    .grid-2 {
        display: grid;
        grid-template-columns: 1fr 1fr;
        gap: 10px;
    }

    .toggle {
        display: flex;
        align-items: center;
        gap: 8px;
        padding: 4px 0;
        cursor: pointer;
    }

    .toggle input {
        accent-color: var(--accent);
    }

    .actions {
        display: flex;
        gap: 8px;
    }

    .actions .primary {
        background: var(--accent);
        color: #1a1a1a;
        border-color: var(--accent);
        font-weight: 600;
    }

    .actions .primary:hover:not(:disabled) {
        filter: brightness(1.05);
        color: #1a1a1a;
    }

    .danger {
        appearance: none;
        background: transparent;
        border: 1px solid var(--error);
        color: var(--error);
        font: inherit;
        padding: 8px 14px;
        border-radius: 6px;
        cursor: pointer;
    }

    .danger:hover:not(:disabled) {
        background: rgba(255, 115, 115, 0.1);
    }

    .danger:disabled {
        opacity: 0.4;
        cursor: not-allowed;
    }

    /* Progress bar */

    .progress-row {
        display: flex;
        justify-content: space-between;
        align-items: baseline;
        margin-bottom: 6px;
    }

    .op-label {
        font-weight: 500;
    }

    .pct {
        font-family: var(--font-mono);
        font-size: 0.85rem;
        color: var(--text-muted);
    }

    .bar {
        height: 8px;
        background: var(--bg);
        border-radius: 4px;
        overflow: hidden;
        border: 1px solid var(--border);
    }

    .bar .fill {
        height: 100%;
        background: var(--accent);
        transition: width 120ms ease-out;
    }

    .bar.indeterminate .fill {
        animation: slide 1.2s ease-in-out infinite;
        background: linear-gradient(
            90deg,
            transparent 0%,
            var(--accent) 50%,
            transparent 100%
        );
    }

    @keyframes slide {
        0% {
            transform: translateX(-100%);
        }
        100% {
            transform: translateX(100%);
        }
    }

    .status.warn {
        color: #ffd166;
        border-color: rgba(255, 209, 102, 0.4);
        background: rgba(255, 209, 102, 0.06);
    }

    .confirm-actions {
        display: flex;
        gap: 8px;
        margin-top: 10px;
    }

    .confirm-actions button {
        appearance: none;
        font: inherit;
        padding: 8px 14px;
        border-radius: 6px;
        cursor: pointer;
        background: var(--surface);
        border: 1px solid var(--border);
        color: var(--text);
    }

    .confirm-actions button:hover:not(:disabled) {
        border-color: var(--accent);
        color: var(--accent);
    }

    .status {
        padding: 10px 14px;
        border-radius: 6px;
        border: 1px solid var(--border);
        background: var(--surface);
    }

    .status.ok {
        color: #06d6a0;
        border-color: rgba(6, 214, 160, 0.4);
        background: rgba(6, 214, 160, 0.06);
    }

    .status.error {
        color: var(--error);
        border-color: var(--error);
        background: rgba(255, 115, 115, 0.08);
    }

    .status.running {
        display: flex;
        align-items: center;
        gap: 10px;
    }

    .error {
        padding: 10px 14px;
        border-radius: 6px;
        background: rgba(255, 115, 115, 0.1);
        border: 1px solid var(--error);
        color: var(--error);
        margin-top: 4px;
    }

    .empty {
        margin: 0;
    }

    .divider {
        height: 1px;
        background: var(--border);
        margin: 14px 0 12px;
    }

    .fetch-error {
        margin: 6px 0 0;
        color: var(--error);
    }

    .refresh {
        appearance: none;
        background: var(--surface);
        border: 1px solid var(--border);
        color: var(--text);
        width: 36px;
        height: 36px;
        border-radius: 6px;
        font: inherit;
        font-size: 1rem;
        cursor: pointer;
    }

    .refresh:hover:not(:disabled) {
        border-color: var(--accent);
        color: var(--accent);
    }

    .refresh:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }

    .single-probe {
        display: flex;
        align-items: center;
        gap: 8px;
        margin: 0;
        font-family: var(--font-mono);
        font-size: 0.85rem;
    }

    .dot {
        width: 8px;
        height: 8px;
        border-radius: 50%;
        background: var(--text-muted);
    }

    .dot.ok {
        background: #06d6a0;
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
