<!--
    Pit-diag view — Slice 2.

    AMS observer mode. Arming emits 0x7F0#DEADBEEF → AMS replies on
    0x7F1 → the 1 Hz diagnostic stream flows here. The stream is 51
    frames per scan today (24 cell-V + 25 NTC + 2 diag); future AMS
    firmware will add balance / boot / crash / firmware-ID frames
    inside the same 0x6C0..=… range.

    Surfaces (slice 2):

      - Pack spread bar: max − min mV with warn/bad thresholds
      - Cell voltages grid: 5 modules × 19 cells = 95 tiles,
        deviation-from-mean colour ramp
      - NTC heatmap: 5 modules × 40 NTCs = 200 tiles, absolute °C
      - FSM extended status card (0x6C0): state badge, mode chip,
        cockpit input LEDs, PEC error count
      - Poll-timing card (0x6C1): V-poll ms last + worst-case +
        T-sweep failure mask

    Safety net: the view counts every pit-diag frame received per
    1 Hz window and banners a warning if the count drifts from the
    expected 51 — a canary for "the AMS firmware's wire shape has
    changed since this constant was last verified".

    Arm UX: the Enable button uses a two-click confirm flow so the
    operator can't accidentally flip the AMS into pit-diag mode
    mid-session. Unmount triggers a best-effort disarm.
-->
<script lang="ts">
    import { onDestroy, onMount } from 'svelte';
    import type { UnlistenFn } from '@tauri-apps/api/event';

    import {
        AMS_CELLS_PER_MODULE,
        AMS_EXPECTED_FRAMES_PER_SCAN,
        AMS_NUM_CELLS,
        AMS_NUM_ICS,
        AMS_NTC_PER_MODULE,
        AMS_NUM_MODULES,
        AMS_NUM_NTCS,
        onPitDiagFrame,
        onPitDiagStatus,
        pitDiagDisable,
        pitDiagEnable,
        writeCellsInto,
        writeNtcsInto,
        type PitDiagProfile,
        type PitDiagStatus,
    } from './pit_diag';
    import { settings } from './settings.svelte';

    // Which ECU's pit-diag stream the view targets. Only AMS is wired
    // end-to-end (arm handshake + 0x6C0..=0x6C8 frames). ECU / uDV are
    // selectable so the view is "for the car", not AMS-only, but they
    // render a placeholder until the firmware team defines a pit-diag
    // stream + its frames in IFS08-DBCinator.
    const PROFILES: { id: PitDiagProfile; label: string }[] = [
        { id: 'ams', label: 'AMS' },
        { id: 'ecu', label: 'ECU' },
        { id: 'udv', label: 'uDV' },
    ];
    let profile = $state<PitDiagProfile>('ams');

    type ArmState =
        | { kind: 'idle' }
        | { kind: 'confirming' }
        | { kind: 'arming' }
        | { kind: 'armed'; since: number }
        | { kind: 'error'; message: string };

    interface FsmSnapshot {
        state: string;
        modeLocked: string;
        tsms: boolean;
        dashChg: boolean;
        amsOk: boolean;
        pecErrorTotal: number;
        faultReason: string;
        faultDetail: number;
    }

    interface PollSnapshot {
        lastVPollMs: number;
        worstVPollMs: number;
        tSweepFailMask: number;
    }

    interface BalanceSnapshot {
        /** Cells 0..=63 discharge mask. BigInt — full u64. */
        dccLo: bigint;
        /** Cells 64..=94 discharge mask (low 31 bits). */
        dccHi: number;
        cyclesTotal: number;
        cyclesActive: number;
    }

    interface BootSnapshot {
        jumpReason: string;
        appInitProgress: number;
        fdcan1StartResult: number;
    }

    interface CrashSnapshot {
        stackOverflowSeen: boolean;
        watermarkLowByte: number;
        taskAddrLo: number;
        mallocFailedCount: number;
        clean: boolean;
    }

    interface FwSnapshot {
        versionMajor: number;
        versionMinor: number;
        versionPatch: number;
        gitHash: number[];
        blNodeId: number;
    }

    const adapterReady = $derived(
        settings.adapter.interface !== null &&
            (settings.adapter.interface === 'virtual' ||
                settings.adapter.channel.length > 0),
    );

    let armState = $state<ArmState>({ kind: 'idle' });

    // Pack-wide arrays — `null` means "no reading yet". The frame
    // events trickle in across the 1-second scan; the UI renders the
    // partial picture rather than waiting for a complete scan.
    let cellsMv = $state<(number | null)[]>(new Array(AMS_NUM_CELLS).fill(null));
    let ntcsC = $state<(number | null)[]>(new Array(AMS_NUM_NTCS).fill(null));

    // Latest FSM + poll-timing snapshots. Each arrives once per
    // scan; we just keep the most recent (no history yet — slice 3
    // could add a trend line for the V-poll latency).
    let fsm = $state<FsmSnapshot | null>(null);
    let poll = $state<PollSnapshot | null>(null);

    // Per-IC PEC error counts — 10 ICs (2 per module). Arrives as
    // two frames (0x6C7 ICs 0..7, 0x6C8 ICs 8..9); we splice each
    // into the pack-wide array as it lands. `null` = not yet seen.
    let icPec = $state<(number | null)[]>(new Array(AMS_NUM_ICS).fill(null));

    // Diag frames added once the AMS 0x6C2..0x6C6 block was decoded
    // (slices 2b/3). Balance arrives split across two frames; we keep
    // the latest half of each and reassemble for the discharge grid.
    let balance = $state<BalanceSnapshot | null>(null);
    let boot = $state<BootSnapshot | null>(null);
    let crash = $state<CrashSnapshot | null>(null);
    let fw = $state<FwSnapshot | null>(null);

    // Is cell `idx` discharging? Mirrors BalanceState::is_discharging
    // in the library: low 64 from dccLo (BigInt), 64..=94 from dccHi.
    function isDischarging(idx: number): boolean {
        if (balance === null) return false;
        if (idx < 64) return ((balance.dccLo >> BigInt(idx)) & 1n) === 1n;
        if (idx < AMS_NUM_CELLS) return ((balance.dccHi >>> (idx - 64)) & 1) === 1;
        return false;
    }

    // Scan-rate tracking — counts EVERY pit-diag frame received in
    // a 1 Hz window. Compared against AMS_EXPECTED_FRAMES_PER_SCAN
    // (51 today) to detect schema drift. A divergence > ±2 fires
    // the warning banner: the wire shape has likely changed since
    // the host's hand-coded layout was last verified.
    let framesThisScan = $state<number>(0);
    let lastScanFrames = $state<number>(0);
    let scanIntervalId: ReturnType<typeof setInterval> | null = null;

    const schemaDriftSuspected = $derived(
        lastScanFrames > 0 &&
            Math.abs(lastScanFrames - AMS_EXPECTED_FRAMES_PER_SCAN) > 2,
    );

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

    // Number of T-sweep bits set in the poll-timing failure mask.
    // Each set bit is one NTC channel that flagged a sweep failure
    // on the most recent sweep — a quick at-a-glance count is more
    // useful than the raw u32.
    const tSweepFailBits = $derived(
        poll === null ? 0 : popcount(poll.tSweepFailMask),
    );

    function popcount(n: number): number {
        // u32 popcount via the standard SWAR trick. Number is f64
        // in JS but the input fits 32 bits so we can use bitwise.
        let x = n >>> 0;
        let count = 0;
        while (x !== 0) {
            count += x & 1;
            x >>>= 1;
        }
        return count;
    }

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
            profile,
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
        // Reset everything so old values don't linger across an
        // arm cycle.
        cellsMv = new Array(AMS_NUM_CELLS).fill(null);
        ntcsC = new Array(AMS_NUM_NTCS).fill(null);
        icPec = new Array(AMS_NUM_ICS).fill(null);
        fsm = null;
        poll = null;
        balance = null;
        boot = null;
        crash = null;
        fw = null;
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
            armState = {
                kind: 'error',
                message: err instanceof Error ? err.message : String(err),
            };
            return;
        }
        armState = { kind: 'idle' };
    }

    // Switch the targeted ECU. If a stream is armed (only AMS can be,
    // today), disarm it first — while `profile` still names the armed
    // ECU — so the disable frame goes to the right node and the AMS
    // doesn't keep streaming in the background behind the placeholder.
    async function selectProfile(next: PitDiagProfile): Promise<void> {
        if (next === profile) return;
        if (armState.kind === 'armed') {
            await disarm();
        }
        profile = next;
    }

    onMount(async () => {
        unlistenStatus = await onPitDiagStatus(handleStatus);
        unlistenFrame = await onPitDiagFrame((event) => {
            if (event.kind === 'cellVoltage') {
                const next = cellsMv.slice();
                writeCellsInto(next, event);
                cellsMv = next;
                framesThisScan += 1;
            } else if (event.kind === 'ntcTemp') {
                const next = ntcsC.slice();
                writeNtcsInto(next, event);
                ntcsC = next;
                framesThisScan += 1;
            } else if (event.kind === 'fsmStatus') {
                fsm = {
                    state: event.state,
                    modeLocked: event.modeLocked,
                    tsms: event.tsms,
                    dashChg: event.dashChg,
                    amsOk: event.amsOk,
                    pecErrorTotal: event.pecErrorTotal,
                    faultReason: event.faultReason,
                    faultDetail: event.faultDetail,
                };
                framesThisScan += 1;
            } else if (event.kind === 'pollTiming') {
                poll = {
                    lastVPollMs: event.lastVPollMs,
                    worstVPollMs: event.worstVPollMs,
                    tSweepFailMask: event.tSweepFailMask,
                };
                framesThisScan += 1;
            } else if (event.kind === 'perIcPec') {
                // Splice this frame's ICs into the pack-wide array.
                const next = icPec.slice();
                for (let i = 0; i < event.valid; i++) {
                    const idx = event.firstIc + i;
                    if (idx < AMS_NUM_ICS) next[idx] = event.counts[i];
                }
                icPec = next;
                framesThisScan += 1;
            } else if (event.kind === 'balanceMaskA') {
                // Keep the hi half if we already have it; replace lo.
                balance = {
                    dccLo: BigInt(event.dccLo),
                    dccHi: balance?.dccHi ?? 0,
                    cyclesTotal: balance?.cyclesTotal ?? 0,
                    cyclesActive: balance?.cyclesActive ?? 0,
                };
                framesThisScan += 1;
            } else if (event.kind === 'balanceMaskB') {
                balance = {
                    dccLo: balance?.dccLo ?? 0n,
                    dccHi: event.dccHi,
                    cyclesTotal: event.cyclesTotal,
                    cyclesActive: event.cyclesActive,
                };
                framesThisScan += 1;
            } else if (event.kind === 'bootDiag') {
                boot = {
                    jumpReason: event.jumpReason,
                    appInitProgress: event.appInitProgress,
                    fdcan1StartResult: event.fdcan1StartResult,
                };
                framesThisScan += 1;
            } else if (event.kind === 'postMortem') {
                crash = {
                    stackOverflowSeen: event.stackOverflowSeen,
                    watermarkLowByte: event.watermarkLowByte,
                    taskAddrLo: event.taskAddrLo,
                    mallocFailedCount: event.mallocFailedCount,
                    clean: event.clean,
                };
                framesThisScan += 1;
            } else if (event.kind === 'fwId') {
                fw = {
                    versionMajor: event.versionMajor,
                    versionMinor: event.versionMinor,
                    versionPatch: event.versionPatch,
                    gitHash: event.gitHash,
                    blNodeId: event.blNodeId,
                };
                framesThisScan += 1;
            }
            // Ack events come during arm/disarm; they're handled by
            // the status listener, not counted toward a scan window.
        });
        scanIntervalId = setInterval(() => {
            lastScanFrames = framesThisScan;
            framesThisScan = 0;
        }, 1000);
    });

    onDestroy(async () => {
        if (unlistenFrame !== null) unlistenFrame();
        if (unlistenStatus !== null) unlistenStatus();
        if (scanIntervalId !== null) clearInterval(scanIntervalId);
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

    // Colour ramps — cells by deviation from pack mean, NTCs on an
    // absolute °C scale. Tuned by eye against typical AMS readings;
    // the operator gets a glanceable picture without having to read
    // numbers off every tile.
    function cellColor(mv: number | null): string {
        if (mv === null || packStats === null) return 'var(--bg)';
        const dev = Math.abs(mv - packStats.mean);
        if (dev > 50) return '#c2185b'; // red — outlier
        if (dev > 20) return '#f57f17'; // amber — drift
        return '#2e7d32'; // green — within band
    }

    function ntcColor(c: number | null): string {
        if (c === null) return 'var(--bg)';
        if (c < 0) return '#1976d2'; // sub-zero — alarming for Li-ion
        if (c < 20) return '#0288d1'; // cool
        if (c < 40) return '#388e3c'; // operating
        if (c < 55) return '#f57c00'; // warm
        return '#c2185b'; // hot — outside spec
    }

    // 5×19 / 5×40 grid layouts — module is rows, slot is cols.
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

    // Map the FSM state string to a pill modifier class. "run" /
    // "charge" are good states; "error" and "unknown(…)" are bad;
    // the rest are transitions.
    function fsmStateTone(state: string): 'success' | 'warning' | 'danger' | 'info' {
        if (state === 'run' || state === 'charge') return 'success';
        if (state === 'error' || state.startsWith('unknown')) return 'danger';
        return 'info';
    }

    // V-poll headroom — firmware budget is ~50 ms per the
    // CAN_MAP.md poll-timing comment. Past 40 ms = warn, past 50 ms
    // = bad.
    function pollTone(ms: number | undefined): 'success' | 'warning' | 'danger' {
        if (ms === undefined) return 'success';
        if (ms > 50) return 'danger';
        if (ms > 40) return 'warning';
        return 'success';
    }

    // Firmware-ID header chip — "AMS v1.6.0 · a1b2c3d4 · node 0x02".
    const fwLabel = $derived(
        fw === null
            ? null
            : `AMS v${fw.versionMajor}.${fw.versionMinor}.${fw.versionPatch}` +
                  ` · ${fw.gitHash.map((b) => b.toString(16).padStart(2, '0')).join('')}` +
                  ` · node 0x${fw.blNodeId.toString(16).toUpperCase().padStart(2, '0')}`,
    );

    // How many cells are actively discharging right now.
    const dischargingCount = $derived.by(() => {
        if (balance === null) return 0;
        let n = 0;
        for (let i = 0; i < AMS_NUM_CELLS; i++) if (isDischarging(i)) n += 1;
        return n;
    });

    // app_init_progress milestone 7 = clean self-exit; anything less
    // means the app didn't reach a clean boot.
    function bootTone(p: BootSnapshot): 'success' | 'warning' {
        return p.appInitProgress >= 7 && p.fdcan1StartResult === 0
            ? 'success'
            : 'warning';
    }
</script>

<div class="view">
    <header>
        <div>
            <h2>Pit diag</h2>
            {#if profile === 'ams'}
                <p class="muted">
                    AMS observer mode. Arming emits <code>0x7F0#DEADBEEF</code>;
                    the AMS replies on <code>0x7F1</code> and starts streaming
                    the diagnostic stream at 1 Hz ({AMS_EXPECTED_FRAMES_PER_SCAN}
                    frames/scan). Disarm sends the zero-payload frame;
                    firmware also clears the flag on reboot.
                </p>
            {:else}
                <p class="muted">
                    Per-ECU diagnostic observer. Select an ECU to arm its
                    pit-diag stream.
                </p>
            {/if}
        </div>
        {#if profile === 'ams' && fwLabel !== null}
            <span
                class="pill fw-chip mono"
                title="From the 0x6C6 firmware-ID frame. Compare against the build you flashed."
            >
                {fwLabel}
            </span>
        {/if}
    </header>

    <!-- ECU profile selector — AMS is wired end-to-end; ECU / uDV are
         selectable but render a placeholder until they grow a pit-diag
         stream + DBCinator frames. -->
    <div class="profile-tabs" role="radiogroup" aria-label="ECU profile">
        {#each PROFILES as p (p.id)}
            <button
                type="button"
                class="profile-tab"
                class:active={profile === p.id}
                role="radio"
                aria-checked={profile === p.id}
                onclick={() => selectProfile(p.id)}
            >
                {p.label}
            </button>
        {/each}
    </div>

    {#if !adapterReady}
        <div class="banner banner-warning">
            <strong>No adapter selected.</strong> Pick one in the
            <em>Adapters</em> view first — pit-diag needs an
            <code>--interface</code>/<code>--channel</code> pair.
        </div>
    {/if}

    {#if profile !== 'ams'}
        <!-- ECU / uDV placeholder. No arm handshake or frame contract
             exists for these yet, so we don't offer to arm — surfacing
             a clear "why" beats a button that errors. -->
        <div class="card placeholder-card">
            <h3>{profile === 'ecu' ? 'ECU' : 'uDV'} pit-diag isn't available yet</h3>
            <p class="muted">
                Only the AMS exposes a pit-diag arm handshake
                (<code>0x7F0</code>) and a decoded frame stream
                (<code>0x6C0..=0x6C8</code>) today. The
                {profile === 'ecu' ? 'ECU' : 'uDV'} has no pit-diag
                protocol or frames defined in IFS08-DBCinator yet, so
                there's nothing to arm or decode.
            </p>
            <p class="muted">
                Once the firmware team ships a pit-diag stream and
                publishes its frames in the DBC, this panel grows the
                same live grids you see under <strong>AMS</strong>. Until
                then, switch to <strong>AMS</strong> above, or use the
                <strong>Bus monitor</strong> / <strong>Signals</strong>
                views to watch raw or DBC-decoded traffic from any node.
            </p>
        </div>
    {:else}
    <!-- Controls row -->
    <div class="toolbar card card-tight">
        {#if armState.kind === 'idle' || armState.kind === 'error'}
            <button
                type="button"
                class="btn btn-primary"
                disabled={!adapterReady}
                onclick={confirmArm}
            >
                Enable AMS pit-diag
            </button>
        {:else if armState.kind === 'confirming'}
            <div class="confirm">
                <span class="confirm-text">Arm AMS pit-diag stream?</span>
                <button type="button" class="btn btn-primary" onclick={arm}>
                    Yes, arm
                </button>
                <button type="button" class="btn" onclick={cancelArm}>
                    Cancel
                </button>
            </div>
        {:else if armState.kind === 'arming'}
            <button type="button" class="btn" disabled>Arming…</button>
        {:else if armState.kind === 'armed'}
            <button type="button" class="btn btn-danger" onclick={disarm}>
                Disable
            </button>
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
        <div class="banner banner-danger">
            <strong>Arm failed:</strong>
            {armState.message}
        </div>
    {/if}

    <!--
        Safety net: when the observed frames/scan count drifts from
        the expected total by more than ±2, banner a warning. This
        catches the AMS team adding or removing frames before silent
        miscalibration bites the operator. (Note: it can't catch an
        in-frame byte repurposing like #276's fault-reason bytes —
        the count stays the same. Those are caught by tracking the
        AMS CAN_MAP per frame, not by this counter.)
    -->
    {#if schemaDriftSuspected}
        <div class="banner banner-warning">
            <strong>Schema drift suspected.</strong>
            Last scan carried {lastScanFrames} frames, expected
            {AMS_EXPECTED_FRAMES_PER_SCAN}. The AMS firmware's pit-diag
            wire shape may have changed since the host's layout was
            last verified — the panels below may be partial or
            mis-routed. Verify against
            <code>src/pit_diag/mod.rs</code> and re-run.
        </div>
    {/if}

    <!-- Crash post-mortem (0x6C5) — only when the previous boot
         recorded a fault. Top placement: this is the loudest signal
         in the stream. -->
    {#if crash !== null && !crash.clean}
        <div class="banner banner-danger">
            <strong>Crash on previous boot.</strong>
            {#if crash.stackOverflowSeen}
                Stack overflow in task at
                <code class="mono">0x{crash.taskAddrLo.toString(16).toUpperCase().padStart(8, '0')}</code>
                (min free watermark {crash.watermarkLowByte} words).
            {/if}
            {#if crash.mallocFailedCount > 0}
                {crash.mallocFailedCount} malloc failure{crash.mallocFailedCount === 1 ? '' : 's'}.
            {/if}
            From the <code>0x6C5</code> post-mortem frame — recorded on the
            boot <em>before</em> this stream started.
        </div>
    {/if}

    <!-- FSM + poll-timing row (only renders once we've seen at
         least one of each). -->
    {#if fsm !== null || poll !== null}
        <div class="diag-row">
            {#if fsm !== null}
                <section class="card diag-card">
                    <div class="card-header">
                        <h3>FSM extended status</h3>
                        <span class="muted small mono">0x6C0</span>
                    </div>
                    <div class="diag-grid">
                        <div class="diag-cell">
                            <span class="diag-label">State</span>
                            <span class="pill pill-{fsmStateTone(fsm.state)}">
                                {fsm.state}
                            </span>
                        </div>
                        <div class="diag-cell">
                            <span class="diag-label">Mode</span>
                            <span class="pill">{fsm.modeLocked}</span>
                        </div>
                        <div class="diag-cell">
                            <span class="diag-label">TSMS</span>
                            <span class="led" class:on={fsm.tsms}></span>
                        </div>
                        <div class="diag-cell">
                            <span class="diag-label" title="Momentary button (AMS #316): live GPIO level, not latched. Reads 0 during Run/Charge once the button is released — that's normal, not a fault. Run/Charge are held by TSMS alone.">
                                DASH_CHG
                                <span class="hint-mark">ⓘ</span>
                            </span>
                            <span class="led" class:on={fsm.dashChg}></span>
                        </div>
                        <div class="diag-cell">
                            <span class="diag-label">AMS_OK</span>
                            <span class="led" class:on={fsm.amsOk}></span>
                        </div>
                        <div class="diag-cell">
                            <span class="diag-label">PEC errors</span>
                            <span class="diag-value mono">
                                {fsm.pecErrorTotal}
                            </span>
                        </div>
                    </div>
                    {#if fsm.faultReason !== 'none'}
                        <div class="fault-line">
                            <span class="pill pill-danger">latched</span>
                            <span class="fault-reason mono">{fsm.faultReason}</span>
                            <span class="muted small">detail 0x{fsm.faultDetail
                                    .toString(16)
                                    .toUpperCase()
                                    .padStart(2, '0')}</span>
                        </div>
                    {/if}
                </section>
            {/if}

            {#if poll !== null}
                <section class="card diag-card">
                    <div class="card-header">
                        <h3>Poll timing</h3>
                        <span class="muted small mono">0x6C1</span>
                    </div>
                    <div class="diag-grid">
                        <div class="diag-cell">
                            <span class="diag-label">V-poll last</span>
                            <span class="pill pill-{pollTone(poll.lastVPollMs)} mono">
                                {poll.lastVPollMs} ms
                            </span>
                        </div>
                        <div class="diag-cell">
                            <span class="diag-label">V-poll worst</span>
                            <span class="pill pill-{pollTone(poll.worstVPollMs)} mono">
                                {poll.worstVPollMs} ms
                            </span>
                        </div>
                        <div class="diag-cell wide">
                            <span class="diag-label">T-sweep fail mask</span>
                            <span class="diag-value mono">
                                0x{poll.tSweepFailMask
                                    .toString(16)
                                    .toUpperCase()
                                    .padStart(8, '0')}
                                {#if tSweepFailBits > 0}
                                    <span class="muted">
                                        · {tSweepFailBits} channel{tSweepFailBits === 1 ? '' : 's'} flagged
                                    </span>
                                {/if}
                            </span>
                        </div>
                    </div>
                    <p class="muted small poll-hint">
                        Budget &lt; 50 ms per scan. Worst-case &gt; 50 ms means
                        the BMS slave bus is saturating; check ISO-SPI line
                        integrity + sample-and-poll batching.
                    </p>
                </section>
            {/if}
        </div>
    {/if}

    <!-- Per-IC PEC counts (0x6C7 / 0x6C8) -->
    {#if icPec.some((c) => c !== null)}
        <section class="card">
            <div class="card-header">
                <h3>Per-IC PEC errors</h3>
                <span class="muted small mono">0x6C7 / 0x6C8</span>
            </div>
            <div class="ic-row">
                {#each icPec as count, ic (ic)}
                    <div
                        class="ic-cell"
                        class:flagged={count !== null && count > 0}
                        title="IC {ic} (module {Math.floor(ic / 2) + 1} {ic % 2 === 0
                            ? 'upper'
                            : 'lower'}) — {count ?? '—'} PEC errors"
                    >
                        <span class="ic-idx">IC{ic}</span>
                        <span class="ic-count mono">{count ?? '—'}</span>
                    </div>
                {/each}
            </div>
            <p class="muted small ic-hint">
                Saturating per-IC CRC-error counter on the slave-bus link.
                IC <code>2m</code>/<code>2m+1</code> = upper/lower of module
                <code>m</code>. Any non-zero count points at ISO-SPI integrity
                on that chain.
            </p>
        </section>
    {/if}

    <!-- Balance (0x6C2 / 0x6C3) — which cells are actively discharging. -->
    {#if balance !== null}
        <section class="card">
            <div class="card-header">
                <h3>Cell balancing</h3>
                <span class="muted small">
                    {dischargingCount}/{AMS_NUM_CELLS} discharging ·
                    {balance.cyclesActive}/{balance.cyclesTotal} cycles active
                </span>
            </div>
            <div class="grid">
                {#each Array(AMS_NUM_MODULES) as _, m (m)}
                    <div class="grid-row" role="row">
                        <span class="row-label">M{m + 1}</span>
                        {#each Array(AMS_CELLS_PER_MODULE) as _, s (s)}
                            {@const idx = m * AMS_CELLS_PER_MODULE + s}
                            <div
                                class="tile bal-tile"
                                class:discharging={isDischarging(idx)}
                                title="Cell {idx + 1} — {isDischarging(idx)
                                    ? 'discharging'
                                    : 'idle'}"
                            ></div>
                        {/each}
                    </div>
                {/each}
            </div>
            <p class="muted small bal-hint">
                Highlighted = DCC (discharge) bit set this scan. Heavy
                discharge on a handful of cells is normal mid-balance; the
                pack-spread bar below should be shrinking while it runs.
            </p>
        </section>
    {/if}

    <!-- Boot diagnostics (0x6C4). -->
    {#if boot !== null}
        <section class="card">
            <div class="card-header">
                <h3>Boot diagnostics</h3>
                <span class="muted small mono">0x6C4</span>
            </div>
            <div class="diag-grid">
                <div class="diag-cell">
                    <span class="diag-label">Jump reason</span>
                    <span class="pill">{boot.jumpReason}</span>
                </div>
                <div class="diag-cell">
                    <span class="diag-label">Init progress</span>
                    <span class="pill pill-{bootTone(boot)} mono">
                        {boot.appInitProgress}/7
                    </span>
                </div>
                <div class="diag-cell">
                    <span class="diag-label">FDCAN start</span>
                    <span
                        class="pill mono"
                        class:pill-success={boot.fdcan1StartResult === 0}
                        class:pill-danger={boot.fdcan1StartResult !== 0}
                    >
                        {boot.fdcan1StartResult === 0
                            ? 'HAL_OK'
                            : `0x${boot.fdcan1StartResult.toString(16).toUpperCase()}`}
                    </span>
                </div>
            </div>
            <p class="muted small boot-hint">
                Init progress 7 = the app booted through every milestone.
                A jump reason of <code>canTrigger</code>/<code>manual</code>
                means it came up from the bootloader rather than a cold
                power-on.
            </p>
        </section>
    {/if}

    <!-- Pack-spread bar -->
    {#if packStats !== null}
        <section class="card">
            <div class="card-header">
                <h3>Pack spread</h3>
                <span class="spread-mv mono">
                    {packStats.spread} mV (max − min)
                </span>
            </div>
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
    <section class="card">
        <div class="card-header">
            <h3>Cell voltages</h3>
            <span class="muted small">
                5 modules × 19 cells = 95. Colour = deviation from pack mean.
            </span>
        </div>
        <div class="grid">
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
    <section class="card">
        <div class="card-header">
            <h3>NTC temperatures</h3>
            <span class="muted small">
                {#if ntcStats !== null}
                    range {ntcStats.min}…{ntcStats.max} °C ·
                    {ntcStats.count}/{AMS_NUM_NTCS} reading
                {:else}
                    5 modules × 40 NTCs = 200.
                {/if}
            </span>
        </div>
        <div class="grid">
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
    {/if}
</div>

<style>
    /* Most chrome (.view, .card, .card-header, .banner-*, .btn,
       .pill, .muted, .small, .mono) comes from app.css. Local
       styles cover only the bits this view needs that aren't in
       the design system: the toolbar layout, the FSM/poll-timing
       diag grid + LEDs, the pack-spread bar, and the cell-V / NTC
       tile grids. */

    /* ECU profile selector — AMS / ECU / uDV pill tabs sharing a
       track, the active one filled with the accent. */
    .profile-tabs {
        display: inline-flex;
        gap: 2px;
        padding: 2px;
        background: var(--bg);
        border: 1px solid var(--border);
        border-radius: var(--radius-md);
        align-self: flex-start;
    }
    .profile-tab {
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
    .profile-tab:hover {
        color: var(--text);
    }
    .profile-tab.active {
        background: var(--accent);
        color: var(--accent-contrast, #fff);
        font-weight: 600;
    }

    /* Placeholder card for the not-yet-wired ECU / uDV profiles. */
    .placeholder-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
    }
    .placeholder-card h3 {
        margin: 0;
    }
    .placeholder-card p {
        margin: 0;
        max-width: 70ch;
        line-height: 1.6;
    }

    /* Toolbar — controls + live stats. Built on .card .card-tight. */
    .toolbar {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
    }
    .stat {
        display: inline-flex;
        gap: var(--space-1);
        align-items: center;
        font-size: var(--text-sm);
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
        color: var(--success);
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
        gap: var(--space-3);
        align-items: center;
    }
    .confirm-text {
        font-size: var(--text-sm);
        color: var(--text);
    }

    /* Diag row — FSM card + poll-timing card side by side; wraps
       to stacked when the page narrows. */
    .diag-row {
        display: grid;
        grid-template-columns: 1fr 1fr;
        gap: var(--space-3);
    }
    @media (max-width: 900px) {
        .diag-row {
            grid-template-columns: 1fr;
        }
    }

    /* Diag grid — labelled key/value cells laid out auto-fill. The
       .wide modifier lets the T-sweep mask span both columns. */
    .diag-grid {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
        gap: var(--space-3);
    }
    .diag-cell {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
    }
    .diag-cell.wide {
        grid-column: 1 / -1;
    }
    .diag-label {
        font-size: var(--text-xs);
        color: var(--text-muted);
        text-transform: uppercase;
        letter-spacing: 0.05em;
    }
    .diag-value {
        font-size: var(--text-base);
        color: var(--text);
    }

    /* LED — tiny circle, muted when off, accent when on. Mirrors
       the cockpit's binary input semantics. */
    .led {
        width: 14px;
        height: 14px;
        border-radius: 50%;
        background: var(--bg-soft);
        border: 1px solid var(--border-strong);
        transition: background var(--motion-fast), border-color var(--motion-fast);
    }
    .led.on {
        background: var(--success);
        border-color: var(--success);
        box-shadow: 0 0 6px rgba(74, 222, 128, 0.55);
    }

    .poll-hint {
        margin: var(--space-3) 0 0;
    }

    /* Fault line — only rendered when the FSM latched ERROR. Sits
       below the diag grid inside the FSM card. */
    .fault-line {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        margin-top: var(--space-3);
        padding-top: var(--space-3);
        border-top: 1px solid var(--border);
    }
    .fault-reason {
        color: var(--danger);
        font-weight: 600;
    }

    /* Per-IC PEC grid — 10 compact tiles, one per monitor IC. Tile
       turns danger-toned when its count is non-zero. */
    .ic-row {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(64px, 1fr));
        gap: var(--space-2);
    }
    .ic-cell {
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: 2px;
        padding: var(--space-2);
        border: 1px solid var(--border);
        border-radius: var(--radius-md);
        background: var(--surface);
        cursor: help;
    }
    .ic-cell.flagged {
        border-color: var(--danger);
        background: var(--danger-soft);
    }
    .ic-idx {
        font-size: var(--text-xs);
        color: var(--text-muted);
    }
    .ic-count {
        font-size: var(--text-base);
        color: var(--text);
    }
    .ic-cell.flagged .ic-count {
        color: var(--danger);
        font-weight: 600;
    }
    .ic-hint {
        margin: var(--space-3) 0 0;
    }

    /* Firmware-ID header chip — sits top-right of the view header. */
    .fw-chip {
        align-self: flex-start;
        white-space: nowrap;
    }

    /* Small ⓘ affordance next to a label that carries a tooltip. */
    .hint-mark {
        color: var(--text-muted);
        cursor: help;
        font-size: var(--text-xs);
    }

    /* Balance grid reuses the cell-V tile grid; the discharging
       state lights the tile accent-amber (active DCC). */
    .bal-tile {
        height: 22px;
        background: var(--bg);
        border: 1px solid var(--border);
    }
    .bal-tile.discharging {
        background: var(--accent);
        border-color: var(--accent);
    }
    .bal-hint,
    .boot-hint {
        margin: var(--space-3) 0 0;
    }

    /* Pack-spread bar — fills proportional to spread in mV,
       clamped at 100mV. Warn/bad colour breakpoints help the
       operator glance at the pack health. */
    .spread-mv {
        font-size: var(--text-sm);
        color: var(--text);
    }
    .spread-row {
        display: flex;
        align-items: center;
        gap: var(--space-3);
    }
    .spread-bar {
        flex: 1;
        height: 10px;
        background: var(--bg);
        border-radius: var(--radius-md);
        border: 1px solid var(--border);
        overflow: hidden;
    }
    .spread-fill {
        height: 100%;
        background: var(--success);
        transition:
            width 240ms ease-out,
            background 240ms ease-out;
    }
    .spread-bar.warn .spread-fill {
        background: var(--warning);
    }
    .spread-bar.bad .spread-fill {
        background: var(--danger);
    }
    .spread-min,
    .spread-max {
        font-size: var(--text-xs);
        color: var(--text-muted);
    }
    .spread-hint {
        margin: var(--space-2) 0 0;
    }

    /* Cell / NTC tile grids. The cell grid wants legible voltage
       numbers; the NTC grid is heatmap-only so its tiles are
       smaller + label-less. */
    .grid {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
    }
    .grid-row {
        display: flex;
        gap: var(--space-1);
        align-items: center;
    }
    .row-label {
        width: 24px;
        flex: 0 0 24px;
        font-family: var(--font-mono);
        font-size: var(--text-xs);
        color: var(--text-muted);
    }
    .tile {
        flex: 1;
        min-width: 0;
        height: 38px;
        border-radius: var(--radius-sm);
        display: flex;
        align-items: center;
        justify-content: center;
        font-size: var(--text-xs);
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
