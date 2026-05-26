<!--
    Pit-diag view — Slice 1.

    AMS observer mode. Hits Enable → backend sends 0x7F0#DEADBEEF →
    waits for the ACK on 0x7F1 → spawns a streaming task that decodes
    the 56-frame, 1 Hz diagnostic stream.

    Two panels here (the rest of the diag block — FSM, balance, boot,
    crash, firmware ID — gets typed UI in slice 2):

      - Cell voltages: 5 modules × 19 cells = 95 tiles, color-coded
        by deviation from the pack mean.
      - NTC temps:     5 modules × 40 NTCs   = 200 tiles, color-coded
        on an absolute °C scale.

    A spread bar above the cell grid shows max-min in mV so the
    engineer can read pack health at a glance.

    Safety guardrails: the Enable button shows a confirmation step
    (single click → "Hold to arm" → second click). The view does a
    best-effort disarm on unmount so a tab-switch doesn't leave the
    AMS streaming forever — the firmware also clears the flag on
    reboot, but belt + braces.
-->
<script lang="ts">
    import { onDestroy, onMount } from 'svelte';
    import type { UnlistenFn } from '@tauri-apps/api/event';

    import {
        AMS_CELLS_PER_MODULE,
        AMS_NUM_CELLS,
        AMS_NTC_PER_MODULE,
        AMS_NUM_MODULES,
        AMS_NUM_NTCS,
        onPitDiagFrame,
        onPitDiagStatus,
        pitDiagDisable,
        pitDiagEnable,
        writeCellsInto,
        writeNtcsInto,
        type PitDiagStatus,
    } from './pit_diag';
    import { settings } from './settings.svelte';

    type ArmState =
        | { kind: 'idle' }
        | { kind: 'confirming' }
        | { kind: 'arming' }
        | { kind: 'armed'; since: number }
        | { kind: 'error'; message: string };

    const adapterReady = $derived(
        settings.adapter.interface !== null &&
            (settings.adapter.interface === 'virtual' ||
                settings.adapter.channel.length > 0),
    );

    let armState = $state<ArmState>({ kind: 'idle' });

    // Pack-wide arrays — `null` means "no reading yet". The frame
    // events trickle in across the 1-second scan; the UI renders the
    // partial picture rather than waiting for a complete scan, so
    // the operator sees data within ~16ms of the first arrival.
    let cellsMv = $state<(number | null)[]>(new Array(AMS_NUM_CELLS).fill(null));
    let ntcsC = $state<(number | null)[]>(new Array(AMS_NUM_NTCS).fill(null));

    // Scan-rate tracking — increments on every cell-V or NTC frame,
    // resets on a 1Hz tick. Lets the operator see "stream is alive"
    // even when individual cell values aren't visibly changing.
    let framesThisScan = $state<number>(0);
    let lastScanFrames = $state<number>(0);
    let scanIntervalId: ReturnType<typeof setInterval> | null = null;

    // Derived stats — pack mean + spread (max − min mV). Recomputed
    // whenever the cells array changes; null when not enough cells
    // have arrived to be meaningful.
    const packStats = $derived.by(() => {
        let min = Infinity;
        let max = -Infinity;
        let sum = 0;
        let count = 0;
        for (const mv of cellsMv) {
            if (mv === null) continue;
            if (mv < min) min = mv;
            if (mv > max) max = mv;
            sum += mv;
            count += 1;
        }
        if (count === 0) return null;
        return {
            min,
            max,
            spread: max - min,
            mean: Math.round(sum / count),
            count,
        };
    });

    const ntcStats = $derived.by(() => {
        let min = Infinity;
        let max = -Infinity;
        let count = 0;
        for (const c of ntcsC) {
            if (c === null) continue;
            if (c < min) min = c;
            if (c > max) max = c;
            count += 1;
        }
        if (count === 0) return null;
        return { min, max, count };
    });

    let unlistenFrame: UnlistenFn | null = null;
    let unlistenStatus: UnlistenFn | null = null;

    function handleStatus(s: PitDiagStatus): void {
        if (s.kind === 'armed') {
            armState = { kind: 'armed', since: Date.now() };
        } else if (s.kind === 'stopped') {
            // Don't drop to idle if we're mid-arm and the backend
            // emits a stale Stopped — the arm command's Result is
            // authoritative.
            if (armState.kind === 'armed') {
                armState = { kind: 'idle' };
            }
        } else if (s.kind === 'error') {
            armState = { kind: 'error', message: s.message };
        }
    }

    function buildRequest() {
        return {
            interface: settings.adapter.interface!,
            channel:
                settings.adapter.channel.length > 0
                    ? settings.adapter.channel
                    : null,
            bitrate: settings.adapter.bitrate,
            profile: 'ams' as const,
        };
    }

    async function confirmArm(): Promise<void> {
        armState = { kind: 'confirming' };
    }

    async function cancelArm(): Promise<void> {
        armState = { kind: 'idle' };
    }

    async function arm(): Promise<void> {
        if (!adapterReady || settings.adapter.interface === null) {
            armState = {
                kind: 'error',
                message: 'Pick an adapter in the Adapters view first.',
            };
            return;
        }
        armState = { kind: 'arming' };
        // Reset the pack so old values don't linger.
        cellsMv = new Array(AMS_NUM_CELLS).fill(null);
        ntcsC = new Array(AMS_NUM_NTCS).fill(null);
        framesThisScan = 0;
        lastScanFrames = 0;
        try {
            await pitDiagEnable(buildRequest());
            // armState transitions to 'armed' via the Armed status
            // event — the backend fires it from inside the spawned
            // task, so listening for it is more reliable than
            // assuming success on Promise resolution.
        } catch (err) {
            armState = {
                kind: 'error',
                message: err instanceof Error ? err.message : String(err),
            };
        }
    }

    async function disarm(): Promise<void> {
        try {
            await pitDiagDisable(buildRequest());
        } catch (err) {
            // Surface the error but still drop to idle — the user
            // intent is "stop". If the disarm frame didn't make it,
            // the firmware's reboot-clears-flag fallback still saves
            // us.
            armState = {
                kind: 'error',
                message: err instanceof Error ? err.message : String(err),
            };
            return;
        }
        armState = { kind: 'idle' };
    }

    onMount(async () => {
        unlistenStatus = await onPitDiagStatus(handleStatus);
        unlistenFrame = await onPitDiagFrame((event) => {
            if (event.kind === 'cellVoltage') {
                // Mutate-then-reassign so Svelte 5 sees the change.
                const next = cellsMv.slice();
                framesThisScan += writeCellsInto(next, event);
                cellsMv = next;
            } else if (event.kind === 'ntcTemp') {
                const next = ntcsC.slice();
                framesThisScan += writeNtcsInto(next, event);
                ntcsC = next;
            }
            // Ack and Diag are ignored in slice 1; the cell-V grid
            // and temp heatmap are the only displayed surfaces.
        });
        // 1Hz scan-rate ticker — every second, snapshot the running
        // frame count and reset for the next window.
        scanIntervalId = setInterval(() => {
            lastScanFrames = framesThisScan;
            framesThisScan = 0;
        }, 1000);
    });

    onDestroy(async () => {
        if (unlistenFrame !== null) unlistenFrame();
        if (unlistenStatus !== null) unlistenStatus();
        if (scanIntervalId !== null) clearInterval(scanIntervalId);
        // Best-effort disarm on unmount — operator unintentionally
        // navigating away shouldn't leave the AMS streaming.
        if (armState.kind === 'armed') {
            try {
                await pitDiagDisable(buildRequest());
            } catch {
                // Firmware reboot also clears the flag; failing
                // here is the worst-case "extra bus traffic until
                // the AMS power-cycles".
            }
        }
    });

    // Color ramps — cells are scored by deviation from pack mean
    // (green = near mean, yellow = ±20mV away, red = ±50mV+). Temps
    // are scored on an absolute scale (blue = cold, green =
    // operating, orange/red = hot).
    function cellColor(mv: number | null): string {
        if (mv === null || packStats === null) return 'var(--bg)';
        const dev = Math.abs(mv - packStats.mean);
        if (dev > 50) return '#c2185b'; // red — outlier
        if (dev > 20) return '#f57f17'; // amber — drift
        return '#2e7d32'; // green — within band
    }

    function ntcColor(c: number | null): string {
        if (c === null) return 'var(--bg)';
        if (c < 0) return '#1976d2'; // sub-zero — alarming for li-ion
        if (c < 20) return '#0288d1'; // cool
        if (c < 40) return '#388e3c'; // operating
        if (c < 55) return '#f57c00'; // warm
        return '#c2185b';              // hot — outside spec
    }

    // 5×19 / 5×40 grid layout — module is rows, slot is cols.
    const cellRows = $derived(
        Array.from({ length: AMS_NUM_MODULES }, (_, m) =>
            Array.from({ length: AMS_CELLS_PER_MODULE }, (_, s) => {
                const idx = m * AMS_CELLS_PER_MODULE + s;
                return { idx, mv: cellsMv[idx] };
            }),
        ),
    );
    const ntcRows = $derived(
        Array.from({ length: AMS_NUM_MODULES }, (_, m) =>
            Array.from({ length: AMS_NTC_PER_MODULE }, (_, s) => {
                const idx = m * AMS_NTC_PER_MODULE + s;
                return { idx, c: ntcsC[idx] };
            }),
        ),
    );
</script>

<div class="view">
    <header>
        <h2>Pit diag</h2>
        <p class="muted">
            AMS observer mode. Arming emits <code>0x7F0#DEADBEEF</code> →
            AMS replies on <code>0x7F1</code> → the full 1 Hz diagnostic
            stream (56 frames/scan) flows here. Disarm sends the
            zero-payload frame; firmware also clears the flag on reboot.
        </p>
    </header>

    {#if !adapterReady}
        <div class="warning">
            <strong>No adapter selected.</strong> Pick one in the
            <em>Adapters</em> view first — pit-diag needs an
            <code>--interface</code>/<code>--channel</code> pair.
        </div>
    {/if}

    <!-- Controls row -->
    <div class="toolbar">
        {#if armState.kind === 'idle' || armState.kind === 'error'}
            <button
                type="button"
                class="primary"
                disabled={!adapterReady}
                onclick={confirmArm}
            >
                Enable AMS pit-diag
            </button>
        {:else if armState.kind === 'confirming'}
            <div class="confirm">
                <span class="confirm-text">Arm AMS pit-diag stream?</span>
                <button type="button" class="primary" onclick={arm}>
                    Yes, arm
                </button>
                <button type="button" onclick={cancelArm}>Cancel</button>
            </div>
        {:else if armState.kind === 'arming'}
            <button type="button" disabled>Arming…</button>
        {:else if armState.kind === 'armed'}
            <button type="button" onclick={disarm}>Disable</button>
            <span class="stat">
                <strong class="status-dot armed">●</strong>
                <span>armed</span>
            </span>
            <span class="stat">
                <span>frames / scan</span>
                <strong>{lastScanFrames}</strong>
            </span>
            <span class="stat">
                <span>cells</span>
                <strong>{packStats?.count ?? 0}/{AMS_NUM_CELLS}</strong>
            </span>
            <span class="stat">
                <span>NTCs</span>
                <strong>{ntcStats?.count ?? 0}/{AMS_NUM_NTCS}</strong>
            </span>
        {/if}
    </div>

    {#if armState.kind === 'error'}
        <div class="error">
            <strong>Arm failed:</strong>
            {armState.message}
        </div>
    {/if}

    <!-- Pack-spread bar (cell voltages) -->
    {#if packStats !== null}
        <section class="card spread-card">
            <header>
                <h3>Pack spread</h3>
                <span class="spread-mv">
                    {packStats.spread} mV (max − min)
                </span>
            </header>
            <div class="spread-row">
                <span class="spread-min mono">min {packStats.min}</span>
                <div
                    class="spread-bar"
                    class:warn={packStats.spread > 30}
                    class:bad={packStats.spread > 80}
                >
                    <div
                        class="spread-fill"
                        style:width={`${Math.min(100, packStats.spread)}%`}
                    ></div>
                </div>
                <span class="spread-max mono">max {packStats.max}</span>
            </div>
            <p class="muted small spread-hint">
                Healthy pack &lt; 30 mV under load. Sustained spread &gt; 80 mV
                while charging is a balance-cycle problem.
            </p>
        </section>
    {/if}

    <!-- Cell-V grid -->
    <section class="card grid-card">
        <header>
            <h3>Cell voltages</h3>
            <span class="muted small">
                5 modules × 19 cells = 95. Color = deviation from pack mean.
            </span>
        </header>
        <div class="grid cells">
            {#each cellRows as row, m (m)}
                <div class="grid-row" role="row">
                    <span class="row-label">M{m + 1}</span>
                    {#each row as cell (cell.idx)}
                        <div
                            class="tile"
                            class:empty={cell.mv === null}
                            style:background={cellColor(cell.mv)}
                            title="Cell {cell.idx + 1} — {cell.mv ?? '—'} mV"
                        >
                            <span class="tile-value mono">
                                {cell.mv ?? '—'}
                            </span>
                        </div>
                    {/each}
                </div>
            {/each}
        </div>
    </section>

    <!-- NTC temp heatmap -->
    <section class="card grid-card">
        <header>
            <h3>NTC temperatures</h3>
            <span class="muted small">
                {#if ntcStats !== null}
                    range {ntcStats.min}…{ntcStats.max} °C · {ntcStats.count}/{AMS_NUM_NTCS} reading
                {:else}
                    5 modules × 40 NTCs = 200. — = unwired channel.
                {/if}
            </span>
        </header>
        <div class="grid ntcs">
            {#each ntcRows as row, m (m)}
                <div class="grid-row" role="row">
                    <span class="row-label">M{m + 1}</span>
                    {#each row as ntc (ntc.idx)}
                        <div
                            class="tile ntc-tile"
                            class:empty={ntc.c === null}
                            style:background={ntcColor(ntc.c)}
                            title="NTC {ntc.idx + 1} — {ntc.c ?? '—'} °C"
                        ></div>
                    {/each}
                </div>
            {/each}
        </div>
    </section>
</div>

<style>
    /* This file targets the dev branch's chrome (no design system
       yet — that ships separately in PR #253). Once #253 merges
       this view picks up shared utilities; until then the inline
       styles below match the conventions of the existing views
       (BusMonitor / LiveData) so the visual feel is consistent. */

    .view {
        display: flex;
        flex-direction: column;
        gap: 14px;
        padding: 24px 28px;
        overflow: auto;
        height: 100%;
        min-height: 0;
        box-sizing: border-box;
    }
    h2 {
        margin: 0;
        font-size: 1.3rem;
    }
    .muted {
        color: var(--text-muted);
    }
    header > p {
        margin: 4px 0 0;
        font-size: 0.9rem;
        color: var(--text-muted);
    }
    code {
        font-family: var(--font-mono);
        font-size: 0.95em;
        padding: 0 3px;
        border-radius: 3px;
        background: rgba(255, 255, 255, 0.04);
    }
    .small {
        font-size: 0.8rem;
    }

    .warning {
        padding: 10px 14px;
        border: 1px solid var(--accent);
        background: rgba(242, 178, 51, 0.08);
        border-radius: 6px;
    }
    .error {
        padding: 10px 14px;
        border: 1px solid var(--error);
        color: var(--error);
        background: rgba(255, 115, 115, 0.08);
        border-radius: 6px;
        font-size: 0.85rem;
    }

    /* Toolbar — Enable button + confirm-step prompt + live stats. */
    .toolbar {
        display: flex;
        align-items: center;
        gap: 10px;
        padding: 12px 14px;
        border: 1px solid var(--border);
        background: var(--surface);
        border-radius: 8px;
        flex-wrap: wrap;
    }
    .stat {
        display: inline-flex;
        gap: 4px;
        align-items: center;
        font-size: 0.85rem;
        color: var(--text-muted);
        font-family: var(--font-mono);
    }
    .stat strong {
        color: var(--text);
        font-weight: 600;
    }
    .status-dot {
        color: var(--text-muted);
    }
    .status-dot.armed {
        color: #06d6a0;
        animation: pulse 1.2s ease-in-out infinite;
    }
    @keyframes pulse {
        0%,
        100% {
            opacity: 1;
        }
        50% {
            opacity: 0.4;
        }
    }
    .confirm {
        display: flex;
        gap: 10px;
        align-items: center;
    }
    .confirm-text {
        font-size: 0.85rem;
        color: var(--text);
    }

    button {
        appearance: none;
        background: var(--bg);
        color: var(--text);
        border: 1px solid var(--border);
        font: inherit;
        padding: 6px 14px;
        border-radius: 5px;
        cursor: pointer;
        font-size: 0.85rem;
    }
    button:hover:not(:disabled) {
        border-color: var(--accent);
        color: var(--accent);
    }
    button.primary {
        background: var(--accent);
        color: #1a1a1a;
        border-color: var(--accent);
    }
    button.primary:hover:not(:disabled) {
        filter: brightness(1.05);
        color: #1a1a1a;
    }
    button:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }

    /* Card chrome shared by the spread bar + the two grids. */
    .card {
        padding: 14px 18px;
        border: 1px solid var(--border);
        background: var(--surface);
        border-radius: 8px;
    }
    .card > header {
        display: flex;
        align-items: baseline;
        justify-content: space-between;
        gap: 10px;
        margin-bottom: 10px;
    }
    .card h3 {
        margin: 0;
        font-size: 0.85rem;
        text-transform: uppercase;
        letter-spacing: 0.06em;
        color: var(--text-muted);
    }

    /* Spread bar — `bar` fills proportional to spread in mV,
       clamped at 100mV. Color tightens to warn/bad past thresholds
       so a glance tells the operator if anything's drifting. */
    .spread-mv {
        font-family: var(--font-mono);
        font-size: 0.85rem;
        color: var(--text);
    }
    .spread-row {
        display: flex;
        align-items: center;
        gap: 10px;
    }
    .spread-bar {
        flex: 1;
        height: 10px;
        background: var(--bg);
        border-radius: 6px;
        border: 1px solid var(--border);
        overflow: hidden;
    }
    .spread-fill {
        height: 100%;
        background: #2e7d32;
        transition:
            width 240ms ease-out,
            background 240ms ease-out;
    }
    .spread-bar.warn .spread-fill {
        background: #f57c00;
    }
    .spread-bar.bad .spread-fill {
        background: #c2185b;
    }
    .spread-min,
    .spread-max {
        font-size: 0.78rem;
        color: var(--text-muted);
    }
    .spread-hint {
        margin: 8px 0 0;
    }

    /* Grids — one row per module, tiles laid out horizontally.
       The cell grid wants legible voltage numbers; the NTC grid
       is heatmap-only so its tiles are smaller. */
    .grid {
        display: flex;
        flex-direction: column;
        gap: 6px;
    }
    .grid-row {
        display: flex;
        gap: 4px;
        align-items: center;
    }
    .row-label {
        width: 24px;
        flex: 0 0 24px;
        font-family: var(--font-mono);
        font-size: 0.78rem;
        color: var(--text-muted);
    }
    .tile {
        flex: 1;
        min-width: 0;
        height: 38px;
        border-radius: 3px;
        display: flex;
        align-items: center;
        justify-content: center;
        font-size: 0.7rem;
        color: rgba(255, 255, 255, 0.95);
        transition: background 240ms ease-out;
        cursor: help;
    }
    .tile.empty {
        background: var(--bg);
        border: 1px dashed var(--border);
        color: var(--text-muted);
    }
    .tile-value {
        font-family: var(--font-mono);
    }
    .ntc-tile {
        height: 18px;
        font-size: 0.62rem;
    }
</style>
