<!--
    SWD flash view — lay the bootloader (or any firmware) onto a
    bare STM32 via ST-LINK. Wraps the `swd_flash` Tauri command,
    which itself shells into can-flasher's probe-rs integration.

    Solves the chicken-and-egg first-boot problem: a fresh chip
    can't speak the CAN bootloader's protocol until the bootloader
    itself is on the chip. Operators used to drop down to
    STM32CubeProgrammer / OpenOCD for that initial flash; this
    view collapses both jobs into one app.

    Scope (v1 spike):
      - ST-LINK V2 / V3 probes (probe-rs auto-detects)
      - STM32H733 default; any probe-rs target string works
      - Erase + write + verify + optional reset
-->
<script lang="ts">
    import { onMount } from 'svelte';
    import { open as openDialog } from '@tauri-apps/plugin-dialog';

    import {
        defaultSwdFlashArgs,
        listSwdProbes,
        swdFlash,
        type ProbeInfo,
        type SwdFlashArgs,
    } from './swd';

    type FlashState =
        | { kind: 'idle' }
        | { kind: 'running'; startedAt: number }
        | { kind: 'ok'; durationMs: number }
        | { kind: 'error'; message: string };

    let args = $state<SwdFlashArgs>(defaultSwdFlashArgs());
    let probes = $state<ProbeInfo[]>([]);
    let probesLoading = $state<boolean>(false);
    let probesError = $state<string | null>(null);
    let flashState = $state<FlashState>({ kind: 'idle' });

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

    async function runFlash(): Promise<void> {
        if (args.artifactPath.trim().length === 0) {
            flashState = { kind: 'error', message: 'Pick a firmware artifact first.' };
            return;
        }
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

    const running = $derived(flashState.kind === 'running');

    onMount(refreshProbes);
</script>

<div class="view">
    <header>
        <div>
            <h2>Burn bootloader</h2>
            <p class="muted">
                First-boot a bare STM32 via ST-LINK — lay the bootloader (or
                any firmware) on the chip over SWD. Drives probe-rs under the
                hood, same code the CLI's <code>swd-flash</code> uses.
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
                    disabled={running}
                />
                <button type="button" onclick={browseForArtifact} disabled={running}>
                    Browse…
                </button>
            </div>
        </label>
        <p class="muted small">
            <code>.elf</code>, <code>.hex</code>, or <code>.bin</code>. For raw
            <code>.bin</code> the load address comes from <em>Base address</em>;
            <code>.elf</code> and <code>.hex</code> carry their own addresses.
        </p>
    </section>

    <section class="card">
        <h3>Target</h3>
        <div class="grid-2">
            <label class="field">
                <span>Chip</span>
                <input
                    type="text"
                    bind:value={args.chip}
                    placeholder="STM32H733ZGTx"
                    disabled={running}
                />
            </label>
            <label class="field">
                <span>Base address</span>
                <input
                    type="text"
                    bind:value={args.base}
                    placeholder="0x08000000"
                    disabled={running}
                />
            </label>
        </div>
        <p class="muted small">
            Any probe-rs target string works — <code>STM32H7</code>,
            <code>STM32F4</code>, <code>STM32G431RBTx</code>, etc. Base address
            is ignored for ELF / HEX inputs.
        </p>
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
            disabled={running || args.artifactPath.trim().length === 0}
        >
            {#if running}
                Flashing…
            {:else}
                Flash via SWD
            {/if}
        </button>
    </div>

    {#if flashState.kind === 'ok'}
        <div class="status ok">
            ✓ Flashed in {flashState.durationMs} ms
        </div>
    {:else if flashState.kind === 'error'}
        <div class="status error">
            <strong>Flash failed:</strong>
            {flashState.message}
        </div>
    {:else if flashState.kind === 'running'}
        <div class="status running">
            <span class="spinner"></span>
            Writing flash — don't unplug the probe.
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
