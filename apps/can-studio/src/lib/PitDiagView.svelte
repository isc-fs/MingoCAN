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
        ECU_INV_TEMP_DISCONNECTED_C,
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
    import type { ViewId } from './stores';

    interface Props {
        navigateTo: (id: ViewId) => void;
    }
    const { navigateTo }: Props = $props();

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

    // ---- AMS always-on telemetry (0x4A4 / 0x135 / 0x4A1) ----
    // Decoded alongside the gated diag block to round out the HV-state
    // picture (relays + pack voltage/current) next to the FSM. These
    // broadcast at their own cadence OUTSIDE the 58-frame pit-diag scan,
    // so they are deliberately NOT counted toward the scan-rate check.
    interface RelaySnapshot {
        airNegative: boolean;
        airPositive: boolean;
        precharge: boolean;
        amsOk: boolean;
    }
    interface PackSnapshot {
        packVoltageMv: number;
        filteredMa: number;
    }
    interface CurrentsSnapshot {
        accuDa: number;
        dcdcDa: number;
    }
    let relays = $state<RelaySnapshot | null>(null);
    let pack = $state<PackSnapshot | null>(null);
    let currents = $state<CurrentsSnapshot | null>(null);

    // ---- ECU pit-diag snapshots (0x700..=0x705) ----
    // The ECU stream is small and unrelated to the AMS one: five
    // frames at 10 Hz. Each arrives independently; keep the latest.
    interface EcuStatusSnapshot {
        fsmState: string;
        invState: string;
        ev23: boolean;
        t1189: boolean;
        rtdsActive: boolean;
        okPrecharge: boolean;
        startButton: boolean;
        torquePct: number;
        vCellMinMv: number;
        torqueCmd: number;
    }
    interface EcuPedalsSnapshot {
        apps1Raw: number;
        apps2Raw: number;
        brakeRaw: number;
        apps1Pct: number;
        apps2Pct: number;
    }
    interface EcuBrakeSnapshot {
        brakePressureDbar: number;
        brakePct: number;
    }
    interface EcuInverterSnapshot {
        dcBusVoltage: number;
        invRpm: number;
        invError: number;
    }
    interface EcuFwSnapshot {
        versionMajor: number;
        versionMinor: number;
        versionPatch: number;
        gitHash: number[];
    }
    interface EcuInvTempsSnapshot {
        boardDegc: number;
        pwrstgDegc: number;
        motor1Degc: number;
        motor2Degc: number;
    }
    interface EcuHealthSnapshot {
        freeHeap: number;
        minFreeHeap: number;
        taskControl: boolean;
        taskCanRx: boolean;
        taskCanTx: boolean;
        taskDiag: boolean;
        resetCause: string;
        uptimeS: number;
        lastFault: number;
        lastFaultName: string;
    }

    let ecuStatus = $state<EcuStatusSnapshot | null>(null);
    let ecuPedals = $state<EcuPedalsSnapshot | null>(null);
    let ecuBrake = $state<EcuBrakeSnapshot | null>(null);
    let ecuInverter = $state<EcuInverterSnapshot | null>(null);
    let ecuInvTemps = $state<EcuInvTempsSnapshot | null>(null);
    let ecuFw = $state<EcuFwSnapshot | null>(null);
    let ecuHealth = $state<EcuHealthSnapshot | null>(null);

    // APPS plausibility: FSAE T11.8.9 trips at >10% disagreement
    // between the two pedal channels. Surface the live delta so the
    // operator can sanity-check the pedal calibration on the bench.
    const appsDelta = $derived(
        ecuPedals === null
            ? null
            : Math.abs(ecuPedals.apps1Pct - ecuPedals.apps2Pct),
    );
    const appsImplausible = $derived(appsDelta !== null && appsDelta > 10);

    // ECU FSM state → pill tone. "active" is the good driving state;
    // "amsError"/unknown are bad; the rest are transitions.
    function ecuFsmTone(state: string): 'success' | 'warning' | 'danger' | 'info' {
        if (state === 'active') return 'success';
        if (state === 'amsError' || state.startsWith('unknown')) return 'danger';
        return 'info';
    }
    // Inverter state → tone. "ready" good, "standby" neutral.
    function ecuInvTone(state: string): 'success' | 'info' | 'danger' {
        if (state === 'ready') return 'success';
        if (state.startsWith('unknown')) return 'danger';
        return 'info';
    }

    // Inverter-temp sentinel: 205 °C means the sensor is disconnected.
    const INV_TEMP_NC = ECU_INV_TEMP_DISCONNECTED_C;
    function fmtInvTemp(t: number): string {
        return t === INV_TEMP_NC ? 'n/c' : `${t} °C`;
    }

    // ECU firmware header chip — "ECU v1.6.2 · a1b2c3d4".
    const ecuFwLabel = $derived(
        ecuFw === null
            ? null
            : `ECU v${ecuFw.versionMajor}.${ecuFw.versionMinor}.${ecuFw.versionPatch}` +
                  ` · ${ecuFw.gitHash.map((b) => b.toString(16).padStart(2, '0')).join('')}`,
    );

    // True once any ECU frame has landed — gates the panels vs the
    // "arm to begin" hint.
    const ecuHasData = $derived(
        ecuStatus !== null ||
            ecuPedals !== null ||
            ecuBrake !== null ||
            ecuInverter !== null,
    );

    // True once any AMS frame has landed — gates the cell / NTC / diag
    // panels vs the "arm to begin" hint, mirroring `ecuHasData` so both
    // tabs behave the same when idle.
    const amsHasData = $derived(
        fsm !== null ||
            poll !== null ||
            balance !== null ||
            cellsMv.some((v) => v !== null) ||
            ntcsC.some((v) => v !== null),
    );

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
        relays = null;
        pack = null;
        currents = null;
        ecuStatus = null;
        ecuPedals = null;
        ecuBrake = null;
        ecuInverter = null;
        ecuInvTemps = null;
        ecuFw = null;
        ecuHealth = null;
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

    // Switch the targeted ECU. If a stream is armed, disarm it first —
    // while `profile` still names the armed node — so the disable frame
    // goes to the right ID (AMS 0x7F0 vs ECU 0x7E0) and that node
    // doesn't keep streaming in the background behind the new view.
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
                // ---- AMS always-on telemetry (NOT scan-counted) ----
            } else if (event.kind === 'relayStatus') {
                relays = {
                    airNegative: event.airNegative,
                    airPositive: event.airPositive,
                    precharge: event.precharge,
                    amsOk: event.amsOk,
                };
            } else if (event.kind === 'acuCurrents') {
                currents = { accuDa: event.accuDa, dcdcDa: event.dcdcDa };
            } else if (event.kind === 'pack') {
                pack = {
                    packVoltageMv: event.packVoltageMv,
                    filteredMa: event.filteredMa,
                };
                // ---- ECU profile frames (0x700..=0x705) ----
            } else if (event.kind === 'ecuStatus') {
                ecuStatus = {
                    fsmState: event.fsmState,
                    invState: event.invState,
                    ev23: event.ev23,
                    t1189: event.t1189,
                    rtdsActive: event.rtdsActive,
                    okPrecharge: event.okPrecharge,
                    startButton: event.startButton,
                    torquePct: event.torquePct,
                    vCellMinMv: event.vCellMinMv,
                    torqueCmd: event.torqueCmd,
                };
                framesThisScan += 1;
            } else if (event.kind === 'ecuPedals') {
                ecuPedals = {
                    apps1Raw: event.apps1Raw,
                    apps2Raw: event.apps2Raw,
                    brakeRaw: event.brakeRaw,
                    apps1Pct: event.apps1Pct,
                    apps2Pct: event.apps2Pct,
                };
                framesThisScan += 1;
            } else if (event.kind === 'ecuBrake') {
                ecuBrake = {
                    brakePressureDbar: event.brakePressureDbar,
                    brakePct: event.brakePct,
                };
                framesThisScan += 1;
            } else if (event.kind === 'ecuInverter') {
                ecuInverter = {
                    dcBusVoltage: event.dcBusVoltage,
                    invRpm: event.invRpm,
                    invError: event.invError,
                };
                framesThisScan += 1;
            } else if (event.kind === 'ecuInverterTemps') {
                ecuInvTemps = {
                    boardDegc: event.boardDegc,
                    pwrstgDegc: event.pwrstgDegc,
                    motor1Degc: event.motor1Degc,
                    motor2Degc: event.motor2Degc,
                };
                framesThisScan += 1;
            } else if (event.kind === 'ecuFwInfo') {
                ecuFw = {
                    versionMajor: event.versionMajor,
                    versionMinor: event.versionMinor,
                    versionPatch: event.versionPatch,
                    gitHash: event.gitHash,
                };
                framesThisScan += 1;
            } else if (event.kind === 'ecuHealth') {
                ecuHealth = {
                    freeHeap: event.freeHeap,
                    minFreeHeap: event.minFreeHeap,
                    taskControl: event.taskControl,
                    taskCanRx: event.taskCanRx,
                    taskCanTx: event.taskCanTx,
                    taskDiag: event.taskDiag,
                    resetCause: event.resetCause,
                    uptimeS: event.uptimeS,
                    lastFault: event.lastFault,
                    lastFaultName: event.lastFaultName,
                };
                // 0x704 is 1 Hz, not part of the 100 ms cyclic scan —
                // don't count it toward frames/scan.
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
            <h2>Telemetry</h2>
            {#if profile === 'ams'}
                <p class="muted">
                    AMS observer mode. Arming emits <code>0x7F0#DEADBEEF</code>;
                    the AMS replies on <code>0x7F1</code> and starts streaming
                    the diagnostic stream at 1 Hz ({AMS_EXPECTED_FRAMES_PER_SCAN}
                    frames/scan). Disarm sends the zero-payload frame;
                    firmware also clears the flag on reboot.
                </p>
            {:else if profile === 'ecu'}
                <p class="muted">
                    ECU observer mode. Arming emits <code>0x7E0#DEADBEEF</code>;
                    the ECU replies on <code>0x7E1</code> and streams APPS,
                    brake, FSM, and inverter telemetry at 10 Hz
                    (<code>0x700..=0x705</code>). Disarm sends the zero-payload
                    frame; firmware also clears the flag on reboot.
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
        {:else if profile === 'ecu' && ecuFwLabel !== null}
            <span
                class="pill fw-chip mono"
                title="From the 0x703 firmware-ID frame. Compare against the build you flashed."
            >
                {ecuFwLabel}
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
        <div class="banner banner-warning gate">
            <span>
                <strong>No adapter selected.</strong> Pit-diag needs an
                <code>--interface</code>/<code>--channel</code> pair.
            </span>
            <button
                type="button"
                class="btn btn-sm gate-action"
                onclick={() => navigateTo('adapters')}
            >
                Choose adapter →
            </button>
        </div>
    {/if}

    {#if profile === 'udv'}
        <!-- uDV placeholder. No arm handshake or frame contract exists
             for it yet, so we don't offer to arm — surfacing a clear
             "why" beats a button that errors. -->
        <div class="card placeholder-card">
            <h3>uDV pit-diag isn't available yet</h3>
            <p class="muted">
                The AMS (<code>0x7F0</code> / <code>0x6C0..=0x6C8</code>)
                and the ECU (<code>0x7E0</code> / <code>0x700..=0x705</code>)
                each expose a pit-diag arm handshake + decoded stream. The
                uDV has no pit-diag protocol or frames defined in
                IFS08-DBCinator yet, so there's nothing to arm or decode.
            </p>
            <p class="muted">
                Once the firmware team ships a uDV pit-diag stream and
                publishes its frames, this panel grows live readouts like
                the <strong>AMS</strong> and <strong>ECU</strong> tabs.
                Until then, switch profiles above, or use the
                <strong>Bus monitor</strong> / <strong>Signals</strong>
                views to watch raw or DBC-decoded traffic from any node.
            </p>
        </div>
    {:else if profile === 'ecu'}
        <!-- ECU controls -->
        <div class="toolbar card card-tight">
            {#if armState.kind === 'idle' || armState.kind === 'error'}
                <button
                    type="button"
                    class="btn btn-primary"
                    disabled={!adapterReady}
                    onclick={confirmArm}
                >
                    Enable ECU pit-diag
                </button>
            {:else if armState.kind === 'confirming'}
                <div class="confirm">
                    <span class="confirm-text">Arm ECU pit-diag stream?</span>
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
            {/if}
        </div>

        {#if armState.kind === 'error'}
            <div class="banner banner-danger">
                <strong>Arm failed:</strong>
                {armState.message}
            </div>
        {/if}

        {#if !ecuHasData}
            <div class="card placeholder-card">
                <h3>ECU pit-diag</h3>
                <p class="muted">
                    {#if armState.kind === 'armed'}
                        Armed — waiting for the first frames from the ECU…
                    {:else}
                        Connect the ECU and hit
                        <strong>Enable ECU pit-diag</strong>. Arming emits
                        <code>0x7E0#DEADBEEF</code>; the ECU then streams APPS,
                        brake, FSM, and inverter telemetry.
                    {/if}
                </p>
            </div>
        {:else}
            <div class="ecu-grid">
                <!-- Vehicle FSM / status (0x700) -->
                <div class="card">
                    <h3 class="card-h">Vehicle FSM</h3>
                    {#if ecuStatus !== null}
                        <div class="badge-row">
                            <span class="pill pill-{ecuFsmTone(ecuStatus.fsmState)}">
                                {ecuStatus.fsmState}
                            </span>
                            <span class="pill pill-{ecuInvTone(ecuStatus.invState)}">
                                inv: {ecuStatus.invState}
                            </span>
                        </div>
                        <div class="flags">
                            <span class="flag" class:on={ecuStatus.ev23}>EV 2.3</span>
                            <span class="flag" class:on={ecuStatus.t1189}>T11.8/9</span>
                            <span class="flag" class:on={ecuStatus.rtdsActive}>RTDS</span>
                            <span class="flag" class:on={ecuStatus.okPrecharge}>
                                Precharge
                            </span>
                            <span class="flag" class:on={ecuStatus.startButton}>
                                Start btn
                            </span>
                        </div>
                        <div class="reads">
                            <span class="stat">
                                <span>torque</span><strong>{ecuStatus.torquePct}%</strong>
                            </span>
                            <span class="stat">
                                <span>torque cmd</span><strong>{ecuStatus.torqueCmd}</strong>
                            </span>
                            <span class="stat">
                                <span>min cell</span>
                                <strong>{(ecuStatus.vCellMinMv / 1000).toFixed(3)} V</strong>
                            </span>
                        </div>
                    {:else}
                        <p class="muted small">No status frame yet.</p>
                    {/if}
                </div>

                <!-- Pedals / APPS (0x701) -->
                <div class="card">
                    <h3 class="card-h">Pedals (APPS)</h3>
                    {#if ecuPedals !== null}
                        <div class="meter-row">
                            <span class="meter-label">APPS1</span>
                            <div class="meter">
                                <div
                                    class="meter-fill"
                                    style="width: {Math.min(ecuPedals.apps1Pct, 100)}%"
                                ></div>
                            </div>
                            <span class="meter-val mono">{ecuPedals.apps1Pct}%</span>
                            <span class="meter-raw muted mono">{ecuPedals.apps1Raw}</span>
                        </div>
                        <div class="meter-row">
                            <span class="meter-label">APPS2</span>
                            <div class="meter">
                                <div
                                    class="meter-fill"
                                    style="width: {Math.min(ecuPedals.apps2Pct, 100)}%"
                                ></div>
                            </div>
                            <span class="meter-val mono">{ecuPedals.apps2Pct}%</span>
                            <span class="meter-raw muted mono">{ecuPedals.apps2Raw}</span>
                        </div>
                        <div class="reads">
                            <span class="stat" class:bad={appsImplausible}>
                                <span>APPS Δ</span>
                                <strong>{appsDelta}%{appsImplausible ? ' ⚠' : ''}</strong>
                            </span>
                            <span class="stat">
                                <span>brake raw</span>
                                <strong class="mono">{ecuPedals.brakeRaw}</strong>
                            </span>
                        </div>
                    {:else}
                        <p class="muted small">No pedal frame yet.</p>
                    {/if}
                </div>

                <!-- Brake (0x705) -->
                <div class="card">
                    <h3 class="card-h">Brake</h3>
                    {#if ecuBrake !== null}
                        <div class="meter-row">
                            <span class="meter-label">Brake</span>
                            <div class="meter">
                                <div
                                    class="meter-fill"
                                    style="width: {Math.min(ecuBrake.brakePct, 100)}%"
                                ></div>
                            </div>
                            <span class="meter-val mono">{ecuBrake.brakePct}%</span>
                        </div>
                        <div class="reads">
                            <span class="stat">
                                <span>pressure</span>
                                <strong>{(ecuBrake.brakePressureDbar / 10).toFixed(1)} bar</strong>
                            </span>
                        </div>
                    {:else}
                        <p class="muted small">No brake frame yet.</p>
                    {/if}
                </div>

                <!-- Inverter (0x702) -->
                <div class="card">
                    <h3 class="card-h">Inverter</h3>
                    {#if ecuInverter !== null}
                        <div class="reads">
                            <span class="stat">
                                <span>DC bus</span>
                                <strong>{ecuInverter.dcBusVoltage} V</strong>
                            </span>
                            <span class="stat">
                                <span>motor</span>
                                <strong>{ecuInverter.invRpm} rpm</strong>
                            </span>
                            <span class="stat" class:bad={ecuInverter.invError !== 0}>
                                <span>error</span>
                                <strong class="mono">
                                    0x{ecuInverter.invError
                                        .toString(16)
                                        .toUpperCase()
                                        .padStart(2, '0')}
                                </strong>
                            </span>
                        </div>
                    {:else}
                        <p class="muted small">No inverter frame yet.</p>
                    {/if}
                    {#if ecuInvTemps !== null}
                        <div class="reads reads-temps">
                            <span
                                class="stat"
                                class:bad={ecuInvTemps.boardDegc === INV_TEMP_NC}
                            >
                                <span>board</span>
                                <strong>{fmtInvTemp(ecuInvTemps.boardDegc)}</strong>
                            </span>
                            <span
                                class="stat"
                                class:bad={ecuInvTemps.pwrstgDegc === INV_TEMP_NC}
                            >
                                <span>pwr stage</span>
                                <strong>{fmtInvTemp(ecuInvTemps.pwrstgDegc)}</strong>
                            </span>
                            <span
                                class="stat"
                                class:bad={ecuInvTemps.motor1Degc === INV_TEMP_NC}
                            >
                                <span>motor 1</span>
                                <strong>{fmtInvTemp(ecuInvTemps.motor1Degc)}</strong>
                            </span>
                            <span
                                class="stat"
                                class:bad={ecuInvTemps.motor2Degc === INV_TEMP_NC}
                            >
                                <span>motor 2</span>
                                <strong>{fmtInvTemp(ecuInvTemps.motor2Degc)}</strong>
                            </span>
                        </div>
                    {/if}
                </div>

                <!-- Firmware health (0x704, 1 Hz) -->
                <div class="card">
                    <h3 class="card-h">Firmware health</h3>
                    {#if ecuHealth !== null}
                        <div class="flags">
                            <span class="flag" class:on={ecuHealth.taskControl}>
                                Control
                            </span>
                            <span class="flag" class:on={ecuHealth.taskCanRx}>CAN-RX</span>
                            <span class="flag" class:on={ecuHealth.taskCanTx}>CAN-TX</span>
                            <span class="flag" class:on={ecuHealth.taskDiag}>Diag</span>
                        </div>
                        <div class="reads">
                            <span class="stat">
                                <span>free heap</span>
                                <strong class="mono">{ecuHealth.freeHeap} B</strong>
                            </span>
                            <span class="stat">
                                <span>min heap</span>
                                <strong class="mono">{ecuHealth.minFreeHeap} B</strong>
                            </span>
                            <span class="stat">
                                <span>uptime</span><strong>{ecuHealth.uptimeS} s</strong>
                            </span>
                            <span class="stat">
                                <span>reset</span>
                                <strong
                                    class:bad={ecuHealth.resetCause === 'iwdg' ||
                                        ecuHealth.resetCause === 'wwdg'}
                                >
                                    {ecuHealth.resetCause}
                                </strong>
                            </span>
                            <span class="stat" class:bad={ecuHealth.lastFault !== 0}>
                                <span>last fault</span>
                                <strong>{ecuHealth.lastFaultName}</strong>
                            </span>
                        </div>
                    {:else}
                        <p class="muted small">No health frame yet (arrives at 1 Hz).</p>
                    {/if}
                </div>
            </div>
        {/if}
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

    {#if !amsHasData}
        <div class="card placeholder-card">
            <h3>AMS pit-diag</h3>
            <p class="muted">
                {#if armState.kind === 'armed'}
                    Armed — waiting for the first frames from the AMS…
                {:else}
                    Connect the AMS and hit
                    <strong>Enable AMS pit-diag</strong>. Arming emits
                    <code>0x7F0#DEADBEEF</code>; the AMS then streams cell
                    voltages, NTC temperatures, and FSM / balance
                    diagnostics.
                {/if}
            </p>
        </div>
    {:else}

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

    <!-- HV relays + pack current (always-on telemetry, not gated by the
         arm: 0x4A4 contactors, 0x4A1 pack V/I, 0x135 accu/DC-DC currents) -->
    {#if relays !== null || pack !== null || currents !== null}
        <section class="card">
            <div class="card-header">
                <h3>HV relays &amp; current</h3>
                <span class="muted small mono">0x4A4 / 0x4A1 / 0x135</span>
            </div>
            {#if relays !== null}
                <div class="flags hv-relays">
                    <span class="flag" class:on={relays.airNegative}>AIR−</span>
                    <span class="flag" class:on={relays.airPositive}>AIR+</span>
                    <span class="flag" class:on={relays.precharge}>Precharge</span>
                    <span class="flag" class:on={relays.amsOk}>AMS_OK</span>
                </div>
            {/if}
            {#if pack !== null || currents !== null}
                <div class="diag-grid">
                    {#if pack !== null}
                        <div class="diag-cell">
                            <span class="diag-label">Pack voltage</span>
                            <span class="diag-value mono">
                                {(pack.packVoltageMv / 1000).toFixed(2)} V
                            </span>
                        </div>
                        <div class="diag-cell">
                            <span class="diag-label">Pack current</span>
                            <span class="diag-value mono">
                                {(pack.filteredMa / 1000).toFixed(1)} A
                            </span>
                        </div>
                    {/if}
                    {#if currents !== null}
                        <div class="diag-cell">
                            <span class="diag-label">Accu current</span>
                            <span class="diag-value mono">
                                {(currents.accuDa / 10).toFixed(1)} A
                            </span>
                        </div>
                        <div class="diag-cell">
                            <span class="diag-label">DC-DC current</span>
                            <span class="diag-value mono">
                                {(currents.dcdcDa / 10).toFixed(1)} A
                            </span>
                        </div>
                    {/if}
                </div>
            {/if}
            <p class="muted small">
                Relay bits are GPIO read-backs (what the firmware drives the
                coils to), not a physical-closed confirmation. Current sign:
                + discharge / − charge.
            </p>
        </section>
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

    /* ---- ECU pit-diag panels ---- */
    .ecu-grid {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
        gap: var(--space-3);
    }
    .card-h {
        margin: 0 0 var(--space-2);
        font-size: var(--text-sm);
        font-weight: 600;
        color: var(--text-muted);
        text-transform: uppercase;
        letter-spacing: 0.04em;
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
    .hv-relays {
        margin-bottom: var(--space-3);
    }
    /* Cockpit flag chip — dim by default, lit green when the bit is set. */
    .flag {
        font-size: var(--text-xs, 0.7rem);
        font-family: var(--font-mono);
        padding: 1px 6px;
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
    .reads {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-3);
    }
    /* Inverter temps sit below the DC-bus/RPM/error reads. */
    .reads-temps {
        margin-top: var(--space-2);
        padding-top: var(--space-2);
        border-top: 1px solid var(--border);
    }
    /* A stat flagged out-of-spec (implausible APPS, inverter error). */
    .stat.bad strong {
        color: var(--danger);
    }
    .meter-row {
        display: grid;
        grid-template-columns: 3.2rem 1fr auto auto;
        align-items: center;
        gap: var(--space-2);
        margin-bottom: var(--space-1);
    }
    .meter-label {
        font-size: var(--text-sm);
        color: var(--text-muted);
    }
    .meter {
        height: 8px;
        border-radius: 4px;
        background: var(--bg);
        border: 1px solid var(--border);
        overflow: hidden;
    }
    .meter-fill {
        height: 100%;
        background: var(--accent);
        transition: width 0.1s linear;
    }
    .meter-val {
        font-size: var(--text-sm);
        min-width: 3rem;
        text-align: right;
    }
    .meter-raw {
        font-size: var(--text-xs, 0.7rem);
        min-width: 3rem;
        text-align: right;
    }
</style>
