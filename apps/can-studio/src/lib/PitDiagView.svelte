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
      (Poll-timing 0x6C1 and cell-balancing 0x6C2/0x6C3 are decoded but
       not surfaced — charger/bench-BMS concerns, off this app.)

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
        AMS_NTC_PER_MODULE,
        AMS_NUM_MODULES,
        AMS_NUM_NTCS,
        ECU_INV_TEMP_DISCONNECTED_C,
        onPitDiagFrame,
        onPitDiagStatus,
        pitDiagDisable,
        pitDiagUdvCalibrate,
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
        { id: 'all', label: 'All' },
        { id: 'ams', label: 'AMS' },
        { id: 'ecu', label: 'ECU' },
        { id: 'udv', label: 'uDV' },
    ];
    let profile = $state<PitDiagProfile>('all');

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

    // Latest FSM snapshot. Arrives once per scan; we keep the most recent.
    let fsm = $state<FsmSnapshot | null>(null);

    // Balance (0x6C2/0x6C3), per-IC PEC (0x6C7/0x6C8), and boot
    // diagnostics (0x6C4) are decoded on the wire but not surfaced on this
    // bench view — see the count-only handler branch below.
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
        dvMode: boolean;
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
        invErrorName: string;
        demPresent: boolean;
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
    interface EcuDvSnapshot {
        dvR2dReq: boolean;
        dvCmdFresh: boolean;
        tsActive: boolean;
        brakeOverLimit: boolean;
        r2dConfirm: boolean;
        dvTorquePct: number;
        motorRpmMech: number;
    }

    let ecuStatus = $state<EcuStatusSnapshot | null>(null);
    let ecuPedals = $state<EcuPedalsSnapshot | null>(null);
    let ecuBrake = $state<EcuBrakeSnapshot | null>(null);
    let ecuInverter = $state<EcuInverterSnapshot | null>(null);
    let ecuInvTemps = $state<EcuInvTempsSnapshot | null>(null);
    let ecuFw = $state<EcuFwSnapshot | null>(null);
    let ecuHealth = $state<EcuHealthSnapshot | null>(null);
    let ecuDv = $state<EcuDvSnapshot | null>(null);

    // ---- uDV pit-diag snapshots (0x7A0..=0x7A4) ----
    interface UdvStatusSnapshot {
        asState: string;
        signals: number;
        missionId: number;
        ebsInit: string;
        stubMask: number;
        assi: string;
        diagArmed: boolean;
    }
    interface UdvResSnapshot {
        raw191: number;
        resStatus: string;
        bits: number;
        radioQuality: number;
        resAgeMs: number;
        steerMotor: string;
        lwsStatus: number;
    }
    interface UdvPipeSnapshot {
        dvStatus: number;
        dvAgeMs: number;
        accelCmdPct: number;
        steerCmd: number;
        ctrlAgeMs: number;
        setupBits: number;
    }
    interface UdvHealthSnapshot {
        freeHeapWords: number;
        minFreeHeapWords: number;
        taskMask: number;
        flags: number;
        stalledTask: number;
        uptimeS: number;
    }
    interface UdvFwSnapshot {
        gitHash: number;
        heapSizeKb: number;
        uptimeS: number;
    }
    interface UdvCalibSnapshot {
        phase: number;
        phaseName: string;
        error: number;
        errorName: string;
        centerDdeg: number;
        halfRangeDdeg: number;
        limitDdeg: number;
    }
    interface UdvSteerSnapshot {
        lwsRawDdeg: number;
        steerActualDdeg: number;
        steerTargetDdeg: number;
        lwsStatus: number;
        motorState: number;
        motorStateName: string;
    }
    interface UdvCalibRelaySnapshot {
        triggerRxCount: number;
        relayCount: number;
        lastCmd: number;
        armed: boolean;
    }
    interface UdvCanHealthSnapshot {
        flags: number;
        lastErrorCode: number;
        txErrCount: number;
        rxErrCount: number;
        resRxCount: number;
        nmtCount: number;
        ackError: boolean;
    }
    interface UdvEbsPressSnapshot {
        tank1Dbar: number;
        tank2Dbar: number;
        ebsInit: string;
        stubMask: number;
        tank1Ok: boolean;
        tank2Ok: boolean;
    }
    let udvStatus = $state<UdvStatusSnapshot | null>(null);
    let udvRes = $state<UdvResSnapshot | null>(null);
    let udvPipe = $state<UdvPipeSnapshot | null>(null);
    let udvHealth = $state<UdvHealthSnapshot | null>(null);
    let udvFw = $state<UdvFwSnapshot | null>(null);
    let udvCalib = $state<UdvCalibSnapshot | null>(null);
    let udvSteer = $state<UdvSteerSnapshot | null>(null);
    let udvCalibRelay = $state<UdvCalibRelaySnapshot | null>(null);
    let udvCanHealth = $state<UdvCanHealthSnapshot | null>(null);
    let udvEbsPress = $state<UdvEbsPressSnapshot | null>(null);
    // uDV safety-watchdog keep-alive freshness (#482). A 0x7A3 HEALTH frame
    // arrives ~10 Hz; if none landed in the last 1 Hz scan window the monitor
    // stopped kicking, so IWDG_ok must read false even if the last frame's bit
    // was set. `seen` is set on each health frame; the scan tick rolls it into
    // `stale` and clears it.
    let udvHealthSeenThisScan = $state<boolean>(false);
    let udvHealthStale = $state<boolean>(true);
    // Steering-calibration control state (#428). `confirming` shows the
    // "car elevated?" gate; `busy` disables the trigger between click + the
    // first status frame.
    let calibConfirming = $state<boolean>(false);
    let calibBusy = $state<boolean>(false);
    let calibError = $state<string | null>(null);
    // The calibration flow lives in a modal (#439 follow-up): the uDV
    // panel just has a trigger; the live angle + phase guidance + results
    // only appear once the operator opens it.
    let calibModalOpen = $state<boolean>(false);

    function openCalibModal(): void {
        calibError = null;
        calibConfirming = true;
        calibModalOpen = true;
    }
    function closeCalibModal(): void {
        // Hide the modal; does not abort a running calibration (use Abort).
        calibModalOpen = false;
        calibConfirming = false;
    }

    async function startCalibration(): Promise<void> {
        calibConfirming = false;
        calibBusy = true;
        calibError = null;
        try {
            await pitDiagUdvCalibrate(true);
        } catch (err) {
            calibError = err instanceof Error ? err.message : String(err);
        } finally {
            calibBusy = false;
        }
    }
    async function abortCalibration(): Promise<void> {
        try {
            await pitDiagUdvCalibrate(false);
        } catch (err) {
            calibError = err instanceof Error ? err.message : String(err);
        }
    }

    // Bit helper for the raw signal / setup / RES masks.
    function bit(mask: number, b: number): boolean {
        return (mask & (1 << b)) !== 0;
    }

    // AMI mission menu (0x503 index → name); mirrors the uDV firmware
    // registry (Handover/MISSIONS.md). `-1` (int8 on the wire) = none.
    const UDV_MISSIONS = [
        'manual',
        'acceleration',
        'skidpad',
        'autocross',
        'track drive',
        'ebs test',
        'inspection',
        'shutdown',
        'aux1',
        'aux2',
    ];
    function missionName(id: number): string {
        return UDV_MISSIONS[id] ?? `code ${id}`;
    }

    // Bench-stub mask (0x7A0 byte5). Mirrors the uDV firmware
    // pit_diag.cpp stub_mask(): b0 EBS-init, b1 DVPC, b2 EBS-sensors,
    // b3 SDC, b4 steering. Any bit set = a stubbed (non-flight) image.
    // Bench-stub mask bits (0x7A0/0x7A4/0x7A9), mirroring firmware
    // pit_diag.cpp stub_mask(): b0 EBS-init, b1 DVPC, b2 EBS-sensors,
    // b3 SDC, b4 steering, b5 IMU-ROS, b6 TS, b7 DV-stopping (#490).
    // Any bit set = a stubbed (non-flight) image.
    const UDV_STUBS = [
        'EBS-init',
        'DVPC',
        'EBS-sensors',
        'SDC',
        'steering',
        'IMU-ROS',
        'TS',
        'DV-stopping',
    ];
    function udvStubNames(mask: number): string {
        const on = UDV_STUBS.filter((_, b) => bit(mask, b));
        return on.length ? on.join(', ') : 'none';
    }

    // /dv/status enum (0x7A2 byte 0), mirroring the pipeline's status
    // codes (#490). 7 = STOPPING is the new end-of-mission brake-to-stop.
    const UDV_DV_STATUS = [
        'Idle',
        'Preparing',
        'Ready',
        'Running',
        'Finished',
        'Emergency',
        'Failed',
        'Stopping',
    ];
    function dvStatusName(v: number): string {
        return UDV_DV_STATUS[v] ?? `code ${v}`;
    }

    // Steering-calibration operator guidance, keyed on the 0x7A6 phase
    // byte (#439). Turns the blind Calibrate button into a self-guiding
    // step-by-step. Phases 4–8 are the automated homing/sweep.
    function calibGuidance(phase: number): string {
        switch (phase) {
            case 0:
                return 'Ready — elevate the car, then press Calibrate.';
            case 1:
                return 'Turn the wheel FULLY to one stop and hold ~2 s.';
            case 2:
                return 'Now turn FULLY to the OTHER stop and hold ~2 s.';
            case 3:
                return 'Return the wheel near centre (within ±15°).';
            case 4:
            case 5:
            case 6:
            case 7:
            case 8:
                return 'Homing / verification sweep — hands clear, motor moving.';
            case 9:
                return 'Calibrated & saved.';
            case 10:
                return 'Calibration failed.';
            default:
                return '';
        }
    }

    // Live-angle bar geometry (#439). The bar is centred at 0°; a valid
    // end-stop must exceed 30°, and full lock is well under this, so
    // ±130° keeps the needle on-scale without clipping.
    const LWS_BAR_MAX_DEG = 130;
    // A captured end-stop is only valid past this magnitude (firmware rule).
    const LWS_STOP_MIN_DEG = 30;
    function lwsFill(deg: number): { left: number; width: number } {
        const clamped = Math.max(-LWS_BAR_MAX_DEG, Math.min(LWS_BAR_MAX_DEG, deg));
        const pos = (clamped / LWS_BAR_MAX_DEG) * 50; // −50…+50 around the 50% centre
        return pos >= 0 ? { left: 50, width: pos } : { left: 50 + pos, width: -pos };
    }
    // Percentage offset from the bar centre for a given angle (for the
    // ±30° threshold markers). 0° → 0, ±LWS_BAR_MAX_DEG → ±50.
    function lwsPct(deg: number): number {
        return (deg / LWS_BAR_MAX_DEG) * 50;
    }

    // Calibration procedure as an ordered checklist — maps the raw 0x7A6
    // phase to "which step are we on" so the operator can see the whole
    // arc (capture both stops → recentre → auto-verify → save), not just a
    // bare number (#439 follow-up).
    const CALIB_STEPS = [
        'End-stop 1',
        'End-stop 2',
        'Return centre',
        'Verify sweep',
        'Save',
    ];
    function calibStepIndex(phase: number): number {
        if (phase === 1) return 0;
        if (phase === 2) return 1;
        if (phase === 3) return 2;
        if (phase >= 4 && phase <= 8) return 3;
        if (phase === 9) return 4;
        return -1; // idle (0) or failed (10)
    }
    // True while the operator is actively turning to a stop (phases 1–2) —
    // gates the "past 30°?" threshold cue on the live angle.
    function calibCapturing(phase: number): boolean {
        return phase === 1 || phase === 2;
    }

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
            cellsMv.some((v) => v !== null) ||
            ntcsC.some((v) => v !== null),
    );

    // True once any uDV frame has landed — gates the panels vs the hint.
    const udvHasData = $derived(
        udvStatus !== null ||
            udvRes !== null ||
            udvPipe !== null ||
            udvHealth !== null ||
            udvSteer !== null ||
            udvEbsPress !== null,
    );

    // uDV safety-watchdog keep-alive (#482, 0x7A3 flags bit 2). True ONLY
    // when a FRESH health frame reports the monitor is kicking the IWDG —
    // a stale/missing 0x7A3 (wedged node) reads false even if the last bit
    // was set.
    const udvIwdgOk = $derived(
        udvHealth !== null && !udvHealthStale && bit(udvHealth.flags, 2),
    );

    // uDV firmware header chip — "uDV git 1a2b3c4d".
    const udvFwLabel = $derived(
        udvFw === null
            ? null
            : `uDV git ${(udvFw.gitHash >>> 0).toString(16).padStart(8, '0')}`,
    );

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
        fsm = null;
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
        ecuDv = null;
        udvStatus = null;
        udvRes = null;
        udvPipe = null;
        udvHealth = null;
        udvFw = null;
        udvCalib = null;
        udvSteer = null;
        udvCalibRelay = null;
        udvCanHealth = null;
        udvEbsPress = null;
        udvHealthSeenThisScan = false;
        udvHealthStale = true;
        calibConfirming = false;
        calibBusy = false;
        calibError = null;
        calibModalOpen = false;
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
                // Poll timing (0x6C1) isn't surfaced — V-poll latency is a
                // charger/bench-BMS concern, off this app. Still count the
                // frame so the scan-rate drift check stays calibrated.
                framesThisScan += 1;
            } else if (
                event.kind === 'perIcPec' ||
                event.kind === 'balanceMaskA' ||
                event.kind === 'balanceMaskB' ||
                event.kind === 'bootDiag'
            ) {
                // Per-IC PEC (0x6C7/0x6C8), balance (0x6C2/0x6C3), and boot
                // diagnostics (0x6C4) aren't surfaced on this bench view.
                // Still count the frames so the scan-rate drift check stays
                // calibrated.
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
                    dvMode: event.dvMode,
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
                    invErrorName: event.invErrorName,
                    demPresent: event.demPresent,
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
            } else if (event.kind === 'ecuDv') {
                ecuDv = {
                    dvR2dReq: event.dvR2dReq,
                    dvCmdFresh: event.dvCmdFresh,
                    tsActive: event.tsActive,
                    brakeOverLimit: event.brakeOverLimit,
                    r2dConfirm: event.r2dConfirm,
                    dvTorquePct: event.dvTorquePct,
                    motorRpmMech: event.motorRpmMech,
                };
                framesThisScan += 1;
                // ---- uDV profile frames (0x7A0..=0x7A4) ----
            } else if (event.kind === 'udvStatus') {
                udvStatus = {
                    asState: event.asState,
                    signals: event.signals,
                    missionId: event.missionId,
                    ebsInit: event.ebsInit,
                    stubMask: event.stubMask,
                    assi: event.assi,
                    diagArmed: event.diagArmed,
                };
                framesThisScan += 1;
            } else if (event.kind === 'udvRes') {
                udvRes = {
                    raw191: event.raw191,
                    resStatus: event.resStatus,
                    bits: event.bits,
                    radioQuality: event.radioQuality,
                    resAgeMs: event.resAgeMs,
                    steerMotor: event.steerMotor,
                    lwsStatus: event.lwsStatus,
                };
                framesThisScan += 1;
            } else if (event.kind === 'udvPipe') {
                udvPipe = {
                    dvStatus: event.dvStatus,
                    dvAgeMs: event.dvAgeMs,
                    accelCmdPct: event.accelCmdPct,
                    steerCmd: event.steerCmd,
                    ctrlAgeMs: event.ctrlAgeMs,
                    setupBits: event.setupBits,
                };
                framesThisScan += 1;
            } else if (event.kind === 'udvHealth') {
                udvHealth = {
                    freeHeapWords: event.freeHeapWords,
                    minFreeHeapWords: event.minFreeHeapWords,
                    taskMask: event.taskMask,
                    flags: event.flags,
                    stalledTask: event.stalledTask,
                    uptimeS: event.uptimeS,
                };
                udvHealthSeenThisScan = true;
                framesThisScan += 1;
            } else if (event.kind === 'udvFwInfo') {
                udvFw = {
                    gitHash: event.gitHash,
                    heapSizeKb: event.heapSizeKb,
                    uptimeS: event.uptimeS,
                };
                // fwinfo is ~1 Hz — not part of the cyclic scan.
            } else if (event.kind === 'udvCalib') {
                udvCalib = {
                    phase: event.phase,
                    phaseName: event.phaseName,
                    error: event.error,
                    errorName: event.errorName,
                    centerDdeg: event.centerDdeg,
                    halfRangeDdeg: event.halfRangeDdeg,
                    limitDdeg: event.limitDdeg,
                };
                // calib is calibration-only — not part of the cyclic scan.
            } else if (event.kind === 'udvSteer') {
                udvSteer = {
                    lwsRawDdeg: event.lwsRawDdeg,
                    steerActualDdeg: event.steerActualDdeg,
                    steerTargetDdeg: event.steerTargetDdeg,
                    lwsStatus: event.lwsStatus,
                    motorState: event.motorState,
                    motorStateName: event.motorStateName,
                };
            } else if (event.kind === 'udvCalibRelay') {
                udvCalibRelay = {
                    triggerRxCount: event.triggerRxCount,
                    relayCount: event.relayCount,
                    lastCmd: event.lastCmd,
                    armed: event.armed,
                };
                // calib-relay is calibration-only — not a cyclic scan frame.
            } else if (event.kind === 'udvCanHealth') {
                udvCanHealth = {
                    flags: event.flags,
                    lastErrorCode: event.lastErrorCode,
                    txErrCount: event.txErrCount,
                    rxErrCount: event.rxErrCount,
                    resRxCount: event.resRxCount,
                    nmtCount: event.nmtCount,
                    ackError: event.ackError,
                };
                framesThisScan += 1;
            } else if (event.kind === 'udvEbsPress') {
                udvEbsPress = {
                    tank1Dbar: event.tank1Dbar,
                    tank2Dbar: event.tank2Dbar,
                    ebsInit: event.ebsInit,
                    stubMask: event.stubMask,
                    tank1Ok: event.tank1Ok,
                    tank2Ok: event.tank2Ok,
                };
                framesThisScan += 1;
            }
            // Ack events come during arm/disarm; they're handled by
            // the status listener, not counted toward a scan window.
        });
        scanIntervalId = setInterval(() => {
            lastScanFrames = framesThisScan;
            framesThisScan = 0;
            // No 0x7A3 in the last window → the safety monitor stopped
            // emitting; treat the watchdog keep-alive as stale (#482).
            udvHealthStale = !udvHealthSeenThisScan;
            udvHealthSeenThisScan = false;
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

    // Cell-voltage colour scale (absolute, not deviation): grey when a cell
    // isn't reporting, red when out of the safe window, and a
    // yellowish-green → saturated-green ramp across the normal band (low →
    // full). Thresholds are the Li-ion under/over limits — tune here if the
    // pack chemistry changes. NTCs stay on the absolute °C scale below.
    const CELL_MV_UNDER = 3000; // < this = undervoltage (red)
    const CELL_MV_OVER = 4200; // > this = overvoltage (red)
    const CELL_GREEN_LOW = '#a3c93a'; // yellowish-green — low-normal
    const CELL_GREEN_HIGH = '#1f8f3a'; // saturated green — full
    const CELL_GREY = '#6b6b6b'; // not detected
    const CELL_RED = '#d64550'; // out of range

    // Linear RGB blend of two #rrggbb colours (t 0..1).
    function mixHex(a: string, b: string, t: number): string {
        const ca = [1, 3, 5].map((i) => parseInt(a.slice(i, i + 2), 16));
        const cb = [1, 3, 5].map((i) => parseInt(b.slice(i, i + 2), 16));
        const c = ca.map((v, i) => Math.round(v + (cb[i] - v) * t));
        return `rgb(${c[0]}, ${c[1]}, ${c[2]})`;
    }

    function cellColor(mv: number | null): string {
        if (mv === null) return CELL_GREY; // not detected
        if (mv < CELL_MV_UNDER || mv > CELL_MV_OVER) return CELL_RED; // out of range
        const t = (mv - CELL_MV_UNDER) / (CELL_MV_OVER - CELL_MV_UNDER);
        return mixHex(CELL_GREEN_LOW, CELL_GREEN_HIGH, Math.max(0, Math.min(1, t)));
    }

    // NTC temperature colour: a NON-LINEAR green→red ramp weighted toward
    // the hot end (gamma > 1 concentrates the colour change near the limit,
    // so hot cells are easy to spot); grey when a sensor isn't reporting.
    // Cells over NTC_LIMIT_C blink bright red (the `.over` CSS class).
    const NTC_COLD_C = 15; // at/below = full green
    const NTC_LIMIT_C = 60; // over this = blinking red + hard limit
    function ntcColor(c: number | null): string {
        if (c === null) return CELL_GREY; // not reporting
        const t = Math.max(
            0,
            Math.min(1, (c - NTC_COLD_C) / (NTC_LIMIT_C - NTC_COLD_C)),
        );
        const u = Math.pow(t, 2.2); // bias resolution toward higher temps
        const hue = 120 * (1 - u); // 120 green → 0 red, through yellow/orange
        return `hsl(${hue}, 68%, 42%)`;
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

    // Firmware-ID header chip — "AMS v1.6.0 · a1b2c3d4 · node 0x02".
    const fwLabel = $derived(
        fw === null
            ? null
            : `AMS v${fw.versionMajor}.${fw.versionMinor}.${fw.versionPatch}` +
                  ` · ${fw.gitHash.map((b) => b.toString(16).padStart(2, '0')).join('')}` +
                  ` · node 0x${fw.blNodeId.toString(16).toUpperCase().padStart(2, '0')}`,
    );

</script>

<div class="view">
    <header>
        <div>
            <h2>Telemetry</h2>
            {#if profile === 'all'}
                <p class="muted">
                    Cockpit — arms all three boards at once and reads them
                    side by side. AMS shows its states (cell voltages + NTC
                    temperatures live on the AMS tab); ECU and uDV in full.
                </p>
            {:else if profile === 'ams'}
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
                    uDV observer mode. Arming emits <code>0x7DE#DEADBEEF</code>
                    (sticky — no ACK, no disarm; the firmware clears it on
                    reboot); the uDV then streams AS state, RES, the /dv pipe,
                    and health at ~10 Hz (<code>0x7A0..=0x7A4</code>).
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
        {:else if profile === 'udv' && udvFwLabel !== null}
            <span
                class="pill fw-chip mono"
                title="From the 0x7A4 firmware-ID frame. Compare against the build you flashed."
            >
                {udvFwLabel}
            </span>
        {/if}
    </header>

    <!-- Profile selector — AMS / ECU / uDV each expose an arm handshake +
         decoded telemetry stream. -->
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
                <strong>No adapter selected.</strong> Telemetry needs an
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

    {#if profile === 'all'}
        <!-- Cockpit — arms AMS+ECU+uDV at once (best-effort, no ACK) and
             reads them side by side. AMS shows states only (grids on its
             own tab); ECU/uDV in full. -->
        <div class="toolbar card card-tight">
            {#if armState.kind === 'idle' || armState.kind === 'error'}
                <button
                    type="button"
                    class="btn btn-primary"
                    disabled={!adapterReady}
                    onclick={confirmArm}
                >
                    Enable all telemetry
                </button>
            {:else if armState.kind === 'confirming'}
                <div class="confirm">
                    <span class="confirm-text">Arm all three telemetry streams?</span>
                    <button type="button" class="btn btn-primary" onclick={arm}>
                        Yes, arm
                    </button>
                    <button type="button" class="btn" onclick={cancelArm}>Cancel</button>
                </div>
            {:else if armState.kind === 'arming'}
                <button type="button" class="btn" disabled>Arming…</button>
            {:else if armState.kind === 'armed'}
                <button type="button" class="btn btn-danger" onclick={disarm}>
                    Disable
                </button>
                <span class="stat">
                    <strong class="status-dot armed">●</strong><span>armed</span>
                </span>
                <span class="stat">
                    <span>frames / scan</span><strong>{lastScanFrames}</strong>
                </span>
            {/if}
        </div>

        {#if armState.kind === 'error'}
            <div class="banner banner-danger">
                <strong>Arm failed:</strong>
                {armState.message}
            </div>
        {/if}

        <div class="cockpit-full">
            <section class="cockpit-board">
                <div class="board-title">AMS</div>
                {#if amsHasData}
                    {@render amsStateCards()}
                {:else}
                    <div class="card placeholder-card">
                        <p class="muted small">No AMS frame yet.</p>
                    </div>
                {/if}
            </section>

            <section class="cockpit-board">
                <div class="board-title">ECU</div>
                {#if ecuHasData}
                    <div class="ecu-grid">{@render ecuCards(false)}</div>
                {:else}
                    <div class="card placeholder-card">
                        <p class="muted small">No ECU frame yet.</p>
                    </div>
                {/if}
            </section>

            <section class="cockpit-board">
                <div class="board-title">uDV</div>
                {#if udvHasData}
                    {@render udvCalibCard()}
                    <div class="ecu-grid">{@render udvCards()}</div>
                {:else}
                    <div class="card placeholder-card">
                        <p class="muted small">No uDV frame yet.</p>
                    </div>
                {/if}
            </section>
        </div>
    {:else if profile === 'udv'}
        <!-- uDV controls. Arm is sticky/fire-and-forget (0x7DE); no ACK,
             no disarm frame — Disable just tears down the reader. -->
        <div class="toolbar card card-tight">
            {#if armState.kind === 'idle' || armState.kind === 'error'}
                <button
                    type="button"
                    class="btn btn-primary"
                    disabled={!adapterReady}
                    onclick={confirmArm}
                >
                    Enable uDV telemetry
                </button>
            {:else if armState.kind === 'confirming'}
                <div class="confirm">
                    <span class="confirm-text">Arm uDV telemetry stream?</span>
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

        {#if !udvHasData}
            <div class="card placeholder-card">
                <h3>uDV telemetry</h3>
                <p class="muted">
                    {#if armState.kind === 'armed'}
                        Armed — waiting for the first frames from the uDV…
                    {:else}
                        Connect the uDV and hit
                        <strong>Enable uDV telemetry</strong>. Arming emits
                        <code>0x7DE#DEADBEEF</code> (sticky); the uDV then
                        streams AS state, RES, the /dv pipe, and health.
                    {/if}
                </p>
            </div>
        {:else}
            <!-- Steering end-stop calibration (#428). Gated: only shown
                 while pit-diag is armed and 0x7A0.. frames are arriving
                 (this whole branch). Triggers 0x7DF; progress from 0x7A6. -->
            {@render udvCalibCard()}

            <div class="ecu-grid">
                {@render udvCards()}
            </div>
        {/if}
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
                    Enable ECU telemetry
                </button>
            {:else if armState.kind === 'confirming'}
                <div class="confirm">
                    <span class="confirm-text">Arm ECU telemetry stream?</span>
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
                <h3>ECU telemetry</h3>
                <p class="muted">
                    {#if armState.kind === 'armed'}
                        Armed — waiting for the first frames from the ECU…
                    {:else}
                        Connect the ECU and hit
                        <strong>Enable ECU telemetry</strong>. Arming emits
                        <code>0x7E0#DEADBEEF</code>; the ECU then streams APPS,
                        brake, FSM, and inverter telemetry.
                    {/if}
                </p>
            </div>
        {:else}
            <div class="ecu-grid">
                {@render ecuCards(true)}
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
                Enable AMS telemetry
            </button>
        {:else if armState.kind === 'confirming'}
            <div class="confirm">
                <span class="confirm-text">Arm AMS telemetry stream?</span>
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
                <span>cell temps</span>
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
            <h3>AMS telemetry</h3>
            <p class="muted">
                {#if armState.kind === 'armed'}
                    Armed — waiting for the first frames from the AMS…
                {:else}
                    Connect the AMS and hit
                    <strong>Enable AMS telemetry</strong>. Arming emits
                    <code>0x7F0#DEADBEEF</code>; the AMS then streams cell
                    voltages, NTC temperatures, and FSM / balance
                    diagnostics.
                {/if}
            </p>
        </div>
    {:else}

    {@render amsStateCards()}

    <!-- Cell-V grid -->
    <section class="card">
        <div class="card-header">
            <h3>Cell voltages</h3>
            <span class="muted small">
                5 modules × 19 cells = 95. Green = normal (yellow-green low →
                deep green full); red = out of range (&lt;3.0 / &gt;4.2 V);
                grey = no reading.
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
            <h3>Cell temperatures</h3>
            <span class="muted small">
                {#if ntcStats !== null}
                    range {ntcStats.min}…{ntcStats.max} °C ·
                    {ntcStats.count}/{AMS_NUM_NTCS} reading ·
                {:else}
                    5 modules × 40 NTCs = 200 ·
                {/if}
                green→red (hot-weighted); &gt;{NTC_LIMIT_C} °C blinks; grey = no reading
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
                            class:over={ntc.c !== null && ntc.c > NTC_LIMIT_C}
                            style:background={ntcColor(ntc.c)}
                            title="Cell temp {ntc.idx + 1} — {ntc.c ?? '—'} °C"
                        >
                            <span class="tile-value mono">{ntc.c ?? '—'}</span>
                        </div>
                    {/each}
                </div>
            {/each}
        </div>
    </section>
    {/if}
    {/if}
</div>

{@render calibModal()}

<!-- Reusable board card sets — rendered both on the per-board tabs
     above and stacked together in the "All" cockpit. -->
{#snippet amsStateCards()}
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
            {AMS_EXPECTED_FRAMES_PER_SCAN}. The AMS firmware's telemetry
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

    <!-- FSM extended status (0x6C0). -->
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
{/snippet}

{#snippet ecuCards(showHealth: boolean)}
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
                            <span
                                class="pill pill-{ecuStatus.dvMode ? 'warning' : 'info'}"
                                title="Drive mode latched at ready-to-drive (0x700 dv_mode). Driverless = uDV torque source; Manual = pedals."
                            >
                                {ecuStatus.dvMode ? 'driverless' : 'manual'}
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
                        {@const iv = ecuInverter}
                        <!-- App_State + named DEM fault + active/latched (#484). -->
                        <div class="badge-row">
                            {#if ecuStatus !== null}
                                <span class="pill pill-{ecuInvTone(ecuStatus.invState)}">
                                    state: {ecuStatus.invState}
                                </span>
                            {/if}
                            <span
                                class="pill pill-{iv.invError === 0
                                    ? 'success'
                                    : iv.demPresent
                                      ? 'danger'
                                      : 'warning'}"
                                title="Inverter DEM fault (0x702 inv_error) — EPowerLabs W90 code {iv.invError}"
                            >
                                {iv.invErrorName === 'unknown'
                                    ? `code 0x${iv.invError.toString(16).toUpperCase().padStart(2, '0')}`
                                    : iv.invErrorName}
                            </span>
                            {#if iv.invError !== 0}
                                <span class="pill pill-{iv.demPresent ? 'danger' : 'info'}">
                                    {iv.demPresent ? 'active' : 'latched'}
                                </span>
                            {/if}
                        </div>
                        <div class="reads">
                            <span class="stat">
                                <span>DC bus</span>
                                <strong>{iv.dcBusVoltage} V</strong>
                            </span>
                            <span class="stat">
                                <span>motor</span>
                                <strong>{iv.invRpm} rpm</strong>
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

                <!-- Firmware health (0x704, 1 Hz) — diagnostic; hidden in the
                     aggregated cockpit, shown on the dedicated ECU tab. -->
                {#if showHealth}
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
                {/if}

                <!-- DV / autonomy (0x707) — the ECU's view of the uDV
                     handshake. `driverless` on the FSM card above means the
                     0x507 torque here is the torque source, not the pedals. -->
                <div class="card">
                    <h3 class="card-h">DV</h3>
                    {#if ecuDv !== null}
                        <div class="flags">
                            <span class="flag" class:on={ecuDv.dvR2dReq}>R2D req</span>
                            <span class="flag" class:on={ecuDv.dvCmdFresh}>
                                Cmd stream
                            </span>
                            <span class="flag" class:on={ecuDv.tsActive}>TS active</span>
                            <span class="flag" class:on={ecuDv.brakeOverLimit}>
                                EBS brake
                            </span>
                            <span class="flag" class:on={ecuDv.r2dConfirm}>R2D ✓</span>
                        </div>
                        <div class="reads">
                            <span class="stat">
                                <span>DV torque</span>
                                <strong>{ecuDv.dvTorquePct}%</strong>
                            </span>
                            <span class="stat">
                                <span>motor</span>
                                <strong>{ecuDv.motorRpmMech} rpm</strong>
                            </span>
                        </div>
                    {:else}
                        <p class="muted small">
                            No DV frame yet. Streams at 10 Hz while armed;
                            the flags stay low until the uDV drives the bus.
                        </p>
                    {/if}
                </div>
{/snippet}

{#snippet udvCalibCard()}
            <div class="card card-tight calib-card">
                <div class="calib-head">
                    <h3 class="card-h">Steering calibration</h3>
                    <button
                        type="button"
                        class="btn btn-primary"
                        onclick={openCalibModal}
                    >
                        Calibrate steering…
                    </button>
                    {#if udvCalib !== null}
                        <span
                            class="pill pill-{udvCalib.phase === 9
                                ? 'success'
                                : udvCalib.phase === 10
                                  ? 'danger'
                                  : 'info'}"
                        >
                            {udvCalib.phaseName}
                        </span>
                    {/if}
                </div>
                <p class="muted small">
                    Opens a guided pop-up: elevate the car, then follow the live
                    wheel angle to each end-stop. Results are saved on the uDV.
                </p>
            </div>
{/snippet}

<!-- Steering-calibration modal (#439 follow-up). Rendered once at the
     top of the view; opened from the trigger in the uDV panel / cockpit.
     Reads the same live udvCalib / udvSteer state that streams while
     pit-diag is armed. -->
{#snippet calibModal()}
    {#if calibModalOpen}
        <div
            class="modal-backdrop"
            role="button"
            tabindex="-1"
            onclick={closeCalibModal}
            onkeydown={(e) => {
                if (e.key === 'Escape') closeCalibModal();
            }}
        >
            <div
                class="modal calib-modal"
                role="dialog"
                tabindex="-1"
                aria-modal="true"
                aria-label="Steering calibration"
                onclick={(e) => e.stopPropagation()}
                onkeydown={(e) => e.stopPropagation()}
            >
                <div class="modal-head">
                    <h3 class="card-h">Steering calibration</h3>
                    <button
                        type="button"
                        class="modal-close"
                        aria-label="Close"
                        onclick={closeCalibModal}
                    >
                        ×
                    </button>
                </div>

                <div class="modal-body">
                    {#if calibError !== null}
                        <div class="banner banner-danger">{calibError}</div>
                    {/if}

                    {#if calibConfirming}
                        <p class="calib-step">
                            Car elevated? The steering will home and sweep to each
                            end-stop.
                        </p>
                        <p class="muted small">
                            This teaches the wheel its mechanical limits. You'll turn
                            fully to each lock when prompted (hold ~2&nbsp;s, past 30°),
                            then return near centre. From the two captured end-stops the
                            firmware computes the <strong>centre</strong> (new 0°), the
                            usable <strong>half-range</strong> each way, and the motor
                            <strong>soft-limit</strong>, and saves them. Hands clear
                            during the automatic verification sweep.
                        </p>
                    {:else}
                        <!-- Procedure checklist (#439 follow-up): show the whole arc
                             and where we are, so the live angle has visible purpose. -->
                        {@const ph = udvCalib?.phase ?? 0}
                        {@const si = calibStepIndex(ph)}
                        <ol class="calib-steps">
                            {#each CALIB_STEPS as label, i (label)}
                                <li
                                    class="calib-stepitem"
                                    class:done={ph === 9 || (si >= 0 && i < si)}
                                    class:active={ph !== 9 && ph !== 10 && i === si}
                                >
                                    <span class="step-dot">
                                        {#if ph === 9 || (si >= 0 && i < si)}✓{:else}{i + 1}{/if}
                                    </span>
                                    <span class="step-label">{label}</span>
                                </li>
                            {/each}
                        </ol>
                        <!-- Step-by-step operator guidance, keyed on the 0x7A6
                             phase — turns the blind trigger self-guiding. -->
                        {#if calibBusy}
                            <p class="calib-step">Triggering…</p>
                        {:else if udvCalib !== null && calibGuidance(udvCalib.phase) !== ''}
                            <p
                                class="calib-step"
                                class:ok={udvCalib.phase === 9}
                                class:fail={udvCalib.phase === 10}
                            >
                                {#if udvCalib.phase === 9}✅ {:else if udvCalib.phase === 10}❌ {/if}
                                {calibGuidance(udvCalib.phase)}
                                {#if udvCalib.phase === 10 && udvCalib.error !== 0}
                                    ({udvCalib.errorName})
                                {/if}
                            </p>
                        {/if}

                        <!-- Live LWS wheel angle (0x7A7): the primary capture
                             readout — watch it climb toward a stop and hold. -->
                        {#if udvSteer !== null}
                            {@const f = lwsFill(udvSteer.lwsRawDdeg / 10)}
                            <div class="lws">
                                <div class="lws-head">
                                    <span class="lws-angle mono">
                                        {(udvSteer.lwsRawDdeg / 10).toFixed(1)}°
                                    </span>
                                    <span class="lws-cap muted small">LWS wheel angle</span>
                                    <span
                                        class="pill pill-{udvSteer.motorState === -1
                                            ? 'danger'
                                            : udvSteer.motorState === 2
                                              ? 'warning'
                                              : udvSteer.motorState === 1
                                                ? 'success'
                                                : 'info'}"
                                    >
                                        motor: {udvSteer.motorStateName}
                                    </span>
                                </div>
                                <div class="lws-bar">
                                    <div class="lws-tick"></div>
                                    <!-- ±30° end-stop threshold markers -->
                                    <div
                                        class="lws-thresh"
                                        style="left: {50 - lwsPct(LWS_STOP_MIN_DEG)}%"
                                    ></div>
                                    <div
                                        class="lws-thresh"
                                        style="left: {50 + lwsPct(LWS_STOP_MIN_DEG)}%"
                                    ></div>
                                    <div
                                        class="lws-fill"
                                        style="left: {f.left}%; width: {f.width}%"
                                    ></div>
                                </div>
                                {#if calibCapturing(ph)}
                                    {@const mag = Math.abs(udvSteer.lwsRawDdeg / 10)}
                                    <p class="lws-cue" class:ok={mag >= LWS_STOP_MIN_DEG}>
                                        {#if mag >= LWS_STOP_MIN_DEG}
                                            Past {LWS_STOP_MIN_DEG}° — hold steady ~2 s to
                                            capture this end-stop.
                                        {:else}
                                            Keep turning — an end-stop must exceed
                                            {LWS_STOP_MIN_DEG}° ({mag.toFixed(0)}° so far).
                                        {/if}
                                    </p>
                                {/if}
                                <div class="reads">
                                    <span class="stat">
                                        <span>actual</span>
                                        <strong>{(udvSteer.steerActualDdeg / 10).toFixed(1)}°</strong>
                                    </span>
                                    <span class="stat">
                                        <span>target</span>
                                        <strong>{(udvSteer.steerTargetDdeg / 10).toFixed(1)}°</strong>
                                    </span>
                                    <span class="stat">
                                        <span>LWS status</span>
                                        <strong class="mono">
                                            0x{udvSteer.lwsStatus
                                                .toString(16)
                                                .toUpperCase()
                                                .padStart(2, '0')}
                                        </strong>
                                    </span>
                                </div>
                            </div>
                        {/if}

                        {#if udvCalib !== null}
                            <div class="badge-row">
                                <span
                                    class="pill pill-{udvCalib.phase === 9
                                        ? 'success'
                                        : udvCalib.phase === 10
                                          ? 'danger'
                                          : 'info'}"
                                >
                                    {udvCalib.phaseName}
                                </span>
                                {#if udvCalib.error !== 0}
                                    <span class="pill pill-danger">error: {udvCalib.errorName}</span>
                                {/if}
                            </div>
                            {#if udvCalib.phase === 9}
                                <div class="reads">
                                    <span class="stat">
                                        <span>centre</span>
                                        <strong>{(udvCalib.centerDdeg / 10).toFixed(1)}°</strong>
                                    </span>
                                    <span class="stat">
                                        <span>half-range</span>
                                        <strong>{(udvCalib.halfRangeDdeg / 10).toFixed(1)}°</strong>
                                    </span>
                                    <span class="stat">
                                        <span>limit</span>
                                        <strong>{(udvCalib.limitDdeg / 10).toFixed(1)}°</strong>
                                    </span>
                                </div>
                                <p class="muted small">
                                    <strong>Centre</strong> is the wheel's new 0°;
                                    <strong>half-range</strong> is the usable travel each
                                    way from centre; the motor won't drive past
                                    <strong>±limit</strong>. Saved to the wheel.
                                </p>
                            {/if}
                        {:else if udvSteer === null}
                            <p class="muted small">
                                Waiting for the first calibration frames from the uDV…
                            </p>
                        {/if}

                        <!-- Relay diagnostics (0x7A8, #457): confirms the
                             0x7DF trigger reached the uDV and was relayed on
                             to the steering — "is it even firing". -->
                        {#if udvCalibRelay !== null}
                            <div class="calib-relay">
                                <span class="muted small">relay diag</span>
                                <span class="stat">
                                    <span>trigger rx</span>
                                    <strong>{udvCalibRelay.triggerRxCount}</strong>
                                </span>
                                <span class="stat">
                                    <span>relayed</span>
                                    <strong>{udvCalibRelay.relayCount}</strong>
                                </span>
                                <span class="stat">
                                    <span>last cmd</span>
                                    <strong class="mono">
                                        0x{udvCalibRelay.lastCmd
                                            .toString(16)
                                            .toUpperCase()
                                            .padStart(2, '0')}
                                    </strong>
                                </span>
                                <span class="flag" class:on={udvCalibRelay.armed}>armed</span>
                            </div>
                        {/if}
                    {/if}
                </div>

                <div class="modal-foot">
                    {#if calibConfirming}
                        <button type="button" class="btn btn-primary" onclick={startCalibration}>
                            Yes, calibrate
                        </button>
                        <button type="button" class="btn" onclick={closeCalibModal}>
                            Cancel
                        </button>
                    {:else}
                        {#if udvCalib !== null && udvCalib.phase >= 1 && udvCalib.phase <= 8}
                            <button type="button" class="btn btn-danger" onclick={abortCalibration}>
                                Abort
                            </button>
                        {/if}
                        <button type="button" class="btn" onclick={closeCalibModal}>
                            Close
                        </button>
                    {/if}
                </div>
            </div>
        </div>
    {/if}
{/snippet}

{#snippet udvCards()}
                <!-- Live steering angle (0x7A7) — always-on card so the
                     wheel angle is glanceable without opening the calib
                     modal (which keeps the richer capture view). -->
                <div class="card">
                    <h3 class="card-h">Steering angle</h3>
                    {#if udvSteer !== null}
                        {@const f = lwsFill(udvSteer.lwsRawDdeg / 10)}
                        <div class="lws">
                            <div class="lws-head">
                                <span class="lws-angle mono">
                                    {(udvSteer.lwsRawDdeg / 10).toFixed(1)}°
                                </span>
                                <span class="lws-cap muted small">LWS wheel angle</span>
                                <span
                                    class="pill pill-{udvSteer.motorState === -1
                                        ? 'danger'
                                        : udvSteer.motorState === 2
                                          ? 'warning'
                                          : udvSteer.motorState === 1
                                            ? 'success'
                                            : 'info'}"
                                >
                                    motor: {udvSteer.motorStateName}
                                </span>
                            </div>
                            <div class="lws-bar">
                                <div class="lws-tick"></div>
                                <div
                                    class="lws-fill"
                                    style="left: {f.left}%; width: {f.width}%"
                                ></div>
                            </div>
                            <div class="reads">
                                <span class="stat">
                                    <span>actual</span>
                                    <strong>{(udvSteer.steerActualDdeg / 10).toFixed(1)}°</strong>
                                </span>
                                <span class="stat">
                                    <span>target</span>
                                    <strong>{(udvSteer.steerTargetDdeg / 10).toFixed(1)}°</strong>
                                </span>
                                <span class="stat">
                                    <span>LWS status</span>
                                    <strong class="mono">
                                        0x{udvSteer.lwsStatus
                                            .toString(16)
                                            .toUpperCase()
                                            .padStart(2, '0')}
                                    </strong>
                                </span>
                            </div>
                        </div>
                    {:else}
                        <p class="muted small">No steering frame yet (0x7A7).</p>
                    {/if}
                </div>

                <!-- EBS air-tank pressures (0x7A9, #475) — bring-up + sensor
                     calibration: watch each tank charge/vent against a gauge. -->
                <div class="card">
                    <h3 class="card-h">EBS pressures</h3>
                    {#if udvEbsPress !== null}
                        {@const t1 = udvEbsPress.tank1Dbar / 10}
                        {@const t2 = udvEbsPress.tank2Dbar / 10}
                        <div class="badge-row">
                            <span
                                class="pill pill-{udvEbsPress.ebsInit === 'Done'
                                    ? 'success'
                                    : udvEbsPress.ebsInit === 'Failed'
                                      ? 'danger'
                                      : 'info'}"
                            >
                                init: {udvEbsPress.ebsInit}
                            </span>
                            {#if bit(udvEbsPress.stubMask, 2)}
                                <span
                                    class="pill pill-warning"
                                    title="EBS_SENSORS stub: the pass/fail gate is faked, but the pressure value shown is the real sensor reading."
                                >
                                    sensors stubbed (gate faked, reading real)
                                </span>
                            {/if}
                        </div>
                        <div class="meter-row">
                            <span class="meter-label">Tank 1 <span class="muted small">A5</span></span>
                            <div class="meter">
                                <div
                                    class="meter-fill"
                                    style="width: {Math.max(0, Math.min(100, t1 * 10))}%"
                                ></div>
                            </div>
                            <span class="meter-val mono">{t1.toFixed(1)} bar</span>
                            <span class="flag" class:on={udvEbsPress.tank1Ok}>&gt;1.0</span>
                        </div>
                        <div class="meter-row">
                            <span class="meter-label">Tank 2 <span class="muted small">A4</span></span>
                            <div class="meter">
                                <div
                                    class="meter-fill"
                                    style="width: {Math.max(0, Math.min(100, t2 * 10))}%"
                                ></div>
                            </div>
                            <span class="meter-val mono">{t2.toFixed(1)} bar</span>
                            <span class="flag" class:on={udvEbsPress.tank2Ok}>&gt;1.0</span>
                        </div>
                        <p class="muted small">
                            Festo SPAN 0–10 bar. The <code>&gt;1.0</code> flags are the
                            firmware's CheckPressure gate. Charge to a known gauge
                            pressure to validate the decoded bar (sensor calibration).
                        </p>
                    {:else}
                        <p class="muted small">No EBS pressure frame yet (0x7A9).</p>
                    {/if}
                </div>

                <!-- AS status (0x7A0) -->
                <div class="card">
                    <h3 class="card-h">Autonomous system</h3>
                    {#if udvStatus !== null}
                        <div class="badge-row">
                            <span
                                class="pill pill-{udvStatus.asState === 'Emergency'
                                    ? 'danger'
                                    : udvStatus.asState === 'Driving'
                                      ? 'success'
                                      : 'info'}"
                            >
                                {udvStatus.asState}
                            </span>
                            <span class="pill pill-info">assi: {udvStatus.assi}</span>
                            <span class="pill pill-info">ebs: {udvStatus.ebsInit}</span>
                        </div>
                        <div class="flags">
                            <span class="flag" class:on={bit(udvStatus.signals, 0)}>ASMS</span>
                            <span class="flag" class:on={bit(udvStatus.signals, 1)}>TS</span>
                            <span class="flag" class:on={bit(udvStatus.signals, 3)}>EBS</span>
                            <span class="flag" class:on={bit(udvStatus.signals, 4)}>ASB</span>
                            <span class="flag" class:on={bit(udvStatus.signals, 5)}>Brakes</span>
                            <span class="flag" class:on={bit(udvStatus.signals, 7)}>R2D</span>
                            <span class="flag" class:on={bit(udvStatus.signals, 8)}>Standstill</span>
                            <span class="flag" class:on={bit(udvStatus.signals, 2)}>SDC open</span>
                        </div>
                        <div class="reads">
                            <span class="stat">
                                <span>mission</span>
                                <strong>
                                    {udvStatus.missionId < 0
                                        ? 'none'
                                        : `${udvStatus.missionId} · ${missionName(udvStatus.missionId)}`}
                                </strong>
                            </span>
                            <span
                                class="stat"
                                class:bad={udvStatus.stubMask !== 0}
                                title="Compiled-in bench stubs (0x7A0). Any set = a stubbed, non-flight image — revert to 0 before racing."
                            >
                                <span>stubs</span>
                                <strong>{udvStubNames(udvStatus.stubMask)}</strong>
                            </span>
                        </div>
                    {:else}
                        <p class="muted small">No status frame yet.</p>
                    {/if}
                </div>

                <!-- RES + steering (0x7A1) -->
                <div class="card">
                    <h3 class="card-h">RES / steering</h3>
                    {#if udvRes !== null}
                        <div class="badge-row">
                            <span
                                class="pill pill-{udvRes.resStatus === 'Estop'
                                    ? 'danger'
                                    : udvRes.resStatus === 'Go'
                                      ? 'success'
                                      : 'info'}"
                            >
                                RES: {udvRes.resStatus}
                            </span>
                            <span
                                class="pill pill-{udvRes.steerMotor === 'Emergency'
                                    ? 'danger'
                                    : udvRes.steerMotor === 'On'
                                      ? 'success'
                                      : 'info'}"
                            >
                                steer: {udvRes.steerMotor}
                            </span>
                        </div>
                        <div class="flags">
                            <span class="flag" class:on={bit(udvRes.bits, 0)}>E-stop</span>
                            <span class="flag" class:on={bit(udvRes.bits, 1)}>Go</span>
                            <span class="flag" class:on={bit(udvRes.bits, 2)}>Pre-alarm</span>
                            <span class="flag" class:on={bit(udvRes.bits, 3)}>Brake&gt;lim</span>
                            <span class="flag" class:on={bit(udvRes.bits, 6)}>TS active</span>
                        </div>
                        <div class="reads">
                            <span class="stat">
                                <span>radio</span><strong>{udvRes.radioQuality}</strong>
                            </span>
                            <span class="stat" class:bad={udvRes.resAgeMs === 65535}>
                                <span>RES age</span>
                                <strong>{udvRes.resAgeMs === 65535 ? 'never' : `${udvRes.resAgeMs} ms`}</strong>
                            </span>
                        </div>
                    {:else}
                        <p class="muted small">No RES frame yet.</p>
                    {/if}
                </div>

                <!-- /dv pipe (0x7A2) -->
                <div class="card">
                    <h3 class="card-h">/dv pipe</h3>
                    {#if udvPipe !== null}
                        <div class="badge-row">
                            <span
                                class="pill pill-{udvPipe.dvStatus === 5
                                    ? 'danger'
                                    : udvPipe.dvStatus === 6
                                      ? 'danger'
                                      : udvPipe.dvStatus === 3
                                        ? 'success'
                                        : 'info'}"
                                title="/dv/status (0x7A2 byte 0)"
                            >
                                {dvStatusName(udvPipe.dvStatus)}
                            </span>
                        </div>
                        <div class="flags">
                            <span class="flag" class:on={bit(udvPipe.setupBits, 0)}>Setup</span>
                            <span class="flag" class:on={bit(udvPipe.setupBits, 1)}>Ready</span>
                            <span class="flag" class:on={bit(udvPipe.setupBits, 2)}>Going</span>
                            <span class="flag" class:on={bit(udvPipe.setupBits, 3)}>Emergency</span>
                            <span class="flag" class:on={bit(udvPipe.setupBits, 4)}>Finished</span>
                        </div>
                        <div class="reads">
                            <span class="stat">
                                <span>accel cmd</span><strong>{udvPipe.accelCmdPct}%</strong>
                            </span>
                            <span class="stat">
                                <span>steer cmd</span><strong>{udvPipe.steerCmd}</strong>
                            </span>
                            <span class="stat" class:bad={udvPipe.dvAgeMs === 65535}>
                                <span>/dv age</span>
                                <strong>{udvPipe.dvAgeMs === 65535 ? 'never' : `${udvPipe.dvAgeMs} ms`}</strong>
                            </span>
                            <span class="stat">
                                <span>ctrl age</span>
                                <strong>{udvPipe.ctrlAgeMs === 65535 ? 'never' : `${udvPipe.ctrlAgeMs} ms`}</strong>
                            </span>
                        </div>
                    {:else}
                        <p class="muted small">No pipe frame yet.</p>
                    {/if}
                </div>

                <!-- Firmware health (0x7A3) -->
                <div class="card">
                    <h3 class="card-h">Firmware health</h3>
                    <!-- Safety-watchdog keep-alive (#482): green only on a
                         FRESH 0x7A3 with bit2 set; stale/missing reads NOT ok. -->
                    <div class="badge-row">
                        <span
                            class="pill pill-{udvIwdgOk ? 'success' : 'danger'}"
                            title="Safety-monitor keep-alive (0x7A3 flags bit 2). Green only when a fresh HEALTH frame reports the monitor is refreshing the hardware IWDG; a stale or missing frame (wedged node) reads NOT ok."
                        >
                            IWDG keep-alive: {udvIwdgOk ? 'ok' : 'not ok'}
                        </span>
                    </div>
                    {#if udvHealth !== null}
                        <div class="flags">
                            <span class="flag" class:on={bit(udvHealth.taskMask, 0)}>IMU</span>
                            <span class="flag" class:on={bit(udvHealth.taskMask, 1)}>CAN</span>
                            <span class="flag" class:on={bit(udvHealth.taskMask, 2)}>APP</span>
                        </div>
                        <div class="reads">
                            <span class="stat">
                                <span>free heap</span>
                                <strong class="mono">{udvHealth.freeHeapWords * 4} B</strong>
                            </span>
                            <span class="stat">
                                <span>min heap</span>
                                <strong class="mono">{udvHealth.minFreeHeapWords * 4} B</strong>
                            </span>
                            <span class="stat">
                                <span>uptime</span><strong>{udvHealth.uptimeS} s</strong>
                            </span>
                            <span class="stat" class:bad={udvHealth.stalledTask >= 0}>
                                <span>stalled task</span>
                                <strong>{udvHealth.stalledTask < 0 ? 'none' : udvHealth.stalledTask}</strong>
                            </span>
                            <span class="stat" class:bad={bit(udvHealth.flags, 0)}>
                                <span>IWDG reset</span>
                                <strong>{bit(udvHealth.flags, 0) ? 'yes' : 'no'}</strong>
                            </span>
                            <span class="stat" class:bad={bit(udvHealth.flags, 1)}>
                                <span>emergency</span>
                                <strong>{bit(udvHealth.flags, 1) ? 'latched' : 'no'}</strong>
                            </span>
                        </div>
                    {:else}
                        <p class="muted small">No health frame yet.</p>
                    {/if}
                </div>

                <!-- FDCAN1 CAN-health (0x7A5, #457). LEC 3 = ACK (no other
                     node ACKed) — expected on a lone bench uDV. -->
                <div class="card">
                    <h3 class="card-h">CAN health</h3>
                    {#if udvCanHealth !== null}
                        <div class="flags">
                            <span class="flag" class:on={bit(udvCanHealth.flags, 0)}>Bus-off</span>
                            <span class="flag" class:on={bit(udvCanHealth.flags, 1)}>
                                Err-passive
                            </span>
                            <span class="flag" class:on={bit(udvCanHealth.flags, 2)}>Warning</span>
                            <span class="flag" class:on={udvCanHealth.ackError}>ACK err</span>
                        </div>
                        <div class="reads">
                            <span class="stat" title="Last error code — 3 = ACK (no other node), 0/7 = ok">
                                <span>LEC</span><strong>{udvCanHealth.lastErrorCode}</strong>
                            </span>
                            <span class="stat" class:bad={udvCanHealth.txErrCount > 0}>
                                <span>TEC</span><strong>{udvCanHealth.txErrCount}</strong>
                            </span>
                            <span class="stat" class:bad={udvCanHealth.rxErrCount > 0}>
                                <span>REC</span><strong>{udvCanHealth.rxErrCount}</strong>
                            </span>
                            <span class="stat">
                                <span>RES rx</span><strong>{udvCanHealth.resRxCount}</strong>
                            </span>
                            <span class="stat">
                                <span>NMT tx</span><strong>{udvCanHealth.nmtCount}</strong>
                            </span>
                        </div>
                    {:else}
                        <p class="muted small">No CAN-health frame yet.</p>
                    {/if}
                </div>
{/snippet}

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

    /* Diag grid — labelled key/value cells laid out auto-fill. */
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
        border: 1px dashed var(--border);
        color: var(--text-muted);
    }
    .tile-value {
        font-family: var(--font-mono);
    }
    /* Taller than before so the °C value is legible at 40 tiles/row. */
    .ntc-tile {
        height: 30px;
        font-size: 0.62rem;
        overflow: hidden;
    }
    /* Over the 60 °C limit: blink bright red so it can't be missed. The
       animation background overrides the inline ramp colour. */
    .ntc-tile.over {
        animation: ntc-over-blink 0.75s steps(1, end) infinite;
        color: #fff;
        font-weight: 600;
    }
    @keyframes ntc-over-blink {
        0%,
        49% {
            background: #ff1f3d;
        }
        50%,
        100% {
            background: #7a0a18;
        }
    }

    /* ---- ECU pit-diag panels ---- */
    .ecu-grid {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
        gap: var(--space-4);
    }
    /* Cockpit ("All") — the three boards stacked vertically; each board
       shows its full card set (AMS minus the cell/NTC grids, which stay
       on its own tab). Cards flow horizontally within each board's grid. */
    .cockpit-full {
        display: flex;
        flex-direction: column;
        gap: var(--space-5, 2rem);
    }
    .cockpit-board {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
    }
    .board-title {
        font-weight: 700;
        font-size: var(--text-lg, 1.15rem);
        letter-spacing: 0.04em;
        color: var(--text);
        padding-bottom: var(--space-1);
        border-bottom: 1px solid var(--border);
    }
    /* Roomier cards in the telemetry grid — the dense diag reads want
       breathing room to stay glanceable. */
    .ecu-grid > .card {
        padding: var(--space-4);
    }
    .card-h {
        margin: 0 0 var(--space-3);
        font-size: var(--text-base);
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
    .calib-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        margin-bottom: var(--space-3);
    }
    .calib-head {
        display: flex;
        align-items: center;
        flex-wrap: wrap;
        gap: var(--space-3);
    }
    .calib-head .card-h {
        margin: 0;
    }
    /* Per-phase operator guidance (#439) — the loud "do this now" line. */
    .calib-step {
        margin: 0;
        font-size: var(--text-lg, 1.15rem);
        font-weight: 600;
        line-height: 1.4;
        color: var(--text);
    }
    .calib-step.ok {
        color: var(--success);
    }
    .calib-step.fail {
        color: var(--danger);
    }
    /* Relay-diag footer in the calib modal (0x7A8) — trigger/relay counters. */
    .calib-relay {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-2) var(--space-3);
        padding-top: var(--space-2);
        border-top: 1px solid var(--border);
    }
    /* Live LWS wheel-angle readout (#439 / 0x7A7). */
    .lws {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
    }
    .lws-head {
        display: flex;
        align-items: baseline;
        flex-wrap: wrap;
        gap: var(--space-3);
    }
    .lws-angle {
        font-size: 2rem;
        font-weight: 700;
        line-height: 1;
        color: var(--text);
    }
    .lws-cap {
        margin-right: auto;
    }
    /* Centre-origin bar: a full-width track with a centre tick, and a fill
       that grows left or right from the middle with the signed angle. */
    .lws-bar {
        position: relative;
        height: 12px;
        border-radius: var(--radius-sm, 4px);
        background: var(--bg);
        border: 1px solid var(--border);
        overflow: hidden;
    }
    .lws-tick {
        position: absolute;
        left: 50%;
        top: 0;
        bottom: 0;
        width: 1px;
        background: var(--text-muted);
        opacity: 0.6;
    }
    .lws-fill {
        position: absolute;
        top: 0;
        bottom: 0;
        background: var(--accent);
        transition:
            left 80ms linear,
            width 80ms linear;
    }
    /* ±30° end-stop threshold markers — the live angle must pass one of
       these to register a valid stop. */
    .lws-thresh {
        position: absolute;
        top: 0;
        bottom: 0;
        width: 2px;
        margin-left: -1px;
        background: var(--warning);
        opacity: 0.7;
    }
    /* Threshold cue text under the bar during capture (phases 1–2). */
    .lws-cue {
        margin: 0;
        font-size: var(--text-sm);
        color: var(--text-muted);
    }
    .lws-cue.ok {
        color: var(--success);
        font-weight: 600;
    }
    /* Procedure checklist — the whole calibration arc as ordered steps,
       with the current one highlighted (#439 follow-up). */
    .calib-steps {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-2) var(--space-3);
        margin: 0;
        padding: 0;
        list-style: none;
    }
    .calib-stepitem {
        display: inline-flex;
        align-items: center;
        gap: var(--space-1);
        font-size: var(--text-sm);
        color: var(--text-muted);
    }
    .calib-stepitem .step-dot {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 1.35em;
        height: 1.35em;
        border-radius: 50%;
        border: 1px solid var(--border);
        font-size: 0.8em;
        font-weight: 600;
    }
    .calib-stepitem.active {
        color: var(--text);
        font-weight: 600;
    }
    .calib-stepitem.active .step-dot {
        border-color: var(--accent);
        background: var(--accent);
        color: var(--accent-contrast, #fff);
    }
    .calib-stepitem.done .step-dot {
        border-color: var(--success);
        background: var(--success);
        color: var(--accent-contrast, #fff);
    }
    /* Calibration modal (#439 follow-up) — a focused pop-up so the live
       angle + guidance only appear once the operator opts in. */
    .modal-backdrop {
        position: fixed;
        inset: 0;
        z-index: 50;
        display: flex;
        align-items: center;
        justify-content: center;
        padding: var(--space-4);
        background: rgb(0 0 0 / 0.5);
    }
    .modal {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        width: min(560px, 100%);
        max-height: 90vh;
        overflow-y: auto;
        padding: var(--space-4);
        background: var(--surface, var(--bg));
        border: 1px solid var(--border);
        border-radius: var(--radius-lg, 10px);
        box-shadow: 0 12px 40px rgb(0 0 0 / 0.35);
    }
    .modal-head {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
    }
    .modal-head .card-h {
        margin: 0;
    }
    .modal-close {
        appearance: none;
        border: none;
        background: transparent;
        color: var(--text-muted);
        font-size: 1.5rem;
        line-height: 1;
        padding: 0 var(--space-1);
        cursor: pointer;
    }
    .modal-close:hover {
        color: var(--text);
    }
    .modal-body {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
    }
    .modal-foot {
        display: flex;
        justify-content: flex-end;
        gap: var(--space-3);
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
