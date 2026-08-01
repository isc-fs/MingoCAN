<!--
    Pedal calibration (#534). Embedded in the dedicated ECU tab of the
    Telemetry view rather than living as its own top-level entry.

    It is the one thing in that view which WRITES to the car -- the values
    it commits gate the EV.2.3 torque cut and the driverless R2D -- so it
    keeps its own confirmation gates rather than inheriting the
    surrounding view's read-only posture.

    Sitting here does mean it shares the parent's armed session, which is
    what it needs anyway: live pedal values come from 0x701, and an
    operator cannot capture what they cannot see. Arming is the parent's
    job; this panel only reports when it is missing.

    Two modes, because they have different dependencies:

    - MEASURE ONLY needs nothing but the existing 0x701 stream. It works
      against any ECU firmware, including one with no calibration
      protocol at all, and produces the numbers a firmware engineer would
      paste into ecu_config.hpp. That is the fallback if the session
      protocol is unavailable, and it is also the honest way to inspect
      pedals without touching anything.
    - GUIDED SESSION drives 0x7E2-0x7E5 and commits to NVM.

    Both need an ARMED ECU pit-diag session — the live pedal values come
    from 0x701, and an operator cannot capture what they cannot see.
-->
<script lang="ts">
    import { onDestroy, onMount } from 'svelte';
    import type { UnlistenFn } from '@tauri-apps/api/event';

    import {
        CAL_POINTS,
        onPitDiagFrame,
        pitCalCommand,
        type CalPoint,
        type CalStagedSet,
    } from './pit_diag';

    /** Live pedal readings, straight from the parent's `0x701` decode.
     *  Passed in rather than re-derived so there is one copy of the truth. */
    interface PedalReads {
        apps1Raw: number;
        apps2Raw: number;
        brakeRaw: number;
        apps1Pct: number;
        apps2Pct: number;
    }

    interface Props {
        /** Whether the parent has an ECU stream armed. Without one there is
         *  nothing to capture and nowhere for a command to go. */
        armed: boolean;
        pedals: PedalReads | null;
        /** Preconditions, from `0x700` / `0x707` on the same stream. */
        fsmState: string | null;
        tsActive: boolean | null;
        motorRpm: number | null;
        torquePct: number | null;
    }
    const { armed, pedals, fsmState, tsActive, motorRpm, torquePct }: Props = $props();

    // ---- Stability window ----
    //
    // The job of this gate is to tell "the operator is still moving the
    // pedal" apart from "the signal is at rest and simply noisy". It is
    // NOT a noise filter, and it must not try to be one.
    //
    // It is also the ONLY stability check in the system: `SampleUnstable`
    // exists in the protocol but the firmware never emits it (grep
    // cal_session.cpp), so the ECU accepts whatever it is handed. Nothing
    // downstream will catch a capture taken mid-sweep.
    //
    // Thresholds are per-signal because the two sensors are nothing alike:
    //
    //   Brake — the released pedal sits near 580 counts but its noise
    //   reaches ~730 on the car (ecu_config.hpp, on-car cal 2026-06-27).
    //   That is a ~150-count band at rest. Anything tighter than that can
    //   never be satisfied, and BRAKE_REST is the capture the derived
    //   arming threshold depends on.
    //
    //   APPS — the bench calibration applies ~13 counts of margin at each
    //   endpoint, so the channels are far quieter.
    //
    // Both sit well below real pedal movement: APPS travel is 860 and 680
    // counts, brake travel a couple of thousand, so a pedal actually being
    // moved blows past these by an order of magnitude.
    const STABILITY_WINDOW_MS = 1000;
    /** ~5 % of the smaller APPS span (680 counts). */
    const STABLE_SPREAD_APPS = 40;
    /** Clears the measured ~150-count rest-noise band with margin, and is
     *  still under a tenth of usable brake travel. */
    const STABLE_SPREAD_BRAKE = 200;

    interface Sample {
        t: number;
        apps1: number;
        apps2: number;
        brake: number;
    }
    // Deliberately NOT $state. The effect that fills this buffer has to
    // read the previous contents to prune them, and reading + writing the
    // same reactive value inside one effect is an infinite update cycle —
    // Svelte 5 aborts the component when it sees one, which silently takes
    // onMount (and therefore the event listener) down with it. The derived
    // spreads below are the reactive surface instead.
    let samples: Sample[] = [];
    let appsSpread = $state(Infinity);
    let brakeSpread = $state(Infinity);

    function spreadOf(pick: (s: Sample) => number): number {
        if (samples.length < 3) return Infinity;
        const vs = samples.map(pick);
        return Math.max(...vs) - Math.min(...vs);
    }

    /** Which signal a capture point watches — APPS points gate on the two
     *  accelerator channels, brake points on the brake. */
    function isAppsPoint(p: CalPoint): boolean {
        return p === 'appsRest' || p === 'appsFull' || p === 'appsMid';
    }
    function pointSpread(p: CalPoint): number {
        return isAppsPoint(p) ? appsSpread : brakeSpread;
    }
    function pointTolerance(p: CalPoint): number {
        return isAppsPoint(p) ? STABLE_SPREAD_APPS : STABLE_SPREAD_BRAKE;
    }
    function pointIsStable(p: CalPoint): boolean {
        return pointSpread(p) <= pointTolerance(p);
    }

    // ---- Session state (0x7E3) ----

    interface CalStatus {
        sessionState: string;
        result: string;
        resultMessage: string;
        resultIsError: boolean;
        missingPoints: string[];
        allPointsCaptured: boolean;
        validationMessages: string[];
        calLoad: string;
        calLoadIsFallback: boolean;
        pendingRead: string | null;
    }
    let status = $state<CalStatus | null>(null);

    interface ValueSet {
        apps1Min: number;
        apps1Max: number;
        apps2Min: number;
        apps2Max: number;
        brakeRest: number;
        brakeArm: number;
        brakeDvHard: number;
        brakePressed: number;
    }
    // Read-backs. `pendingRead` on the preceding status frame is the only
    // thing that says which of these an incoming pair belongs to, so it is
    // latched here and consumed when the value frames arrive.
    let pendingRead = $state<string | null>(null);
    let stored = $state<Partial<ValueSet>>({});
    let staged = $state<Partial<ValueSet>>({});

    function applyApps(target: Partial<ValueSet>, e: { apps1Min: number; apps1Max: number; apps2Min: number; apps2Max: number }) {
        target.apps1Min = e.apps1Min;
        target.apps1Max = e.apps1Max;
        target.apps2Min = e.apps2Min;
        target.apps2Max = e.apps2Max;
    }
    function applyBrake(target: Partial<ValueSet>, e: { brakeRest: number; brakeArm: number; brakeDvHard: number; brakePressed: number }) {
        target.brakeRest = e.brakeRest;
        target.brakeArm = e.brakeArm;
        target.brakeDvHard = e.brakeDvHard;
        target.brakePressed = e.brakePressed;
    }

    const stagedComplete = $derived(
        (['apps1Min', 'apps1Max', 'apps2Min', 'apps2Max', 'brakeRest', 'brakeArm', 'brakeDvHard', 'brakePressed'] as const).every(
            (k) => typeof staged[k] === 'number',
        ),
    );

    // ---- View state ----

    type Mode = 'idle' | 'measure' | 'session';
    let mode = $state<Mode>('idle');
    let busy = $state(false);
    let error = $state<string | null>(null);
    let stepIdx = $state(0);
    let confirmedCommit = $state(false);
    let committedAt = $state<number | null>(null);
    let showResetConfirm = $state(false);

    // Clamped rather than nullable: a nullable $derived would need the
    // template to narrow it before every use, which is fragile. stepIdx
    // never exceeds the last point.
    const currentPoint = $derived(CAL_POINTS[Math.min(stepIdx, CAL_POINTS.length - 1)]);

    // Measure-only captures, held entirely client-side.
    let measured = $state<Record<string, { apps1: number; apps2: number; brake: number }>>({});

    let unlistenFrame: UnlistenFn | undefined;

    // Feed the stability window from the parent's readings. The parent
    // already decodes 0x701 for its own pedal card; duplicating that here
    // would mean two copies drifting apart under load.
    $effect(() => {
        const p = pedals;
        if (p === null) return;
        const now = Date.now();
        samples = samples.filter((s) => now - s.t < STABILITY_WINDOW_MS);
        samples.push({ t: now, apps1: p.apps1Raw, apps2: p.apps2Raw, brake: p.brakeRaw });
        appsSpread = Math.max(spreadOf((s) => s.apps1), spreadOf((s) => s.apps2));
        brakeSpread = spreadOf((s) => s.brake);
    });

    onMount(async () => {
        // Only the calibration frames — the parent owns everything else on
        // this channel. Tauri allows multiple listeners per event, so this
        // sits alongside the parent's without interfering.
        unlistenFrame = await onPitDiagFrame((event) => {
            if (event.kind === 'calStatus') {
                status = {
                    sessionState: event.sessionState,
                    result: event.result,
                    resultMessage: event.resultMessage,
                    resultIsError: event.resultIsError,
                    missingPoints: event.missingPoints,
                    allPointsCaptured: event.allPointsCaptured,
                    validationMessages: event.validationMessages,
                    calLoad: event.calLoad,
                    calLoadIsFallback: event.calLoadIsFallback,
                    pendingRead: event.pendingRead,
                };
                if (event.pendingRead !== null) pendingRead = event.pendingRead;
                if (event.sessionState === 'Committed') committedAt = Date.now();
            } else if (event.kind === 'calApps') {
                applyApps(pendingRead === 'Staged' ? staged : stored, event);
            } else if (event.kind === 'calBrake') {
                applyBrake(pendingRead === 'Staged' ? staged : stored, event);
            }
        });
    });

    onDestroy(() => {
        // #534: aborting on navigate-away is the client's job. The ECU
        // times out after 30 s, but leaving a session open in the interim
        // means the next operator finds a half-captured set they did not
        // take. Best-effort — if the session already ended this errors
        // harmlessly.
        if (mode === 'session') void pitCalCommand('abort').catch(() => {});
        unlistenFrame?.();
    });

    async function run(fn: () => Promise<void>) {
        busy = true;
        error = null;
        try {
            await fn();
        } catch (e) {
            error = String(e);
        } finally {
            busy = false;
        }
    }

    const startSession = () =>
        run(async () => {
            stepIdx = 0;
            confirmedCommit = false;
            committedAt = null;
            staged = {};
            stored = {};
            await pitCalCommand('enter');
            // Show the operator what is in force before they change it.
            await pitCalCommand('readStored');
            mode = 'session';
        });

    const capture = (p: CalPoint) =>
        run(async () => {
            await pitCalCommand('capture', p);
            if (stepIdx < CAL_POINTS.length - 1) stepIdx += 1;
            else await pitCalCommand('readStaged');
        });

    const abort = () =>
        run(async () => {
            await pitCalCommand('abort');
            mode = 'idle';
            status = null;
        });

    const commit = () =>
        run(async () => {
            // The values passed here are exactly the ones rendered in the
            // review table. The ECU CRCs them, so a mismatch between what
            // was shown and what is sent is rejected rather than applied.
            await pitCalCommand('commit', undefined, staged as CalStagedSet);
            // #534 requires an automatic read-back after every commit.
            await pitCalCommand('readStored');
        });

    const resetDefaults = () =>
        run(async () => {
            await pitCalCommand('resetDefaults');
            await pitCalCommand('readStored');
            showResetConfirm = false;
        });

    // Measure-only: record the current reading against a point, purely
    // client-side. Nothing is sent to the ECU.
    function measure(p: CalPoint) {
        if (pedals === null) return;
        measured[p] = { apps1: pedals.apps1Raw, apps2: pedals.apps2Raw, brake: pedals.brakeRaw };
    }

    const measuredSnippet = $derived.by(() => {
        const rest = measured['appsRest'];
        const full = measured['appsFull'];
        const bRest = measured['brakeRest'];
        const bPressed = measured['brakePressed'];
        if (!rest || !full) return null;
        const lines = [
            `Apps1AdcMin = ${rest.apps1};`,
            `Apps1AdcMax = ${full.apps1};`,
            `Apps2AdcMin = ${rest.apps2};`,
            `Apps2AdcMax = ${full.apps2};`,
        ];
        if (bRest) lines.push(`// brake released raw: ${bRest.brake}`);
        if (bPressed) lines.push(`// brake pressed  raw: ${bPressed.brake}`);
        return lines.join('\n');
    });

    const preconditionsOk = $derived(
        tsActive === false && fsmState !== 'active' && motorRpm === 0 && torquePct === 0,
    );

    function pointLabel(p: string): string {
        switch (p) {
            case 'appsRest': case 'AppsRest': return 'Accelerator — released';
            case 'appsFull': case 'AppsFull': return 'Accelerator — full travel';
            case 'appsMid': case 'AppsMid': return 'Accelerator — half travel';
            case 'brakeRest': case 'BrakeRest': return 'Brake — released';
            case 'brakePressed': case 'BrakePressed': return 'Brake — pressed';
            default: return p;
        }
    }
    function instruction(p: CalPoint): string {
        switch (p) {
            case 'appsRest': return 'Release the accelerator fully and let the reading settle.';
            case 'appsFull': return 'Press the accelerator to the mechanical stop and hold it.';
            case 'appsMid': return 'Hold the accelerator at roughly half travel and keep it steady.';
            case 'brakeRest': return 'Release the brake fully and let the reading settle.';
            case 'brakePressed': return 'Press the brake firmly, as in hard braking, and hold it.';
        }
    }

    const VALUE_ROWS = [
        { key: 'apps1Min', label: 'APPS1 rest' },
        { key: 'apps1Max', label: 'APPS1 full' },
        { key: 'apps2Min', label: 'APPS2 rest' },
        { key: 'apps2Max', label: 'APPS2 full' },
        { key: 'brakeRest', label: 'Brake rest' },
        { key: 'brakeArm', label: 'Brake arm (R2D)', derived: true },
        { key: 'brakeDvHard', label: 'Brake DV hard (driverless R2D)', derived: true },
        { key: 'brakePressed', label: 'Brake pressed (EV.2.3)' },
    ] as const;
</script>

<section class="cal-panel">
    <header class="card-header">
        <h3>Pedal calibration</h3>
        <p class="muted">
            Set the accelerator and brake endpoints on the ECU over CAN — no rebuild, no
            reflash. These values gate the EV.2.3 torque cut and the driverless
            ready-to-drive, so the car must be on stands.
        </p>
    </header>

    {#if error !== null}
        <div class="banner banner-danger"><strong>Error:</strong> {error}</div>
    {/if}

    <!-- The car must be on stands before anything else happens. #534
         requires this before the session opens, not after. -->
    <div class="banner banner-warning">
        <strong>Wheels off the ground.</strong> Calibration moves the thresholds that decide
        when torque is allowed. Put the car on stands before starting.
    </div>

    {#if !armed}
        <div class="card">
            <h3 class="card-h">Telemetry not armed</h3>
            <p class="muted small">
                Calibration reads live pedal values from the ECU stream — without it there is
                nothing to capture, and no path for a calibration command to travel on. Use
                <strong>Enable ECU telemetry</strong> above to start it.
            </p>
        </div>
    {:else}
        <!-- Live readings, shared by both modes. -->
        <div class="card">
            <h3 class="card-h">Live pedal readings</h3>
            {#if pedals !== null}
                <div class="reads">
                    <span class="stat"><span>APPS1</span><strong class="mono">{pedals.apps1Raw}</strong></span>
                    <span class="stat"><span>APPS2</span><strong class="mono">{pedals.apps2Raw}</strong></span>
                    <span class="stat"><span>Brake</span><strong class="mono">{pedals.brakeRaw}</strong></span>
                    <span class="stat"><span>APPS1 %</span><strong>{pedals.apps1Pct}%</strong></span>
                    <span class="stat"><span>APPS2 %</span><strong>{pedals.apps2Pct}%</strong></span>
                </div>
                <div class="reads">
                    <span class="stat" class:bad={appsSpread > STABLE_SPREAD_APPS}>
                        <span>accelerator spread (1 s)</span>
                        <strong class="mono">{appsSpread === Infinity ? '—' : appsSpread}</strong>
                    </span>
                    <span class="stat" class:bad={brakeSpread > STABLE_SPREAD_BRAKE}>
                        <span>brake spread (1 s)</span>
                        <strong class="mono">{brakeSpread === Infinity ? '—' : brakeSpread}</strong>
                    </span>
                </div>
            {:else}
                <p class="muted small">Waiting for the first pedal frame…</p>
            {/if}
        </div>

        {#if status !== null && status.calLoadIsFallback}
            <div class="banner banner-warning">
                <strong>The ECU is running compile-time defaults.</strong>
                Reported calibration state: <code>{status.calLoad}</code>. Anything other than
                <code>Loaded</code> means no stored calibration is in force — either none was ever
                committed, or one was found and rejected at boot.
            </div>
        {/if}

        {#if mode === 'idle'}
            <div class="card">
                <h3 class="card-h">Choose a mode</h3>
                <div class="badge-row">
                    <button class="btn" onclick={() => (mode = 'measure')} disabled={busy}>
                        Measure only
                    </button>
                    <button class="btn btn-primary" onclick={startSession} disabled={busy}>
                        Guided calibration
                    </button>
                </div>
                <p class="muted small">
                    <strong>Measure only</strong> records the readings and shows you the numbers —
                    it never writes to the ECU, and works even against firmware without the
                    calibration protocol. <strong>Guided calibration</strong> captures on the ECU
                    and commits to flash, surviving a reflash.
                </p>
            </div>
        {/if}

        <!-- ---- Measure-only ---- -->
        {#if mode === 'measure'}
            <div class="card">
                <h3 class="card-h">Measure only — nothing is written</h3>
                {#each CAL_POINTS as p (p)}
                    <div class="meter-row">
                        <span class="meter-label">{pointLabel(p)}</span>
                        <span class="muted small">{instruction(p)}</span>
                        <button class="btn" onclick={() => measure(p)} disabled={pedals === null || !pointIsStable(p)}>
                            {measured[p] ? 'Re-record' : 'Record'}
                        </button>
                        {#if measured[p]}
                            <span class="mono small">
                                A1 {measured[p].apps1} · A2 {measured[p].apps2} · B {measured[p].brake}
                            </span>
                        {:else if !pointIsStable(p)}
                            <span class="muted small">hold steady…</span>
                        {/if}
                    </div>
                {/each}
                {#if measuredSnippet !== null}
                    <h4 class="card-h">Values for <code>ecu_config.hpp</code></h4>
                    <p class="muted small">
                        The firmware applies a small margin at each end rather than using the raw
                        captures directly — check the comments next to the existing constants
                        before pasting.
                    </p>
                    <pre class="mono">{measuredSnippet}</pre>
                {/if}
                <button class="btn" onclick={() => (mode = 'idle')}>Done</button>
            </div>
        {/if}

        <!-- ---- Guided session ---- -->
        {#if mode === 'session'}
            {#if status !== null && status.resultIsError}
                <div class="banner banner-danger">
                    <strong>{status.result}.</strong> {status.resultMessage}
                </div>
            {/if}
            {#if status !== null && status.validationMessages.length > 0}
                <div class="banner banner-danger">
                    <strong>The calibration was not accepted:</strong>
                    <ul>
                        {#each status.validationMessages as m (m)}<li>{m}</li>{/each}
                    </ul>
                </div>
            {/if}

            <div class="card">
                <h3 class="card-h">Preconditions</h3>
                <div class="flags">
                    <span class="flag" class:on={tsActive === false}>TS inactive</span>
                    <span class="flag" class:on={fsmState !== null && fsmState !== 'active'}>FSM not driving</span>
                    <span class="flag" class:on={motorRpm === 0}>Motor stopped</span>
                    <span class="flag" class:on={torquePct === 0}>Zero torque</span>
                </div>
                {#if !preconditionsOk}
                    <p class="muted small">
                        The ECU checks these itself and refuses to open a session unless all four
                        hold. They are shown here so a refusal is not a surprise.
                    </p>
                {/if}
            </div>

            {#if status !== null && !status.allPointsCaptured}
                <div class="card">
                    <h3 class="card-h">
                        Step {stepIdx + 1} of {CAL_POINTS.length} — {pointLabel(currentPoint)}
                    </h3>
                    <p>{instruction(currentPoint)}</p>
                    {#if pedals !== null}
                        <div class="reads">
                            {#if isAppsPoint(currentPoint)}
                                <span class="stat"><span>APPS1</span><strong class="mono">{pedals.apps1Raw}</strong></span>
                                <span class="stat"><span>APPS2</span><strong class="mono">{pedals.apps2Raw}</strong></span>
                            {:else}
                                <span class="stat"><span>Brake</span><strong class="mono">{pedals.brakeRaw}</strong></span>
                            {/if}
                            <span class="stat" class:bad={!pointIsStable(currentPoint)}>
                                <span>spread</span>
                                <strong class="mono">{pointSpread(currentPoint) === Infinity ? '—' : pointSpread(currentPoint)}</strong>
                            </span>
                        </div>
                    {/if}
                    <button
                        class="btn btn-primary"
                        onclick={() => capture(currentPoint)}
                        disabled={busy || pedals === null || !pointIsStable(currentPoint)}
                        title={pointIsStable(currentPoint)
                            ? 'Capture this point'
                            : `Still moving — spread ${pointSpread(currentPoint)} counts, needs ${pointTolerance(currentPoint)} or less. The ECU does not check this, so a capture taken mid-sweep would be accepted and stored.`}
                    >
                        Capture
                    </button>
                    {#if !pointIsStable(currentPoint)}
                        <p class="muted small">Waiting for the reading to settle…</p>
                    {/if}
                    {#if status.missingPoints.length > 0}
                        <p class="muted small">
                            Still to capture: {status.missingPoints.map(pointLabel).join(', ')}.
                        </p>
                    {/if}
                </div>
            {/if}

            <!-- Review: stored vs staged, with the derived thresholds shown
                 as the ECU computed them. #534 requires the full diff before
                 Commit is enabled. -->
            {#if status !== null && status.allPointsCaptured}
                <div class="card">
                    <h3 class="card-h">Review</h3>
                    <div class="table-wrap">
                        <table>
                            <thead>
                                <tr><th>Value</th><th>Currently stored</th><th>New</th><th>Change</th></tr>
                            </thead>
                            <tbody>
                                {#each VALUE_ROWS as row (row.key)}
                                    {@const before = stored[row.key]}
                                    {@const after = staged[row.key]}
                                    <tr>
                                        <td>
                                            {row.label}
                                            {#if 'derived' in row && row.derived}
                                                <span class="muted small" title="Derived by the ECU from the captured rest and pressed points — shown as the firmware computed it, not recomputed here.">derived</span>
                                            {/if}
                                        </td>
                                        <td class="mono">{before ?? '—'}</td>
                                        <td class="mono">{after ?? '—'}</td>
                                        <td class="mono">
                                            {typeof before === 'number' && typeof after === 'number'
                                                ? (after - before > 0 ? `+${after - before}` : `${after - before}`)
                                                : '—'}
                                        </td>
                                    </tr>
                                {/each}
                            </tbody>
                        </table>
                    </div>

                    <label class="confirm">
                        <input type="checkbox" bind:checked={confirmedCommit} />
                        I have checked these values and the car is on stands.
                    </label>
                    <div class="badge-row">
                        <button
                            class="btn btn-primary"
                            onclick={commit}
                            disabled={busy || !confirmedCommit || !stagedComplete}
                            title={stagedComplete
                                ? 'Validate and write to the ECU'
                                : 'Waiting for the staged values to be read back from the ECU'}
                        >
                            Commit
                        </button>
                        <button class="btn" onclick={abort} disabled={busy}>Abort</button>
                    </div>
                </div>
            {/if}

            {#if committedAt !== null}
                <div class="banner banner-success">
                    <strong>Committed.</strong> The values above were read back from the ECU after
                    the write — what is shown in the "Currently stored" column is what the ECU
                    now holds. Sweep both pedals once and confirm they read 0 % and 100 %.
                </div>
            {/if}

            <div class="badge-row">
                <button class="btn" onclick={abort} disabled={busy}>Abort session</button>
            </div>
        {/if}

        <!-- Destructive, and deliberately kept away from the normal flow. -->
        <div class="card">
            <h3 class="card-h">Reset to defaults</h3>
            <p class="muted small">
                Discards the stored calibration and restores the compile-time values. Only useful
                if a committed calibration is wrong and you want the known-good baseline back.
            </p>
            {#if showResetConfirm}
                <div class="banner banner-danger">
                    <strong>This overwrites the stored calibration.</strong> The pedals will revert
                    to the compile-time defaults, which may not match this car.
                </div>
                <div class="badge-row">
                    <button class="btn btn-danger" onclick={resetDefaults} disabled={busy}>
                        Yes, reset to defaults
                    </button>
                    <button class="btn" onclick={() => (showResetConfirm = false)}>Cancel</button>
                </div>
            {:else}
                <button class="btn" onclick={() => (showResetConfirm = true)} disabled={busy}>
                    Reset to defaults…
                </button>
            {/if}
        </div>

    {/if}
</section>

<style>
    /* Chrome (.view, .card, .banner-*, .btn, .pill, .flag, .stat, .muted,
       .mono, .small) comes from app.css; only the bits specific to this
       wizard live here. */
    /* .stat / .reads / .flags / .flag / .badge-row are NOT global — every
       view defines its own copy (see PitDiagView, BusMonitorView,
       DataLogsView). Svelte scopes styles per component, so using them
       without defining them here would render unstyled and no check would
       catch it. Kept consistent with PitDiagView's definitions. */
    .stat {
        display: inline-flex;
        gap: var(--space-2);
        align-items: center;
        font-size: var(--text-base);
        color: var(--text-muted);
        font-family: var(--font-mono);
    }
    .stat strong {
        color: var(--text);
        font-weight: 600;
    }
    .stat.bad strong {
        color: var(--danger);
    }
    .reads {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-3);
        margin-bottom: var(--space-2);
    }
    .badge-row {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-2);
        margin-bottom: var(--space-2);
    }
    .flags {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-1);
        margin-bottom: var(--space-2);
    }
    .flag {
        font-size: var(--text-sm, 0.8rem);
        font-family: var(--font-mono);
        padding: 3px 9px;
        border-radius: var(--radius-sm, 4px);
        border: 1px solid var(--border);
        color: var(--text-muted);
        background: var(--bg);
    }
    .flag.on {
        border-color: var(--success);
        color: var(--success);
        background: var(--success-soft, transparent);
    }
    .meter-row {
        display: grid;
        grid-template-columns: 14rem 1fr auto auto;
        gap: var(--space-3);
        align-items: center;
        padding: var(--space-2) 0;
    }
    .table-wrap {
        overflow-x: auto;
    }
    table {
        width: 100%;
        border-collapse: collapse;
    }
    th,
    td {
        text-align: left;
        padding: var(--space-2) var(--space-3);
        border-bottom: 1px solid var(--border);
    }
    .confirm {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        margin: var(--space-3) 0;
    }
    pre {
        overflow-x: auto;
        padding: var(--space-3);
        background: var(--surface);
        border: 1px solid var(--border);
        border-radius: var(--radius-md);
    }
    </style>
