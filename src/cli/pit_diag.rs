//! `cf pit-diag` — terminal-side AMS observer.
//!
//! Three subcommands:
//!
//! ```text
//! cf pit-diag enable                 # arm the stream
//! cf pit-diag disable                # disarm the stream
//! cf pit-diag stream [--json]        # arm + stream + disarm-on-exit
//! ```
//!
//! Wraps the `pit_diag` library module (handshake + decoders) with a
//! tokio runtime + a Ctrl-C / duration-bound loop. Useful for:
//!
//!  - Bench validation without firing up MingoCAN. Pipe `stream
//!    --json` through `jq` to grep specific fields, or watch the
//!    human-readable form roll past on a serial terminal.
//!  - CI smoke checks. The virtual-bus stub can play canned frames
//!    and this binary decodes them, exiting non-zero on schema drift.
//!  - A reference decoder MingoCAN could shell out to in a pinch
//!    (the Tauri command currently runs in-process; if the workspace
//!    ever splits further, this CLI is the canonical implementation).
//!
//! The profile flag only accepts `"ams"` today. The shape exists for
//! the slice 5 plugin layer (VCU + UDV profiles) so any external
//! script written against today's flags keeps working once more
//! profiles land.

use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};

use crate::cli::{GlobalFlags, InterfaceType};
use crate::pit_diag::{
    build_arm_frame, decode_frame, FaultReason, PitDiagFrame, AMS_ACK_ID,
    AMS_EXPECTED_FRAMES_PER_SCAN,
};
use crate::transport::{open_backend, CanBackend, TransportError};

/// How long to wait for the AMS to ACK an arm / disarm command. The
/// firmware ACKs within ~10ms in the happy case; 2s leaves room for
/// a noisy bus + slow USB adapter without making `cf pit-diag enable`
/// feel unresponsive.
const ACK_TIMEOUT: Duration = Duration::from_millis(2_000);

/// Per-recv timeout inside the stream loop. Short enough that the
/// Ctrl-C / duration check happens within one cycle.
const POLL_TIMEOUT: Duration = Duration::from_millis(50);

// ---- Argument parsing -------------------------------------------

#[derive(Debug, Args)]
pub struct PitDiagArgs {
    #[command(subcommand)]
    pub command: PitDiagCommand,
}

#[derive(Debug, Subcommand)]
pub enum PitDiagCommand {
    /// Arm the pit-diag stream — sends `0x7F0#DEADBEEF` and waits
    /// for the `0x7F1` ACK. Idempotent: re-arming an already-armed
    /// stream is a no-op on the firmware side.
    Enable(ProfileArgs),

    /// Disarm the pit-diag stream — sends `0x7F0#00000000` and waits
    /// for the `0x7F1` ACK. The firmware also clears the flag on
    /// reboot, so this is belt-and-braces against a tool crash.
    Disable(ProfileArgs),

    /// Arm, decode the stream to stdout, then disarm on exit.
    ///
    /// Default output is one human-readable line per decoded record.
    /// `--json` switches to NDJSON (one JSON object per line) for
    /// pipe-into-jq workflows. Exit reasons:
    ///
    ///   - `--duration` seconds elapsed → exit 0
    ///   - Ctrl-C (SIGINT)              → exit 0, with disarm
    ///   - Bus error                    → exit non-zero
    ///   - Schema drift (frames/scan
    ///     diverges from expected 53)   → exit non-zero
    Stream(StreamArgs),
}

#[derive(Debug, Args)]
pub struct ProfileArgs {
    /// Which ECU profile to address. Only `ams` is supported today;
    /// the flag exists for forward compatibility with VCU + UDV.
    #[arg(long, default_value = "ams")]
    pub profile: String,
}

#[derive(Debug, Args)]
pub struct StreamArgs {
    /// Which ECU profile to address. Only `ams` is supported today.
    #[arg(long, default_value = "ams")]
    pub profile: String,

    /// Stop after this many seconds. Omit to stream until Ctrl-C.
    #[arg(long)]
    pub duration: Option<u64>,

    /// Fail the run if any 1-second window's frame count drifts from
    /// the expected per-profile total (51 for AMS today) by more
    /// than ±2. Off by default because operators inspecting a
    /// known-broken bus want to *see* the wrong count, not have the
    /// tool bail. Enable in CI / scripted bench checks.
    #[arg(long, default_value_t = false)]
    pub strict_scan: bool,
}

// ---- Dispatch ---------------------------------------------------

pub async fn run(args: PitDiagArgs, global: &GlobalFlags) -> Result<()> {
    match args.command {
        PitDiagCommand::Enable(a) => run_enable(a, global).await,
        PitDiagCommand::Disable(a) => run_disable(a, global).await,
        PitDiagCommand::Stream(a) => run_stream(a, global).await,
    }
}

// ---- enable / disable -------------------------------------------

async fn run_enable(args: ProfileArgs, global: &GlobalFlags) -> Result<()> {
    require_ams_profile(&args.profile)?;
    let backend = open(global)?;
    arm(&*backend, true)
        .await
        .context("arming AMS pit-diag stream")?;
    print_ok(global.json, &args.profile, true);
    Ok(())
}

async fn run_disable(args: ProfileArgs, global: &GlobalFlags) -> Result<()> {
    require_ams_profile(&args.profile)?;
    let backend = open(global)?;
    arm(&*backend, false)
        .await
        .context("disarming AMS pit-diag stream")?;
    print_ok(global.json, &args.profile, false);
    Ok(())
}

fn print_ok(json: bool, profile: &str, enabled: bool) {
    if json {
        println!(r#"{{"profile":"{profile}","enabled":{enabled}}}"#);
    } else {
        let verb = if enabled { "armed" } else { "disarmed" };
        println!("✓ {profile} pit-diag {verb}");
    }
}

// ---- stream -----------------------------------------------------

async fn run_stream(args: StreamArgs, global: &GlobalFlags) -> Result<()> {
    require_ams_profile(&args.profile)?;
    let backend = open(global)?;

    arm(&*backend, true)
        .await
        .context("arming AMS pit-diag stream")?;
    if !global.json {
        eprintln!("✓ {} pit-diag armed — streaming…", args.profile);
    }

    // Race the stream loop against Ctrl-C + the optional duration.
    // Whatever completes first triggers a graceful shutdown.
    let started = Instant::now();
    let duration = args.duration.map(Duration::from_secs);

    let loop_result = tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            if !global.json {
                eprintln!("\n— Ctrl-C — disarming");
            }
            Ok(())
        }
        result = stream_loop(&*backend, &args, started, duration, global.json) => result,
    };

    // Disarm best-effort, even after a bus error in the loop. A
    // failed disarm just means the AMS keeps streaming until the
    // operator power-cycles it — annoying but not dangerous.
    let disarm_result = arm(&*backend, false).await;
    if !global.json {
        match &disarm_result {
            Ok(()) => eprintln!("✓ {} pit-diag disarmed", args.profile),
            Err(err) => eprintln!("⚠ disarm failed: {err}"),
        }
    }

    loop_result?;
    Ok(())
}

/// Inner stream loop — reads frames until duration / strict-scan
/// trip / bus error. Ctrl-C is handled one level up via `select!`.
async fn stream_loop(
    backend: &dyn CanBackend,
    args: &StreamArgs,
    started: Instant,
    duration: Option<Duration>,
    json: bool,
) -> Result<()> {
    // Scan-rate tracking — mirrors the Studio's schema-drift safety
    // net. Each second we snapshot the running tally and reset.
    let mut frames_this_scan: usize = 0;
    let mut last_scan_at = Instant::now();

    loop {
        if let Some(d) = duration {
            if started.elapsed() >= d {
                return Ok(());
            }
        }

        // 1-second scan-rate roll. Done before the recv so a slow
        // bus doesn't delay the roll indefinitely.
        if last_scan_at.elapsed() >= Duration::from_secs(1) {
            if args.strict_scan
                && frames_this_scan > 0
                && frames_this_scan.abs_diff(AMS_EXPECTED_FRAMES_PER_SCAN) > 2
            {
                return Err(anyhow!(
                    "schema drift: last scan had {frames_this_scan} frames, expected {} (±2). \
                     The AMS firmware's pit-diag wire shape may have changed since this tool \
                     was built — check src/pit_diag/mod.rs against IFS08-CE-AMS/docs/CAN_MAP.md.",
                    AMS_EXPECTED_FRAMES_PER_SCAN
                ));
            }
            frames_this_scan = 0;
            last_scan_at = Instant::now();
        }

        match backend.recv(POLL_TIMEOUT).await {
            Ok(frame) => {
                if let Some(record) = decode_frame(&frame) {
                    let ts_ms = started.elapsed().as_millis() as u64;
                    // Don't count the ACK frame — it's not part of
                    // the 1Hz scan.
                    if !matches!(record, PitDiagFrame::Ack { .. }) {
                        frames_this_scan += 1;
                    }
                    if json {
                        print_record_json(ts_ms, &record);
                    } else {
                        print_record_human(ts_ms, &record);
                    }
                }
                // Non-pit-diag frames are silently dropped — the
                // bus monitor is the place to see other traffic.
            }
            Err(TransportError::Timeout(_)) => {
                // Expected during the gaps between scans — keep
                // polling.
            }
            Err(err) => {
                return Err(anyhow!("bus error: {err}"));
            }
        }
    }
}

// ---- Output formatting ------------------------------------------

fn print_record_human(ts_ms: u64, record: &PitDiagFrame) {
    let prefix = format!("[+{:>7.3}s]", (ts_ms as f64) / 1000.0);
    match record {
        PitDiagFrame::Ack { enabled } => {
            eprintln!("{prefix} ack enabled={enabled}");
        }
        PitDiagFrame::CellVoltage(c) => {
            let cells = c
                .voltages_mv
                .iter()
                .map(|mv| format!("{mv:>4}"))
                .collect::<Vec<_>>()
                .join(" ");
            println!(
                "{prefix} cell  frame={:>2} cells[{:>2}..{:>2}] = {cells} mV",
                c.frame_idx,
                c.first_cell,
                c.first_cell + 4,
            );
        }
        PitDiagFrame::NtcTemp(t) => {
            let temps = t
                .temps_c
                .iter()
                .map(|c| format!("{c:>4}"))
                .collect::<Vec<_>>()
                .join(" ");
            println!(
                "{prefix} ntc   frame={:>2} ntcs[{:>3}..{:>3}] = {temps} °C",
                t.frame_idx,
                t.first_ntc,
                t.first_ntc + 8,
            );
        }
        PitDiagFrame::FsmStatus(f) => {
            // Only print the fault columns when something's latched —
            // keeps the happy-path line short.
            let fault = if matches!(f.fault_reason, FaultReason::None) {
                String::new()
            } else {
                format!(" fault={:?} detail={}", f.fault_reason, f.fault_detail)
            };
            println!(
                "{prefix} fsm   state={:?} mode={:?} tsms={} dash={} ams_ok={} pec={}{fault}",
                f.state,
                f.mode_locked,
                f.tsms as u8,
                f.dash_chg as u8,
                f.ams_ok as u8,
                f.pec_error_total,
            );
        }
        PitDiagFrame::PollTiming(p) => {
            println!(
                "{prefix} poll  v_last={:>4}ms v_worst={:>4}ms tsweep_fail=0x{:08X}",
                p.last_v_poll_ms, p.worst_v_poll_ms, p.t_sweep_fail_mask,
            );
        }
        PitDiagFrame::PerIcPec(p) => {
            let counts = p
                .counts
                .iter()
                .take(p.valid as usize)
                .map(|c| format!("{c:>3}"))
                .collect::<Vec<_>>()
                .join(" ");
            println!(
                "{prefix} pec   ICs[{:>2}..{:>2}] = {counts}",
                p.first_ic,
                p.first_ic + p.valid,
            );
        }
    }
}

fn print_record_json(ts_ms: u64, record: &PitDiagFrame) {
    // Hand-written JSON to keep the line format stable + avoid a
    // serde_json transitive dep dragging into the CLI's already-lean
    // dependency tree. Field names match the Studio's
    // PitDiagEvent serde shape so any consumer scripting against
    // either side can share parsers.
    match record {
        PitDiagFrame::Ack { enabled } => {
            println!(r#"{{"tsMs":{ts_ms},"kind":"ack","enabled":{enabled}}}"#);
        }
        PitDiagFrame::CellVoltage(c) => {
            println!(
                r#"{{"tsMs":{ts_ms},"kind":"cellVoltage","frameIdx":{},"firstCell":{},"voltagesMv":[{}]}}"#,
                c.frame_idx,
                c.first_cell,
                c.voltages_mv
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        PitDiagFrame::NtcTemp(t) => {
            println!(
                r#"{{"tsMs":{ts_ms},"kind":"ntcTemp","frameIdx":{},"firstNtc":{},"tempsC":[{}]}}"#,
                t.frame_idx,
                t.first_ntc,
                t.temps_c
                    .iter()
                    .map(i8::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        PitDiagFrame::FsmStatus(f) => {
            // FSM state / mode_locked / fault_reason are typed enums
            // on this side; mirror the Studio's stringified shape so
            // consumers can switch on the value without a translation
            // table.
            println!(
                r#"{{"tsMs":{ts_ms},"kind":"fsmStatus","state":"{:?}","modeLocked":"{:?}","tsms":{},"dashChg":{},"amsOk":{},"pecErrorTotal":{},"faultReason":"{:?}","faultDetail":{}}}"#,
                f.state,
                f.mode_locked,
                f.tsms,
                f.dash_chg,
                f.ams_ok,
                f.pec_error_total,
                f.fault_reason,
                f.fault_detail,
            );
        }
        PitDiagFrame::PollTiming(p) => {
            println!(
                r#"{{"tsMs":{ts_ms},"kind":"pollTiming","lastVPollMs":{},"worstVPollMs":{},"tSweepFailMask":{}}}"#,
                p.last_v_poll_ms, p.worst_v_poll_ms, p.t_sweep_fail_mask,
            );
        }
        PitDiagFrame::PerIcPec(p) => {
            println!(
                r#"{{"tsMs":{ts_ms},"kind":"perIcPec","firstIc":{},"valid":{},"counts":[{}]}}"#,
                p.first_ic,
                p.valid,
                p.counts
                    .iter()
                    .take(p.valid as usize)
                    .map(u8::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
    }
}

// ---- Helpers ----------------------------------------------------

fn require_ams_profile(profile: &str) -> Result<()> {
    if profile != "ams" {
        return Err(anyhow!(
            "unknown pit-diag profile {profile:?}: only 'ams' is supported today \
             (VCU + UDV land in the slice-5 plugin work)"
        ));
    }
    Ok(())
}

fn open(global: &GlobalFlags) -> Result<Box<dyn CanBackend>> {
    open_backend(
        map_interface(global.interface),
        global.channel.as_deref(),
        global.bitrate,
    )
    .map_err(|e| anyhow!("opening backend: {e}"))
}

/// Convert the CLI's `InterfaceType` enum into the transport-layer
/// equivalent. Same mapping the other subcommands use — promoted
/// from there to keep this file self-contained.
fn map_interface(i: InterfaceType) -> InterfaceType {
    // The transport layer's `open_backend` takes our InterfaceType
    // directly today; this indirection is a no-op but documents the
    // boundary in case the two types diverge in the future.
    i
}

/// Send the arm (or disarm) frame and wait for the `0x7F1` ACK with
/// the matching `enabled` flag. Returns `Err` on timeout / wrong-
/// flavour ACK / bus error.
async fn arm(backend: &dyn CanBackend, enable: bool) -> Result<()> {
    backend
        .send(build_arm_frame(enable))
        .await
        .map_err(|e| anyhow!("sending arm frame: {e}"))?;

    let started = Instant::now();
    while started.elapsed() < ACK_TIMEOUT {
        match backend.recv(POLL_TIMEOUT).await {
            Ok(frame) if frame.id == AMS_ACK_ID => {
                if let Some(PitDiagFrame::Ack { enabled }) = decode_frame(&frame) {
                    if enabled == enable {
                        return Ok(());
                    }
                    // Wrong-flavour ACK (e.g. a stale `01` echo while
                    // we're trying to disarm). Keep waiting for the
                    // matching one; the firmware re-fires on every
                    // state change.
                }
            }
            Ok(_) => {
                // Some other ID during the arm window — ignore.
            }
            Err(TransportError::Timeout(_)) => {
                // Quiet bus during the poll window. Keep waiting.
            }
            Err(err) => {
                return Err(anyhow!("waiting for ACK: {err}"));
            }
        }
    }
    Err(anyhow!(
        "no {} ACK from AMS within {}ms — is it on the bus, with pit-diag firmware?",
        if enable { "enable" } else { "disable" },
        ACK_TIMEOUT.as_millis()
    ))
}
