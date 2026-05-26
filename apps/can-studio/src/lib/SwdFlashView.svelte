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
        type SwdFlashReport,
        type SwdOp,
    } from './swd';

    type FlashState =
        | { kind: 'idle' }
        | { kind: 'running'; startedAt: number }
        | { kind: 'ok'; durationMs: number; report: SwdFlashReport }
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
            const report = await swdFlash(submitted);
            flashState = {
                kind: 'ok',
                durationMs: Math.round(performance.now() - startedAt),
                report,
            };
        } catch (err) {
            flashState = {
                kind: 'error',
                message: err instanceof Error ? err.message : String(err),
            };
        }
    }

    function formatSize(bytes: number): string {
        if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(2)} MB`;
        if (bytes >= 1_024) return `${(bytes / 1_024).toFixed(1)} KB`;
        return `${bytes} B`;
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
                  : 'Working…',
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
            class="icon-btn"
            onclick={refreshProbes}
            disabled={probesLoading}
            title="Re-enumerate attached probes"
            aria-label="Refresh probes"
        >
            {probesLoading ? '…' : '⟳'}
        </button>
    </header>

    <section class="card">
        <div class="card-header">
            <h3>Probe</h3>
        </div>
        {#if probesError !== null}
            <div class="banner banner-danger">
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
        <div class="card-header">
            <h3>Firmware</h3>
        </div>
        <label class="field">
            <span>Artifact</span>
            <div class="input-row">
                <input
                    class="input mono"
                    type="text"
                    bind:value={args.artifactPath}
                    placeholder="/path/to/bootloader.elf"
                    disabled={running || fetchState.kind === 'fetching'}
                />
                <button
                    type="button"
                    class="btn btn-sm"
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

        <hr class="divider" />

        <label class="field">
            <span>or — fetch from <code>isc-fs/stm32-can-bootloader</code></span>
            <div class="input-row">
                <input
                    class="input mono"
                    type="text"
                    bind:value={releaseTag}
                    placeholder="(latest)"
                    disabled={running || fetchState.kind === 'fetching'}
                />
                <button
                    type="button"
                    class="btn btn-sm"
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
            <p class="small fetch-error">
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
        <div class="card-header">
            <h3>Options</h3>
        </div>
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
            class="btn btn-primary"
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
            class="btn btn-danger"
            onclick={() => (eraseState = { kind: 'confirming' })}
            disabled={running || erasing}
        >
            Erase chip
        </button>
    </div>

    {#if flashState.kind === 'running'}
        <div class="banner">
            <div class="progress-row">
                <span class="op-label">{opLabel}</span>
                <span class="pct mono small">
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
            <p class="muted small no-margin">Don't unplug the probe.</p>
        </div>
    {:else if flashState.kind === 'ok'}
        <div class="banner banner-success">
            <div>
                ✓ Bootloader burned in {flashState.durationMs} ms. The chip is
                ready to be flashed over CAN from the <strong>Flash</strong> tab.
            </div>
            <!--
                Verifiable fingerprint of what's on the chip. probe-rs
                read back the flash and byte-compared it against this
                CRC's source bytes, so this is a true post-write proof
                — operators can record it in bench notes and reconcile
                between boards (cf. isc-fs/stm32-can-bootloader#117).
            -->
            <dl class="fingerprint">
                <dt>Size</dt>
                <dd>{formatSize(flashState.report.sizeBytes)}</dd>
                <dt>CRC32</dt>
                <dd><code>{flashState.report.crc32Hex}</code></dd>
                <dt>Verify</dt>
                <dd>
                    {#if flashState.report.verified}
                        ✓ probe-rs read-back compare
                    {:else}
                        ✗ skipped (--no-verify)
                    {/if}
                </dd>
                {#if flashState.report.targetVoltageV !== null}
                    <dt>VTref</dt>
                    <dd>{flashState.report.targetVoltageV.toFixed(2)} V</dd>
                {/if}
            </dl>
        </div>
    {:else if flashState.kind === 'error'}
        <div class="banner banner-danger">
            <strong>Burn failed:</strong>
            {flashState.message}
        </div>
    {/if}

    {#if eraseState.kind === 'confirming'}
        <div class="banner banner-warning">
            <strong>Erase the entire chip?</strong>
            This wipes the bootloader and any application code. The chip will
            need the bootloader burned again before CAN flashing works.
            <div class="confirm-actions">
                <button
                    type="button"
                    class="btn btn-danger"
                    onclick={startErase}
                >
                    Yes, erase
                </button>
                <button
                    type="button"
                    class="btn"
                    onclick={() => (eraseState = { kind: 'idle' })}
                >
                    Cancel
                </button>
            </div>
        </div>
    {:else if eraseState.kind === 'running'}
        <div class="banner spinner-row">
            <span class="spinner"></span>
            Erasing chip — don't unplug the probe.
        </div>
    {:else if eraseState.kind === 'ok'}
        <div class="banner banner-success">
            ✓ Chip erased. Burn the bootloader next to make it CAN-flashable.
        </div>
    {:else if eraseState.kind === 'error'}
        <div class="banner banner-danger">
            <strong>Erase failed:</strong>
            {eraseState.message}
        </div>
    {/if}
</div>

<style>
    /* The .view, .card, .card-header, .field, .toggle, .btn (+
       variants), .icon-btn, .banner (+ variants), .divider, .small,
       .muted, .mono utilities all come from app.css. This file only
       defines the bits that are genuinely view-specific:
         - input-row: textbox + adjacent action button
         - actions, confirm-actions: button row layout
         - progress bar (op label + percent + animated fill)
         - probe-fingerprint dl
         - single-probe dot indicator
         - inline spinner
    */

    /* Input + right-side button row. Used by Artifact + Tag fields. */
    .input-row {
        display: flex;
        gap: var(--space-2);
    }
    .input-row .input {
        flex: 1;
    }

    /* Page-level CTA row. */
    .actions {
        display: flex;
        gap: var(--space-2);
    }

    /* Inline button row inside the confirm banner — buttons live
       below the prompt text, so we top-margin the group. */
    .confirm-actions {
        display: flex;
        gap: var(--space-2);
        margin-top: var(--space-3);
    }

    /* Spinner banner — needs flex layout so the spinner + label
       align on the same baseline. The .banner utility doesn't
       imply layout, so we add it here. */
    .spinner-row {
        display: flex;
        align-items: center;
        gap: var(--space-3);
    }

    /* Progress bar — bespoke (no .progress utility in the design
       system yet). Header row pairs an op label with the percent
       reading; the bar below fills with the accent or slides an
       indeterminate gradient when probe-rs hasn't told us the byte
       count yet. */
    .progress-row {
        display: flex;
        justify-content: space-between;
        align-items: baseline;
        margin-bottom: var(--space-2);
    }
    .op-label {
        font-weight: 500;
    }
    .pct {
        margin: 0;
    }
    .no-margin {
        margin: var(--space-2) 0 0;
    }
    .bar {
        height: 8px;
        background: var(--bg);
        border-radius: var(--radius-sm);
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

    /* Empty-state paragraph inside .card — strip the default
       paragraph margin so it sits flush with the card header. */
    .empty {
        margin: 0;
    }

    /* Fetch-error helper text — uses --danger but lives outside a
       .banner because it's a tail-line below the Fetch field. */
    .fetch-error {
        margin: var(--space-2) 0 0;
        color: var(--danger);
    }

    /* Post-flash fingerprint — verifiable proof of what's on the
       chip. Compact key/value grid that lines up vertically so an
       operator can grep / copy the row they need. */
    .fingerprint {
        display: grid;
        grid-template-columns: max-content 1fr;
        gap: var(--space-1) var(--space-4);
        margin: var(--space-3) 0 0;
        font-size: var(--text-sm);
    }
    .fingerprint dt {
        color: var(--text-muted);
        font-weight: 500;
    }
    .fingerprint dd {
        margin: 0;
        font-family: var(--font-mono);
    }

    /* Single-probe row — short status line shown when only one
       probe is attached (no dropdown needed). Mono so the serial
       number stays grep-friendly. */
    .single-probe {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        margin: 0;
        font-family: var(--font-mono);
        font-size: var(--text-sm);
    }
    .dot {
        width: 8px;
        height: 8px;
        border-radius: 50%;
        background: var(--text-muted);
    }
    .dot.ok {
        background: var(--success);
    }

    /* Inline spinner for the chip-erase "running" banner. */
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
