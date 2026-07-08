// Typed wrappers + display helpers for the AMS pit-diag observer
// (Tier 2). Mirrors PitDiagRequest / PitDiagEvent / PitDiagStatus in
// `apps/can-studio/src-tauri/src/pit_diag.rs`.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import type { InterfaceType } from './types';

// ---- Pack geometry ----
// Mirrors the AMS_* constants in `can_flasher::pit_diag`. Hardcoded
// here for the grid layout maths; the values will move to a shared
// types file once VCU/UDV profiles land in slice 5.

export const AMS_NUM_MODULES = 5;
export const AMS_CELLS_PER_MODULE = 19;
export const AMS_NUM_CELLS = AMS_NUM_MODULES * AMS_CELLS_PER_MODULE; // 95
export const AMS_NTC_PER_MODULE = 40;
export const AMS_NUM_NTCS = AMS_NUM_MODULES * AMS_NTC_PER_MODULE; // 200
/** Sentinel value emitted in slots past the last real cell. */
export const AMS_CELLV_SENTINEL = 0xffff;
/** Sentinel value for an unwired / shorted NTC channel (INT8_MIN). */
export const AMS_NTC_SENTINEL = -128;
/**
 * Total frames the AMS emits per 1 Hz scan when armed.
 *
 * Source of truth: `docs/dbc/ams.dbc` in IFS08-CE-AMS. The diag
 * block is the contiguous `0x6C0..=0x6C8` (9 frames):
 *
 *   58 = 24 cell-V + 25 NTC + 9 diag
 *      = FSM(0x6C0) + poll(0x6C1) + balance×2(0x6C2/0x6C3)
 *        + boot(0x6C4) + crash(0x6C5) + fw-id(0x6C6)
 *        + per-IC PEC×2(0x6C7/0x6C8)
 *
 * (Historical note: the host briefly used 53 in PR #263 — built off
 * the `CAN_MAP.md` prose table, which had skipped `0x6C2..=0x6C6`.
 * The DBC modelled them all along; AMS #293 fixed the doc table.)
 *
 * The view banners a "schema drift suspected" warning if the
 * observed scan rate diverges from this value by more than ±2.
 */
export const AMS_EXPECTED_FRAMES_PER_SCAN = 58;
/** Monitor ICs in the pack — 2 per module × 5 modules. */
export const AMS_NUM_ICS = 10;

// ---- Request ----

export interface PitDiagRequest {
    interface: InterfaceType;
    channel: string | null;
    bitrate: number;
    /**
     * Which ECU profile to arm. Only `"ams"` is implemented today —
     * the AMS exposes a 0x7F0 arm handshake + the 0x6C0..=0x6C8
     * stream. `"ecu"` / `"udv"` are selectable in the UI but have no
     * pit-diag firmware/frames defined yet (the backend rejects them
     * with a clean "not implemented" error), so the view shows a
     * placeholder rather than arming.
     */
    profile: 'ams' | 'ecu' | 'udv' | 'all';
}

/** The ECU profiles the pit-diag view can target. Only `ams` is
 *  wired end-to-end; the others render a "not available yet" panel. */
export type PitDiagProfile = PitDiagRequest['profile'];

// ---- Streamed events ----

/**
 * Lifecycle status on `pit-diag:status`. Lets the UI render
 * armed / waiting / failed without parsing per-frame events.
 */
export type PitDiagStatus =
    | { kind: 'armed'; profile: string }
    | { kind: 'stopped' }
    | { kind: 'error'; message: string };

/**
 * Per-frame event on `pit-diag:frame`. Tagged union — discriminator
 * is `kind`, payload depends on the variant. Mirrors the Rust enum
 * `PitDiagEvent` field-for-field.
 */
export type PitDiagEvent =
    | { kind: 'ack'; enabled: boolean }
    | {
          kind: 'cellVoltage';
          frameIdx: number;
          firstCell: number;
          voltagesMv: [number, number, number, number];
      }
    | {
          kind: 'ntcTemp';
          frameIdx: number;
          firstNtc: number;
          tempsC: [number, number, number, number, number, number, number, number];
      }
    | {
          /** 0x6C0 — FSM extended status. */
          kind: 'fsmStatus';
          /** Stringified FSM state from the firmware enum:
           *  "start" | "precharge" | "transition" | "run" | "charge"
           *  | "error" | "unknown(0xNN)". */
          state: string;
          /** Mode lock as a string: "undecided" | "car" | "charger"
           *  | "unknown(0xNN)". */
          modeLocked: string;
          tsms: boolean;
          dashChg: boolean;
          amsOk: boolean;
          pecErrorTotal: number;
          /** Latched-ERROR predicate branch (#276): "none" when
           *  healthy, else "bmsStale" / "cellOverVoltage" / … /
           *  "fsmError" / "unknown(0xNN)". */
          faultReason: string;
          /** Context for faultReason: module index (bmsStale),
           *  module_online_mask (bmsModuleOffline), or 0. */
          faultDetail: number;
      }
    | {
          /** 0x6C1 — V-poll latency + T-sweep failure mask. */
          kind: 'pollTiming';
          lastVPollMs: number;
          worstVPollMs: number;
          tSweepFailMask: number;
      }
    | {
          /** 0x6C2 — balance DCC mask, cells 0..=63. Decimal string
           *  (full u64 exceeds JS safe-integer range). */
          kind: 'balanceMaskA';
          dccLo: string;
      }
    | {
          /** 0x6C3 — balance DCC mask hi (cells 64..=94, low 31 bits)
           *  + cycle counters. */
          kind: 'balanceMaskB';
          dccHi: number;
          cyclesTotal: number;
          cyclesActive: number;
      }
    | {
          /** 0x6C4 — boot diagnostics. */
          kind: 'bootDiag';
          /** "powerOn" | "canTrigger" | "manual" | "unknown(0x…)". */
          jumpReason: string;
          /** 0..7 init milestone; 7 = clean self-exit. */
          appInitProgress: number;
          /** Low 24 bits of HAL_FDCAN_Start; 0 = HAL_OK. */
          fdcan1StartResult: number;
      }
    | {
          /** 0x6C5 — crash post-mortem from the previous boot. */
          kind: 'postMortem';
          stackOverflowSeen: boolean;
          watermarkLowByte: number;
          taskAddrLo: number;
          mallocFailedCount: number;
          /** true when nothing crashed — suppress the banner. */
          clean: boolean;
      }
    | {
          /** 0x6C6 — firmware identity. */
          kind: 'fwId';
          versionMajor: number;
          versionMinor: number;
          versionPatch: number;
          gitHash: number[];
          blNodeId: number;
      }
    | {
          /** 0x6C7 / 0x6C8 — per-IC PEC error counts. `firstIc` is 0
           *  for 0x6C7 (ICs 0..7) or 8 for 0x6C8 (ICs 8..9). Only the
           *  first `valid` entries of `counts` are real ICs. */
          kind: 'perIcPec';
          firstIc: number;
          valid: number;
          counts: number[];
      }
    | {
          /** 0x4A4 — always-on contactor / AMS_OK GPIO read-backs.
           *  What the firmware is driving the coils to (not a closed
           *  confirmation). */
          kind: 'relayStatus';
          airNegative: boolean;
          airPositive: boolean;
          precharge: boolean;
          amsOk: boolean;
      }
    | {
          /** 0x135 — accu + DC-DC currents in deci-amps (×0.1 = A,
           *  + = discharge). */
          kind: 'acuCurrents';
          accuDa: number;
          dcdcDa: number;
      }
    | {
          /** 0x4A1 — pack voltage (mV) + filtered pack current (mA,
           *  + discharge / − charge). */
          kind: 'pack';
          packVoltageMv: number;
          filteredMa: number;
      }
    // ---- ECU profile (0x700..=0x705) ----
    | {
          /** ECU 0x700 — FSM / inverter state, cockpit flags, torque,
           *  min cell-V. */
          kind: 'ecuStatus';
          /** FSM state name: "waitInvVdcConfig" | "precharge" |
           *  "waitStartBrake" | "r2dDelay" | "waitInvStandby" |
           *  "active" | "amsError" | "unknown(0xNN)". */
          fsmState: string;
          /** Inverter state: "standby" | "ready" | "unknown(0xNN)". */
          invState: string;
          /** APPS plausibility (EV 2.3) OK. */
          ev23: boolean;
          /** Brake/throttle plausibility (T11.8/9) OK. */
          t1189: boolean;
          /** Ready-to-drive sound active. */
          rtdsActive: boolean;
          /** Precharge complete. */
          okPrecharge: boolean;
          /** Start button pressed. */
          startButton: boolean;
          /** DV (driverless) drive latched this cycle (#109). */
          dvMode: boolean;
          torquePct: number;
          vCellMinMv: number;
          torqueCmd: number;
      }
    | {
          /** ECU 0x701 — APPS pedal channels + brake raw ADC. */
          kind: 'ecuPedals';
          apps1Raw: number;
          apps2Raw: number;
          brakeRaw: number;
          apps1Pct: number;
          apps2Pct: number;
      }
    | {
          /** ECU 0x705 — physical brake. `brakePressureDbar` is
           *  deci-bar; divide by 10 for bar. */
          kind: 'ecuBrake';
          brakePressureDbar: number;
          brakePct: number;
      }
    | {
          /** ECU 0x702 — inverter telemetry. `invRpm` is signed. */
          kind: 'ecuInverter';
          dcBusVoltage: number;
          invRpm: number;
          invError: number;
      }
    | {
          /** ECU 0x706 — inverter temperatures, °C. `205` = sensor
           *  disconnected (raw 0xFF − 50). */
          kind: 'ecuInverterTemps';
          boardDegc: number;
          pwrstgDegc: number;
          motor1Degc: number;
          motor2Degc: number;
      }
    | {
          /** ECU 0x703 — firmware identity. */
          kind: 'ecuFwInfo';
          versionMajor: number;
          versionMinor: number;
          versionPatch: number;
          gitHash: number[];
      }
    | {
          /** ECU 0x704 — firmware health (1 Hz). */
          kind: 'ecuHealth';
          freeHeap: number;
          minFreeHeap: number;
          taskControl: boolean;
          taskCanRx: boolean;
          taskCanTx: boolean;
          taskDiag: boolean;
          /** "powerOn" | "pin" | "software" | "iwdg" | … */
          resetCause: string;
          uptimeS: number;
          lastFault: number;
          /** "none" | "hardFault" | "stackOverflow" | … */
          lastFaultName: string;
      }
    | {
          /** ECU 0x707 — DV (driverless) integration view (#109). The
           *  `dvMode` latch itself rides `ecuStatus`; this carries the
           *  handshake around it. */
          kind: 'ecuDv';
          /** uDV 0x510 R2D request set AND fresh. */
          dvR2dReq: boolean;
          /** uDV 0x507 torque stream fresh. */
          dvCmdFresh: boolean;
          /** ECU TX 0x504 — tractive-system-active view. */
          tsActive: boolean;
          /** ECU TX 0x505 — EBS hard-braking verdict. */
          brakeOverLimit: boolean;
          /** ECU TX 0x511 — R2D confirmed (== DV drive latched). */
          r2dConfirm: boolean;
          /** Conditioned autonomous torque actually applied, 0..100 %. */
          dvTorquePct: number;
          /** Mechanical rpm streamed to the uDV on 0x506 (signed). */
          motorRpmMech: number;
      }
    | {
          /** uDV 0x7A0 — AS state + 10-bit signal mask + mission + EBS-init
           *  + ASSI. Enum fields are debug names ("Driving"/"Ready"/…). */
          kind: 'udvStatus';
          asState: string;
          /** Raw signal bitmask (b0 ASMS, b1 TS, b2 SDC_open, b3 EBS_act,
           *  b4 ABS_ok, b5 brakes, b6 mission_sel, b7 R2D, b8 standstill,
           *  b9 finished). */
          signals: number;
          missionId: number;
          ebsInit: string;
          /** Stub mask: b0 EBS, b1 DVPC. */
          stubMask: number;
          assi: string;
          diagArmed: boolean;
      }
    | {
          /** uDV 0x7A1 — RES + steering. `bits`: b0 estop, b1 go,
           *  b2 pre_alarm, b3 brake_over_limit, b4 listen_go,
           *  b5 sdc_res_open, b6 ts_active_can. */
          kind: 'udvRes';
          raw191: number;
          resStatus: string;
          bits: number;
          radioQuality: number;
          /** RES frame age ms (65535 = never). */
          resAgeMs: number;
          steerMotor: string;
          lwsStatus: number;
      }
    | {
          /** uDV 0x7A2 — /dv pipe. `setupBits`: b0 in_progress, b1 ready,
           *  b2 going, b3 emergency, b4 finished. */
          kind: 'udvPipe';
          dvStatus: number;
          dvAgeMs: number;
          accelCmdPct: number;
          steerCmd: number;
          ctrlAgeMs: number;
          setupBits: number;
      }
    | {
          /** uDV 0x7A3 — health. Heap in words (×4 = bytes). `taskMask`:
           *  b0 IMU, b1 CAN, b2 APP. `flags`: b0 IWDG-reset, b1 emergency. */
          kind: 'udvHealth';
          freeHeapWords: number;
          minFreeHeapWords: number;
          taskMask: number;
          flags: number;
          stalledTask: number;
          uptimeS: number;
      }
    | {
          /** uDV 0x7A4 — firmware identity. */
          kind: 'udvFwInfo';
          gitHash: number;
          stubMask: number;
          heapSizeKb: number;
          uptimeS: number;
      }
    | {
          /** uDV 0x7A5 — FDCAN1 CAN-health. `flags`: b0 bus-off,
           *  b1 error-passive, b2 warning. */
          kind: 'udvCanHealth';
          flags: number;
          lastErrorCode: number;
          txErrCount: number;
          rxErrCount: number;
          resRxCount: number;
          nmtCount: number;
          ackError: boolean;
      }
    | {
          /** uDV 0x7A6 — steering end-stop calibration status (#428).
           *  Angles are deci-degrees (÷10 = degrees). */
          kind: 'udvCalib';
          phase: number;
          phaseName: string;
          error: number;
          errorName: string;
          centerDdeg: number;
          halfRangeDdeg: number;
          limitDdeg: number;
      }
    | {
          /** uDV 0x7A7 — live steering angle (uDV #123, #439). Angles are
           *  deci-degrees (÷10 = degrees). `motorState` is signed
           *  (−1 emergency / 0 off / 1 on / 2 calibrating). */
          kind: 'udvSteer';
          lwsRawDdeg: number;
          steerActualDdeg: number;
          steerTargetDdeg: number;
          lwsStatus: number;
          motorState: number;
          motorStateName: string;
      };

/** The inverter-temperature value that means "sensor disconnected"
 *  (raw 0xFF − 50). */
export const ECU_INV_TEMP_DISCONNECTED_C = 205;

// ---- Wrappers ----

export function pitDiagEnable(request: PitDiagRequest): Promise<void> {
    return invoke<void>('pit_diag_enable', { request });
}

export function pitDiagDisable(request: PitDiagRequest): Promise<void> {
    return invoke<void>('pit_diag_disable', { request });
}

/** Trigger (or abort) the uDV steering end-stop calibration (#428). Only
 *  valid while a uDV pit-diag session is armed. */
export function pitDiagUdvCalibrate(start: boolean): Promise<void> {
    return invoke<void>('pit_diag_udv_calibrate', { start });
}

export function onPitDiagFrame(
    handler: (event: PitDiagEvent) => void,
): Promise<UnlistenFn> {
    return listen<PitDiagEvent>('pit-diag:frame', (e) => handler(e.payload));
}

export function onPitDiagStatus(
    handler: (status: PitDiagStatus) => void,
): Promise<UnlistenFn> {
    return listen<PitDiagStatus>('pit-diag:status', (e) => handler(e.payload));
}

// ---- Display helpers ----

/**
 * Snap a cell's index to its (module, slot-within-module) coords.
 * Module 0 = cells 0..18, module 1 = cells 19..37, etc.
 */
export function cellCoords(cellIdx: number): { module: number; slot: number } {
    return {
        module: Math.floor(cellIdx / AMS_CELLS_PER_MODULE),
        slot: cellIdx % AMS_CELLS_PER_MODULE,
    };
}

/** Same idea for NTCs. */
export function ntcCoords(ntcIdx: number): { module: number; slot: number } {
    return {
        module: Math.floor(ntcIdx / AMS_NTC_PER_MODULE),
        slot: ntcIdx % AMS_NTC_PER_MODULE,
    };
}

/**
 * Pack the four-element voltage tuple from a CellVoltage frame back
 * into the pack-wide array, skipping the sentinel. Returns the
 * cells actually written (1..=4) so the caller can update its
 * count of "frames received this scan".
 */
export function writeCellsInto(
    cells: (number | null)[],
    frame: Extract<PitDiagEvent, { kind: 'cellVoltage' }>,
): number {
    let written = 0;
    for (let i = 0; i < 4; i++) {
        const cellIdx = frame.firstCell + i;
        if (cellIdx >= AMS_NUM_CELLS) break;
        const mv = frame.voltagesMv[i];
        if (mv === AMS_CELLV_SENTINEL) break;
        cells[cellIdx] = mv;
        written += 1;
    }
    return written;
}

/** Same idea for NTCs — no sentinel-by-value, but the unwired
 *  channels report INT8_MIN, which we map to `null` for the UI. */
export function writeNtcsInto(
    ntcs: (number | null)[],
    frame: Extract<PitDiagEvent, { kind: 'ntcTemp' }>,
): number {
    let written = 0;
    for (let i = 0; i < 8; i++) {
        const ntcIdx = frame.firstNtc + i;
        if (ntcIdx >= AMS_NUM_NTCS) break;
        const c = frame.tempsC[i];
        ntcs[ntcIdx] = c === AMS_NTC_SENTINEL ? null : c;
        written += 1;
    }
    return written;
}
